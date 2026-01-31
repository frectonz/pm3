use pm3::config::{self, ProcessConfig, RestartPolicy};
use pm3::daemon;
use pm3::log::LOG_ROTATION_SIZE;
use pm3::paths::Paths;
use pm3::protocol::{self, ProcessStatus, Request, Response};
use regex::Regex;
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::time::{Duration, Instant};
use tempfile::TempDir;

fn test_config(command: &str) -> ProcessConfig {
    ProcessConfig {
        command: command.to_string(),
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
        restart: None,
        group: None,
        pre_start: None,
        post_stop: None,
        notify: None,
        cron_restart: None,
        log_date_format: None,
        environments: HashMap::new(),
    }
}

fn test_config_with_kill(
    command: &str,
    kill_timeout: Option<u64>,
    kill_signal: Option<&str>,
) -> ProcessConfig {
    let mut config = test_config(command);
    config.kill_timeout = kill_timeout;
    config.kill_signal = kill_signal.map(|s| s.to_string());
    config
}

async fn start_test_daemon(paths: &Paths) -> tokio::task::JoinHandle<color_eyre::Result<()>> {
    let p = paths.clone();
    let handle = tokio::spawn(async move { daemon::run(p).await });

    // Wait for socket file to appear
    let socket = paths.socket_file();
    for _ in 0..50 {
        if socket.exists() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(socket.exists(), "daemon socket was not created");

    handle
}

fn send_raw_request_sync(paths: &Paths, request: &Request) -> Response {
    let mut stream = UnixStream::connect(paths.socket_file()).unwrap();
    let encoded = protocol::encode_request(request).unwrap();
    stream.write_all(&encoded).unwrap();
    stream.shutdown(std::net::Shutdown::Write).unwrap();

    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader.read_line(&mut line).unwrap();
    protocol::decode_response(&line).unwrap()
}

fn send_streaming_request_sync(paths: &Paths, request: &Request) -> Vec<Response> {
    let mut stream = UnixStream::connect(paths.socket_file()).unwrap();
    let encoded = protocol::encode_request(request).unwrap();
    stream.write_all(&encoded).unwrap();
    stream.shutdown(std::net::Shutdown::Write).unwrap();

    let reader = BufReader::new(stream);
    let mut responses = Vec::new();
    for line_result in reader.lines() {
        let line = line_result.unwrap();
        if line.is_empty() {
            continue;
        }
        responses.push(protocol::decode_response(&line).unwrap());
    }
    responses
}

async fn send_streaming_request(paths: &Paths, request: &Request) -> Vec<Response> {
    let p = paths.clone();
    let req = request.clone();
    tokio::task::spawn_blocking(move || send_streaming_request_sync(&p, &req))
        .await
        .unwrap()
}

async fn send_raw_request(paths: &Paths, request: &Request) -> Response {
    let p = paths.clone();
    let req = request.clone();
    tokio::task::spawn_blocking(move || send_raw_request_sync(&p, &req))
        .await
        .unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_daemon_creates_pid_and_socket() {
    let dir = TempDir::new().unwrap();
    let paths = Paths::with_base(dir.path().to_path_buf());

    let handle = start_test_daemon(&paths).await;

    assert!(paths.pid_file().exists(), "PID file should exist");
    assert!(paths.socket_file().exists(), "socket file should exist");

    // Shut down
    send_raw_request(&paths, &Request::Kill).await;
    let _ = handle.await;

    assert!(!paths.pid_file().exists(), "PID file should be cleaned up");
    assert!(
        !paths.socket_file().exists(),
        "socket file should be cleaned up"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_client_sends_request_gets_response() {
    let dir = TempDir::new().unwrap();
    let paths = Paths::with_base(dir.path().to_path_buf());

    let handle = start_test_daemon(&paths).await;

    let response = send_raw_request(&paths, &Request::List).await;
    assert!(
        matches!(&response, Response::ProcessList { processes } if processes.is_empty()),
        "expected empty process list, got: {response:?}"
    );

    send_raw_request(&paths, &Request::Kill).await;
    let _ = handle.await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_daemon_handles_multiple_sequential_connections() {
    let dir = TempDir::new().unwrap();
    let paths = Paths::with_base(dir.path().to_path_buf());

    let handle = start_test_daemon(&paths).await;

    for i in 0..5 {
        let response = send_raw_request(&paths, &Request::List).await;
        assert!(
            matches!(&response, Response::ProcessList { processes } if processes.is_empty()),
            "request {i}: expected empty process list, got: {response:?}"
        );
    }

    send_raw_request(&paths, &Request::Kill).await;
    let _ = handle.await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_daemon_rejects_duplicate_instance() {
    let dir = TempDir::new().unwrap();
    let paths = Paths::with_base(dir.path().to_path_buf());

    let handle = start_test_daemon(&paths).await;

    // Try to start a second daemon — should error
    let paths2 = paths.clone();
    let result = daemon::run(paths2).await;
    assert!(result.is_err(), "second daemon should fail to start");
    let err_msg = result.unwrap_err().to_string();
    assert!(
        err_msg.contains("already running"),
        "error should mention 'already running', got: {err_msg}"
    );

    send_raw_request(&paths, &Request::Kill).await;
    let _ = handle.await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_spawn_process_tracks_pid() {
    let dir = TempDir::new().unwrap();
    let paths = Paths::with_base(dir.path().to_path_buf());

    let handle = start_test_daemon(&paths).await;

    // Start a long-running process
    let mut configs = HashMap::new();
    configs.insert("sleeper".to_string(), test_config("sleep 999"));
    let start_resp = send_raw_request(
        &paths,
        &Request::Start {
            configs,
            names: None,
            env: None,
        },
    )
    .await;
    assert!(
        matches!(&start_resp, Response::Success { .. }),
        "expected Success, got: {start_resp:?}"
    );

    // List and verify the process appears
    let list_resp = send_raw_request(&paths, &Request::List).await;
    match &list_resp {
        Response::ProcessList { processes } => {
            assert_eq!(processes.len(), 1);
            let info = &processes[0];
            assert_eq!(info.name, "sleeper");
            assert!(info.pid.is_some(), "PID should be present");
            assert_eq!(info.status, pm3::protocol::ProcessStatus::Online);

            // Verify PID is alive
            let pid = nix::unistd::Pid::from_raw(info.pid.unwrap() as i32);
            assert!(
                nix::sys::signal::kill(pid, None).is_ok(),
                "process should be alive"
            );
        }
        other => panic!("expected ProcessList, got: {other:?}"),
    }

    send_raw_request(&paths, &Request::Kill).await;
    let _ = handle.await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_spawn_with_cwd() {
    let dir = TempDir::new().unwrap();
    let paths = Paths::with_base(dir.path().to_path_buf());

    // Create a subdirectory to use as cwd
    let cwd_dir = dir.path().join("workdir");
    std::fs::create_dir_all(&cwd_dir).unwrap();

    let handle = start_test_daemon(&paths).await;

    let mut config = test_config("sh -c 'pwd > output.txt'");
    config.cwd = Some(cwd_dir.to_str().unwrap().to_string());

    let mut configs = HashMap::new();
    configs.insert("pwd-test".to_string(), config);
    let start_resp = send_raw_request(
        &paths,
        &Request::Start {
            configs,
            names: None,
            env: None,
        },
    )
    .await;
    assert!(
        matches!(&start_resp, Response::Success { .. }),
        "expected Success, got: {start_resp:?}"
    );

    // Wait for the child to finish writing
    tokio::time::sleep(Duration::from_millis(500)).await;

    let output_file = cwd_dir.join("output.txt");
    assert!(output_file.exists(), "output.txt should have been created");

    let output = std::fs::read_to_string(&output_file).unwrap();
    let actual = std::fs::canonicalize(output.trim()).unwrap();
    let expected = std::fs::canonicalize(&cwd_dir).unwrap();
    assert_eq!(actual, expected);

    send_raw_request(&paths, &Request::Kill).await;
    let _ = handle.await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_log_capture_stdout() {
    let dir = TempDir::new().unwrap();
    let paths = Paths::with_base(dir.path().to_path_buf());

    let handle = start_test_daemon(&paths).await;

    let mut configs = HashMap::new();
    configs.insert("echoer".to_string(), test_config("sh -c 'echo hello'"));
    let start_resp = send_raw_request(
        &paths,
        &Request::Start {
            configs,
            names: None,
            env: None,
        },
    )
    .await;
    assert!(
        matches!(&start_resp, Response::Success { .. }),
        "expected Success, got: {start_resp:?}"
    );

    tokio::time::sleep(Duration::from_millis(500)).await;

    let stdout_log = paths.stdout_log("echoer");
    assert!(stdout_log.exists(), "stdout log file should exist");
    let content = std::fs::read_to_string(&stdout_log).unwrap();
    assert!(
        content.contains("hello"),
        "stdout log should contain 'hello', got: {content}"
    );

    send_raw_request(&paths, &Request::Kill).await;
    let _ = handle.await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_log_capture_stderr() {
    let dir = TempDir::new().unwrap();
    let paths = Paths::with_base(dir.path().to_path_buf());

    let handle = start_test_daemon(&paths).await;

    let mut configs = HashMap::new();
    configs.insert(
        "err-writer".to_string(),
        test_config("sh -c 'echo error >&2'"),
    );
    let start_resp = send_raw_request(
        &paths,
        &Request::Start {
            configs,
            names: None,
            env: None,
        },
    )
    .await;
    assert!(
        matches!(&start_resp, Response::Success { .. }),
        "expected Success, got: {start_resp:?}"
    );

    tokio::time::sleep(Duration::from_millis(500)).await;

    let stderr_log = paths.stderr_log("err-writer");
    assert!(stderr_log.exists(), "stderr log file should exist");
    let content = std::fs::read_to_string(&stderr_log).unwrap();
    assert!(
        content.contains("error"),
        "stderr log should contain 'error', got: {content}"
    );

    send_raw_request(&paths, &Request::Kill).await;
    let _ = handle.await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_log_directory_created() {
    let dir = TempDir::new().unwrap();
    let paths = Paths::with_base(dir.path().to_path_buf());

    // Verify log dir doesn't exist yet
    assert!(!paths.log_dir().exists(), "log dir should not exist yet");

    let handle = start_test_daemon(&paths).await;

    let mut configs = HashMap::new();
    configs.insert("logtest".to_string(), test_config("sleep 999"));
    let start_resp = send_raw_request(
        &paths,
        &Request::Start {
            configs,
            names: None,
            env: None,
        },
    )
    .await;
    assert!(
        matches!(&start_resp, Response::Success { .. }),
        "expected Success, got: {start_resp:?}"
    );

    assert!(
        paths.log_dir().exists(),
        "log directory should have been created"
    );

    send_raw_request(&paths, &Request::Kill).await;
    let _ = handle.await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_start_one_process_from_config() {
    let dir = TempDir::new().unwrap();
    let paths = Paths::with_base(dir.path().to_path_buf());

    let config_path = dir.path().join("pm3.toml");
    std::fs::write(
        &config_path,
        r#"
[sleeper]
command = "sleep 999"
"#,
    )
    .unwrap();

    let configs = config::load_config(&config_path).unwrap();

    let handle = start_test_daemon(&paths).await;

    let start_resp = send_raw_request(
        &paths,
        &Request::Start {
            configs,
            names: None,
            env: None,
        },
    )
    .await;
    assert!(
        matches!(&start_resp, Response::Success { .. }),
        "expected Success, got: {start_resp:?}"
    );

    let list_resp = send_raw_request(&paths, &Request::List).await;
    match &list_resp {
        Response::ProcessList { processes } => {
            assert_eq!(processes.len(), 1);
            assert_eq!(processes[0].name, "sleeper");
            assert_eq!(processes[0].status, pm3::protocol::ProcessStatus::Online);
        }
        other => panic!("expected ProcessList, got: {other:?}"),
    }

    send_raw_request(&paths, &Request::Kill).await;
    let _ = handle.await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_start_two_processes_from_config() {
    let dir = TempDir::new().unwrap();
    let paths = Paths::with_base(dir.path().to_path_buf());

    let config_path = dir.path().join("pm3.toml");
    std::fs::write(
        &config_path,
        r#"
[sleeper1]
command = "sleep 999"

[sleeper2]
command = "sleep 999"
"#,
    )
    .unwrap();

    let configs = config::load_config(&config_path).unwrap();

    let handle = start_test_daemon(&paths).await;

    let start_resp = send_raw_request(
        &paths,
        &Request::Start {
            configs,
            names: None,
            env: None,
        },
    )
    .await;
    assert!(
        matches!(&start_resp, Response::Success { .. }),
        "expected Success, got: {start_resp:?}"
    );

    let list_resp = send_raw_request(&paths, &Request::List).await;
    match &list_resp {
        Response::ProcessList { processes } => {
            assert_eq!(processes.len(), 2);
            let mut names: Vec<&str> = processes.iter().map(|p| p.name.as_str()).collect();
            names.sort();
            assert_eq!(names, vec!["sleeper1", "sleeper2"]);
        }
        other => panic!("expected ProcessList, got: {other:?}"),
    }

    send_raw_request(&paths, &Request::Kill).await;
    let _ = handle.await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_start_named_process_from_config() {
    let dir = TempDir::new().unwrap();
    let paths = Paths::with_base(dir.path().to_path_buf());

    let config_path = dir.path().join("pm3.toml");
    std::fs::write(
        &config_path,
        r#"
[web]
command = "sleep 999"

[worker]
command = "sleep 999"
"#,
    )
    .unwrap();

    let configs = config::load_config(&config_path).unwrap();

    let handle = start_test_daemon(&paths).await;

    let start_resp = send_raw_request(
        &paths,
        &Request::Start {
            configs,
            names: Some(vec!["web".to_string()]),
            env: None,
        },
    )
    .await;
    assert!(
        matches!(&start_resp, Response::Success { .. }),
        "expected Success, got: {start_resp:?}"
    );

    let list_resp = send_raw_request(&paths, &Request::List).await;
    match &list_resp {
        Response::ProcessList { processes } => {
            assert_eq!(processes.len(), 1);
            assert_eq!(processes[0].name, "web");
        }
        other => panic!("expected ProcessList, got: {other:?}"),
    }

    send_raw_request(&paths, &Request::Kill).await;
    let _ = handle.await;
}

#[test]
fn test_load_config_file_not_found() {
    let result = config::load_config(std::path::Path::new("/nonexistent/pm3.toml"));
    assert!(result.is_err());
    assert!(matches!(
        result.unwrap_err(),
        config::ConfigError::IoError(_)
    ));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_list_empty_returns_no_processes() {
    let dir = TempDir::new().unwrap();
    let paths = Paths::with_base(dir.path().to_path_buf());

    let handle = start_test_daemon(&paths).await;

    let response = send_raw_request(&paths, &Request::List).await;
    match &response {
        Response::ProcessList { processes } => {
            assert!(processes.is_empty(), "expected empty list");
        }
        other => panic!("expected ProcessList, got: {other:?}"),
    }

    send_raw_request(&paths, &Request::Kill).await;
    let _ = handle.await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_list_process_info_fields() {
    let dir = TempDir::new().unwrap();
    let paths = Paths::with_base(dir.path().to_path_buf());

    let handle = start_test_daemon(&paths).await;

    let mut config = test_config("sleep 999");
    config.group = Some("workers".to_string());

    let mut configs = HashMap::new();
    configs.insert("worker".to_string(), config);
    let start_resp = send_raw_request(
        &paths,
        &Request::Start {
            configs,
            names: None,
            env: None,
        },
    )
    .await;
    assert!(
        matches!(&start_resp, Response::Success { .. }),
        "expected Success, got: {start_resp:?}"
    );

    tokio::time::sleep(Duration::from_millis(100)).await;

    let list_resp = send_raw_request(&paths, &Request::List).await;
    match &list_resp {
        Response::ProcessList { processes } => {
            assert_eq!(processes.len(), 1);
            let info = &processes[0];
            assert_eq!(info.name, "worker");
            assert!(info.pid.is_some(), "PID should be present");
            assert!(info.pid.unwrap() > 0, "PID should be > 0");
            assert_eq!(info.status, ProcessStatus::Online);
            assert!(info.uptime.is_some(), "uptime should be present");
            assert_eq!(info.restarts, 0);
            assert_eq!(info.group, Some("workers".to_string()));
        }
        other => panic!("expected ProcessList, got: {other:?}"),
    }

    send_raw_request(&paths, &Request::Kill).await;
    let _ = handle.await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_list_multiple_processes_all_fields() {
    let dir = TempDir::new().unwrap();
    let paths = Paths::with_base(dir.path().to_path_buf());

    let handle = start_test_daemon(&paths).await;

    let mut alpha_config = test_config("sleep 999");
    alpha_config.group = Some("group-a".to_string());

    let beta_config = test_config("sleep 999");

    let mut configs = HashMap::new();
    configs.insert("alpha".to_string(), alpha_config);
    configs.insert("beta".to_string(), beta_config);
    let start_resp = send_raw_request(
        &paths,
        &Request::Start {
            configs,
            names: None,
            env: None,
        },
    )
    .await;
    assert!(
        matches!(&start_resp, Response::Success { .. }),
        "expected Success, got: {start_resp:?}"
    );

    tokio::time::sleep(Duration::from_millis(100)).await;

    let list_resp = send_raw_request(&paths, &Request::List).await;
    match &list_resp {
        Response::ProcessList { processes } => {
            assert_eq!(processes.len(), 2);

            let mut sorted: Vec<_> = processes.iter().collect();
            sorted.sort_by_key(|p| &p.name);

            let alpha = sorted[0];
            assert_eq!(alpha.name, "alpha");
            assert!(alpha.pid.is_some());
            assert_eq!(alpha.status, ProcessStatus::Online);
            assert!(alpha.uptime.is_some());
            assert_eq!(alpha.restarts, 0);
            assert_eq!(alpha.group, Some("group-a".to_string()));

            let beta = sorted[1];
            assert_eq!(beta.name, "beta");
            assert!(beta.pid.is_some());
            assert_eq!(beta.status, ProcessStatus::Online);
            assert!(beta.uptime.is_some());
            assert_eq!(beta.restarts, 0);
            assert_eq!(beta.group, None);
        }
        other => panic!("expected ProcessList, got: {other:?}"),
    }

    send_raw_request(&paths, &Request::Kill).await;
    let _ = handle.await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_stop_process_handles_sigterm() {
    let dir = TempDir::new().unwrap();
    let paths = Paths::with_base(dir.path().to_path_buf());

    let handle = start_test_daemon(&paths).await;

    let mut configs = HashMap::new();
    configs.insert("sleeper".to_string(), test_config("sleep 999"));
    send_raw_request(
        &paths,
        &Request::Start {
            configs,
            names: None,
            env: None,
        },
    )
    .await;

    // Get PID from list
    let list_resp = send_raw_request(&paths, &Request::List).await;
    let pid = match &list_resp {
        Response::ProcessList { processes } => {
            assert_eq!(processes.len(), 1);
            let info = &processes[0];
            assert_eq!(info.status, ProcessStatus::Online);
            info.pid.unwrap()
        }
        other => panic!("expected ProcessList, got: {other:?}"),
    };

    // Stop the process
    let stop_resp = send_raw_request(
        &paths,
        &Request::Stop {
            names: Some(vec!["sleeper".to_string()]),
        },
    )
    .await;
    assert!(
        matches!(&stop_resp, Response::Success { .. }),
        "expected Success, got: {stop_resp:?}"
    );

    // Verify process is dead
    let nix_pid = nix::unistd::Pid::from_raw(pid as i32);
    assert!(
        nix::sys::signal::kill(nix_pid, None).is_err(),
        "process should be dead after stop"
    );

    // Verify status is Stopped
    let list_resp = send_raw_request(&paths, &Request::List).await;
    match &list_resp {
        Response::ProcessList { processes } => {
            assert_eq!(processes.len(), 1);
            assert_eq!(processes[0].status, ProcessStatus::Stopped);
        }
        other => panic!("expected ProcessList, got: {other:?}"),
    }

    send_raw_request(&paths, &Request::Kill).await;
    let _ = handle.await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_stop_process_ignores_sigterm_gets_sigkill() {
    let dir = TempDir::new().unwrap();
    let paths = Paths::with_base(dir.path().to_path_buf());

    let handle = start_test_daemon(&paths).await;

    // Process that traps SIGTERM and ignores it.
    // Use bash explicitly for reliable signal handling.
    let mut configs = HashMap::new();
    configs.insert(
        "stubborn".to_string(),
        test_config_with_kill(
            "bash -c 'trap \"\" TERM; while true; do sleep 60; done'",
            Some(500),
            None,
        ),
    );
    send_raw_request(
        &paths,
        &Request::Start {
            configs,
            names: None,
            env: None,
        },
    )
    .await;

    // Wait for the process to start and install the trap
    tokio::time::sleep(Duration::from_millis(300)).await;

    // Get PID
    let list_resp = send_raw_request(&paths, &Request::List).await;
    let pid = match &list_resp {
        Response::ProcessList { processes } => {
            assert_eq!(processes.len(), 1);
            assert_eq!(processes[0].status, ProcessStatus::Online);
            processes[0].pid.unwrap()
        }
        other => panic!("expected ProcessList, got: {other:?}"),
    };

    let start = std::time::Instant::now();

    // Stop — should timeout on SIGTERM and escalate to SIGKILL
    let stop_resp = send_raw_request(
        &paths,
        &Request::Stop {
            names: Some(vec!["stubborn".to_string()]),
        },
    )
    .await;
    assert!(
        matches!(&stop_resp, Response::Success { .. }),
        "expected Success, got: {stop_resp:?}"
    );

    let elapsed = start.elapsed();
    assert!(
        elapsed >= Duration::from_millis(400),
        "should have waited for timeout, elapsed: {elapsed:?}"
    );

    // Verify process is dead
    let nix_pid = nix::unistd::Pid::from_raw(pid as i32);
    assert!(
        nix::sys::signal::kill(nix_pid, None).is_err(),
        "process should be dead after SIGKILL"
    );

    send_raw_request(&paths, &Request::Kill).await;
    let _ = handle.await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_stop_custom_kill_signal_sigint() {
    let dir = TempDir::new().unwrap();
    let paths = Paths::with_base(dir.path().to_path_buf());

    // Create a workdir for the marker file
    let workdir = dir.path().join("workdir");
    std::fs::create_dir_all(&workdir).unwrap();
    let marker = workdir.join("got_sigint");

    let handle = start_test_daemon(&paths).await;

    // Process that traps SIGINT to write a marker and exit, but ignores SIGTERM.
    // Use a short sleep interval so bash can check for pending signals between iterations.
    let marker_path = marker.display();
    let command = format!(
        r#"bash -c "trap '' TERM; trap 'echo yes > {marker_path}; exit 0' INT; while true; do sleep 0.1; done""#
    );
    let mut config = test_config_with_kill(&command, Some(2000), Some("SIGINT"));
    config.cwd = Some(workdir.to_str().unwrap().to_string());

    let mut configs = HashMap::new();
    configs.insert("sigint-handler".to_string(), config);
    send_raw_request(
        &paths,
        &Request::Start {
            configs,
            names: None,
            env: None,
        },
    )
    .await;

    // Wait for process to start and install signal traps
    tokio::time::sleep(Duration::from_millis(300)).await;

    // Stop with SIGINT
    let stop_resp = send_raw_request(
        &paths,
        &Request::Stop {
            names: Some(vec!["sigint-handler".to_string()]),
        },
    )
    .await;
    assert!(
        matches!(&stop_resp, Response::Success { .. }),
        "expected Success, got: {stop_resp:?}"
    );

    // Verify marker file exists — proves SIGINT was received
    assert!(
        marker.exists(),
        "marker file should exist, proving SIGINT was received"
    );

    send_raw_request(&paths, &Request::Kill).await;
    let _ = handle.await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_restart_preserves_process_config() {
    let dir = TempDir::new().unwrap();
    let paths = Paths::with_base(dir.path().to_path_buf());

    let handle = start_test_daemon(&paths).await;

    let mut config = test_config("sleep 999");
    config.group = Some("workers".to_string());

    let mut configs = HashMap::new();
    configs.insert("worker".to_string(), config);
    let start_resp = send_raw_request(
        &paths,
        &Request::Start {
            configs,
            names: None,
            env: None,
        },
    )
    .await;
    assert!(
        matches!(&start_resp, Response::Success { .. }),
        "expected Success, got: {start_resp:?}"
    );

    // Get PID before restart
    let list_resp = send_raw_request(&paths, &Request::List).await;
    let old_pid = match &list_resp {
        Response::ProcessList { processes } => {
            assert_eq!(processes.len(), 1);
            assert_eq!(processes[0].status, ProcessStatus::Online);
            processes[0].pid.unwrap()
        }
        other => panic!("expected ProcessList, got: {other:?}"),
    };

    // Restart
    let restart_resp = send_raw_request(
        &paths,
        &Request::Restart {
            names: Some(vec!["worker".to_string()]),
        },
    )
    .await;
    assert!(
        matches!(&restart_resp, Response::Success { .. }),
        "expected Success, got: {restart_resp:?}"
    );

    // Verify: online, new PID, restarts == 1, group preserved
    let list_resp = send_raw_request(&paths, &Request::List).await;
    match &list_resp {
        Response::ProcessList { processes } => {
            assert_eq!(processes.len(), 1);
            let info = &processes[0];
            assert_eq!(info.name, "worker");
            assert_eq!(info.status, ProcessStatus::Online);
            assert!(info.pid.is_some());
            assert_ne!(
                info.pid.unwrap(),
                old_pid,
                "PID should change after restart"
            );
            assert_eq!(info.restarts, 1);
            assert_eq!(info.group, Some("workers".to_string()));
        }
        other => panic!("expected ProcessList, got: {other:?}"),
    }

    send_raw_request(&paths, &Request::Kill).await;
    let _ = handle.await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_start_nonexistent_name_returns_error() {
    let dir = TempDir::new().unwrap();
    let paths = Paths::with_base(dir.path().to_path_buf());

    let config_path = dir.path().join("pm3.toml");
    std::fs::write(
        &config_path,
        r#"
[web]
command = "sleep 999"
"#,
    )
    .unwrap();

    let configs = config::load_config(&config_path).unwrap();

    let handle = start_test_daemon(&paths).await;

    let resp = send_raw_request(
        &paths,
        &Request::Start {
            configs,
            names: Some(vec!["nonexistent".to_string()]),
            env: None,
        },
    )
    .await;
    match &resp {
        Response::Error { message } => {
            assert!(
                message.contains("not found"),
                "error should mention 'not found', got: {message}"
            );
        }
        other => panic!("expected Error, got: {other:?}"),
    }

    send_raw_request(&paths, &Request::Kill).await;
    let _ = handle.await;
}

// ── Item 14: Log command ────────────────────────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_log_shows_stdout_lines() {
    let dir = TempDir::new().unwrap();
    let paths = Paths::with_base(dir.path().to_path_buf());

    let handle = start_test_daemon(&paths).await;

    let mut configs = HashMap::new();
    configs.insert(
        "echoer".to_string(),
        test_config("sh -c 'echo line1; echo line2; echo line3'"),
    );
    send_raw_request(
        &paths,
        &Request::Start {
            configs,
            names: None,
            env: None,
        },
    )
    .await;

    // Wait for process to finish and logs to be written
    tokio::time::sleep(Duration::from_millis(500)).await;

    let responses = send_streaming_request(
        &paths,
        &Request::Log {
            name: Some("echoer".to_string()),
            lines: 15,
            follow: false,
        },
    )
    .await;

    let log_lines: Vec<&str> = responses
        .iter()
        .filter_map(|r| match r {
            Response::LogLine { line, .. } => Some(line.as_str()),
            _ => None,
        })
        .collect();

    assert!(
        log_lines.iter().any(|l| l.contains("line1")),
        "should contain line1, got: {log_lines:?}"
    );
    assert!(
        log_lines.iter().any(|l| l.contains("line2")),
        "should contain line2, got: {log_lines:?}"
    );
    assert!(
        log_lines.iter().any(|l| l.contains("line3")),
        "should contain line3, got: {log_lines:?}"
    );

    send_raw_request(&paths, &Request::Kill).await;
    let _ = handle.await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_log_lines_param_limits_output() {
    let dir = TempDir::new().unwrap();
    let paths = Paths::with_base(dir.path().to_path_buf());

    let handle = start_test_daemon(&paths).await;

    let mut configs = HashMap::new();
    configs.insert(
        "counter".to_string(),
        test_config("sh -c 'for i in 1 2 3 4 5 6 7 8 9 10; do echo line$i; done'"),
    );
    send_raw_request(
        &paths,
        &Request::Start {
            configs,
            names: None,
            env: None,
        },
    )
    .await;

    tokio::time::sleep(Duration::from_millis(500)).await;

    let responses = send_streaming_request(
        &paths,
        &Request::Log {
            name: Some("counter".to_string()),
            lines: 5,
            follow: false,
        },
    )
    .await;

    let log_lines: Vec<&str> = responses
        .iter()
        .filter_map(|r| match r {
            Response::LogLine { line, .. } => Some(line.as_str()),
            _ => None,
        })
        .collect();

    assert_eq!(
        log_lines.len(),
        5,
        "should show exactly 5 lines, got: {log_lines:?}"
    );
    // Should be the last 5 lines (line6..line10)
    assert!(
        log_lines.iter().any(|l| l.contains("line10")),
        "should contain line10, got: {log_lines:?}"
    );

    send_raw_request(&paths, &Request::Kill).await;
    let _ = handle.await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_log_no_name_interleaves_all_processes() {
    let dir = TempDir::new().unwrap();
    let paths = Paths::with_base(dir.path().to_path_buf());

    let handle = start_test_daemon(&paths).await;

    let mut configs = HashMap::new();
    configs.insert(
        "alpha".to_string(),
        test_config("sh -c 'echo alpha_output'"),
    );
    configs.insert("beta".to_string(), test_config("sh -c 'echo beta_output'"));
    send_raw_request(
        &paths,
        &Request::Start {
            configs,
            names: None,
            env: None,
        },
    )
    .await;

    tokio::time::sleep(Duration::from_millis(500)).await;

    let responses = send_streaming_request(
        &paths,
        &Request::Log {
            name: None,
            lines: 15,
            follow: false,
        },
    )
    .await;

    let log_lines: Vec<(&Option<String>, &str)> = responses
        .iter()
        .filter_map(|r| match r {
            Response::LogLine { name, line } => Some((name, line.as_str())),
            _ => None,
        })
        .collect();

    // Should have lines from both processes
    let has_alpha = log_lines.iter().any(|(_, l)| l.contains("alpha_output"));
    let has_beta = log_lines.iter().any(|(_, l)| l.contains("beta_output"));
    assert!(has_alpha, "should have alpha output, got: {log_lines:?}");
    assert!(has_beta, "should have beta output, got: {log_lines:?}");

    // All lines should have process name prefix when multiple processes
    for (name, _) in &log_lines {
        assert!(
            name.is_some(),
            "all lines should have process name when showing multiple processes"
        );
    }

    send_raw_request(&paths, &Request::Kill).await;
    let _ = handle.await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_log_single_process_no_name_prefix() {
    let dir = TempDir::new().unwrap();
    let paths = Paths::with_base(dir.path().to_path_buf());

    let handle = start_test_daemon(&paths).await;

    let mut configs = HashMap::new();
    configs.insert("solo".to_string(), test_config("sh -c 'echo solo_output'"));
    send_raw_request(
        &paths,
        &Request::Start {
            configs,
            names: None,
            env: None,
        },
    )
    .await;

    tokio::time::sleep(Duration::from_millis(500)).await;

    let responses = send_streaming_request(
        &paths,
        &Request::Log {
            name: Some("solo".to_string()),
            lines: 15,
            follow: false,
        },
    )
    .await;

    let log_lines: Vec<(&Option<String>, &str)> = responses
        .iter()
        .filter_map(|r| match r {
            Response::LogLine { name, line } => Some((name, line.as_str())),
            _ => None,
        })
        .collect();

    // Single process: no name prefix
    for (name, _) in &log_lines {
        assert!(name.is_none(), "single process should NOT have name prefix");
    }

    send_raw_request(&paths, &Request::Kill).await;
    let _ = handle.await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_log_nonexistent_process_returns_error() {
    let dir = TempDir::new().unwrap();
    let paths = Paths::with_base(dir.path().to_path_buf());

    let handle = start_test_daemon(&paths).await;

    let responses = send_streaming_request(
        &paths,
        &Request::Log {
            name: Some("nope".to_string()),
            lines: 15,
            follow: false,
        },
    )
    .await;

    assert_eq!(responses.len(), 1, "should get one error response");
    match &responses[0] {
        Response::Error { message } => {
            assert!(
                message.contains("not found"),
                "error should mention 'not found', got: {message}"
            );
        }
        other => panic!("expected Error, got: {other:?}"),
    }

    send_raw_request(&paths, &Request::Kill).await;
    let _ = handle.await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_log_follow_streams_new_lines() {
    let dir = TempDir::new().unwrap();
    let paths = Paths::with_base(dir.path().to_path_buf());

    let handle = start_test_daemon(&paths).await;

    // Start a process that writes lines slowly
    let mut configs = HashMap::new();
    configs.insert(
        "slow".to_string(),
        test_config("sh -c 'echo initial; sleep 0.3; echo follow1; sleep 0.3; echo follow2'"),
    );
    send_raw_request(
        &paths,
        &Request::Start {
            configs,
            names: None,
            env: None,
        },
    )
    .await;

    // Wait for the initial line to be written
    tokio::time::sleep(Duration::from_millis(200)).await;

    // Start a follow request in a background task with a timeout
    let paths_clone = paths.clone();
    let follow_handle = tokio::task::spawn_blocking(move || {
        let mut stream = UnixStream::connect(paths_clone.socket_file()).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();

        let request = Request::Log {
            name: Some("slow".to_string()),
            lines: 15,
            follow: true,
        };
        let encoded = protocol::encode_request(&request).unwrap();
        stream.write_all(&encoded).unwrap();
        stream.shutdown(std::net::Shutdown::Write).unwrap();

        let reader = BufReader::new(stream);
        let mut responses = Vec::new();
        for line_result in reader.lines() {
            match line_result {
                Ok(line) if !line.is_empty() => {
                    responses.push(protocol::decode_response(&line).unwrap());
                }
                _ => break, // timeout or EOF
            }
        }
        responses
    });

    let responses = follow_handle.await.unwrap();

    let log_lines: Vec<&str> = responses
        .iter()
        .filter_map(|r| match r {
            Response::LogLine { line, .. } => Some(line.as_str()),
            _ => None,
        })
        .collect();

    // Should have received the initial line and then the follow lines
    assert!(
        log_lines.iter().any(|l| l.contains("initial")),
        "should contain 'initial', got: {log_lines:?}"
    );
    assert!(
        log_lines.iter().any(|l| l.contains("follow1")),
        "should contain 'follow1', got: {log_lines:?}"
    );
    assert!(
        log_lines.iter().any(|l| l.contains("follow2")),
        "should contain 'follow2', got: {log_lines:?}"
    );

    send_raw_request(&paths, &Request::Kill).await;
    let _ = handle.await;
}

// ── Item 15: Flush command ──────────────────────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_flush_empties_log_file() {
    let dir = TempDir::new().unwrap();
    let paths = Paths::with_base(dir.path().to_path_buf());

    let handle = start_test_daemon(&paths).await;

    let mut configs = HashMap::new();
    configs.insert(
        "echoer".to_string(),
        test_config("sh -c 'echo flush_test_output; echo flush_err >&2'"),
    );
    send_raw_request(
        &paths,
        &Request::Start {
            configs,
            names: None,
            env: None,
        },
    )
    .await;

    tokio::time::sleep(Duration::from_millis(500)).await;

    // Verify logs have content
    let stdout_log = paths.stdout_log("echoer");
    let stderr_log = paths.stderr_log("echoer");
    assert!(stdout_log.exists(), "stdout log should exist");
    assert!(stderr_log.exists(), "stderr log should exist");
    assert!(
        !std::fs::read_to_string(&stdout_log).unwrap().is_empty(),
        "stdout log should have content before flush"
    );
    assert!(
        !std::fs::read_to_string(&stderr_log).unwrap().is_empty(),
        "stderr log should have content before flush"
    );

    // Flush by name
    let resp = send_raw_request(
        &paths,
        &Request::Flush {
            names: Some(vec!["echoer".to_string()]),
        },
    )
    .await;
    assert!(
        matches!(&resp, Response::Success { .. }),
        "expected Success, got: {resp:?}"
    );

    // Verify log files exist but are empty
    assert!(stdout_log.exists(), "stdout log should still exist");
    assert!(stderr_log.exists(), "stderr log should still exist");
    assert!(
        std::fs::read_to_string(&stdout_log).unwrap().is_empty(),
        "stdout log should be empty after flush"
    );
    assert!(
        std::fs::read_to_string(&stderr_log).unwrap().is_empty(),
        "stderr log should be empty after flush"
    );

    send_raw_request(&paths, &Request::Kill).await;
    let _ = handle.await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_flush_all_empties_all_logs() {
    let dir = TempDir::new().unwrap();
    let paths = Paths::with_base(dir.path().to_path_buf());

    let handle = start_test_daemon(&paths).await;

    let mut configs = HashMap::new();
    configs.insert(
        "alpha".to_string(),
        test_config("sh -c 'echo alpha_output'"),
    );
    configs.insert("beta".to_string(), test_config("sh -c 'echo beta_output'"));
    send_raw_request(
        &paths,
        &Request::Start {
            configs,
            names: None,
            env: None,
        },
    )
    .await;

    tokio::time::sleep(Duration::from_millis(500)).await;

    // Verify logs have content
    for name in &["alpha", "beta"] {
        let stdout_log = paths.stdout_log(name);
        assert!(stdout_log.exists(), "{name} stdout log should exist");
        assert!(
            !std::fs::read_to_string(&stdout_log).unwrap().is_empty(),
            "{name} stdout log should have content before flush"
        );
    }

    // Flush all (no names)
    let resp = send_raw_request(&paths, &Request::Flush { names: None }).await;
    assert!(
        matches!(&resp, Response::Success { .. }),
        "expected Success, got: {resp:?}"
    );

    // Verify all log files are empty
    for name in &["alpha", "beta"] {
        let stdout_log = paths.stdout_log(name);
        assert!(stdout_log.exists(), "{name} stdout log should still exist");
        assert!(
            std::fs::read_to_string(&stdout_log).unwrap().is_empty(),
            "{name} stdout log should be empty after flush"
        );
    }

    send_raw_request(&paths, &Request::Kill).await;
    let _ = handle.await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_flush_deletes_rotated_files() {
    let dir = TempDir::new().unwrap();
    let paths = Paths::with_base(dir.path().to_path_buf());

    let handle = start_test_daemon(&paths).await;

    // Start a process so it exists in the process table
    let mut configs = HashMap::new();
    configs.insert("worker".to_string(), test_config("sleep 999"));
    send_raw_request(
        &paths,
        &Request::Start {
            configs,
            names: None,
            env: None,
        },
    )
    .await;

    tokio::time::sleep(Duration::from_millis(300)).await;

    // Create rotated log files manually
    std::fs::create_dir_all(paths.log_dir()).unwrap();
    for i in 1..=3 {
        let rotated_stdout = paths.rotated_stdout_log("worker", i);
        let rotated_stderr = paths.rotated_stderr_log("worker", i);
        std::fs::write(&rotated_stdout, format!("rotated stdout {i}")).unwrap();
        std::fs::write(&rotated_stderr, format!("rotated stderr {i}")).unwrap();
    }

    // Verify rotated files exist
    for i in 1..=3 {
        assert!(
            paths.rotated_stdout_log("worker", i).exists(),
            "rotated stdout.{i} should exist before flush"
        );
        assert!(
            paths.rotated_stderr_log("worker", i).exists(),
            "rotated stderr.{i} should exist before flush"
        );
    }

    // Flush
    let resp = send_raw_request(
        &paths,
        &Request::Flush {
            names: Some(vec!["worker".to_string()]),
        },
    )
    .await;
    assert!(
        matches!(&resp, Response::Success { .. }),
        "expected Success, got: {resp:?}"
    );

    // Verify rotated files are deleted
    for i in 1..=3 {
        assert!(
            !paths.rotated_stdout_log("worker", i).exists(),
            "rotated stdout.{i} should be deleted after flush"
        );
        assert!(
            !paths.rotated_stderr_log("worker", i).exists(),
            "rotated stderr.{i} should be deleted after flush"
        );
    }

    send_raw_request(&paths, &Request::Kill).await;
    let _ = handle.await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_flush_nonexistent_process_returns_error() {
    let dir = TempDir::new().unwrap();
    let paths = Paths::with_base(dir.path().to_path_buf());

    let handle = start_test_daemon(&paths).await;

    let resp = send_raw_request(
        &paths,
        &Request::Flush {
            names: Some(vec!["nope".to_string()]),
        },
    )
    .await;
    match &resp {
        Response::Error { message } => {
            assert!(
                message.contains("not found"),
                "error should mention 'not found', got: {message}"
            );
        }
        other => panic!("expected Error, got: {other:?}"),
    }

    send_raw_request(&paths, &Request::Kill).await;
    let _ = handle.await;
}

// ── Item 16: Log timestamp tests ────────────────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_log_timestamp_prefix() {
    let dir = TempDir::new().unwrap();
    let paths = Paths::with_base(dir.path().to_path_buf());

    let handle = start_test_daemon(&paths).await;

    let mut config = test_config("sh -c 'echo line1; echo line2; echo line3'");
    config.log_date_format = Some("%Y-%m-%d %H:%M:%S".to_string());

    let mut configs = HashMap::new();
    configs.insert("ts-echo".to_string(), config);
    let start_resp = send_raw_request(
        &paths,
        &Request::Start {
            configs,
            names: None,
            env: None,
        },
    )
    .await;
    assert!(
        matches!(&start_resp, Response::Success { .. }),
        "expected Success, got: {start_resp:?}"
    );

    tokio::time::sleep(Duration::from_millis(500)).await;

    let stdout_log = paths.stdout_log("ts-echo");
    assert!(stdout_log.exists(), "stdout log file should exist");

    let content = std::fs::read_to_string(&stdout_log).unwrap();
    let re = Regex::new(r"^\d{4}-\d{2}-\d{2} \d{2}:\d{2}:\d{2} \| .+$").unwrap();
    let lines: Vec<&str> = content.lines().collect();
    assert_eq!(lines.len(), 3, "should have 3 lines, got: {lines:?}");
    for line in &lines {
        assert!(
            re.is_match(line),
            "line did not match timestamp pattern: {line}"
        );
    }
    // Verify original content is preserved after the separator
    assert!(content.contains("line1"), "content should contain 'line1'");
    assert!(content.contains("line2"), "content should contain 'line2'");
    assert!(content.contains("line3"), "content should contain 'line3'");

    send_raw_request(&paths, &Request::Kill).await;
    let _ = handle.await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_log_no_timestamp_without_format() {
    let dir = TempDir::new().unwrap();
    let paths = Paths::with_base(dir.path().to_path_buf());

    let handle = start_test_daemon(&paths).await;

    // Default test_config has log_date_format = None
    let mut configs = HashMap::new();
    configs.insert(
        "no-ts".to_string(),
        test_config("sh -c 'echo plain1; echo plain2'"),
    );
    let start_resp = send_raw_request(
        &paths,
        &Request::Start {
            configs,
            names: None,
            env: None,
        },
    )
    .await;
    assert!(
        matches!(&start_resp, Response::Success { .. }),
        "expected Success, got: {start_resp:?}"
    );

    tokio::time::sleep(Duration::from_millis(500)).await;

    let stdout_log = paths.stdout_log("no-ts");
    assert!(stdout_log.exists(), "stdout log file should exist");

    let content = std::fs::read_to_string(&stdout_log).unwrap();
    let lines: Vec<&str> = content.lines().collect();
    assert_eq!(lines.len(), 2, "should have 2 lines, got: {lines:?}");
    assert_eq!(lines[0], "plain1");
    assert_eq!(lines[1], "plain2");
    for line in &lines {
        assert!(
            !line.contains(" | "),
            "line should not contain ' | ' separator: {line}"
        );
    }

    send_raw_request(&paths, &Request::Kill).await;
    let _ = handle.await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_log_timestamp_stderr() {
    let dir = TempDir::new().unwrap();
    let paths = Paths::with_base(dir.path().to_path_buf());

    let handle = start_test_daemon(&paths).await;

    let mut config = test_config("sh -c 'echo err1 >&2; echo err2 >&2'");
    config.log_date_format = Some("%Y-%m-%d %H:%M:%S".to_string());

    let mut configs = HashMap::new();
    configs.insert("ts-err".to_string(), config);
    let start_resp = send_raw_request(
        &paths,
        &Request::Start {
            configs,
            names: None,
            env: None,
        },
    )
    .await;
    assert!(
        matches!(&start_resp, Response::Success { .. }),
        "expected Success, got: {start_resp:?}"
    );

    tokio::time::sleep(Duration::from_millis(500)).await;

    let stderr_log = paths.stderr_log("ts-err");
    assert!(stderr_log.exists(), "stderr log file should exist");

    let content = std::fs::read_to_string(&stderr_log).unwrap();
    let re = Regex::new(r"^\d{4}-\d{2}-\d{2} \d{2}:\d{2}:\d{2} \| .+$").unwrap();
    let lines: Vec<&str> = content.lines().collect();
    assert_eq!(lines.len(), 2, "should have 2 lines, got: {lines:?}");
    for line in &lines {
        assert!(
            re.is_match(line),
            "stderr line did not match timestamp pattern: {line}"
        );
    }
    assert!(content.contains("err1"), "content should contain 'err1'");
    assert!(content.contains("err2"), "content should contain 'err2'");

    send_raw_request(&paths, &Request::Kill).await;
    let _ = handle.await;
}

// ── Item 17: Log rotation ───────────────────────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_log_rotation_creates_rotated_file() {
    let dir = TempDir::new().unwrap();
    let paths = Paths::with_base(dir.path().to_path_buf());

    let handle = start_test_daemon(&paths).await;

    // Generate 12,000 lines × 1000 bytes = 12MB (> 10MB threshold)
    let mut configs = HashMap::new();
    configs.insert(
        "biglog".to_string(),
        test_config("sh -c 'yes \"$(printf \"%0999d\" 0)\" | head -n 12000'"),
    );
    send_raw_request(
        &paths,
        &Request::Start {
            configs,
            names: None,
            env: None,
        },
    )
    .await;

    // Wait for the log copier to flush
    tokio::time::sleep(Duration::from_secs(5)).await;

    let rotated = paths.rotated_stdout_log("biglog", 1);
    assert!(
        rotated.exists(),
        "rotated stdout log .1 should exist after writing >10MB"
    );

    send_raw_request(&paths, &Request::Kill).await;
    let _ = handle.await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_log_rotation_only_keeps_three_files() {
    let dir = TempDir::new().unwrap();
    let paths = Paths::with_base(dir.path().to_path_buf());

    let handle = start_test_daemon(&paths).await;

    // 1. Start "rotator" with a quick command that exits immediately
    let mut configs = HashMap::new();
    configs.insert(
        "rotator".to_string(),
        test_config("sh -c 'echo setup_done'"),
    );
    send_raw_request(
        &paths,
        &Request::Start {
            configs,
            names: None,
            env: None,
        },
    )
    .await;

    // 2. Wait for log copier to finish
    tokio::time::sleep(Duration::from_millis(500)).await;

    // 3. Stop "rotator" (may already be stopped)
    let _ = send_raw_request(
        &paths,
        &Request::Stop {
            names: Some(vec!["rotator".to_string()]),
        },
    )
    .await;

    // 4. Wait for stop to settle
    tokio::time::sleep(Duration::from_millis(300)).await;

    // 5. Seed: overwrite stdout log with exactly LOG_ROTATION_SIZE bytes of padding
    let stdout_log = paths.stdout_log("rotator");
    std::fs::write(&stdout_log, vec![b'X'; LOG_ROTATION_SIZE as usize]).unwrap();

    // 6. Seed: create .1, .2, .3 manually
    for i in 1..=3 {
        std::fs::write(
            paths.rotated_stdout_log("rotator", i),
            format!("old-rotated-{i}"),
        )
        .unwrap();
    }

    // 7. Restart "rotator" — log copier opens file, sees 10MB, first line triggers rotation
    let restart_resp = send_raw_request(
        &paths,
        &Request::Restart {
            names: Some(vec!["rotator".to_string()]),
        },
    )
    .await;
    assert!(
        matches!(&restart_resp, Response::Success { .. }),
        "expected Success, got: {restart_resp:?}"
    );

    // 8. Wait for rotation to happen
    tokio::time::sleep(Duration::from_secs(2)).await;

    // 9. Verify .1, .2, .3 exist; .4 does NOT
    for i in 1..=3 {
        assert!(
            paths.rotated_stdout_log("rotator", i).exists(),
            "rotated stdout log .{i} should exist"
        );
    }
    assert!(
        !paths.rotated_stdout_log("rotator", 4).exists(),
        "rotated stdout log .4 should NOT exist (max keep is 3)"
    );

    send_raw_request(&paths, &Request::Kill).await;
    let _ = handle.await;
}

// ── Item 18: Restart policy ─────────────────────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_restart_policy_never_does_not_restart() {
    let dir = TempDir::new().unwrap();
    let paths = Paths::with_base(dir.path().to_path_buf());

    let handle = start_test_daemon(&paths).await;

    let mut config = test_config("sh -c 'exit 1'");
    config.restart = Some(RestartPolicy::Never);

    let mut configs = HashMap::new();
    configs.insert("never-restart".to_string(), config);
    send_raw_request(
        &paths,
        &Request::Start {
            configs,
            names: None,
            env: None,
        },
    )
    .await;

    tokio::time::sleep(Duration::from_millis(500)).await;

    let list_resp = send_raw_request(&paths, &Request::List).await;
    match &list_resp {
        Response::ProcessList { processes } => {
            assert_eq!(processes.len(), 1);
            let info = &processes[0];
            assert_eq!(info.status, ProcessStatus::Errored);
            assert_eq!(info.restarts, 0);
            assert!(info.pid.is_none(), "pid should be None after exit");
        }
        other => panic!("expected ProcessList, got: {other:?}"),
    }

    send_raw_request(&paths, &Request::Kill).await;
    let _ = handle.await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_restart_policy_always_restarts_on_clean_exit() {
    let dir = TempDir::new().unwrap();
    let paths = Paths::with_base(dir.path().to_path_buf());

    let handle = start_test_daemon(&paths).await;

    let mut config = test_config("sh -c 'exit 0'");
    config.restart = Some(RestartPolicy::Always);
    config.max_restarts = Some(2);

    let mut configs = HashMap::new();
    configs.insert("always-restart".to_string(), config);
    send_raw_request(
        &paths,
        &Request::Start {
            configs,
            names: None,
            env: None,
        },
    )
    .await;

    tokio::time::sleep(Duration::from_millis(1500)).await;

    let list_resp = send_raw_request(&paths, &Request::List).await;
    match &list_resp {
        Response::ProcessList { processes } => {
            assert_eq!(processes.len(), 1);
            let info = &processes[0];
            assert_eq!(info.status, ProcessStatus::Stopped);
            assert_eq!(info.restarts, 2);
            assert!(info.pid.is_none(), "pid should be None after final exit");
        }
        other => panic!("expected ProcessList, got: {other:?}"),
    }

    send_raw_request(&paths, &Request::Kill).await;
    let _ = handle.await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_restart_policy_on_failure_exit_zero_not_restarted() {
    let dir = TempDir::new().unwrap();
    let paths = Paths::with_base(dir.path().to_path_buf());

    let handle = start_test_daemon(&paths).await;

    let mut config = test_config("sh -c 'exit 0'");
    config.restart = Some(RestartPolicy::OnFailure);

    let mut configs = HashMap::new();
    configs.insert("on-failure-clean".to_string(), config);
    send_raw_request(
        &paths,
        &Request::Start {
            configs,
            names: None,
            env: None,
        },
    )
    .await;

    tokio::time::sleep(Duration::from_millis(500)).await;

    let list_resp = send_raw_request(&paths, &Request::List).await;
    match &list_resp {
        Response::ProcessList { processes } => {
            assert_eq!(processes.len(), 1);
            let info = &processes[0];
            assert_eq!(info.status, ProcessStatus::Stopped);
            assert_eq!(info.restarts, 0);
            assert!(info.pid.is_none(), "pid should be None after exit");
        }
        other => panic!("expected ProcessList, got: {other:?}"),
    }

    send_raw_request(&paths, &Request::Kill).await;
    let _ = handle.await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_restart_policy_on_failure_exit_nonzero_restarts() {
    let dir = TempDir::new().unwrap();
    let paths = Paths::with_base(dir.path().to_path_buf());

    let handle = start_test_daemon(&paths).await;

    let mut config = test_config("sh -c 'exit 1'");
    config.restart = Some(RestartPolicy::OnFailure);
    config.max_restarts = Some(2);

    let mut configs = HashMap::new();
    configs.insert("on-failure-err".to_string(), config);
    send_raw_request(
        &paths,
        &Request::Start {
            configs,
            names: None,
            env: None,
        },
    )
    .await;

    tokio::time::sleep(Duration::from_millis(1500)).await;

    let list_resp = send_raw_request(&paths, &Request::List).await;
    match &list_resp {
        Response::ProcessList { processes } => {
            assert_eq!(processes.len(), 1);
            let info = &processes[0];
            assert_eq!(info.status, ProcessStatus::Errored);
            assert_eq!(info.restarts, 2);
            assert!(info.pid.is_none(), "pid should be None after final exit");
        }
        other => panic!("expected ProcessList, got: {other:?}"),
    }

    send_raw_request(&paths, &Request::Kill).await;
    let _ = handle.await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_restart_policy_stop_exit_codes_prevents_restart() {
    let dir = TempDir::new().unwrap();
    let paths = Paths::with_base(dir.path().to_path_buf());

    let handle = start_test_daemon(&paths).await;

    let mut config = test_config("sh -c 'exit 42'");
    config.restart = Some(RestartPolicy::OnFailure);
    config.stop_exit_codes = Some(vec![42]);

    let mut configs = HashMap::new();
    configs.insert("stop-code".to_string(), config);
    send_raw_request(
        &paths,
        &Request::Start {
            configs,
            names: None,
            env: None,
        },
    )
    .await;

    tokio::time::sleep(Duration::from_millis(500)).await;

    let list_resp = send_raw_request(&paths, &Request::List).await;
    match &list_resp {
        Response::ProcessList { processes } => {
            assert_eq!(processes.len(), 1);
            let info = &processes[0];
            assert_eq!(info.status, ProcessStatus::Errored);
            assert_eq!(info.restarts, 0);
            assert!(info.pid.is_none(), "pid should be None after exit");
        }
        other => panic!("expected ProcessList, got: {other:?}"),
    }

    send_raw_request(&paths, &Request::Kill).await;
    let _ = handle.await;
}

// ── Item 19: Auto-restart ───────────────────────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_auto_restart_recovers_after_crash() {
    let dir = TempDir::new().unwrap();
    let paths = Paths::with_base(dir.path().to_path_buf());

    let handle = start_test_daemon(&paths).await;

    let marker = dir.path().join("marker");
    let cmd = format!(
        "sh -c 'MARKER={}; if [ ! -f $MARKER ]; then touch $MARKER; exit 1; fi; sleep 999'",
        marker.display()
    );
    let mut config = test_config(&cmd);
    config.restart = Some(RestartPolicy::OnFailure);
    config.max_restarts = Some(5);

    let mut configs = HashMap::new();
    configs.insert("crash-once".to_string(), config);
    send_raw_request(
        &paths,
        &Request::Start {
            configs,
            names: None,
            env: None,
        },
    )
    .await;

    tokio::time::sleep(Duration::from_millis(1000)).await;

    let list_resp = send_raw_request(&paths, &Request::List).await;
    match &list_resp {
        Response::ProcessList { processes } => {
            assert_eq!(processes.len(), 1);
            let info = &processes[0];
            assert_eq!(info.status, ProcessStatus::Online);
            assert_eq!(info.restarts, 1);
            assert!(info.pid.is_some(), "pid should be present after recovery");
        }
        other => panic!("expected ProcessList, got: {other:?}"),
    }

    send_raw_request(&paths, &Request::Kill).await;
    let _ = handle.await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_auto_restart_stops_after_max_restarts() {
    let dir = TempDir::new().unwrap();
    let paths = Paths::with_base(dir.path().to_path_buf());

    let handle = start_test_daemon(&paths).await;

    let mut config = test_config("sh -c 'exit 1'");
    config.restart = Some(RestartPolicy::OnFailure);
    config.max_restarts = Some(3);

    let mut configs = HashMap::new();
    configs.insert("max-restart".to_string(), config);
    send_raw_request(
        &paths,
        &Request::Start {
            configs,
            names: None,
            env: None,
        },
    )
    .await;

    tokio::time::sleep(Duration::from_millis(2000)).await;

    let list_resp = send_raw_request(&paths, &Request::List).await;
    match &list_resp {
        Response::ProcessList { processes } => {
            assert_eq!(processes.len(), 1);
            let info = &processes[0];
            assert_eq!(info.status, ProcessStatus::Errored);
            assert_eq!(info.restarts, 3);
            assert!(info.pid.is_none(), "pid should be None after max restarts");
        }
        other => panic!("expected ProcessList, got: {other:?}"),
    }

    send_raw_request(&paths, &Request::Kill).await;
    let _ = handle.await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_auto_restart_list_shows_restart_count() {
    let dir = TempDir::new().unwrap();
    let paths = Paths::with_base(dir.path().to_path_buf());

    let handle = start_test_daemon(&paths).await;

    let stable_config = test_config("sleep 999");

    let mut crasher_config = test_config("sh -c 'exit 1'");
    crasher_config.restart = Some(RestartPolicy::OnFailure);
    crasher_config.max_restarts = Some(2);

    let mut configs = HashMap::new();
    configs.insert("stable".to_string(), stable_config);
    configs.insert("crasher".to_string(), crasher_config);
    send_raw_request(
        &paths,
        &Request::Start {
            configs,
            names: None,
            env: None,
        },
    )
    .await;

    tokio::time::sleep(Duration::from_millis(1500)).await;

    let list_resp = send_raw_request(&paths, &Request::List).await;
    match &list_resp {
        Response::ProcessList { processes } => {
            assert_eq!(processes.len(), 2);
            let stable = processes.iter().find(|p| p.name == "stable").unwrap();
            let crasher = processes.iter().find(|p| p.name == "crasher").unwrap();

            assert_eq!(stable.status, ProcessStatus::Online);
            assert_eq!(stable.restarts, 0);

            assert_eq!(crasher.status, ProcessStatus::Errored);
            assert_eq!(crasher.restarts, 2);
        }
        other => panic!("expected ProcessList, got: {other:?}"),
    }

    send_raw_request(&paths, &Request::Kill).await;
    let _ = handle.await;
}

// ── Item 20: Exponential backoff ────────────────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_exponential_backoff_increases_delay_between_restarts() {
    let dir = TempDir::new().unwrap();
    let paths = Paths::with_base(dir.path().to_path_buf());

    let handle = start_test_daemon(&paths).await;

    let mut config = test_config("sh -c 'exit 1'");
    config.restart = Some(RestartPolicy::OnFailure);
    config.max_restarts = Some(3);

    let mut configs = HashMap::new();
    configs.insert("backoff-test".to_string(), config);

    let start = Instant::now();
    send_raw_request(
        &paths,
        &Request::Start {
            configs,
            names: None,
            env: None,
        },
    )
    .await;

    // Wait for all 3 restarts to exhaust.
    // Backoff: 100ms (count=0) + 200ms (count=1) + 400ms (count=2) = 700ms minimum
    tokio::time::sleep(Duration::from_millis(2000)).await;

    let elapsed = start.elapsed();

    let list_resp = send_raw_request(&paths, &Request::List).await;
    match &list_resp {
        Response::ProcessList { processes } => {
            assert_eq!(processes.len(), 1);
            let info = &processes[0];
            assert_eq!(info.status, ProcessStatus::Errored);
            assert_eq!(info.restarts, 3);
            // Total backoff must be at least 700ms (100 + 200 + 400)
            assert!(
                elapsed >= Duration::from_millis(700),
                "expected at least 700ms of backoff, but only {elapsed:?} elapsed"
            );
        }
        other => panic!("expected ProcessList, got: {other:?}"),
    }

    send_raw_request(&paths, &Request::Kill).await;
    let _ = handle.await;
}

// ── Item 21: min_uptime ─────────────────────────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_min_uptime_stable_run_resets_restart_count() {
    let dir = TempDir::new().unwrap();
    let paths = Paths::with_base(dir.path().to_path_buf());

    let handle = start_test_daemon(&paths).await;

    // Marker-file counter: first two runs crash instantly, third runs past
    // min_uptime then crashes, fourth stays up.
    let counter = dir.path().join("counter");
    let cmd = format!(
        concat!(
            "sh -c '",
            "C=$(cat {} 2>/dev/null || echo 0); ",
            "echo $((C + 1)) > {}; ",
            "if [ $C -lt 2 ]; then exit 1; fi; ",
            "if [ $C -eq 2 ]; then sleep 0.5; exit 1; fi; ",
            "sleep 999",
            "'"
        ),
        counter.display(),
        counter.display()
    );
    let mut config = test_config(&cmd);
    config.restart = Some(RestartPolicy::OnFailure);
    config.max_restarts = Some(2);
    config.min_uptime = Some(200);

    let mut configs = HashMap::new();
    configs.insert("min-uptime-reset".to_string(), config);
    send_raw_request(
        &paths,
        &Request::Start {
            configs,
            names: None,
            env: None,
        },
    )
    .await;

    // Two quick crashes (100+200ms backoff) + one 500ms run + 100ms backoff + spawn
    tokio::time::sleep(Duration::from_millis(2000)).await;

    let list_resp = send_raw_request(&paths, &Request::List).await;
    match &list_resp {
        Response::ProcessList { processes } => {
            assert_eq!(processes.len(), 1);
            let info = &processes[0];
            assert_eq!(info.status, ProcessStatus::Online);
            // Without min_uptime reset restarts would hit max (2) and stop.
            // The reset allows the fourth run to succeed with restarts = 1.
            assert_eq!(info.restarts, 1);
            assert!(info.pid.is_some(), "pid should be present");
        }
        other => panic!("expected ProcessList, got: {other:?}"),
    }

    send_raw_request(&paths, &Request::Kill).await;
    let _ = handle.await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_min_uptime_quick_crash_increments_restart_count() {
    let dir = TempDir::new().unwrap();
    let paths = Paths::with_base(dir.path().to_path_buf());

    let handle = start_test_daemon(&paths).await;

    let mut config = test_config("sh -c 'exit 1'");
    config.restart = Some(RestartPolicy::OnFailure);
    config.max_restarts = Some(3);
    config.min_uptime = Some(5000); // 5s — process never lives this long

    let mut configs = HashMap::new();
    configs.insert("quick-crash".to_string(), config);
    send_raw_request(
        &paths,
        &Request::Start {
            configs,
            names: None,
            env: None,
        },
    )
    .await;

    tokio::time::sleep(Duration::from_millis(2000)).await;

    let list_resp = send_raw_request(&paths, &Request::List).await;
    match &list_resp {
        Response::ProcessList { processes } => {
            assert_eq!(processes.len(), 1);
            let info = &processes[0];
            assert_eq!(info.status, ProcessStatus::Errored);
            assert_eq!(info.restarts, 3);
            assert!(info.pid.is_none(), "pid should be None after max restarts");
        }
        other => panic!("expected ProcessList, got: {other:?}"),
    }

    send_raw_request(&paths, &Request::Kill).await;
    let _ = handle.await;
}
