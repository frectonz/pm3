use crate::paths::Paths;
use crate::pid;
use crate::protocol::{self, Request, Response};
use color_eyre::eyre::bail;
use std::fs;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixListener;
use tokio::sync::watch;

pub async fn run(paths: Paths) -> color_eyre::Result<()> {
    fs::create_dir_all(paths.data_dir())?;

    if pid::is_daemon_running(&paths)? {
        bail!("daemon is already running");
    }

    pid::write_pid_file(&paths)?;

    // Remove stale socket file if it exists
    let socket_path = paths.socket_file();
    if socket_path.exists() {
        fs::remove_file(&socket_path)?;
    }

    let listener = UnixListener::bind(&socket_path)?;

    let (shutdown_tx, mut shutdown_rx) = watch::channel(false);

    let result = run_accept_loop(&paths, &listener, &shutdown_tx, &mut shutdown_rx).await;

    // Cleanup
    let _ = fs::remove_file(paths.socket_file());
    pid::remove_pid_file(&paths);

    result
}

async fn run_accept_loop(
    paths: &Paths,
    listener: &UnixListener,
    shutdown_tx: &watch::Sender<bool>,
    shutdown_rx: &mut watch::Receiver<bool>,
) -> color_eyre::Result<()> {
    loop {
        tokio::select! {
            accept_result = listener.accept() => {
                let (stream, _addr) = accept_result?;
                let tx = shutdown_tx.clone();
                let _paths = paths.clone();
                tokio::spawn(async move {
                    if let Err(e) = handle_connection(stream, &tx).await {
                        eprintln!("connection error: {e}");
                    }
                });
            }
            _ = shutdown_rx.changed() => {
                if *shutdown_rx.borrow() {
                    break;
                }
            }
            _ = signal_shutdown() => {
                break;
            }
        }
    }

    Ok(())
}

async fn signal_shutdown() {
    let mut sigterm =
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()).unwrap();
    let mut sigint =
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt()).unwrap();

    tokio::select! {
        _ = sigterm.recv() => {}
        _ = sigint.recv() => {}
    }
}

async fn handle_connection(
    stream: tokio::net::UnixStream,
    shutdown_tx: &watch::Sender<bool>,
) -> color_eyre::Result<()> {
    let (reader, mut writer) = stream.into_split();
    let mut buf_reader = BufReader::new(reader);
    let mut line = String::new();
    buf_reader.read_line(&mut line).await?;

    if line.is_empty() {
        return Ok(());
    }

    let request = protocol::decode_request(&line)?;
    let response = dispatch(request, shutdown_tx);
    let encoded = protocol::encode_response(&response)?;
    writer.write_all(&encoded).await?;
    writer.shutdown().await?;

    Ok(())
}

fn dispatch(request: Request, shutdown_tx: &watch::Sender<bool>) -> Response {
    match request {
        Request::List => Response::ProcessList {
            processes: vec![],
        },
        Request::Kill => {
            let _ = shutdown_tx.send(true);
            Response::Success {
                message: Some("daemon shutting down".to_string()),
            }
        }
        _ => Response::Error {
            message: "not implemented".to_string(),
        },
    }
}
