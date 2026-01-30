use crate::config::ProcessConfig;
use crate::paths::Paths;
use crate::pid;
use crate::process::{self, ProcessTable};
use crate::protocol::{self, Request, Response};
use color_eyre::eyre::bail;
use std::collections::HashMap;
use std::io::SeekFrom;
use std::sync::Arc;
use tokio::fs::{self, File};
use tokio::io::{AsyncBufReadExt, AsyncSeekExt, AsyncWriteExt, BufReader};
use tokio::net::UnixListener;
use tokio::sync::RwLock;
use tokio::sync::watch;

pub async fn run(paths: Paths) -> color_eyre::Result<()> {
    fs::create_dir_all(paths.data_dir()).await?;

    if pid::is_daemon_running(&paths).await? {
        bail!("daemon is already running");
    }

    pid::write_pid_file(&paths).await?;

    // Remove stale socket file if it exists
    let socket_path = paths.socket_file();
    if socket_path.exists() {
        fs::remove_file(&socket_path).await?;
    }

    let listener = UnixListener::bind(&socket_path)?;

    let (shutdown_tx, mut shutdown_rx) = watch::channel(false);
    let processes: Arc<RwLock<ProcessTable>> = Arc::new(RwLock::new(HashMap::new()));

    let result = run_accept_loop(
        &paths,
        &listener,
        &shutdown_tx,
        &mut shutdown_rx,
        &processes,
    )
    .await;

    // Gracefully stop all managed processes before cleanup
    {
        let mut table = processes.write().await;
        for (_, managed) in table.iter_mut() {
            let _ = managed.graceful_stop().await;
        }
    }

    // Cleanup
    let _ = fs::remove_file(paths.socket_file()).await;
    pid::remove_pid_file(&paths).await;

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
                let paths = paths.clone();
                let procs = Arc::clone(processes);
                tokio::spawn(async move {
                    if let Err(e) = handle_connection(stream, &tx, &procs, &paths).await {
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
    paths: &Paths,
) -> color_eyre::Result<()> {
    let (reader, mut writer) = stream.into_split();
    let mut buf_reader = BufReader::new(reader);
    let mut line = String::new();
    buf_reader.read_line(&mut line).await?;

    if line.is_empty() {
        return Ok(());
    }

    let request = protocol::decode_request(&line)?;

    // Log requests with follow need special handling (streaming)
    if let Request::Log { name, lines, follow } = request {
        handle_log(name, lines, follow, processes, paths, &mut writer).await?;
    } else {
        let response = dispatch(request, shutdown_tx, processes, paths).await;
        let encoded = protocol::encode_response(&response)?;
        writer.write_all(&encoded).await?;
    }

    writer.shutdown().await?;

    Ok(())
}

async fn dispatch(
    request: Request,
    shutdown_tx: &watch::Sender<bool>,
    processes: &Arc<RwLock<ProcessTable>>,
    paths: &Paths,
) -> Response {
    match request {
        Request::Start { configs, names, .. } => {
            handle_start(configs, names, processes, paths).await
        }
        Request::List => {
            let table = processes.read().await;
            let infos: Vec<_> = table.values().map(|m| m.to_process_info()).collect();
            Response::ProcessList { processes: infos }
        }
        Request::Stop { names } => handle_stop(names, processes).await,
        Request::Restart { names } => handle_restart(names, processes, paths).await,
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
    paths: &Paths,
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

        match process::spawn_process(name.clone(), config, paths).await {
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

    if started.is_empty() {
        Response::Success {
            message: Some("everything is already running".to_string()),
        }
    } else {
        Response::Success {
            message: Some(format!("started: {}", started.join(", "))),
        }
    }
}

async fn handle_stop(
    names: Option<Vec<String>>,
    processes: &Arc<RwLock<ProcessTable>>,
) -> Response {
    let mut table = processes.write().await;

    let targets: Vec<String> = match names {
        Some(ref requested) => {
            for name in requested {
                if !table.contains_key(name) {
                    return Response::Error {
                        message: format!("process not found: {name}"),
                    };
                }
            }
            requested.clone()
        }
        None => table.keys().cloned().collect(),
    };

    let mut stopped = Vec::new();
    for name in &targets {
        let managed = table.get_mut(name).unwrap();
        if managed.status == protocol::ProcessStatus::Stopped {
            continue;
        }
        if let Err(e) = managed.graceful_stop().await {
            return Response::Error {
                message: format!("failed to stop '{}': {}", name, e),
            };
        }
        stopped.push(name.clone());
    }

    Response::Success {
        message: Some(format!("stopped: {}", stopped.join(", "))),
    }
}

async fn handle_restart(
    names: Option<Vec<String>>,
    processes: &Arc<RwLock<ProcessTable>>,
    paths: &Paths,
) -> Response {
    let mut table = processes.write().await;

    let targets: Vec<String> = match names {
        Some(ref requested) => {
            for name in requested {
                if !table.contains_key(name) {
                    return Response::Error {
                        message: format!("process not found: {name}"),
                    };
                }
            }
            requested.clone()
        }
        None => table.keys().cloned().collect(),
    };

    let mut restarted = Vec::new();
    for name in &targets {
        let managed = table.get_mut(name).unwrap();
        let config = managed.config.clone();
        let old_restarts = managed.restarts;

        if managed.status != protocol::ProcessStatus::Stopped
            && let Err(e) = managed.graceful_stop().await
        {
            return Response::Error {
                message: format!("failed to stop '{}': {}", name, e),
            };
        }

        match process::spawn_process(name.clone(), config, paths).await {
            Ok(mut new_managed) => {
                new_managed.restarts = old_restarts + 1;
                table.insert(name.clone(), new_managed);
                restarted.push(name.clone());
            }
            Err(e) => {
                return Response::Error {
                    message: format!("failed to restart '{}': {}", name, e),
                };
            }
        }
    }

    Response::Success {
        message: Some(format!("restarted: {}", restarted.join(", "))),
    }
}

// ---------------------------------------------------------------------------
// Log command
// ---------------------------------------------------------------------------

async fn send_response<W: AsyncWriteExt + Unpin>(
    writer: &mut W,
    response: &Response,
) -> color_eyre::Result<()> {
    let encoded = protocol::encode_response(response)?;
    writer.write_all(&encoded).await?;
    Ok(())
}

/// Read the last N lines from a file
async fn read_last_lines(path: &std::path::Path, n: usize) -> std::io::Result<Vec<String>> {
    // If file doesn't exist, return empty
    if !path.exists() {
        return Ok(Vec::new());
    }

    let file = File::open(path).await?;
    let metadata = file.metadata().await?;
    let file_size = metadata.len();

    if file_size == 0 {
        return Ok(Vec::new());
    }

    // Read the entire file and get last N lines (simple approach for now)
    // A more efficient approach would be to seek from the end, but this is simpler
    let mut reader = BufReader::new(file);
    let mut all_lines = Vec::new();
    let mut line = String::new();

    loop {
        line.clear();
        let bytes_read = reader.read_line(&mut line).await?;
        if bytes_read == 0 {
            break;
        }
        // Remove trailing newline
        let trimmed = line.trim_end_matches('\n').trim_end_matches('\r').to_string();
        all_lines.push(trimmed);
    }

    // Return last N lines
    let start = if all_lines.len() > n {
        all_lines.len() - n
    } else {
        0
    };
    Ok(all_lines[start..].to_vec())
}

async fn handle_log<W: AsyncWriteExt + Unpin>(
    name: Option<String>,
    lines: usize,
    follow: bool,
    processes: &Arc<RwLock<ProcessTable>>,
    paths: &Paths,
    writer: &mut W,
) -> color_eyre::Result<()> {
    // Get list of process names to show logs for
    let names: Vec<String> = match name {
        Some(ref n) => {
            let table = processes.read().await;
            if !table.contains_key(n) {
                // Even if process is not running, try to read logs if they exist
                // This allows viewing logs of stopped processes
            }
            vec![n.clone()]
        }
        None => {
            let table = processes.read().await;
            table.keys().cloned().collect()
        }
    };

    if names.is_empty() && name.is_none() {
        send_response(
            writer,
            &Response::Success {
                message: Some("no processes to show logs for".to_string()),
            },
        )
        .await?;
        return Ok(());
    }

    let show_prefix = name.is_none() || names.len() > 1;

    if follow {
        // Follow mode: continuously stream new lines
        handle_log_follow(&names, paths, writer, show_prefix).await?;
    } else {
        // Non-follow mode: read last N lines
        for proc_name in &names {
            // Read stdout log
            let stdout_path = paths.stdout_log(proc_name);
            if let Ok(stdout_lines) = read_last_lines(&stdout_path, lines).await {
                for line in stdout_lines {
                    let response = Response::LogLine {
                        name: if show_prefix {
                            Some(proc_name.clone())
                        } else {
                            None
                        },
                        line,
                    };
                    send_response(writer, &response).await?;
                }
            }

            // Read stderr log
            let stderr_path = paths.stderr_log(proc_name);
            if let Ok(stderr_lines) = read_last_lines(&stderr_path, lines).await {
                for line in stderr_lines {
                    let response = Response::LogLine {
                        name: if show_prefix {
                            Some(format!("{}:err", proc_name))
                        } else {
                            None
                        },
                        line,
                    };
                    send_response(writer, &response).await?;
                }
            }
        }

        // Send a success response to indicate we're done
        send_response(
            writer,
            &Response::Success {
                message: None,
            },
        )
        .await?;
    }

    Ok(())
}

async fn handle_log_follow<W: AsyncWriteExt + Unpin>(
    names: &[String],
    paths: &Paths,
    writer: &mut W,
    show_prefix: bool,
) -> color_eyre::Result<()> {
    use std::collections::HashMap;

    struct LogTail {
        name: String,
        stdout_pos: u64,
        stderr_pos: u64,
    }

    // Initialize tails for each process
    let mut tails: HashMap<String, LogTail> = HashMap::new();
    for name in names {
        let stdout_path = paths.stdout_log(name);
        let stderr_path = paths.stderr_log(name);

        let stdout_pos = if stdout_path.exists() {
            tokio::fs::metadata(&stdout_path)
                .await
                .map(|m| m.len())
                .unwrap_or(0)
        } else {
            0
        };
        let stderr_pos = if stderr_path.exists() {
            tokio::fs::metadata(&stderr_path)
                .await
                .map(|m| m.len())
                .unwrap_or(0)
        } else {
            0
        };

        tails.insert(
            name.clone(),
            LogTail {
                name: name.clone(),
                stdout_pos,
                stderr_pos,
            },
        );
    }

    // Poll for new lines every 100ms
    loop {
        let mut any_output = false;

        for tail in tails.values_mut() {
            // Check stdout
            let stdout_path = paths.stdout_log(&tail.name);
            if stdout_path.exists() {
                if let Ok(mut file) = File::open(&stdout_path).await {
                    let metadata = file.metadata().await?;
                    if metadata.len() > tail.stdout_pos {
                        file.seek(SeekFrom::Start(tail.stdout_pos)).await?;
                        let mut reader = BufReader::new(file);
                        let mut line = String::new();

                        loop {
                            line.clear();
                            let bytes_read = reader.read_line(&mut line).await?;
                            if bytes_read == 0 {
                                break;
                            }
                            tail.stdout_pos += bytes_read as u64;
                            let trimmed =
                                line.trim_end_matches('\n').trim_end_matches('\r').to_string();

                            let response = Response::LogLine {
                                name: if show_prefix {
                                    Some(tail.name.clone())
                                } else {
                                    None
                                },
                                line: trimmed,
                            };
                            send_response(writer, &response).await?;
                            any_output = true;
                        }
                    }
                }
            }

            // Check stderr
            let stderr_path = paths.stderr_log(&tail.name);
            if stderr_path.exists() {
                if let Ok(mut file) = File::open(&stderr_path).await {
                    let metadata = file.metadata().await?;
                    if metadata.len() > tail.stderr_pos {
                        file.seek(SeekFrom::Start(tail.stderr_pos)).await?;
                        let mut reader = BufReader::new(file);
                        let mut line = String::new();

                        loop {
                            line.clear();
                            let bytes_read = reader.read_line(&mut line).await?;
                            if bytes_read == 0 {
                                break;
                            }
                            tail.stderr_pos += bytes_read as u64;
                            let trimmed =
                                line.trim_end_matches('\n').trim_end_matches('\r').to_string();

                            let response = Response::LogLine {
                                name: if show_prefix {
                                    Some(format!("{}:err", tail.name))
                                } else {
                                    None
                                },
                                line: trimmed,
                            };
                            send_response(writer, &response).await?;
                            any_output = true;
                        }
                    }
                }
            }
        }

        // Flush after each poll cycle if we wrote anything
        if any_output {
            writer.flush().await?;
        }

        // Sleep before next poll
        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
    }
}
