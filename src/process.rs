use crate::config::{ProcessConfig, RestartPolicy};
use crate::log::{self, LogEntry, LogStream};
use crate::paths::Paths;
use crate::protocol::{ProcessDetail, ProcessInfo, ProcessStatus};
use crate::{cron, health, memory, watch as file_watch};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::fs;
use tokio::process::{Child, Command};
use tokio::sync::{RwLock, broadcast, watch};

pub const DEFAULT_KILL_TIMEOUT_MS: u64 = 5000;
pub const DEFAULT_KILL_SIGNAL: &str = "SIGTERM";
pub const DEFAULT_MAX_RESTARTS: u32 = 15;
pub const BACKOFF_BASE_MS: u64 = 100;
pub const BACKOFF_CAP_MS: u64 = 30_000;
pub const DEFAULT_MIN_UPTIME_MS: u64 = 1000;
pub const SPAWN_VERIFY_DELAY_MS: u64 = 50;

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
    #[error("env file error: {0}")]
    EnvFile(String),
    #[error("hook failed: {0}")]
    HookFailed(String),
    #[error("process exited immediately (exit code: {exit_code:?})")]
    ImmediateExit { exit_code: Option<i32> },
}

/// Parse a command string into a program name and arguments.
///
/// On Unix, uses POSIX shell word-splitting rules (via `shell_words`).
/// On Windows, uses a simpler parser that treats backslashes as literal
/// characters so that Windows paths (e.g. `C:\Users\...`) are preserved.
#[cfg(unix)]
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

/// Parse a command string into a program name and arguments (Windows version).
///
/// Splits on whitespace, respecting double-quoted segments.
/// Backslashes are always treated as literal characters so that Windows
/// paths like `C:\Users\alpha\node.exe` are not mangled.
#[cfg(windows)]
pub fn parse_command(command: &str) -> Result<(String, Vec<String>), ProcessError> {
    let command = command.trim();
    if command.is_empty() {
        return Err(ProcessError::InvalidCommand("command is empty".to_string()));
    }

    let mut words = Vec::new();
    let mut current = String::new();
    let mut in_quotes = false;

    for c in command.chars() {
        match c {
            '"' => in_quotes = !in_quotes,
            ' ' | '\t' if !in_quotes => {
                if !current.is_empty() {
                    words.push(std::mem::take(&mut current));
                }
            }
            _ => current.push(c),
        }
    }

    if !current.is_empty() {
        words.push(current);
    }

    if words.is_empty() {
        return Err(ProcessError::InvalidCommand("command is empty".to_string()));
    }

    let program = words[0].clone();
    let args = words[1..].to_vec();
    Ok((program, args))
}

pub fn parse_signal(name: &str) -> Result<crate::sys::Signal, ProcessError> {
    crate::sys::parse_signal(name)
}

pub async fn run_hook(
    hook: &str,
    name: &str,
    cwd: Option<&str>,
    paths: &Paths,
) -> Result<(), ProcessError> {
    fs::create_dir_all(paths.log_dir()).await?;

    let stdout_file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(paths.stdout_log(name))
        .map_err(ProcessError::SpawnFailed)?;
    let stderr_file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(paths.stderr_log(name))
        .map_err(ProcessError::SpawnFailed)?;

    let mut cmd = crate::sys::hook_command(hook);
    if let Some(dir) = cwd {
        cmd.current_dir(dir);
    }
    cmd.stdin(std::process::Stdio::null());
    cmd.stdout(stdout_file);
    cmd.stderr(stderr_file);

    let status = cmd.status().await.map_err(ProcessError::SpawnFailed)?;

    if !status.success() {
        return Err(ProcessError::HookFailed(format!(
            "pre_start '{}' exited with code {}",
            hook,
            status.code().unwrap_or(-1)
        )));
    }

    Ok(())
}

pub struct ManagedProcess {
    pub name: String,
    pub config: ProcessConfig,
    pub pid: Option<u32>,
    pub status: ProcessStatus,
    pub started_at: tokio::time::Instant,
    pub restarts: u32,
    pub log_broadcaster: broadcast::Sender<LogEntry>,
    pub monitor_shutdown: Option<watch::Sender<bool>>,
    /// On Windows, a Job Object that groups the process and its children
    /// so they can all be terminated together.
    #[cfg(windows)]
    pub job_object: Option<crate::sys::JobObject>,
}

impl ManagedProcess {
    pub fn to_process_info(&self, stats_cache: &memory::StatsCache) -> ProcessInfo {
        let stats = self.pid.and_then(|pid| stats_cache.get(&pid));
        ProcessInfo {
            name: self.name.clone(),
            pid: self.pid,
            status: self.status,
            uptime: Some(self.started_at.elapsed().as_secs()),
            restarts: self.restarts,
            cpu_percent: stats.and_then(|s| s.cpu_percent),
            memory_bytes: stats.and_then(|s| s.memory_bytes),
            group: self.config.group.clone(),
        }
    }

    pub fn to_process_detail(
        &self,
        paths: &Paths,
        stats_cache: &memory::StatsCache,
    ) -> ProcessDetail {
        let stats = self.pid.and_then(|pid| stats_cache.get(&pid));
        ProcessDetail {
            name: self.name.clone(),
            pid: self.pid,
            status: self.status,
            uptime: Some(self.started_at.elapsed().as_secs()),
            restarts: self.restarts,
            cpu_percent: stats.and_then(|s| s.cpu_percent),
            memory_bytes: stats.and_then(|s| s.memory_bytes),
            group: self.config.group.clone(),
            command: self.config.command.clone(),
            cwd: self.config.cwd.clone(),
            env: self.config.env.clone(),
            exit_code: None,
            stdout_log: Some(paths.stdout_log(&self.name).to_string_lossy().into_owned()),
            stderr_log: Some(paths.stderr_log(&self.name).to_string_lossy().into_owned()),
            health_check: self.config.health_check.clone(),
            depends_on: self.config.depends_on.clone(),
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

        // Signal the entire process group so child processes are also stopped.
        if let Err(e) = crate::sys::send_signal_to_group(raw_pid, signal) {
            eprintln!("failed to send {signal_name} to process group {raw_pid}: {e}");
            if !crate::sys::is_pid_alive(raw_pid) {
                self.pid = None;
                self.status = ProcessStatus::Stopped;
                return Ok(());
            }
        }

        // Poll for process exit
        let deadline = tokio::time::Instant::now() + duration;
        while crate::sys::is_pid_alive(raw_pid) {
            if tokio::time::Instant::now() >= deadline {
                // Timeout — escalate to force kill the entire process group
                let _ = crate::sys::force_kill_group(raw_pid);
                // On Windows, also terminate via the job object to catch all children
                #[cfg(windows)]
                if let Some(ref job) = self.job_object {
                    let _ = job.terminate();
                }
                // Brief wait for kill to take effect
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

pub fn spawn_aux_monitors(
    name: String,
    config: ProcessConfig,
    processes: Arc<RwLock<ProcessTable>>,
    paths: Paths,
    shutdown_tx: watch::Sender<bool>,
) {
    if let Some(hc) = config.health_check.clone() {
        health::spawn_health_checker(
            name.clone(),
            hc,
            Arc::clone(&processes),
            shutdown_tx.subscribe(),
        );
    }
    if let Some(mm) = config.max_memory.clone() {
        memory::spawn_memory_monitor(
            name.clone(),
            mm,
            Arc::clone(&processes),
            paths.clone(),
            shutdown_tx.subscribe(),
        );
    }
    // Watcher handles watch disabled internally
    file_watch::spawn_watcher(
        name.clone(),
        config.clone(),
        Arc::clone(&processes),
        paths.clone(),
        shutdown_tx.subscribe(),
    );
    if let Some(cr) = config.cron_restart.clone() {
        cron::spawn_cron_restart(
            name,
            cr,
            Arc::clone(&processes),
            paths,
            shutdown_tx.subscribe(),
        );
    }
}

pub type ProcessTable = HashMap<String, ManagedProcess>;

pub async fn spawn_process(
    name: String,
    config: ProcessConfig,
    paths: &Paths,
) -> Result<(ManagedProcess, Child), ProcessError> {
    if let Some(ref hook) = config.pre_start {
        run_hook(hook, &name, config.cwd.as_deref(), paths).await?;
    }

    let (program, args) = parse_command(&config.command)?;

    fs::create_dir_all(paths.log_dir()).await?;

    let mut cmd = Command::new(&program);
    cmd.args(&args);

    // Put each process in its own process group so that stop/kill
    // can signal the entire tree (parent + children).
    #[cfg(unix)]
    cmd.process_group(0);

    if let Some(ref cwd) = config.cwd {
        cmd.current_dir(cwd);
    }

    if config.env_file.is_some() {
        let env_file_vars = config
            .load_env_files()
            .map_err(|e| ProcessError::EnvFile(e.to_string()))?;
        cmd.envs(&env_file_vars);
    }

    if let Some(ref env_vars) = config.env {
        cmd.envs(env_vars);
    }

    cmd.stdin(std::process::Stdio::null());

    // On Unix, use a PTY for stdout so child processes see isatty(1) == true
    // and keep line-buffered output instead of block-buffered.
    // Stderr stays piped (it's unbuffered by default in C).
    #[cfg(unix)]
    let pty_reader = {
        let (reader, slave_fd) = crate::sys::create_pty()?;
        cmd.stdout(std::process::Stdio::from(slave_fd));
        cmd.stderr(std::process::Stdio::piped());
        Some(reader)
    };
    #[cfg(not(unix))]
    {
        cmd.stdout(std::process::Stdio::piped());
        cmd.stderr(std::process::Stdio::piped());
    }

    let mut child = cmd.spawn().map_err(ProcessError::SpawnFailed)?;
    let pid = child.id();

    // On Windows, assign the child to a Job Object so that all its
    // descendants can be terminated together on stop/kill.
    #[cfg(windows)]
    let job_object = pid.and_then(|p| {
        let job = crate::sys::JobObject::new().ok()?;
        job.assign_process(p).ok()?;
        Some(job)
    });

    let (log_tx, _) = broadcast::channel(1024);
    let (monitor_tx, _monitor_rx) = watch::channel(false);

    let log_date_format = config.log_date_format.clone();

    // Spawn stdout log copier
    #[cfg(unix)]
    if let Some(reader) = pty_reader {
        log::spawn_log_copier(
            name.clone(),
            LogStream::Stdout,
            reader,
            paths.stdout_log(&name),
            log_date_format.clone(),
            log_tx.clone(),
        );
    }
    #[cfg(not(unix))]
    if let Some(stdout) = child.stdout.take() {
        log::spawn_log_copier(
            name.clone(),
            LogStream::Stdout,
            stdout,
            paths.stdout_log(&name),
            log_date_format.clone(),
            log_tx.clone(),
        );
    }

    if let Some(stderr) = child.stderr.take() {
        log::spawn_log_copier(
            name.clone(),
            LogStream::Stderr,
            stderr,
            paths.stderr_log(&name),
            log_date_format,
            log_tx.clone(),
        );
    }

    // Brief delay to let immediately-failing processes exit
    tokio::time::sleep(Duration::from_millis(SPAWN_VERIFY_DELAY_MS)).await;

    let status = match child.try_wait() {
        Ok(Some(exit_status)) => {
            // Process already exited
            let exit_code = exit_status.code();
            if exit_code == Some(0) {
                ProcessStatus::Stopped
            } else {
                ProcessStatus::Errored
            }
        }
        Ok(None) => {
            // Still running
            if config.health_check.is_some() {
                ProcessStatus::Starting
            } else {
                ProcessStatus::Online
            }
        }
        Err(_) => {
            // Cannot query status, assume running
            if config.health_check.is_some() {
                ProcessStatus::Starting
            } else {
                ProcessStatus::Online
            }
        }
    };

    let managed = ManagedProcess {
        name,
        config,
        pid,
        status,
        started_at: tokio::time::Instant::now(),
        restarts: 0,
        log_broadcaster: log_tx,
        monitor_shutdown: Some(monitor_tx),
        #[cfg(windows)]
        job_object,
    };

    Ok((managed, child))
}

/// Spawn a process, register it in the table, and attach monitors.
pub async fn spawn_and_attach(
    name: String,
    config: ProcessConfig,
    restarts: u32,
    processes: &Arc<RwLock<ProcessTable>>,
    paths: &Paths,
) -> Result<(), ProcessError> {
    let (mut managed, child) = spawn_process(name.clone(), config.clone(), paths).await?;
    managed.restarts = restarts;

    let pid = managed.pid;
    let shutdown_tx = managed
        .monitor_shutdown
        .as_ref()
        .expect("monitor shutdown sender missing")
        .clone();
    let shutdown_rx = shutdown_tx.subscribe();

    {
        let mut table = processes.write().await;
        table.insert(name.clone(), managed);
    }

    spawn_monitor(
        name.clone(),
        child,
        pid,
        Arc::clone(processes),
        paths.clone(),
        shutdown_rx,
    );
    spawn_aux_monitors(
        name,
        config,
        Arc::clone(processes),
        paths.clone(),
        shutdown_tx,
    );
    Ok(())
}

pub fn evaluate_restart_policy(
    config: &ProcessConfig,
    exit_code: Option<i32>,
    _uptime: Duration,
    restarts: u32,
) -> bool {
    let policy = config.restart.unwrap_or(RestartPolicy::OnFailure);
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

/// Monitor a reattached process by polling `is_pid_alive`.
///
/// Used during `restore_from_dump` where we have no `Child` handle.
/// When the PID dies, `handle_child_exit` is called so restart policies apply.
pub fn spawn_pid_monitor(
    name: String,
    pid: u32,
    processes: Arc<RwLock<ProcessTable>>,
    paths: Paths,
    mut shutdown_rx: watch::Receiver<bool>,
) {
    tokio::spawn(async move {
        loop {
            tokio::select! {
                _ = tokio::time::sleep(Duration::from_secs(1)) => {}
                _ = shutdown_rx.changed() => {
                    if *shutdown_rx.borrow() {
                        return;
                    }
                }
            }
            if *shutdown_rx.borrow() {
                return;
            }
            if !crate::sys::is_pid_alive(pid) {
                handle_child_exit(&name, Some(pid), None, &processes, &paths).await;
                return;
            }
        }
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

    // Re-check shutdown wasn't signaled while we were sleeping
    {
        let mut table = processes.write().await;
        let Some(managed) = table.get_mut(name) else {
            return;
        };
        if let Some(ref tx) = managed.monitor_shutdown
            && *tx.borrow()
        {
            managed.status = ProcessStatus::Stopped;
            return;
        }
    }

    if let Err(e) = spawn_and_attach(
        name.to_string(),
        config.clone(),
        restarts + 1,
        processes,
        paths,
    )
    .await
    {
        eprintln!("failed to restart '{name}': {e}");
        let mut table = processes.write().await;
        if let Some(managed) = table.get_mut(name) {
            managed.status = ProcessStatus::Errored;
            managed.pid = None;
        }
    }
}

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

    #[cfg(unix)]
    #[test]
    fn test_parse_command_single_quotes() {
        let (prog, args) = parse_command("echo 'hello world'").unwrap();
        assert_eq!(prog, "echo");
        assert_eq!(args, vec!["hello world"]);
    }

    #[cfg(windows)]
    #[test]
    fn test_parse_command_windows_backslash_path() {
        let (prog, args) = parse_command(r"C:\Users\alpha\nvs\default\node.exe server.js").unwrap();
        assert_eq!(prog, r"C:\Users\alpha\nvs\default\node.exe");
        assert_eq!(args, vec!["server.js"]);
    }

    #[cfg(windows)]
    #[test]
    fn test_parse_command_windows_quoted_path_with_spaces() {
        let (prog, args) =
            parse_command(r#""C:\Program Files\nodejs\node.exe" server.js"#).unwrap();
        assert_eq!(prog, r"C:\Program Files\nodejs\node.exe");
        assert_eq!(args, vec!["server.js"]);
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

    #[cfg(unix)]
    #[test]
    fn test_parse_signal_sigterm() {
        let sig = parse_signal("SIGTERM").unwrap();
        assert_eq!(sig, nix::sys::signal::Signal::SIGTERM);
    }

    #[cfg(unix)]
    #[test]
    fn test_parse_signal_sigint() {
        let sig = parse_signal("SIGINT").unwrap();
        assert_eq!(sig, nix::sys::signal::Signal::SIGINT);
    }

    #[cfg(unix)]
    #[test]
    fn test_parse_signal_sighup() {
        let sig = parse_signal("SIGHUP").unwrap();
        assert_eq!(sig, nix::sys::signal::Signal::SIGHUP);
    }

    #[cfg(unix)]
    #[test]
    fn test_parse_signal_sigusr1() {
        let sig = parse_signal("SIGUSR1").unwrap();
        assert_eq!(sig, nix::sys::signal::Signal::SIGUSR1);
    }

    #[cfg(unix)]
    #[test]
    fn test_parse_signal_sigusr2() {
        let sig = parse_signal("SIGUSR2").unwrap();
        assert_eq!(sig, nix::sys::signal::Signal::SIGUSR2);
    }

    #[cfg(unix)]
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
            watch_debounce: None,
            depends_on: None,
            restart,
            group: None,
            pre_start: None,
            post_stop: None,
            cron_restart: None,
            log_date_format: None,
            instances: None,
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
