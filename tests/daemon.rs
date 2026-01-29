use pm3::daemon;
use pm3::paths::Paths;
use pm3::protocol::{self, Request, Response};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::time::Duration;
use tempfile::TempDir;

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
