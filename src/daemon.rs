use crate::config::ProcessConfig;
use crate::paths::Paths;
use crate::pid;
use crate::process::{self, ProcessTable};
use crate::protocol::{self, Request, Response};
use color_eyre::eyre::bail;
use std::collections::HashMap;
use std::fs;
use std::sync::Arc;
use tokio::sync::RwLock;
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
    let processes: Arc<RwLock<ProcessTable>> = Arc::new(RwLock::new(HashMap::new()));

    let result =
        run_accept_loop(&paths, &listener, &shutdown_tx, &mut shutdown_rx, &processes).await;

    // Kill all managed processes before cleanup
    {
        let mut table = processes.write().await;
        for (_, managed) in table.iter_mut() {
            let _ = managed.child.start_kill();
        }
    }

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
    processes: &Arc<RwLock<ProcessTable>>,
) -> color_eyre::Result<()> {
    loop {
        tokio::select! {
            accept_result = listener.accept() => {
                let (stream, _addr) = accept_result?;
                let tx = shutdown_tx.clone();
                let _paths = paths.clone();
                let procs = Arc::clone(processes);
                tokio::spawn(async move {
                    if let Err(e) = handle_connection(stream, &tx, &procs).await {
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
    processes: &Arc<RwLock<ProcessTable>>,
) -> color_eyre::Result<()> {
    let (reader, mut writer) = stream.into_split();
    let mut buf_reader = BufReader::new(reader);
    let mut line = String::new();
    buf_reader.read_line(&mut line).await?;

    if line.is_empty() {
        return Ok(());
    }

    let request = protocol::decode_request(&line)?;
    let response = dispatch(request, shutdown_tx, processes).await;
    let encoded = protocol::encode_response(&response)?;
    writer.write_all(&encoded).await?;
    writer.shutdown().await?;

    Ok(())
}

async fn dispatch(
    request: Request,
    shutdown_tx: &watch::Sender<bool>,
    processes: &Arc<RwLock<ProcessTable>>,
) -> Response {
    match request {
        Request::Start {
            configs, names, ..
        } => handle_start(configs, names, processes).await,
        Request::List => {
            let table = processes.read().await;
            let infos: Vec<_> = table.values().map(|m| m.to_process_info()).collect();
            Response::ProcessList { processes: infos }
        }
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

async fn handle_start(
    configs: HashMap<String, ProcessConfig>,
    names: Option<Vec<String>>,
    processes: &Arc<RwLock<ProcessTable>>,
) -> Response {
    let to_start: Vec<(String, ProcessConfig)> = match names {
        Some(ref requested) => {
            let mut selected = Vec::new();
            for name in requested {
                match configs.get(name) {
                    Some(config) => selected.push((name.clone(), config.clone())),
                    None => {
                        return Response::Error {
                            message: format!("process '{}' not found in configs", name),
                        };
                    }
                }
            }
            selected
        }
        None => configs.into_iter().collect(),
    };

    let mut started = Vec::new();
    let mut table = processes.write().await;

    for (name, config) in to_start {
        if table.contains_key(&name) {
            continue;
        }

        match process::spawn_process(name.clone(), config) {
            Ok(managed) => {
                table.insert(name.clone(), managed);
                started.push(name);
            }
            Err(e) => {
                return Response::Error {
                    message: format!("failed to start '{}': {}", name, e),
                };
            }
        }
    }

    Response::Success {
        message: Some(format!("started: {}", started.join(", "))),
    }
}
