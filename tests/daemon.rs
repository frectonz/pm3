use pm3::config::{self, ProcessConfig};
use pm3::daemon;
use pm3::paths::Paths;
use pm3::protocol::{self, ProcessStatus, Request, Response};
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::time::Duration;
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
