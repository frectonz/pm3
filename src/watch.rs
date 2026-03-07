use crate::config::{ProcessConfig, Watch};
use crate::paths::Paths;
use crate::process::{self, ProcessTable};
use crate::protocol::ProcessStatus;
use notify::event::EventKind;
use notify::{RecursiveMode, Watcher};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{RwLock, watch};
pub const DEFAULT_DEBOUNCE_DURATION: Duration = Duration::from_millis(500);

pub fn debounce_duration(config: &ProcessConfig) -> Duration {
    config
        .watch_debounce
        .map(Duration::from_millis)
        .unwrap_or(DEFAULT_DEBOUNCE_DURATION)
}
pub fn resolve_watch_path(config: &ProcessConfig) -> Option<PathBuf> {
    match config.watch.as_ref()? {
        Watch::Enabled(false) => None,
        Watch::Enabled(true) => {
            let base = config.cwd.as_deref().unwrap_or(".");
            Some(PathBuf::from(base))
        }
        Watch::Path(p) => {
            if std::path::Path::new(p).is_absolute() {
                Some(PathBuf::from(p))
            } else {
                let base = config.cwd.as_deref().unwrap_or(".");
                Some(PathBuf::from(base).join(p))
            }
        }
    }
}
fn should_ignore(path: &std::path::Path, ignore_patterns: &[String]) -> bool {
    let path_str = path.to_string_lossy();
    for pattern in ignore_patterns {
        // Check if any path component matches the pattern
        for component in path.components() {
            if let std::path::Component::Normal(name) = component
                && name.to_string_lossy() == *pattern
            {
                return true;
            }
        }
        // Also check glob-style suffix match
        if path_str.contains(pattern.as_str()) {
            return true;
        }
    }
    false
}

fn should_restart_for_event_kind(kind: &EventKind) -> bool {
    matches!(
        kind,
        EventKind::Create(_) | EventKind::Modify(_) | EventKind::Remove(_) | EventKind::Any
    )
}

pub fn spawn_watcher(
    name: String,
    config: ProcessConfig,
    processes: Arc<RwLock<ProcessTable>>,
    paths: Paths,
    mut shutdown_rx: watch::Receiver<bool>,
) {
    let Some(watch_path) = resolve_watch_path(&config) else {
        return;
    };

    let watch_debounce_duration = debounce_duration(&config);
    let ignore_patterns: Vec<String> = config.watch_ignore.clone().unwrap_or_default();

    tokio::spawn(async move {
        // Create a channel for notify events
        let (tx, mut rx) = tokio::sync::mpsc::channel(256);

        let event_tx = tx.clone();
        let mut watcher = match notify::recommended_watcher(move |res: Result<notify::Event, _>| {
            if let Ok(event) = res {
                let _ = event_tx.blocking_send(event);
            }
        }) {
            Ok(w) => w,
            Err(e) => {
                eprintln!("failed to create file watcher for '{}': {}", name, e);
                return;
            }
        };

        if let Err(e) = watcher.watch(&watch_path, RecursiveMode::Recursive) {
            eprintln!(
                "failed to watch path '{}' for '{}': {}",
                watch_path.display(),
                name,
                e
            );
            return;
        }

        loop {
            // Wait for first event or shutdown
            let first_event = tokio::select! {
                event = rx.recv() => match event {
                    Some(e) => e,
                    None => return,
                },
                _ = shutdown_rx.changed() => {
                    if *shutdown_rx.borrow() {
                        return;
                    }
                    continue;
                }
            };

            // Check if the first event is relevant (not ignored).
            // Skip directory paths — on macOS, FSEvents fires events for parent
            // directories when child files change, and those parent paths may not
            // contain the ignored component.
            let mut has_relevant = first_event.paths.iter().any(|p| {
                should_restart_for_event_kind(&first_event.kind)
                    && !p.is_dir()
                    && !should_ignore(p, &ignore_patterns)
            });

            // Debounce: wait configured duration, drain any further events
            tokio::select! {
                _ = tokio::time::sleep(watch_debounce_duration) => {}
                _ = shutdown_rx.changed() => {
                    if *shutdown_rx.borrow() {
                        return;
                    }
                }
            }

            // Drain buffered events during debounce
            while let Ok(event) = rx.try_recv() {
                if !has_relevant {
                    for path in &event.paths {
                        if should_restart_for_event_kind(&event.kind)
                            && !path.is_dir()
                            && !should_ignore(path, &ignore_patterns)
                        {
                            has_relevant = true;
                            break;
                        }
                    }
                }
            }

            if !has_relevant {
                continue;
            }

            // Check shutdown
            if *shutdown_rx.borrow() {
                return;
            }

            // Check process is still running
            {
                let table = processes.read().await;
                match table.get(&name) {
                    Some(managed)
                        if managed.status == ProcessStatus::Online
                            || managed.status == ProcessStatus::Starting =>
                    {
                        // Process is running, proceed with restart
                    }
                    _ => return, // Process gone or stopped
                }
            }

            eprintln!("file change detected for '{}', restarting", name);

            // Graceful stop
            let (old_config, old_restarts) = {
                let mut table = processes.write().await;
                let Some(managed) = table.get_mut(&name) else {
                    return;
                };

                // Signal the process monitor not to auto-restart
                if let Some(ref tx) = managed.monitor_shutdown {
                    let _ = tx.send(true);
                }

                let cfg = managed.config.clone();
                let restarts = managed.restarts;

                // Perform graceful stop inline
                let _ = managed.graceful_stop().await;
                if let Some(ref hook) = cfg.post_stop {
                    let _ = process::run_hook(hook, &name, cfg.cwd.as_deref(), &paths).await;
                }

                (cfg, restarts)
            };

            // Spawn replacement (this will also attach monitors including a new watcher)
            match process::spawn_and_attach(
                name.clone(),
                old_config.clone(),
                old_restarts + 1,
                &processes,
                &paths,
            )
            .await
            {
                Ok(()) => {
                    // This watcher instance terminates; the new one takes over
                    return;
                }
                Err(e) => {
                    eprintln!("failed to restart '{}' after file change: {}", name, e);
                    let mut table = processes.write().await;
                    if let Some(managed) = table.get_mut(&name) {
                        managed.status = ProcessStatus::Errored;
                        managed.pid = None;
                    }
                    return;
                }
            }
        }
    });
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn base_config() -> ProcessConfig {
        ProcessConfig {
            command: "echo test".to_string(),
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
            watch_debounce: None,
            depends_on: None,
            restart: None,
            group: None,
            pre_start: None,
            post_stop: None,
            cron_restart: None,
            log_date_format: None,
            instances: None,
            environments: HashMap::new(),
        }
    }

    #[test]
    fn test_resolve_watch_none() {
        let config = base_config();
        assert!(resolve_watch_path(&config).is_none());
    }

    #[test]
    fn test_resolve_watch_false() {
        let mut config = base_config();
        config.watch = Some(Watch::Enabled(false));
        assert!(resolve_watch_path(&config).is_none());
    }

    #[test]
    fn test_resolve_watch_true_no_cwd() {
        let mut config = base_config();
        config.watch = Some(Watch::Enabled(true));
        assert_eq!(resolve_watch_path(&config).unwrap(), PathBuf::from("."));
    }

    #[test]
    fn test_resolve_watch_true_with_cwd() {
        let mut config = base_config();
        config.watch = Some(Watch::Enabled(true));
        config.cwd = Some("/app".to_string());
        assert_eq!(resolve_watch_path(&config).unwrap(), PathBuf::from("/app"));
    }

    #[test]
    fn test_resolve_watch_path_relative() {
        let mut config = base_config();
        config.watch = Some(Watch::Path("./src".to_string()));
        config.cwd = Some("/app".to_string());
        assert_eq!(
            resolve_watch_path(&config).unwrap(),
            PathBuf::from("/app/./src")
        );
    }

    #[test]
    fn test_resolve_watch_path_absolute() {
        let mut config = base_config();
        config.watch = Some(Watch::Path("/tmp/watched".to_string()));
        config.cwd = Some("/app".to_string());
        assert_eq!(
            resolve_watch_path(&config).unwrap(),
            PathBuf::from("/tmp/watched")
        );
    }

    #[test]
    fn test_resolve_watch_path_relative_no_cwd() {
        let mut config = base_config();
        config.watch = Some(Watch::Path("./src".to_string()));
        assert_eq!(
            resolve_watch_path(&config).unwrap(),
            PathBuf::from("././src")
        );
    }

    #[test]
    fn test_should_ignore_matching_component() {
        let path = std::path::Path::new("/app/node_modules/foo/bar.js");
        assert!(should_ignore(path, &["node_modules".to_string()]));
    }

    #[test]
    fn test_should_ignore_no_match() {
        let path = std::path::Path::new("/app/src/main.rs");
        assert!(!should_ignore(path, &["node_modules".to_string()]));
    }

    #[test]
    fn test_should_ignore_git() {
        let path = std::path::Path::new("/app/.git/HEAD");
        assert!(should_ignore(path, &[".git".to_string()]));
    }

    #[test]
    fn test_should_ignore_empty_patterns() {
        let path = std::path::Path::new("/app/src/main.rs");
        assert!(!should_ignore(path, &[]));
    }

    #[test]
    fn test_should_ignore_multiple_patterns() {
        let path = std::path::Path::new("/app/logs/app.log");
        assert!(should_ignore(
            path,
            &[
                "node_modules".to_string(),
                ".git".to_string(),
                "logs".to_string()
            ]
        ));
    }

    #[test]
    fn test_debounce_duration_default_and_custom() {
        let mut config = base_config();
        assert_eq!(debounce_duration(&config), DEFAULT_DEBOUNCE_DURATION);

        config.watch_debounce = Some(1200);
        assert_eq!(debounce_duration(&config), Duration::from_millis(1200));
    }

    #[test]
    fn test_should_restart_for_event_kind() {
        use notify::event::{AccessKind, CreateKind, ModifyKind, RemoveKind};

        assert!(should_restart_for_event_kind(&EventKind::Create(
            CreateKind::Any
        )));
        assert!(should_restart_for_event_kind(&EventKind::Modify(
            ModifyKind::Any
        )));
        assert!(should_restart_for_event_kind(&EventKind::Remove(
            RemoveKind::Any
        )));
        assert!(should_restart_for_event_kind(&EventKind::Any));
        assert!(!should_restart_for_event_kind(&EventKind::Access(
            AccessKind::Any
        )));
    }
}
