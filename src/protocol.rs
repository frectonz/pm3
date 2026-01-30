use crate::config::ProcessConfig;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

// ---------------------------------------------------------------------------
// Request
// ---------------------------------------------------------------------------

fn default_log_lines() -> usize {
    15
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Request {
    Start {
        configs: HashMap<String, ProcessConfig>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        names: Option<Vec<String>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        env: Option<String>,
    },
    Stop {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        names: Option<Vec<String>>,
    },
    Restart {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        names: Option<Vec<String>>,
    },
    List,
    Kill,
    Reload {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        names: Option<Vec<String>>,
    },
    Info {
        name: String,
    },
    Signal {
        name: String,
        signal: String,
    },
    Save,
    Resurrect,
    Flush {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        names: Option<Vec<String>>,
    },
    Log {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<String>,
        #[serde(default = "default_log_lines")]
        lines: usize,
        #[serde(default)]
        follow: bool,
    },
}

// ---------------------------------------------------------------------------
// Response
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Response {
    Success {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        message: Option<String>,
    },
    Error {
        message: String,
    },
    ProcessList {
        processes: Vec<ProcessInfo>,
    },
    ProcessDetail {
        info: ProcessDetail,
    },
    LogLine {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<String>,
        line: String,
    },
}

// ---------------------------------------------------------------------------
// Supporting types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProcessStatus {
    Starting,
    Online,
    Unhealthy,
    Stopped,
    Errored,
}

impl std::fmt::Display for ProcessStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ProcessStatus::Starting => write!(f, "starting"),
            ProcessStatus::Online => write!(f, "online"),
            ProcessStatus::Unhealthy => write!(f, "unhealthy"),
            ProcessStatus::Stopped => write!(f, "stopped"),
            ProcessStatus::Errored => write!(f, "errored"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProcessInfo {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pid: Option<u32>,
    pub status: ProcessStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uptime: Option<u64>,
    #[serde(default)]
    pub restarts: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cpu_percent: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory_bytes: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProcessDetail {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pid: Option<u32>,
    pub status: ProcessStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uptime: Option<u64>,
    #[serde(default)]
    pub restarts: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cpu_percent: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory_bytes: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<String>,
    pub command: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub env: Option<HashMap<String, String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stdout_log: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stderr_log: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub health_check: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub depends_on: Option<Vec<String>>,
}

// ---------------------------------------------------------------------------
// Error
// ---------------------------------------------------------------------------

#[derive(Debug, thiserror::Error)]
pub enum ProtocolError {
    #[error("failed to serialize/deserialize JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("malformed message: {0}")]
    Malformed(String),
}

// ---------------------------------------------------------------------------
// Encode / decode helpers
// ---------------------------------------------------------------------------

pub fn encode_request(req: &Request) -> Result<Vec<u8>, ProtocolError> {
    let mut buf = serde_json::to_vec(req)?;
    buf.push(b'\n');
    Ok(buf)
}

pub fn decode_request(line: &str) -> Result<Request, ProtocolError> {
    let trimmed = line.trim_end();
    Ok(serde_json::from_str(trimmed)?)
}

pub fn encode_response(resp: &Response) -> Result<Vec<u8>, ProtocolError> {
    let mut buf = serde_json::to_vec(resp)?;
    buf.push(b'\n');
    Ok(buf)
}

pub fn decode_response(line: &str) -> Result<Response, ProtocolError> {
    let trimmed = line.trim_end();
    Ok(serde_json::from_str(trimmed)?)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // Helper: roundtrip a Request through encode → decode
    fn roundtrip_request(req: &Request) -> Request {
        let bytes = encode_request(req).unwrap();
        let line = std::str::from_utf8(&bytes).unwrap();
        decode_request(line).unwrap()
    }

    // Helper: roundtrip a Response through encode → decode
    fn roundtrip_response(resp: &Response) -> Response {
        let bytes = encode_response(resp).unwrap();
        let line = std::str::from_utf8(&bytes).unwrap();
        decode_response(line).unwrap()
    }

    // -----------------------------------------------------------------------
    // Request roundtrips (12)
    // -----------------------------------------------------------------------

    #[test]
    fn test_request_start_roundtrip() {
        let mut configs = HashMap::new();
        configs.insert(
            "web".to_string(),
            ProcessConfig {
                command: "node server.js".to_string(),
                cwd: Some("/app".to_string()),
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
                restart: None,
                group: None,
                pre_start: None,
                post_stop: None,
                notify: None,
                cron_restart: None,
                log_date_format: None,
                environments: HashMap::new(),
            },
        );
        let req = Request::Start {
            configs,
            names: Some(vec!["web".to_string()]),
            env: Some("production".to_string()),
        };
        assert_eq!(roundtrip_request(&req), req);
    }

    #[test]
    fn test_request_stop_roundtrip() {
        let req = Request::Stop {
            names: Some(vec!["web".to_string(), "api".to_string()]),
        };
        assert_eq!(roundtrip_request(&req), req);
    }

    #[test]
    fn test_request_restart_roundtrip() {
        let req = Request::Restart { names: None };
        assert_eq!(roundtrip_request(&req), req);
    }

    #[test]
    fn test_request_list_roundtrip() {
        let req = Request::List;
        assert_eq!(roundtrip_request(&req), req);
    }

    #[test]
    fn test_request_kill_roundtrip() {
        let req = Request::Kill;
        assert_eq!(roundtrip_request(&req), req);
    }

    #[test]
    fn test_request_reload_roundtrip() {
        let req = Request::Reload {
            names: Some(vec!["worker".to_string()]),
        };
        assert_eq!(roundtrip_request(&req), req);
    }

    #[test]
    fn test_request_info_roundtrip() {
        let req = Request::Info {
            name: "web".to_string(),
        };
        assert_eq!(roundtrip_request(&req), req);
    }

    #[test]
    fn test_request_signal_roundtrip() {
        let req = Request::Signal {
            name: "web".to_string(),
            signal: "SIGHUP".to_string(),
        };
        assert_eq!(roundtrip_request(&req), req);
    }

    #[test]
    fn test_request_save_roundtrip() {
        let req = Request::Save;
        assert_eq!(roundtrip_request(&req), req);
    }

    #[test]
    fn test_request_resurrect_roundtrip() {
        let req = Request::Resurrect;
        assert_eq!(roundtrip_request(&req), req);
    }

    #[test]
    fn test_request_flush_roundtrip() {
        let req = Request::Flush { names: None };
        assert_eq!(roundtrip_request(&req), req);
    }

    #[test]
    fn test_request_log_roundtrip() {
        let req = Request::Log {
            name: Some("web".to_string()),
            lines: 30,
            follow: true,
        };
        assert_eq!(roundtrip_request(&req), req);
    }

    // -----------------------------------------------------------------------
    // Response roundtrips (5)
    // -----------------------------------------------------------------------

    #[test]
    fn test_response_success_roundtrip() {
        let resp = Response::Success {
            message: Some("all processes started".to_string()),
        };
        assert_eq!(roundtrip_response(&resp), resp);

        let resp_none = Response::Success { message: None };
        assert_eq!(roundtrip_response(&resp_none), resp_none);
    }

    #[test]
    fn test_response_error_roundtrip() {
        let resp = Response::Error {
            message: "process not found".to_string(),
        };
        assert_eq!(roundtrip_response(&resp), resp);
    }

    #[test]
    fn test_response_process_list_roundtrip() {
        let resp = Response::ProcessList {
            processes: vec![
                ProcessInfo {
                    name: "web".to_string(),
                    pid: Some(1234),
                    status: ProcessStatus::Online,
                    uptime: Some(3600),
                    restarts: 2,
                    cpu_percent: Some(1.5),
                    memory_bytes: Some(52_428_800),
                    group: Some("backend".to_string()),
                },
                ProcessInfo {
                    name: "worker".to_string(),
                    pid: None,
                    status: ProcessStatus::Stopped,
                    uptime: None,
                    restarts: 0,
                    cpu_percent: None,
                    memory_bytes: None,
                    group: None,
                },
            ],
        };
        assert_eq!(roundtrip_response(&resp), resp);
    }

    #[test]
    fn test_response_process_detail_roundtrip() {
        let resp = Response::ProcessDetail {
            info: ProcessDetail {
                name: "web".to_string(),
                pid: Some(1234),
                status: ProcessStatus::Online,
                uptime: Some(3600),
                restarts: 0,
                cpu_percent: Some(2.3),
                memory_bytes: Some(104_857_600),
                group: Some("backend".to_string()),
                command: "node server.js".to_string(),
                cwd: Some("/app".to_string()),
                env: Some(HashMap::from([("PORT".to_string(), "3000".to_string())])),
                exit_code: None,
                stdout_log: Some("/home/user/.local/share/pm3/logs/web-out.log".to_string()),
                stderr_log: Some("/home/user/.local/share/pm3/logs/web-err.log".to_string()),
                health_check: Some("http://localhost:3000/health".to_string()),
                depends_on: Some(vec!["db".to_string()]),
            },
        };
        assert_eq!(roundtrip_response(&resp), resp);
    }

    #[test]
    fn test_response_log_line_roundtrip() {
        let resp = Response::LogLine {
            name: Some("web".to_string()),
            line: "Server started on port 3000".to_string(),
        };
        assert_eq!(roundtrip_response(&resp), resp);

        let resp_no_name = Response::LogLine {
            name: None,
            line: "some output".to_string(),
        };
        assert_eq!(roundtrip_response(&resp_no_name), resp_no_name);
    }

    // -----------------------------------------------------------------------
    // Malformed JSON (3)
    // -----------------------------------------------------------------------

    #[test]
    fn test_decode_invalid_json() {
        let result = decode_request("not json at all");
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            ProtocolError::Json(_)
        ));
    }

    #[test]
    fn test_decode_unknown_type() {
        let result = decode_request(r#"{"type":"bogus"}"#);
        assert!(result.is_err());
    }

    #[test]
    fn test_decode_missing_required_field() {
        // "info" requires a "name" field
        let result = decode_request(r#"{"type":"info"}"#);
        assert!(result.is_err());
    }

    // -----------------------------------------------------------------------
    // Encode/decode helpers (2)
    // -----------------------------------------------------------------------

    #[test]
    fn test_encode_appends_newline() {
        let req = Request::List;
        let bytes = encode_request(&req).unwrap();
        assert_eq!(*bytes.last().unwrap(), b'\n');

        let resp = Response::Success { message: None };
        let bytes = encode_response(&resp).unwrap();
        assert_eq!(*bytes.last().unwrap(), b'\n');
    }

    #[test]
    fn test_process_status_display() {
        assert_eq!(ProcessStatus::Starting.to_string(), "starting");
        assert_eq!(ProcessStatus::Online.to_string(), "online");
        assert_eq!(ProcessStatus::Unhealthy.to_string(), "unhealthy");
        assert_eq!(ProcessStatus::Stopped.to_string(), "stopped");
        assert_eq!(ProcessStatus::Errored.to_string(), "errored");
    }

    #[test]
    fn test_decode_trims_newline() {
        let req = Request::Kill;
        let bytes = encode_request(&req).unwrap();
        let line = std::str::from_utf8(&bytes).unwrap();
        // line ends with '\n' — decode should handle it
        assert_eq!(decode_request(line).unwrap(), req);

        // Also with extra whitespace
        let padded = format!("{line}  \r\n");
        assert_eq!(decode_request(&padded).unwrap(), req);
    }
}
