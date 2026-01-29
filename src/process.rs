use crate::config::ProcessConfig;
use crate::protocol::{ProcessInfo, ProcessStatus};
use std::collections::HashMap;
use tokio::process::{Child, Command};

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
}

// ---------------------------------------------------------------------------
// Command parsing
// ---------------------------------------------------------------------------

pub fn parse_command(command: &str) -> Result<(String, Vec<String>), ProcessError> {
    let words = shell_words::split(command)
        .map_err(|e| ProcessError::InvalidCommand(format!("failed to parse: {e}")))?;

    if words.is_empty() {
        return Err(ProcessError::InvalidCommand(
            "command is empty".to_string(),
        ));
    }

    let program = words[0].clone();
    let args = words[1..].to_vec();
    Ok((program, args))
}

// ---------------------------------------------------------------------------
// ManagedProcess
// ---------------------------------------------------------------------------

pub struct ManagedProcess {
    pub name: String,
    pub config: ProcessConfig,
    pub child: Child,
    pub status: ProcessStatus,
    pub started_at: tokio::time::Instant,
    pub restarts: u32,
}

impl ManagedProcess {
    pub fn to_process_info(&self) -> ProcessInfo {
        ProcessInfo {
            name: self.name.clone(),
            pid: self.child.id(),
            status: self.status,
            uptime: Some(self.started_at.elapsed().as_secs()),
            restarts: self.restarts,
            cpu_percent: None,
            memory_bytes: None,
            group: self.config.group.clone(),
        }
    }
}

// ---------------------------------------------------------------------------
// ProcessTable
// ---------------------------------------------------------------------------

pub type ProcessTable = HashMap<String, ManagedProcess>;

// ---------------------------------------------------------------------------
// Spawning
// ---------------------------------------------------------------------------

pub fn spawn_process(
    name: String,
    config: ProcessConfig,
) -> Result<ManagedProcess, ProcessError> {
    let (program, args) = parse_command(&config.command)?;

    let mut cmd = Command::new(&program);
    cmd.args(&args);

    if let Some(ref cwd) = config.cwd {
        cmd.current_dir(cwd);
    }

    cmd.stdin(std::process::Stdio::null());
    cmd.stdout(std::process::Stdio::null());
    cmd.stderr(std::process::Stdio::null());

    let child = cmd.spawn().map_err(ProcessError::SpawnFailed)?;

    Ok(ManagedProcess {
        name,
        config,
        child,
        status: ProcessStatus::Online,
        started_at: tokio::time::Instant::now(),
        restarts: 0,
    })
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
        assert!(matches!(result.unwrap_err(), ProcessError::InvalidCommand(_)));
    }

    #[test]
    fn test_parse_whitespace_only() {
        let result = parse_command("   ");
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), ProcessError::InvalidCommand(_)));
    }
}
