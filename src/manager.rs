use crate::config::ProcessConfig;
use crate::deps;
use crate::log;
use crate::paths::Paths;
use crate::process::{self, ProcessTable};
use crate::protocol::{self, ProcessStatus, Request, Response};
use crate::{cron, health, memory, watch as file_watch};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::fs;
use tokio::io::AsyncWriteExt;
use tokio::sync::{RwLock, watch};

#[derive(Clone)]
pub struct Manager {
    paths: Paths,
    processes: Arc<RwLock<ProcessTable>>,
    stats_cache: Arc<RwLock<memory::StatsCache>>,
}

impl Manager {
    pub fn new(paths: Paths) -> Self {
        Self {
            paths,
            processes: Arc::new(RwLock::new(HashMap::new())),
            stats_cache: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    pub fn paths(&self) -> &Paths {
        &self.paths
    }

    pub fn processes(&self) -> Arc<RwLock<ProcessTable>> {
        Arc::clone(&self.processes)
    }

    pub fn stats_cache(&self) -> Arc<RwLock<memory::StatsCache>> {
        Arc::clone(&self.stats_cache)
    }

    pub async fn shutdown_all(&self) {
        let names: Vec<String> = {
            let table = self.processes.read().await;
            table.keys().cloned().collect()
        };

        for name in &names {
            {
                let mut table = self.processes.write().await;
                if let Some(managed) = table.get_mut(name) {
                    let _ = managed.graceful_stop().await;
                }
            }
            // Run post_stop hook outside the lock
            let hook_info = {
                let table = self.processes.read().await;
                table.get(name).and_then(|m| {
                    m.config
                        .post_stop
                        .as_ref()
                        .map(|hook| (hook.clone(), m.config.cwd.clone()))
                })
            };
            if let Some((hook, cwd)) = hook_info {
                let _ = process::run_hook(&hook, name, cwd.as_deref(), &self.paths).await;
            }
        }
    }

    pub async fn dispatch(&self, request: Request, shutdown_tx: &watch::Sender<bool>) -> Response {
        match request {
            Request::Start {
                configs,
                names,
                env,
                wait,
                path,
            } => self.start(configs, names, env, wait, path).await,
            Request::List => self.list().await,
            Request::Stop { names } => self.stop(names).await,
            Request::Restart { names } => self.restart(names).await,
            Request::Kill => {
                let _ = shutdown_tx.send(true);
                Response::Success {
                    message: Some("daemon shutting down".to_string()),
                }
            }
            Request::Info { name } => self.info(name).await,
            Request::Signal { name, signal } => self.signal(name, signal).await,
            Request::Flush { names } => self.flush(names).await,
            Request::Log { .. } => Response::Error {
                message: "unexpected dispatch for log".to_string(),
            },
            Request::Reload { names, path } => self.reload(names, path).await,
            Request::Save => self.save().await,
            Request::Resurrect { path } => self.resurrect(path).await,
        }
    }

    pub async fn list(&self) -> Response {
        let table = self.processes.read().await;
        let cache = self.stats_cache.read().await;
        let infos: Vec<_> = table.values().map(|m| m.to_process_info(&cache)).collect();
        Response::ProcessList { processes: infos }
    }

    pub async fn start(
        &self,
        configs: HashMap<String, ProcessConfig>,
        names: Option<Vec<String>>,
        env: Option<String>,
        wait: bool,
        path: Option<String>,
    ) -> Response {
        let configs = expand_instances(configs);

        let mut to_start: Vec<(String, ProcessConfig)> = match names {
            Some(ref requested) => {
                let resolved = match resolve_config_names(requested, &configs) {
                    Ok(r) => r,
                    Err(msg) => return Response::Error { message: msg },
                };
                resolved
                    .into_iter()
                    .map(|name| {
                        let config = configs.get(&name).unwrap().clone();
                        (name, config)
                    })
                    .collect()
            }
            None => configs.into_iter().collect(),
        };

        if let Some(ref env_name) = env {
            let mut any_applied = false;
            for (_, config) in &mut to_start {
                if config.apply_environment(env_name) {
                    any_applied = true;
                }
            }
            if !any_applied {
                return Response::Error {
                    message: format!("unknown environment: '{}'", env_name),
                };
            }
        }

        if let Some(ref p) = path {
            inject_path(&mut to_start, p);
        }

        let subset_configs: HashMap<String, ProcessConfig> = to_start.iter().cloned().collect();

        if let Err(e) = deps::validate_deps(&subset_configs) {
            return Response::Error {
                message: e.to_string(),
            };
        }

        let levels = match deps::topological_levels(&subset_configs) {
            Ok(l) => l,
            Err(e) => {
                return Response::Error {
                    message: e.to_string(),
                };
            }
        };

        let mut started = Vec::new();

        for (level_idx, level) in levels.iter().enumerate() {
            let mut spawned: Vec<SpawnedProcess> = Vec::new();
            let mut level_names: Vec<String> = Vec::new();

            {
                let mut table = self.processes.write().await;
                for name in level {
                    let mut old_restarts = None;
                    if let Some(existing) = table.get_mut(name) {
                        match existing.status {
                            ProcessStatus::Stopped | ProcessStatus::Errored => {
                                old_restarts = Some(existing.restarts);
                            }
                            _ => {
                                // If config changed, stop the old process and
                                // restart with the new config.
                                let config = subset_configs.get(name).unwrap();
                                if existing.config != *config {
                                    let _ = existing.graceful_stop().await;
                                } else {
                                    continue;
                                }
                            }
                        }
                    }
                    let config = subset_configs.get(name).unwrap().clone();
                    match process::spawn_process(name.clone(), config.clone(), &self.paths).await {
                        Ok((mut managed, child)) => {
                            if let Some(previous) = old_restarts {
                                managed.restarts = previous;
                            }
                            let pid = managed.pid;
                            let shutdown_tx = managed
                                .monitor_shutdown
                                .as_ref()
                                .expect("monitor shutdown sender missing")
                                .clone();
                            table.insert(name.clone(), managed);
                            spawned.push(SpawnedProcess {
                                name: name.clone(),
                                child,
                                pid,
                                config,
                                shutdown_tx,
                            });
                            level_names.push(name.clone());
                        }
                        Err(e) => {
                            return Response::Error {
                                message: format!("failed to start '{}': {}", name, e),
                            };
                        }
                    }
                }
            }

            for spawned_process in spawned {
                spawned_process.spawn_monitors(Arc::clone(&self.processes), self.paths.clone());
            }

            started.extend(level_names.clone());

            let is_last_level = level_idx == levels.len() - 1;
            let should_wait = !is_last_level || wait;
            if should_wait
                && !level_names.is_empty()
                && let Err(msg) = wait_for_online(&level_names, &self.processes).await
            {
                return Response::Error { message: msg };
            }
        }

        if started.is_empty() {
            return Response::Success {
                message: Some("everything is already running".to_string()),
            };
        }

        // Check for processes that exited immediately after spawn
        {
            let table = self.processes.read().await;
            let failures: Vec<&String> = started
                .iter()
                .filter(|name| {
                    table
                        .get(*name)
                        .is_some_and(|p| p.status == ProcessStatus::Errored)
                })
                .collect();
            if !failures.is_empty() {
                return Response::Error {
                    message: format!(
                        "failed to start '{}': process exited immediately",
                        failures
                            .iter()
                            .map(|s| s.as_str())
                            .collect::<Vec<_>>()
                            .join(", ")
                    ),
                };
            }
        }

        Response::Success {
            message: Some(format!("started: {}", started.join(", "))),
        }
    }

    pub async fn stop(&self, names: Option<Vec<String>>) -> Response {
        let mut table = self.processes.write().await;

        let targets: Vec<String> = match names {
            Some(ref requested) => match resolve_table_names(requested, &table) {
                Ok(r) => r,
                Err(msg) => return Response::Error { message: msg },
            },
            None => table.keys().cloned().collect(),
        };

        let running_configs: HashMap<String, ProcessConfig> = table
            .iter()
            .map(|(k, v)| (k.clone(), v.config.clone()))
            .collect();

        let stop_order = match deps::expand_dependents(&targets, &running_configs) {
            Ok(order) => order,
            Err(e) => {
                return Response::Error {
                    message: e.to_string(),
                };
            }
        };

        let mut stopped = Vec::new();
        for name in &stop_order {
            let managed = match table.get_mut(name) {
                Some(m) => m,
                None => continue,
            };
            if managed.status == ProcessStatus::Stopped {
                continue;
            }
            if let Err(e) = managed.graceful_stop().await {
                return Response::Error {
                    message: format!("failed to stop '{}': {}", name, e),
                };
            }
            if let Some(ref hook) = managed.config.post_stop {
                let _ =
                    process::run_hook(hook, name, managed.config.cwd.as_deref(), &self.paths).await;
            }
            stopped.push(name.clone());
        }

        Response::Success {
            message: Some(format!("stopped: {}", stopped.join(", "))),
        }
    }

    pub async fn restart(&self, names: Option<Vec<String>>) -> Response {
        let (targets, restart_configs) = {
            let table = self.processes.read().await;

            let targets: Vec<String> = match names {
                Some(ref requested) => match resolve_table_names(requested, &table) {
                    Ok(r) => r,
                    Err(msg) => return Response::Error { message: msg },
                },
                None => table.keys().cloned().collect(),
            };

            let running_configs: HashMap<String, ProcessConfig> = table
                .iter()
                .map(|(k, v)| (k.clone(), v.config.clone()))
                .collect();

            (targets, running_configs)
        };

        let stop_order = match deps::expand_dependents(&targets, &restart_configs) {
            Ok(order) => order,
            Err(e) => {
                return Response::Error {
                    message: e.to_string(),
                };
            }
        };

        let mut old_restarts_map: HashMap<String, u32> = HashMap::new();
        {
            let mut table = self.processes.write().await;
            for name in &stop_order {
                let managed = match table.get_mut(name) {
                    Some(m) => m,
                    None => continue,
                };
                old_restarts_map.insert(name.clone(), managed.restarts);

                if managed.status != ProcessStatus::Stopped
                    && let Err(e) = managed.graceful_stop().await
                {
                    return Response::Error {
                        message: format!("failed to stop '{}': {}", name, e),
                    };
                }
                if let Some(ref hook) = managed.config.post_stop {
                    let _ =
                        process::run_hook(hook, name, managed.config.cwd.as_deref(), &self.paths)
                            .await;
                }
            }
        }

        let subset_configs: HashMap<String, ProcessConfig> = restart_configs
            .iter()
            .filter(|(k, _)| stop_order.contains(k))
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();

        let levels = match deps::topological_levels(&subset_configs) {
            Ok(l) => l,
            Err(e) => {
                return Response::Error {
                    message: e.to_string(),
                };
            }
        };

        let mut restarted = Vec::new();

        for (level_idx, level) in levels.iter().enumerate() {
            let mut spawned: Vec<SpawnedProcess> = Vec::new();
            let mut level_names: Vec<String> = Vec::new();

            {
                let mut table = self.processes.write().await;
                for name in level {
                    let config = match subset_configs.get(name) {
                        Some(c) => c.clone(),
                        None => continue,
                    };
                    let old_restarts = old_restarts_map.get(name).copied().unwrap_or(0);
                    match process::spawn_process(name.clone(), config.clone(), &self.paths).await {
                        Ok((mut new_managed, child)) => {
                            new_managed.restarts = old_restarts + 1;
                            let pid = new_managed.pid;
                            let shutdown_tx = new_managed
                                .monitor_shutdown
                                .as_ref()
                                .expect("monitor shutdown sender missing")
                                .clone();
                            table.insert(name.clone(), new_managed);
                            spawned.push(SpawnedProcess {
                                name: name.clone(),
                                child,
                                pid,
                                config,
                                shutdown_tx,
                            });
                            level_names.push(name.clone());
                        }
                        Err(e) => {
                            return Response::Error {
                                message: format!("failed to restart '{}': {}", name, e),
                            };
                        }
                    }
                }
            }

            for spawned_process in spawned {
                spawned_process.spawn_monitors(Arc::clone(&self.processes), self.paths.clone());
            }

            restarted.extend(level_names.clone());

            let is_last_level = level_idx == levels.len() - 1;
            if !is_last_level
                && !level_names.is_empty()
                && let Err(msg) = wait_for_online(&level_names, &self.processes).await
            {
                return Response::Error { message: msg };
            }
        }

        Response::Success {
            message: Some(format!("restarted: {}", restarted.join(", "))),
        }
    }

    pub async fn reload(&self, names: Option<Vec<String>>, path: Option<String>) -> Response {
        let targets = {
            let table = self.processes.read().await;
            let targets: Vec<String> = match names {
                Some(ref requested) => match resolve_table_names(requested, &table) {
                    Ok(r) => r,
                    Err(msg) => return Response::Error { message: msg },
                },
                None => table.keys().cloned().collect(),
            };
            targets
        };

        // Force-insert PATH into stored configs so reloaded processes use the current PATH
        if let Some(ref p) = path {
            let mut table = self.processes.write().await;
            for name in &targets {
                if let Some(managed) = table.get_mut(name) {
                    let env = managed.config.env.get_or_insert_with(HashMap::new);
                    env.insert("PATH".to_string(), p.clone());
                }
            }
        }

        let mut with_hc: Vec<(String, ProcessConfig, u32)> = Vec::new();
        let mut without_hc: Vec<String> = Vec::new();

        {
            let table = self.processes.read().await;
            for name in &targets {
                if let Some(managed) = table.get(name) {
                    if managed.config.health_check.is_some() {
                        with_hc.push((name.clone(), managed.config.clone(), managed.restarts));
                    } else {
                        without_hc.push(name.clone());
                    }
                }
            }
        }

        let mut reloaded = Vec::new();
        let mut failed = Vec::new();

        for (name, config, old_restarts) in with_hc {
            let temp_name = format!("__reload_{}", name);
            let health_check = config.health_check.clone();
            let max_memory = config.max_memory.clone();
            let cron_restart = config.cron_restart.clone();

            match process::spawn_process(temp_name.clone(), config.clone(), &self.paths).await {
                Ok((mut new_managed, new_child)) => {
                    new_managed.restarts = old_restarts;
                    let new_pid = new_managed.pid;
                    let shutdown_tx = new_managed
                        .monitor_shutdown
                        .as_ref()
                        .expect("monitor shutdown sender missing")
                        .clone();
                    let shutdown_rx = shutdown_tx.subscribe();
                    let health_shutdown_rx = health_check.as_ref().map(|_| shutdown_tx.subscribe());

                    {
                        let mut table = self.processes.write().await;
                        table.insert(temp_name.clone(), new_managed);
                    }

                    process::spawn_monitor(
                        temp_name.clone(),
                        new_child,
                        new_pid,
                        Arc::clone(&self.processes),
                        self.paths.clone(),
                        shutdown_rx,
                    );

                    if let (Some(hc), Some(hc_rx)) = (health_check, health_shutdown_rx) {
                        health::spawn_health_checker(
                            temp_name.clone(),
                            hc,
                            Arc::clone(&self.processes),
                            hc_rx,
                        );
                    }

                    match wait_for_online(std::slice::from_ref(&temp_name), &self.processes).await {
                        Ok(()) => {
                            let mut table = self.processes.write().await;

                            if let Some(old_managed) = table.get_mut(&name) {
                                let _ = old_managed.graceful_stop().await;
                                if let Some(ref hook) = config.post_stop {
                                    let _ = process::run_hook(
                                        hook,
                                        &name,
                                        config.cwd.as_deref(),
                                        &self.paths,
                                    )
                                    .await;
                                }
                            }

                            if let Some(mut new_managed) = table.remove(&temp_name) {
                                new_managed.name = name.clone();
                                let shutdown_tx = new_managed
                                    .monitor_shutdown
                                    .as_ref()
                                    .expect("monitor shutdown sender missing")
                                    .clone();
                                table.insert(name.clone(), new_managed);
                                drop(table);

                                // Attach remaining monitors after swap
                                if let Some(mm) = max_memory.clone() {
                                    memory::spawn_memory_monitor(
                                        name.clone(),
                                        mm,
                                        Arc::clone(&self.processes),
                                        self.paths.clone(),
                                        shutdown_tx.subscribe(),
                                    );
                                }
                                file_watch::spawn_watcher(
                                    name.clone(),
                                    config.clone(),
                                    Arc::clone(&self.processes),
                                    self.paths.clone(),
                                    shutdown_tx.subscribe(),
                                );
                                if let Some(cr) = cron_restart.clone() {
                                    cron::spawn_cron_restart(
                                        name.clone(),
                                        cr,
                                        Arc::clone(&self.processes),
                                        self.paths.clone(),
                                        shutdown_tx.subscribe(),
                                    );
                                }
                            }

                            reloaded.push(name);
                        }
                        Err(_) => {
                            let mut table = self.processes.write().await;
                            if let Some(temp_managed) = table.get_mut(&temp_name) {
                                let _ = temp_managed.graceful_stop().await;
                            }
                            table.remove(&temp_name);
                            failed.push(name);
                        }
                    }
                }
                Err(e) => {
                    failed.push(format!("{} (spawn failed: {})", name, e));
                }
            }
        }

        if !without_hc.is_empty() {
            match self.restart(Some(without_hc.clone())).await {
                Response::Success { .. } => {
                    reloaded.extend(without_hc);
                }
                Response::Error { message } => {
                    return Response::Error { message };
                }
                _ => {}
            }
        }

        if reloaded.is_empty() && !failed.is_empty() {
            return Response::Error {
                message: format!("reload failed: {}", failed.join(", ")),
            };
        }

        let mut msg = format!("reloaded: {}", reloaded.join(", "));
        if !failed.is_empty() {
            msg.push_str(&format!(" (failed: {})", failed.join(", ")));
        }

        Response::Success { message: Some(msg) }
    }

    pub async fn save(&self) -> Response {
        let table = self.processes.read().await;

        let entries: Vec<DumpEntry> = table
            .values()
            .map(|managed| DumpEntry {
                name: managed.name.clone(),
                config: managed.config.clone(),
                pid: managed.pid,
                restarts: managed.restarts,
            })
            .collect();

        drop(table);

        let json = match serde_json::to_string_pretty(&entries) {
            Ok(j) => j,
            Err(e) => {
                return Response::Error {
                    message: format!("failed to serialize state: {}", e),
                };
            }
        };

        if let Err(e) = fs::write(self.paths.dump_file(), json.as_bytes()).await {
            return Response::Error {
                message: format!("failed to write dump file: {}", e),
            };
        }

        Response::Success {
            message: Some(format!("saved {} process(es) to dump file", entries.len())),
        }
    }

    /// Core restore logic shared by `resurrect` (CLI command) and `auto_restore` (daemon startup).
    /// Returns `Ok(restored_names)` on success, `Err(message)` on failure.
    async fn restore_from_dump(&self, path: Option<String>) -> Result<Vec<String>, String> {
        let dump_path = self.paths.dump_file();
        if !dump_path.exists() {
            return Err("no dump file found".to_string());
        }

        let data = fs::read_to_string(&dump_path)
            .await
            .map_err(|e| format!("failed to read dump file: {}", e))?;

        let mut entries: Vec<DumpEntry> =
            serde_json::from_str(&data).map_err(|e| format!("failed to parse dump file: {}", e))?;

        // Force-insert PATH into restored configs so they use the current CLI PATH
        if let Some(ref p) = path {
            for entry in &mut entries {
                let env = entry.config.env.get_or_insert_with(HashMap::new);
                env.insert("PATH".to_string(), p.clone());
            }
        }

        let already_running: Vec<String> = {
            let table = self.processes.read().await;
            entries
                .iter()
                .filter(|e| table.contains_key(&e.name))
                .map(|e| e.name.clone())
                .collect()
        };

        let to_restore: Vec<DumpEntry> = entries
            .into_iter()
            .filter(|e| !already_running.contains(&e.name))
            .collect();

        if to_restore.is_empty() {
            return Ok(vec![]);
        }

        let subset_configs: HashMap<String, ProcessConfig> = to_restore
            .iter()
            .map(|e| (e.name.clone(), e.config.clone()))
            .collect();

        deps::validate_deps(&subset_configs).map_err(|e| e.to_string())?;

        let levels = deps::topological_levels(&subset_configs).map_err(|e| e.to_string())?;

        let entry_map: HashMap<String, &DumpEntry> =
            to_restore.iter().map(|e| (e.name.clone(), e)).collect();

        let mut restored = Vec::new();

        for (level_idx, level) in levels.iter().enumerate() {
            let mut spawned: Vec<SpawnedProcess> = Vec::new();
            let mut level_names: Vec<String> = Vec::new();

            {
                let mut table = self.processes.write().await;

                for name in level {
                    if table.contains_key(name) {
                        continue;
                    }

                    let entry = match entry_map.get(name) {
                        Some(e) => e,
                        None => continue,
                    };

                    let old_alive = entry.pid.is_some_and(is_pid_alive);

                    if old_alive {
                        let (log_tx, _) = tokio::sync::broadcast::channel(1024);
                        let (monitor_tx, _) = watch::channel(false);

                        let status = if entry.config.health_check.is_some() {
                            ProcessStatus::Starting
                        } else {
                            ProcessStatus::Online
                        };

                        // On Windows, re-assign the restored process to a new Job Object
                        #[cfg(windows)]
                        let job_object = entry.pid.and_then(|p| {
                            let job = crate::sys::JobObject::new().ok()?;
                            job.assign_process(p).ok()?;
                            Some(job)
                        });

                        let managed = process::ManagedProcess {
                            name: name.clone(),
                            config: entry.config.clone(),
                            pid: entry.pid,
                            status,
                            started_at: tokio::time::Instant::now(),
                            restarts: entry.restarts,
                            log_broadcaster: log_tx,
                            monitor_shutdown: Some(monitor_tx),
                            #[cfg(windows)]
                            job_object,
                        };

                        table.insert(name.clone(), managed);
                        level_names.push(name.clone());
                    } else {
                        let config = entry.config.clone();
                        match process::spawn_process(name.clone(), config.clone(), &self.paths)
                            .await
                        {
                            Ok((mut managed, child)) => {
                                managed.restarts = entry.restarts;
                                let pid = managed.pid;
                                let shutdown_tx = managed
                                    .monitor_shutdown
                                    .as_ref()
                                    .expect("monitor shutdown sender missing")
                                    .clone();
                                table.insert(name.clone(), managed);
                                spawned.push(SpawnedProcess {
                                    name: name.clone(),
                                    child,
                                    pid,
                                    config,
                                    shutdown_tx,
                                });
                                level_names.push(name.clone());
                            }
                            Err(e) => {
                                return Err(format!("failed to resurrect '{}': {}", name, e));
                            }
                        }
                    }
                }
            }

            for spawned_process in spawned {
                spawned_process.spawn_monitors(Arc::clone(&self.processes), self.paths.clone());
            }

            {
                let table = self.processes.read().await;
                for name in &level_names {
                    let managed = match table.get(name) {
                        Some(m) => m,
                        None => continue,
                    };
                    let entry = match entry_map.get(name) {
                        Some(e) => e,
                        None => continue,
                    };
                    if !entry.pid.is_some_and(is_pid_alive) {
                        continue;
                    }

                    if let Some(ref hc) = entry.config.health_check {
                        let hc_rx = managed
                            .monitor_shutdown
                            .as_ref()
                            .expect("monitor shutdown sender missing")
                            .subscribe();
                        health::spawn_health_checker(
                            name.clone(),
                            hc.clone(),
                            Arc::clone(&self.processes),
                            hc_rx,
                        );
                    }
                    if let Some(ref mm) = entry.config.max_memory {
                        let mm_rx = managed
                            .monitor_shutdown
                            .as_ref()
                            .expect("monitor shutdown sender missing")
                            .subscribe();
                        memory::spawn_memory_monitor(
                            name.clone(),
                            mm.clone(),
                            Arc::clone(&self.processes),
                            self.paths.clone(),
                            mm_rx,
                        );
                    }
                    if let Some(ref cr) = entry.config.cron_restart {
                        let cr_rx = managed
                            .monitor_shutdown
                            .as_ref()
                            .expect("monitor shutdown sender missing")
                            .subscribe();
                        cron::spawn_cron_restart(
                            name.clone(),
                            cr.clone(),
                            Arc::clone(&self.processes),
                            self.paths.clone(),
                            cr_rx,
                        );
                    }

                    if let Some(pid) = entry.pid {
                        let pm_rx = managed
                            .monitor_shutdown
                            .as_ref()
                            .expect("monitor shutdown sender missing")
                            .subscribe();
                        process::spawn_pid_monitor(
                            name.clone(),
                            pid,
                            Arc::clone(&self.processes),
                            self.paths.clone(),
                            pm_rx,
                        );
                    }
                }
            }

            restored.extend(level_names.clone());

            let is_last_level = level_idx == levels.len() - 1;
            if !is_last_level
                && !level_names.is_empty()
                && let Err(msg) = wait_for_online(&level_names, &self.processes).await
            {
                return Err(msg);
            }
        }

        Ok(restored)
    }

    pub async fn resurrect(&self, path: Option<String>) -> Response {
        match self.restore_from_dump(path).await {
            Ok(restored) if restored.is_empty() => Response::Success {
                message: Some("all processes already running".to_string()),
            },
            Ok(restored) => Response::Success {
                message: Some(format!("resurrected: {}", restored.join(", "))),
            },
            Err(message) => Response::Error { message },
        }
    }

    /// Auto-restore processes from dump file on daemon startup.
    /// Silently skips if no dump file exists.
    pub async fn auto_restore(&self) {
        match self.restore_from_dump(None).await {
            Ok(restored) if restored.is_empty() => {}
            Ok(restored) => {
                eprintln!(
                    "auto-restored {} process(es): {}",
                    restored.len(),
                    restored.join(", ")
                );
            }
            Err(msg) if msg == "no dump file found" => {
                // No dump file — silently continue
            }
            Err(msg) => {
                eprintln!("auto-restore failed: {}", msg);
            }
        }
    }

    pub async fn flush(&self, names: Option<Vec<String>>) -> Response {
        let table = self.processes.read().await;

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

        drop(table);

        for name in &targets {
            let stdout_path = self.paths.stdout_log(name);
            let stderr_path = self.paths.stderr_log(name);

            if stdout_path.exists()
                && let Err(e) = fs::write(&stdout_path, b"").await
            {
                return Response::Error {
                    message: format!("failed to truncate stdout log for '{}': {}", name, e),
                };
            }
            if stderr_path.exists()
                && let Err(e) = fs::write(&stderr_path, b"").await
            {
                return Response::Error {
                    message: format!("failed to truncate stderr log for '{}': {}", name, e),
                };
            }

            for i in 1..=log::LOG_ROTATION_KEEP {
                let _ = fs::remove_file(self.paths.rotated_stdout_log(name, i)).await;
                let _ = fs::remove_file(self.paths.rotated_stderr_log(name, i)).await;
            }
        }

        Response::Success {
            message: Some(format!("flushed logs: {}", targets.join(", "))),
        }
    }

    pub async fn info(&self, name: String) -> Response {
        let table = self.processes.read().await;
        let cache = self.stats_cache.read().await;
        match table.get(&name) {
            Some(managed) => {
                let detail = managed.to_process_detail(&self.paths, &cache);
                Response::ProcessDetail {
                    info: Box::new(detail),
                }
            }
            None => Response::Error {
                message: format!("process not found: {name}"),
            },
        }
    }

    pub async fn signal(&self, name: String, signal: String) -> Response {
        let table = self.processes.read().await;
        let managed = match table.get(&name) {
            Some(m) => m,
            None => {
                return Response::Error {
                    message: format!("process not found: {name}"),
                };
            }
        };

        let raw_pid = match managed.pid {
            Some(pid) => pid,
            None => {
                return Response::Error {
                    message: format!("process '{name}' is not running"),
                };
            }
        };

        let sig = match process::parse_signal(&signal) {
            Ok(s) => s,
            Err(e) => {
                return Response::Error {
                    message: e.to_string(),
                };
            }
        };

        if let Err(e) = crate::sys::send_signal(raw_pid, sig) {
            return Response::Error {
                message: format!("failed to send signal to '{}': {}", name, e),
            };
        }

        Response::Success {
            message: Some(format!("sent {} to '{}'", signal, name)),
        }
    }

    const MAX_LOG_LINES: usize = 10_000;

    pub async fn stream_logs(
        &self,
        name: Option<String>,
        lines: usize,
        follow: bool,
        writer: &mut (impl AsyncWriteExt + Unpin),
    ) -> color_eyre::Result<()> {
        let lines = lines.min(Self::MAX_LOG_LINES);
        let table = self.processes.read().await;

        let targets: Vec<String> = match name {
            Some(ref n) => {
                if !table.contains_key(n) {
                    let resp = Response::Error {
                        message: format!("process not found: {n}"),
                    };
                    let encoded = protocol::encode_response(&resp)?;
                    writer.write_all(&encoded).await?;
                    return Ok(());
                }
                vec![n.clone()]
            }
            None => table.keys().cloned().collect(),
        };

        let multi = targets.len() > 1;

        for target in &targets {
            let stdout_lines =
                log::tail_file(&self.paths.stdout_log(target), lines).unwrap_or_default();
            let stderr_lines =
                log::tail_file(&self.paths.stderr_log(target), lines).unwrap_or_default();

            for line in stdout_lines {
                let resp = Response::LogLine {
                    name: if multi { Some(target.clone()) } else { None },
                    line,
                };
                let encoded = protocol::encode_response(&resp)?;
                writer.write_all(&encoded).await?;
            }
            for line in stderr_lines {
                let resp = Response::LogLine {
                    name: if multi { Some(target.clone()) } else { None },
                    line,
                };
                let encoded = protocol::encode_response(&resp)?;
                writer.write_all(&encoded).await?;
            }
        }

        if !follow {
            return Ok(());
        }

        let mut receivers = Vec::new();
        for target in &targets {
            if let Some(managed) = table.get(target) {
                let rx = managed.log_broadcaster.subscribe();
                receivers.push((target.clone(), rx));
            }
        }

        drop(table);

        writer.flush().await?;

        loop {
            let mut any_received = false;
            let mut closed_targets = Vec::new();

            for (i, (target, rx)) in receivers.iter_mut().enumerate() {
                match rx.try_recv() {
                    Ok(entry) => {
                        let resp = Response::LogLine {
                            name: if multi { Some(target.clone()) } else { None },
                            line: entry.line,
                        };
                        let encoded = protocol::encode_response(&resp)?;
                        if writer.write_all(&encoded).await.is_err() {
                            return Ok(());
                        }
                        any_received = true;
                    }
                    Err(tokio::sync::broadcast::error::TryRecvError::Empty) => {}
                    Err(tokio::sync::broadcast::error::TryRecvError::Lagged(n)) => {
                        let resp = Response::LogLine {
                            name: if multi { Some(target.clone()) } else { None },
                            line: format!("[pm3: {n} log lines dropped due to lag]"),
                        };
                        let encoded = protocol::encode_response(&resp)?;
                        if writer.write_all(&encoded).await.is_err() {
                            return Ok(());
                        }
                        any_received = true;
                    }
                    Err(tokio::sync::broadcast::error::TryRecvError::Closed) => {
                        closed_targets.push(i);
                    }
                }
            }

            // Resubscribe to processes whose broadcasters were closed (process restarted)
            if !closed_targets.is_empty() {
                let table = self.processes.read().await;
                for &i in &closed_targets {
                    let target = &receivers[i].0;
                    if let Some(managed) = table.get(target) {
                        receivers[i].1 = managed.log_broadcaster.subscribe();
                    }
                }
            }

            if !any_received {
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            }

            if writer.flush().await.is_err() {
                return Ok(());
            }
        }
    }
}

struct SpawnedProcess {
    name: String,
    child: tokio::process::Child,
    pid: Option<u32>,
    config: ProcessConfig,
    shutdown_tx: watch::Sender<bool>,
}

impl SpawnedProcess {
    fn spawn_monitors(self, processes: Arc<RwLock<ProcessTable>>, paths: Paths) {
        let shutdown_rx = self.shutdown_tx.subscribe();
        process::spawn_monitor(
            self.name.clone(),
            self.child,
            self.pid,
            Arc::clone(&processes),
            paths.clone(),
            shutdown_rx,
        );
        process::spawn_aux_monitors(self.name, self.config, processes, paths, self.shutdown_tx);
    }
}

/// Inject a `PATH` value into each config's env map.
/// Uses `or_insert` so a user-explicit PATH in pm3.toml takes precedence.
fn inject_path(configs: &mut [(String, ProcessConfig)], path: &str) {
    for (_, config) in configs.iter_mut() {
        let env = config.env.get_or_insert_with(HashMap::new);
        env.entry("PATH".to_string())
            .or_insert_with(|| path.to_string());
    }
}

fn resolve_config_names(
    requested: &[String],
    configs: &HashMap<String, ProcessConfig>,
) -> Result<Vec<String>, String> {
    let mut result = Vec::new();
    for name in requested {
        if configs.contains_key(name) {
            result.push(name.clone());
        } else {
            // Try cluster prefix match: "web" -> "web:0", "web:1", ...
            let prefix = format!("{}:", name);
            let cluster_matches: Vec<String> = configs
                .keys()
                .filter(|k| k.starts_with(&prefix))
                .cloned()
                .collect();
            if !cluster_matches.is_empty() {
                result.extend(cluster_matches);
            } else {
                let group_matches: Vec<String> = configs
                    .iter()
                    .filter(|(_, c)| c.group.as_deref() == Some(name))
                    .map(|(k, _)| k.clone())
                    .collect();
                if group_matches.is_empty() {
                    return Err(format!("process or group '{}' not found in configs", name));
                }
                result.extend(group_matches);
            }
        }
    }
    Ok(result)
}

fn resolve_table_names(requested: &[String], table: &ProcessTable) -> Result<Vec<String>, String> {
    let mut result = Vec::new();
    for name in requested {
        if table.contains_key(name) {
            result.push(name.clone());
        } else {
            // Try cluster prefix match: "web" -> "web:0", "web:1", ...
            let prefix = format!("{}:", name);
            let cluster_matches: Vec<String> = table
                .keys()
                .filter(|k| k.starts_with(&prefix))
                .cloned()
                .collect();
            if !cluster_matches.is_empty() {
                result.extend(cluster_matches);
            } else {
                let group_matches: Vec<String> = table
                    .iter()
                    .filter(|(_, m)| m.config.group.as_deref() == Some(name))
                    .map(|(k, _)| k.clone())
                    .collect();
                if group_matches.is_empty() {
                    return Err(format!("process or group not found: {name}"));
                }
                result.extend(group_matches);
            }
        }
    }
    Ok(result)
}

const DEP_WAIT_TIMEOUT: Duration = Duration::from_secs(60);
const DEP_POLL_INTERVAL: Duration = Duration::from_millis(200);

async fn wait_for_online(
    names: &[String],
    processes: &Arc<RwLock<ProcessTable>>,
) -> Result<(), String> {
    let deadline = tokio::time::Instant::now() + DEP_WAIT_TIMEOUT;

    loop {
        if tokio::time::Instant::now() >= deadline {
            return Err(format!(
                "timeout waiting for dependencies to come online: {}",
                names.join(", ")
            ));
        }

        {
            let table = processes.read().await;
            let mut all_online = true;
            for name in names {
                if let Some(managed) = table.get(name) {
                    match managed.status {
                        ProcessStatus::Online => {}
                        ProcessStatus::Stopped | ProcessStatus::Errored => {
                            return Err(format!(
                                "dependency '{}' failed (status: {})",
                                name, managed.status
                            ));
                        }
                        ProcessStatus::Unhealthy => {
                            return Err(format!("dependency '{}' is unhealthy", name));
                        }
                        ProcessStatus::Starting => {
                            all_online = false;
                        }
                    }
                } else {
                    return Err(format!("dependency '{}' not found in process table", name));
                }
            }
            if all_online {
                return Ok(());
            }
        }

        tokio::time::sleep(DEP_POLL_INTERVAL).await;
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct DumpEntry {
    name: String,
    config: ProcessConfig,
    pid: Option<u32>,
    restarts: u32,
}

fn is_pid_alive(pid: u32) -> bool {
    crate::sys::is_pid_alive(pid)
}

/// Expand cluster-mode processes: entries with `instances > 1` are replaced by
/// N individual entries named `<name>:0` .. `<name>:N-1`.  Dependencies that
/// reference a clustered name are rewritten to point at the individual instances.
pub fn expand_instances(configs: HashMap<String, ProcessConfig>) -> HashMap<String, ProcessConfig> {
    // First pass: identify which logical names are clustered and how many instances.
    let mut cluster_map: HashMap<String, u32> = HashMap::new();
    for (name, config) in &configs {
        let n = config.instances.unwrap_or(1);
        if n > 1 {
            cluster_map.insert(name.clone(), n);
        }
    }

    // If nothing is clustered, return as-is.
    if cluster_map.is_empty() {
        return configs;
    }

    let mut result: HashMap<String, ProcessConfig> = HashMap::new();

    for (name, config) in configs {
        let n = config.instances.unwrap_or(1);

        // Rewrite depends_on: replace any clustered dep name with its instance names.
        let rewrite_deps = |deps: &Option<Vec<String>>| -> Option<Vec<String>> {
            let deps = deps.as_ref()?;
            let mut new_deps = Vec::new();
            for dep in deps {
                if let Some(&count) = cluster_map.get(dep) {
                    for i in 0..count {
                        new_deps.push(format!("{}:{}", dep, i));
                    }
                } else {
                    new_deps.push(dep.clone());
                }
            }
            Some(new_deps)
        };

        if n <= 1 {
            // Non-clustered: just rewrite deps if needed.
            let mut cfg = config;
            cfg.depends_on = rewrite_deps(&cfg.depends_on);
            result.insert(name, cfg);
        } else {
            // Clustered: expand into N entries.
            for i in 0..n {
                let instance_name = format!("{}:{}", name, i);
                let mut cfg = config.clone();
                cfg.instances = Some(1);
                cfg.depends_on = rewrite_deps(&cfg.depends_on);

                // Auto-set group to logical name if user didn't specify one.
                if cfg.group.is_none() {
                    cfg.group = Some(name.clone());
                }

                // Inject PM3_INSTANCE_ID and PM3_INSTANCE_COUNT.
                let env = cfg.env.get_or_insert_with(HashMap::new);
                env.insert("PM3_INSTANCE_ID".to_string(), i.to_string());
                env.insert("PM3_INSTANCE_COUNT".to_string(), n.to_string());

                result.insert(instance_name, cfg);
            }
        }
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ProcessConfig;

    fn cfg(command: &str) -> ProcessConfig {
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
    fn test_expand_instances_no_clusters() {
        let mut configs = HashMap::new();
        configs.insert("web".to_string(), cfg("node server.js"));
        configs.insert("db".to_string(), cfg("postgres"));
        let result = expand_instances(configs.clone());
        assert_eq!(result.len(), 2);
        assert!(result.contains_key("web"));
        assert!(result.contains_key("db"));
    }

    #[test]
    fn test_expand_instances_basic() {
        let mut configs = HashMap::new();
        let mut web = cfg("node server.js");
        web.instances = Some(3);
        configs.insert("web".to_string(), web);

        let result = expand_instances(configs);
        assert_eq!(result.len(), 3);
        for i in 0..3 {
            let name = format!("web:{}", i);
            let c = result.get(&name).unwrap();
            assert_eq!(c.command, "node server.js");
            assert_eq!(c.instances, Some(1));
            assert_eq!(c.group.as_deref(), Some("web"));
            let env = c.env.as_ref().unwrap();
            assert_eq!(env.get("PM3_INSTANCE_ID").unwrap(), &i.to_string());
            assert_eq!(env.get("PM3_INSTANCE_COUNT").unwrap(), "3");
        }
    }

    #[test]
    fn test_expand_instances_preserves_custom_group() {
        let mut configs = HashMap::new();
        let mut web = cfg("node server.js");
        web.instances = Some(2);
        web.group = Some("backend".to_string());
        configs.insert("web".to_string(), web);

        let result = expand_instances(configs);
        for i in 0..2 {
            let c = result.get(&format!("web:{}", i)).unwrap();
            assert_eq!(c.group.as_deref(), Some("backend"));
        }
    }

    #[test]
    fn test_expand_instances_rewrites_deps() {
        let mut configs = HashMap::new();

        let mut db = cfg("postgres");
        db.instances = Some(2);
        configs.insert("db".to_string(), db);

        let mut web = cfg("node server.js");
        web.depends_on = Some(vec!["db".to_string()]);
        configs.insert("web".to_string(), web);

        let result = expand_instances(configs);
        // web should now depend on db:0 and db:1
        let web_cfg = result.get("web").unwrap();
        let deps = web_cfg.depends_on.as_ref().unwrap();
        assert!(deps.contains(&"db:0".to_string()));
        assert!(deps.contains(&"db:1".to_string()));
        assert_eq!(deps.len(), 2);
    }

    #[test]
    fn test_expand_instances_clustered_depends_on_clustered() {
        let mut configs = HashMap::new();

        let mut db = cfg("postgres");
        db.instances = Some(2);
        configs.insert("db".to_string(), db);

        let mut web = cfg("node server.js");
        web.instances = Some(3);
        web.depends_on = Some(vec!["db".to_string()]);
        configs.insert("web".to_string(), web);

        let result = expand_instances(configs);
        assert_eq!(result.len(), 5); // 2 db + 3 web

        // Each web instance should depend on db:0 and db:1
        for i in 0..3 {
            let c = result.get(&format!("web:{}", i)).unwrap();
            let deps = c.depends_on.as_ref().unwrap();
            assert!(deps.contains(&"db:0".to_string()));
            assert!(deps.contains(&"db:1".to_string()));
            assert_eq!(deps.len(), 2);
        }
    }

    #[test]
    fn test_expand_instances_single_passthrough() {
        let mut configs = HashMap::new();
        let mut web = cfg("node server.js");
        web.instances = Some(1);
        configs.insert("web".to_string(), web);

        let result = expand_instances(configs);
        assert_eq!(result.len(), 1);
        assert!(result.contains_key("web"));
    }

    #[test]
    fn test_resolve_config_names_cluster_prefix() {
        let mut configs = HashMap::new();
        configs.insert("web:0".to_string(), cfg("node server.js"));
        configs.insert("web:1".to_string(), cfg("node server.js"));
        configs.insert("web:2".to_string(), cfg("node server.js"));
        configs.insert("db".to_string(), cfg("postgres"));

        let result = resolve_config_names(&["web".to_string()], &configs).unwrap();
        assert_eq!(result.len(), 3);
        assert!(result.contains(&"web:0".to_string()));
        assert!(result.contains(&"web:1".to_string()));
        assert!(result.contains(&"web:2".to_string()));
    }

    #[test]
    fn test_resolve_config_names_exact_match_preferred() {
        let mut configs = HashMap::new();
        configs.insert("web".to_string(), cfg("node server.js"));
        configs.insert("web:0".to_string(), cfg("node server.js"));

        let result = resolve_config_names(&["web".to_string()], &configs).unwrap();
        assert_eq!(result, vec!["web".to_string()]);
    }

    #[test]
    fn test_resolve_config_names_group_fallback() {
        let mut configs = HashMap::new();
        let mut web = cfg("node server.js");
        web.group = Some("backend".to_string());
        configs.insert("web".to_string(), web);

        let result = resolve_config_names(&["backend".to_string()], &configs).unwrap();
        assert_eq!(result, vec!["web".to_string()]);
    }
}
