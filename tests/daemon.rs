use pm3::config::ProcessConfig;
use pm3::daemon;
use pm3::paths::Paths;
use pm3::protocol::{self, Request, Response};
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
            assert_eq!(
                info.status,
                pm3::protocol::ProcessStatus::Online
            );

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
