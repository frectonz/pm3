use crate::config::{ProcessConfig, RestartPolicy};
use crate::log::{self, LogEntry, LogStream};
use crate::paths::Paths;
use crate::protocol::{ProcessInfo, ProcessStatus};
use std::collections::HashMap;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;
use tokio::fs;
use tokio::process::{Child, Command};
use tokio::sync::{RwLock, broadcast, watch};

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

pub const DEFAULT_KILL_TIMEOUT_MS: u64 = 5000;
pub const DEFAULT_KILL_SIGNAL: &str = "SIGTERM";
pub const DEFAULT_MAX_RESTARTS: u32 = 15;
pub const BACKOFF_BASE_MS: u64 = 100;
pub const BACKOFF_CAP_MS: u64 = 30_000;
pub const DEFAULT_MIN_UPTIME_MS: u64 = 1000;

// ---------------------------------------------------------------------------
// Error
// ---------------------------------------------------------------------------

#[derive(Debug, thiserror::Error)]
pub enum ProcessError {
    #[error("invalid command: {0}")]
    InvalidCommand(String),
    #[error("failed to spawn process: {0}")]
    SpawnFailed(#[from] std::io::Error),
    #[error("process not found: {0}")]
    NotFound(String),
    #[error("invalid signal: {0}")]
    InvalidSignal(String),
}

// ---------------------------------------------------------------------------
// Command parsing
// ---------------------------------------------------------------------------

pub fn parse_command(command: &str) -> Result<(String, Vec<String>), ProcessError> {
    let words = shell_words::split(command)
        .map_err(|e| ProcessError::InvalidCommand(format!("failed to parse: {e}")))?;

    if words.is_empty() {
        return Err(ProcessError::InvalidCommand("command is empty".to_string()));
    }

    let program = words[0].clone();
    let args = words[1..].to_vec();
    Ok((program, args))
}

// ---------------------------------------------------------------------------
// Signal parsing
// ---------------------------------------------------------------------------

pub fn parse_signal(name: &str) -> Result<nix::sys::signal::Signal, ProcessError> {
    let normalized = if name.starts_with("SIG") {
        name.to_string()
    } else {
        format!("SIG{name}")
    };
    nix::sys::signal::Signal::from_str(&normalized)
        .map_err(|_| ProcessError::InvalidSignal(name.to_string()))
}

// ---------------------------------------------------------------------------
// ManagedProcess
// ---------------------------------------------------------------------------

pub struct ManagedProcess {
    pub name: String,
    pub config: ProcessConfig,
    pub pid: Option<u32>,
    pub status: ProcessStatus,
    pub started_at: tokio::time::Instant,
    pub restarts: u32,
    pub log_broadcaster: broadcast::Sender<LogEntry>,
    pub monitor_shutdown: Option<watch::Sender<bool>>,
}

impl ManagedProcess {
    pub fn to_process_info(&self) -> ProcessInfo {
        ProcessInfo {
            name: self.name.clone(),
            pid: self.pid,
            status: self.status,
            uptime: Some(self.started_at.elapsed().as_secs()),
            restarts: self.restarts,
            cpu_percent: None,
            memory_bytes: None,
            group: self.config.group.clone(),
        }
    }

    pub async fn graceful_stop(&mut self) -> Result<(), ProcessError> {
        // Signal the monitor not to auto-restart
        if let Some(ref tx) = self.monitor_shutdown {
            let _ = tx.send(true);
        }

        let raw_pid = match self.pid {
            Some(pid) => pid,
            None => {
                self.status = ProcessStatus::Stopped;
                return Ok(());
            }
        };

        let signal_name = self
            .config
            .kill_signal
            .as_deref()
            .unwrap_or(DEFAULT_KILL_SIGNAL);
        let signal = parse_signal(signal_name)?;

        let timeout_ms = self.config.kill_timeout.unwrap_or(DEFAULT_KILL_TIMEOUT_MS);
        let duration = Duration::from_millis(timeout_ms);

        let pid = nix::unistd::Pid::from_raw(raw_pid as i32);
        let _ = nix::sys::signal::kill(pid, signal);

        // Poll for process exit
        let deadline = tokio::time::Instant::now() + duration;
        while nix::sys::signal::kill(pid, None).is_ok() {
            if tokio::time::Instant::now() >= deadline {
                // Timeout — escalate to SIGKILL
                let _ = nix::sys::signal::kill(pid, nix::sys::signal::Signal::SIGKILL);
                // Brief wait for SIGKILL to take effect
                tokio::time::sleep(Duration::from_millis(100)).await;
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }

        self.pid = None;
        self.status = ProcessStatus::Stopped;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// ProcessTable
// ---------------------------------------------------------------------------

pub type ProcessTable = HashMap<String, ManagedProcess>;

// ---------------------------------------------------------------------------
// Spawning
// ---------------------------------------------------------------------------

pub async fn spawn_process(
    name: String,
    config: ProcessConfig,
    paths: &Paths,
) -> Result<(ManagedProcess, Child), ProcessError> {
    let (program, args) = parse_command(&config.command)?;

    fs::create_dir_all(paths.log_dir()).await?;

    let mut cmd = Command::new(&program);
    cmd.args(&args);

    if let Some(ref cwd) = config.cwd {
        cmd.current_dir(cwd);
    }

    cmd.stdin(std::process::Stdio::null());
    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::piped());

    let mut child = cmd.spawn().map_err(ProcessError::SpawnFailed)?;
    let pid = child.id();

    let (log_tx, _) = broadcast::channel(1024);
    let (monitor_tx, _monitor_rx) = watch::channel(false);

    // Take ownership of piped streams
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();

    let log_date_format = config.log_date_format.clone();

    // Spawn log copiers
    if let Some(stdout) = stdout {
        log::spawn_log_copier(
            name.clone(),
            LogStream::Stdout,
            stdout,
            paths.stdout_log(&name),
            log_date_format.clone(),
            log_tx.clone(),
        );
    }

    if let Some(stderr) = stderr {
        log::spawn_log_copier(
            name.clone(),
            LogStream::Stderr,
            stderr,
            paths.stderr_log(&name),
            log_date_format,
            log_tx.clone(),
        );
    }

    let managed = ManagedProcess {
        name,
        config,
        pid,
        status: ProcessStatus::Online,
        started_at: tokio::time::Instant::now(),
        restarts: 0,
        log_broadcaster: log_tx,
        monitor_shutdown: Some(monitor_tx),
    };

    Ok((managed, child))
}

// ---------------------------------------------------------------------------
// Restart policy evaluation
// ---------------------------------------------------------------------------

pub fn evaluate_restart_policy(
    config: &ProcessConfig,
    exit_code: Option<i32>,
    _uptime: Duration,
    restarts: u32,
) -> bool {
    let policy = config.restart.clone().unwrap_or(RestartPolicy::OnFailure);
    let max_restarts = config.max_restarts.unwrap_or(DEFAULT_MAX_RESTARTS);

    // Check if we've exceeded max restarts (reset logic handled by caller for min_uptime)
    if restarts >= max_restarts {
        return false;
    }

    match policy {
        RestartPolicy::Never => false,
        RestartPolicy::Always => true,
        RestartPolicy::OnFailure => {
            match exit_code {
                Some(0) => false,
                Some(code) => {
                    // Check stop_exit_codes
                    if let Some(ref stop_codes) = config.stop_exit_codes
                        && stop_codes.contains(&code)
                    {
                        return false;
                    }
                    true
                }
                None => true, // Signal-killed — treat as failure
            }
        }
    }
}

/// Compute exponential backoff delay: 100ms * 2^count, capped at 30s
pub fn compute_backoff(restart_count: u32) -> Duration {
    let ms = BACKOFF_BASE_MS.saturating_mul(2u64.saturating_pow(restart_count));
    Duration::from_millis(ms.min(BACKOFF_CAP_MS))
}

// ---------------------------------------------------------------------------
// Process monitor task
// ---------------------------------------------------------------------------

pub fn spawn_monitor(
    name: String,
    mut child: Child,
    monitored_pid: Option<u32>,
    processes: Arc<RwLock<ProcessTable>>,
    paths: Paths,
    _shutdown_rx: watch::Receiver<bool>,
) {
    tokio::spawn(async move {
        // Wait for child to exit (graceful_stop handles killing via PID signals)
        let status = child.wait().await;
        let exit_code = status.ok().and_then(|s| s.code());
        handle_child_exit(&name, monitored_pid, exit_code, &processes, &paths).await;
    });
}

async fn handle_child_exit(
    name: &str,
    monitored_pid: Option<u32>,
    exit_code: Option<i32>,
    processes: &Arc<RwLock<ProcessTable>>,
    paths: &Paths,
) {
    let (config, uptime, restarts, should_restart);

    {
        let mut table = processes.write().await;
        let Some(managed) = table.get_mut(name) else {
            return;
        };

        // If the process has been replaced (e.g., by a manual restart), skip
        if managed.pid != monitored_pid || (managed.pid.is_some() && monitored_pid.is_none()) {
            return;
        }

        // If shutdown was already signaled (manual stop), don't restart
        if let Some(ref tx) = managed.monitor_shutdown
            && *tx.borrow()
        {
            managed.status = ProcessStatus::Stopped;
            managed.pid = None;
            return;
        }

        let uptime_dur = managed.started_at.elapsed();
        let min_uptime_ms = managed.config.min_uptime.unwrap_or(DEFAULT_MIN_UPTIME_MS);

        // If uptime >= min_uptime, process was stable — reset restart counter
        if uptime_dur >= Duration::from_millis(min_uptime_ms) {
            managed.restarts = 0;
        }

        config = managed.config.clone();
        uptime = uptime_dur;
        restarts = managed.restarts;
        should_restart = evaluate_restart_policy(&config, exit_code, uptime, restarts);

        if !should_restart {
            if exit_code == Some(0) {
                managed.status = ProcessStatus::Stopped;
            } else {
                managed.status = ProcessStatus::Errored;
            }
            managed.pid = None;
            return;
        }

        // Mark as restarting
        managed.pid = None;
    }

    // Compute backoff and sleep outside the lock
    let backoff = compute_backoff(restarts);
    tokio::time::sleep(backoff).await;

    // Re-acquire lock and spawn new process
    let mut table = processes.write().await;
    let Some(managed) = table.get_mut(name) else {
        return;
    };

    // Re-check shutdown wasn't signaled while we were sleeping
    if let Some(ref tx) = managed.monitor_shutdown
        && *tx.borrow()
    {
        managed.status = ProcessStatus::Stopped;
        return;
    }

    match spawn_process(name.to_string(), config, paths).await {
        Ok((mut new_managed, new_child)) => {
            new_managed.restarts = restarts + 1;
            let new_pid = new_managed.pid;
            let shutdown_rx = new_managed
                .monitor_shutdown
                .as_ref()
                .map(|tx| tx.subscribe())
                .unwrap();

            *managed = new_managed;

            // Must drop the lock before spawning monitor (it needs lock access)
            let procs = Arc::clone(processes);
            let p = paths.clone();
            let n = name.to_string();
            drop(table);
            spawn_monitor(n, new_child, new_pid, procs, p, shutdown_rx);
        }
        Err(e) => {
            eprintln!("failed to restart '{name}': {e}");
            managed.status = ProcessStatus::Errored;
            managed.pid = None;
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_simple_command() {
        let (prog, args) = parse_command("node server.js").unwrap();
        assert_eq!(prog, "node");
        assert_eq!(args, vec!["server.js"]);
    }

    #[test]
    fn test_parse_command_no_args() {
        let (prog, args) = parse_command("sleep").unwrap();
        assert_eq!(prog, "sleep");
        assert!(args.is_empty());
    }

    #[test]
    fn test_parse_command_multiple_args() {
        let (prog, args) = parse_command("echo hello world").unwrap();
        assert_eq!(prog, "echo");
        assert_eq!(args, vec!["hello", "world"]);
    }

    #[test]
    fn test_parse_command_quoted_args() {
        let (prog, args) = parse_command(r#"bash -c "echo hello""#).unwrap();
        assert_eq!(prog, "bash");
        assert_eq!(args, vec!["-c", "echo hello"]);
    }

    #[test]
    fn test_parse_command_single_quotes() {
        let (prog, args) = parse_command("echo 'hello world'").unwrap();
        assert_eq!(prog, "echo");
        assert_eq!(args, vec!["hello world"]);
    }

    #[test]
    fn test_parse_empty_command() {
        let result = parse_command("");
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            ProcessError::InvalidCommand(_)
        ));
    }

    #[test]
    fn test_parse_whitespace_only() {
        let result = parse_command("   ");
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            ProcessError::InvalidCommand(_)
        ));
    }

    // -------------------------------------------------------------------
    // Graceful stop constants & parse_signal
    // -------------------------------------------------------------------

    #[test]
    fn test_default_kill_timeout() {
        assert_eq!(DEFAULT_KILL_TIMEOUT_MS, 5000);
    }

    #[test]
    fn test_default_kill_signal() {
        assert_eq!(DEFAULT_KILL_SIGNAL, "SIGTERM");
    }

    #[test]
    fn test_parse_signal_sigterm() {
        let sig = parse_signal("SIGTERM").unwrap();
        assert_eq!(sig, nix::sys::signal::Signal::SIGTERM);
    }

    #[test]
    fn test_parse_signal_sigint() {
        let sig = parse_signal("SIGINT").unwrap();
        assert_eq!(sig, nix::sys::signal::Signal::SIGINT);
    }

    #[test]
    fn test_parse_signal_sighup() {
        let sig = parse_signal("SIGHUP").unwrap();
        assert_eq!(sig, nix::sys::signal::Signal::SIGHUP);
    }

    #[test]
    fn test_parse_signal_sigusr1() {
        let sig = parse_signal("SIGUSR1").unwrap();
        assert_eq!(sig, nix::sys::signal::Signal::SIGUSR1);
    }

    #[test]
    fn test_parse_signal_sigusr2() {
        let sig = parse_signal("SIGUSR2").unwrap();
        assert_eq!(sig, nix::sys::signal::Signal::SIGUSR2);
    }

    #[test]
    fn test_parse_signal_without_sig_prefix() {
        let sig = parse_signal("TERM").unwrap();
        assert_eq!(sig, nix::sys::signal::Signal::SIGTERM);
    }

    #[test]
    fn test_parse_signal_invalid() {
        let result = parse_signal("BOGUS");
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            ProcessError::InvalidSignal(_)
        ));
    }

    #[test]
    fn test_parse_signal_empty() {
        let result = parse_signal("");
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            ProcessError::InvalidSignal(_)
        ));
    }

    #[test]
    fn test_config_kill_timeout_default_when_none() {
        assert_eq!(DEFAULT_KILL_TIMEOUT_MS, 5000);
    }

    #[test]
    fn test_config_kill_signal_default_when_none() {
        assert_eq!(DEFAULT_KILL_SIGNAL, "SIGTERM");
    }

    // -------------------------------------------------------------------
    // Restart policy
    // -------------------------------------------------------------------

    fn test_config(restart: Option<RestartPolicy>) -> ProcessConfig {
        ProcessConfig {
            command: "echo test".to_string(),
            cwd: None,
            env: None,
            env_file: None,
            health_check: None,
            kill_timeout: None,
            kill_signal: None,
            max_restarts: None,
            max_memory: None,
            min_uptime: None,
            stop_exit_codes: None,
            watch: None,
            watch_ignore: None,
            depends_on: None,
            restart,
            group: None,
            pre_start: None,
            post_stop: None,
            notify: None,
            cron_restart: None,
            log_date_format: None,
            environments: HashMap::new(),
        }
    }

    #[test]
    fn test_restart_never() {
        let config = test_config(Some(RestartPolicy::Never));
        assert!(!evaluate_restart_policy(
            &config,
            Some(1),
            Duration::from_secs(0),
            0
        ));
        assert!(!evaluate_restart_policy(
            &config,
            Some(0),
            Duration::from_secs(0),
            0
        ));
    }

    #[test]
    fn test_restart_always() {
        let config = test_config(Some(RestartPolicy::Always));
        assert!(evaluate_restart_policy(
            &config,
            Some(0),
            Duration::from_secs(0),
            0
        ));
        assert!(evaluate_restart_policy(
            &config,
            Some(1),
            Duration::from_secs(0),
            0
        ));
    }

    #[test]
    fn test_restart_on_failure_exit_zero() {
        let config = test_config(Some(RestartPolicy::OnFailure));
        assert!(!evaluate_restart_policy(
            &config,
            Some(0),
            Duration::from_secs(0),
            0
        ));
    }

    #[test]
    fn test_restart_on_failure_exit_nonzero() {
        let config = test_config(Some(RestartPolicy::OnFailure));
        assert!(evaluate_restart_policy(
            &config,
            Some(1),
            Duration::from_secs(0),
            0
        ));
    }

    #[test]
    fn test_restart_default_is_on_failure() {
        let config = test_config(None);
        assert!(!evaluate_restart_policy(
            &config,
            Some(0),
            Duration::from_secs(0),
            0
        ));
        assert!(evaluate_restart_policy(
            &config,
            Some(1),
            Duration::from_secs(0),
            0
        ));
    }

    #[test]
    fn test_restart_stop_exit_codes() {
        let mut config = test_config(Some(RestartPolicy::OnFailure));
        config.stop_exit_codes = Some(vec![42, 143]);
        assert!(!evaluate_restart_policy(
            &config,
            Some(42),
            Duration::from_secs(0),
            0
        ));
        assert!(!evaluate_restart_policy(
            &config,
            Some(143),
            Duration::from_secs(0),
            0
        ));
        assert!(evaluate_restart_policy(
            &config,
            Some(1),
            Duration::from_secs(0),
            0
        ));
    }

    #[test]
    fn test_restart_max_restarts_exceeded() {
        let mut config = test_config(Some(RestartPolicy::Always));
        config.max_restarts = Some(3);
        assert!(evaluate_restart_policy(
            &config,
            Some(1),
            Duration::from_secs(0),
            2
        ));
        assert!(!evaluate_restart_policy(
            &config,
            Some(1),
            Duration::from_secs(0),
            3
        ));
        assert!(!evaluate_restart_policy(
            &config,
            Some(1),
            Duration::from_secs(0),
            4
        ));
    }

    #[test]
    fn test_restart_signal_killed_no_exit_code() {
        let config = test_config(Some(RestartPolicy::OnFailure));
        assert!(evaluate_restart_policy(
            &config,
            None,
            Duration::from_secs(0),
            0
        ));
    }

    // -------------------------------------------------------------------
    // Backoff
    // -------------------------------------------------------------------

    #[test]
    fn test_backoff_sequence() {
        assert_eq!(compute_backoff(0), Duration::from_millis(100));
        assert_eq!(compute_backoff(1), Duration::from_millis(200));
        assert_eq!(compute_backoff(2), Duration::from_millis(400));
        assert_eq!(compute_backoff(3), Duration::from_millis(800));
        assert_eq!(compute_backoff(4), Duration::from_millis(1600));
    }

    #[test]
    fn test_backoff_cap() {
        // 100 * 2^20 = 104_857_600 which exceeds cap
        assert_eq!(compute_backoff(20), Duration::from_millis(BACKOFF_CAP_MS));
        assert_eq!(compute_backoff(30), Duration::from_millis(BACKOFF_CAP_MS));
    }

    // -------------------------------------------------------------------
    // min_uptime
    // -------------------------------------------------------------------

    #[test]
    fn test_min_uptime_resets_counter_before_policy_check() {
        let mut config = test_config(Some(RestartPolicy::OnFailure));
        config.max_restarts = Some(3);
        config.min_uptime = Some(500);

        // Uptime exceeds min_uptime: counter resets, restart is allowed
        let mut restarts: u32 = 3;
        let uptime = Duration::from_millis(600);
        let min_uptime_ms = config.min_uptime.unwrap_or(DEFAULT_MIN_UPTIME_MS);
        if uptime >= Duration::from_millis(min_uptime_ms) {
            restarts = 0;
        }
        assert!(evaluate_restart_policy(&config, Some(1), uptime, restarts));

        // Uptime below min_uptime: counter stays at max, restart is blocked
        let mut restarts: u32 = 3;
        let uptime = Duration::from_millis(100);
        if uptime >= Duration::from_millis(min_uptime_ms) {
            restarts = 0;
        }
        assert!(!evaluate_restart_policy(&config, Some(1), uptime, restarts));
    }
}
