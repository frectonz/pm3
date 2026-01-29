use crate::paths::Paths;
use crate::pid;
use crate::protocol::{self, Request, Response};
use color_eyre::eyre::{bail, Context};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::time::Duration;

pub fn send_request(paths: &Paths, request: &Request) -> color_eyre::Result<Response> {
    ensure_daemon_running(paths)?;
    let mut stream = connect_with_retry(paths, 10, Duration::from_millis(200))?;

    let encoded = protocol::encode_request(request)?;
    stream.write_all(&encoded)?;
    stream.shutdown(std::net::Shutdown::Write)?;

    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader.read_line(&mut line)?;

    let response = protocol::decode_response(&line)?;
    Ok(response)
}

fn ensure_daemon_running(paths: &Paths) -> color_eyre::Result<()> {
    if pid::is_daemon_running(paths)? {
        return Ok(());
    }

    spawn_daemon()?;

    // Wait for socket file to appear
    let socket = paths.socket_file();
    for _ in 0..50 {
        if socket.exists() {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(100));
    }

    bail!("timed out waiting for daemon to start");
}

fn spawn_daemon() -> color_eyre::Result<()> {
    let exe = std::env::current_exe().context("failed to get current executable path")?;

    std::process::Command::new(exe)
        .arg("--daemon")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .process_group(0)
        .spawn()
        .context("failed to spawn daemon")?;

    Ok(())
}

fn connect_with_retry(
    paths: &Paths,
    retries: u32,
    delay: Duration,
) -> color_eyre::Result<UnixStream> {
    let socket = paths.socket_file();

    for attempt in 0..retries {
        match UnixStream::connect(&socket) {
            Ok(stream) => return Ok(stream),
            Err(e) => {
                if attempt == retries - 1 {
                    bail!("failed to connect to daemon after {retries} attempts: {e}");
                }
                std::thread::sleep(delay);
            }
        }
    }

    unreachable!()
}
