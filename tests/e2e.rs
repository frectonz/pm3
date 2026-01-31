use assert_cmd::Command;
use assert_cmd::cargo::cargo_bin_cmd;
use pm3::protocol::{ProcessInfo, ProcessStatus, Response};
use predicates::prelude::*;
use std::path::Path;
use std::time::Duration;
use tempfile::TempDir;

fn pm3(data_dir: &Path, work_dir: &Path) -> Command {
    let mut cmd: Command = cargo_bin_cmd!("pm3").into();
    cmd.env("PM3_DATA_DIR", data_dir);
    cmd.current_dir(work_dir);
    cmd.timeout(Duration::from_secs(30));
    cmd
}

fn kill_daemon(data_dir: &Path, work_dir: &Path) {
    let _ = pm3(data_dir, work_dir).arg("kill").output();
    std::thread::sleep(Duration::from_millis(300));
}

fn parse_json_response(output: &std::process::Output) -> Response {
    let stdout = String::from_utf8_lossy(&output.stdout);
    serde_json::from_str(stdout.trim()).expect("failed to parse JSON response")
}

fn get_process_list(data_dir: &Path, work_dir: &Path) -> Vec<ProcessInfo> {
    let output = pm3(data_dir, work_dir)
        .args(["--json", "list"])
        .output()
        .unwrap();
    match parse_json_response(&output) {
        Response::ProcessList { processes } => processes,
        other => panic!("expected ProcessList, got: {other:?}"),
    }
}

fn find_process_pid(processes: &[ProcessInfo], name: &str) -> u32 {
    processes
        .iter()
        .find(|p| p.name == name)
        .unwrap_or_else(|| panic!("process '{name}' not found"))
        .pid
        .unwrap_or_else(|| panic!("process '{name}' has no pid"))
}

#[test]
fn test_e2e_stop_one_process_others_keep_running() {
    let dir = TempDir::new().unwrap();
    let work_dir = dir.path();
    let data_dir = dir.path().join("data");

    std::fs::write(
        work_dir.join("pm3.toml"),
        r#"
[web]
command = "sleep 999"

[worker]
command = "sleep 999"
"#,
    )
    .unwrap();

    // Start all processes
    pm3(&data_dir, work_dir).arg("start").assert().success();

    // Stop only web
    pm3(&data_dir, work_dir)
        .args(["stop", "web"])
        .assert()
        .success()
        .stdout(predicate::str::contains("stopped: web"));

    // Verify via list: web is stopped, worker is still online
    let processes = get_process_list(&data_dir, work_dir);

    let web = processes
        .iter()
        .find(|p| p.name == "web")
        .expect("web should appear in list");
    assert_eq!(web.status, ProcessStatus::Stopped, "web should be stopped");

    let worker = processes
        .iter()
        .find(|p| p.name == "worker")
        .expect("worker should appear in list");
    assert_eq!(
        worker.status,
        ProcessStatus::Online,
        "worker should be online"
    );

    kill_daemon(&data_dir, work_dir);
}

#[test]
fn test_e2e_stop_all_processes() {
    let dir = TempDir::new().unwrap();
    let work_dir = dir.path();
    let data_dir = dir.path().join("data");

    std::fs::write(
        work_dir.join("pm3.toml"),
        r#"
[web]
command = "sleep 999"

[worker]
command = "sleep 999"
"#,
    )
    .unwrap();

    // Start all processes
    pm3(&data_dir, work_dir).arg("start").assert().success();

    // Stop all (no name argument)
    pm3(&data_dir, work_dir)
        .arg("stop")
        .assert()
        .success()
        .stdout(predicate::str::contains("stopped:"));

    // Verify via list: all processes are stopped
    let processes = get_process_list(&data_dir, work_dir);
    assert!(!processes.is_empty(), "should have processes in list");
    for p in &processes {
        assert_eq!(
            p.status,
            ProcessStatus::Stopped,
            "process '{}' should be stopped",
            p.name
        );
    }

    kill_daemon(&data_dir, work_dir);
}

#[test]
fn test_e2e_stop_nonexistent_prints_error() {
    let dir = TempDir::new().unwrap();
    let work_dir = dir.path();
    let data_dir = dir.path().join("data");

    std::fs::write(
        work_dir.join("pm3.toml"),
        r#"
[web]
command = "sleep 999"
"#,
    )
    .unwrap();

    // Start a process so the daemon has a process table
    pm3(&data_dir, work_dir).arg("start").assert().success();

    // Try to stop a nonexistent process (use --json to get structured error)
    let output = pm3(&data_dir, work_dir)
        .args(["--json", "stop", "nonexistent"])
        .output()
        .unwrap();
    let response = parse_json_response(&output);
    match response {
        Response::Error { message } => {
            assert!(
                message.contains("not found"),
                "error should contain 'not found', got: {message}"
            );
        }
        other => panic!("expected Error response, got: {other:?}"),
    }

    kill_daemon(&data_dir, work_dir);
}

#[test]
fn test_e2e_restart_one_process_gets_new_pid() {
    let dir = TempDir::new().unwrap();
    let work_dir = dir.path();
    let data_dir = dir.path().join("data");

    std::fs::write(
        work_dir.join("pm3.toml"),
        r#"
[web]
command = "sleep 999"

[worker]
command = "sleep 999"
"#,
    )
    .unwrap();

    // Start all processes
    pm3(&data_dir, work_dir).arg("start").assert().success();

    // Record PIDs
    let processes = get_process_list(&data_dir, work_dir);
    let web_pid_before = find_process_pid(&processes, "web");
    let worker_pid_before = find_process_pid(&processes, "worker");

    // Restart only web
    pm3(&data_dir, work_dir)
        .args(["restart", "web"])
        .assert()
        .success()
        .stdout(predicate::str::contains("restarted: web"));

    // Verify: web has new PID, worker unchanged, both online
    let processes = get_process_list(&data_dir, work_dir);
    let web_pid_after = find_process_pid(&processes, "web");
    let worker_pid_after = find_process_pid(&processes, "worker");

    assert_ne!(
        web_pid_before, web_pid_after,
        "web PID should change after restart"
    );
    assert_eq!(
        worker_pid_before, worker_pid_after,
        "worker PID should not change"
    );

    let web = processes.iter().find(|p| p.name == "web").unwrap();
    assert_eq!(web.status, ProcessStatus::Online, "web should be online");
    let worker = processes.iter().find(|p| p.name == "worker").unwrap();
    assert_eq!(
        worker.status,
        ProcessStatus::Online,
        "worker should be online"
    );

    kill_daemon(&data_dir, work_dir);
}

#[test]
fn test_e2e_restart_all_processes_get_new_pids() {
    let dir = TempDir::new().unwrap();
    let work_dir = dir.path();
    let data_dir = dir.path().join("data");

    std::fs::write(
        work_dir.join("pm3.toml"),
        r#"
[web]
command = "sleep 999"

[worker]
command = "sleep 999"
"#,
    )
    .unwrap();

    // Start all processes
    pm3(&data_dir, work_dir).arg("start").assert().success();

    // Record PIDs
    let processes = get_process_list(&data_dir, work_dir);
    let web_pid_before = find_process_pid(&processes, "web");
    let worker_pid_before = find_process_pid(&processes, "worker");

    // Restart all (no args)
    pm3(&data_dir, work_dir)
        .arg("restart")
        .assert()
        .success()
        .stdout(predicate::str::contains("restarted:"));

    // Verify: both have new PIDs, both online
    let processes = get_process_list(&data_dir, work_dir);
    let web_pid_after = find_process_pid(&processes, "web");
    let worker_pid_after = find_process_pid(&processes, "worker");

    assert_ne!(
        web_pid_before, web_pid_after,
        "web PID should change after restart"
    );
    assert_ne!(
        worker_pid_before, worker_pid_after,
        "worker PID should change after restart"
    );

    let web = processes.iter().find(|p| p.name == "web").unwrap();
    assert_eq!(web.status, ProcessStatus::Online, "web should be online");
    let worker = processes.iter().find(|p| p.name == "worker").unwrap();
    assert_eq!(
        worker.status,
        ProcessStatus::Online,
        "worker should be online"
    );

    kill_daemon(&data_dir, work_dir);
}

// ── Step 13: Kill command ───────────────────────────────────────────

#[test]
fn test_e2e_kill_stops_processes_and_cleans_up() {
    let dir = TempDir::new().unwrap();
    let work_dir = dir.path();
    let data_dir = dir.path().join("data");

    std::fs::write(
        work_dir.join("pm3.toml"),
        r#"
[web]
command = "sleep 999"

[worker]
command = "sleep 999"
"#,
    )
    .unwrap();

    // Start both processes
    pm3(&data_dir, work_dir).arg("start").assert().success();

    // Verify socket and PID file exist
    assert!(
        data_dir.join("pm3.sock").exists(),
        "pm3.sock should exist after start"
    );
    assert!(
        data_dir.join("pm3.pid").exists(),
        "pm3.pid should exist after start"
    );

    // Kill the daemon
    pm3(&data_dir, work_dir)
        .arg("kill")
        .assert()
        .success()
        .stdout(predicate::str::contains("daemon shutting down"));

    // Wait for async cleanup
    std::thread::sleep(Duration::from_millis(500));

    // Socket and PID file should be cleaned up
    assert!(
        !data_dir.join("pm3.sock").exists(),
        "pm3.sock should be removed after kill"
    );
    assert!(
        !data_dir.join("pm3.pid").exists(),
        "pm3.pid should be removed after kill"
    );
}

#[test]
fn test_e2e_kill_then_list_auto_starts_fresh_daemon() {
    let dir = TempDir::new().unwrap();
    let work_dir = dir.path();
    let data_dir = dir.path().join("data");

    std::fs::write(
        work_dir.join("pm3.toml"),
        r#"
[web]
command = "sleep 999"
"#,
    )
    .unwrap();

    // Start a process and verify it appears
    pm3(&data_dir, work_dir).arg("start").assert().success();
    let processes = get_process_list(&data_dir, work_dir);
    assert!(
        processes.iter().any(|p| p.name == "web"),
        "web should appear in list before kill"
    );

    // Kill the daemon
    pm3(&data_dir, work_dir).arg("kill").assert().success();
    std::thread::sleep(Duration::from_millis(500));

    // List should auto-start a fresh daemon with no processes
    let processes = get_process_list(&data_dir, work_dir);
    assert!(
        processes.is_empty(),
        "process list should be empty after kill"
    );

    // Fresh daemon should have recreated socket and PID file
    assert!(
        data_dir.join("pm3.sock").exists(),
        "pm3.sock should be re-created by fresh daemon"
    );
    assert!(
        data_dir.join("pm3.pid").exists(),
        "pm3.pid should be re-created by fresh daemon"
    );

    kill_daemon(&data_dir, work_dir);
}

// ── Step 8: Start command ───────────────────────────────────────────

#[test]
fn test_e2e_start_one_process_running() {
    let dir = TempDir::new().unwrap();
    let work_dir = dir.path();
    let data_dir = dir.path().join("data");

    std::fs::write(
        work_dir.join("pm3.toml"),
        r#"
[web]
command = "sleep 999"
"#,
    )
    .unwrap();

    // Start should report the process
    pm3(&data_dir, work_dir)
        .arg("start")
        .assert()
        .success()
        .stdout(predicate::str::contains("started: web"));

    // List should show web online
    let processes = get_process_list(&data_dir, work_dir);
    let web = processes
        .iter()
        .find(|p| p.name == "web")
        .expect("web should appear in list");
    assert_eq!(web.status, ProcessStatus::Online, "web should be online");

    kill_daemon(&data_dir, work_dir);
}

#[test]
fn test_e2e_start_two_processes_running() {
    let dir = TempDir::new().unwrap();
    let work_dir = dir.path();
    let data_dir = dir.path().join("data");

    std::fs::write(
        work_dir.join("pm3.toml"),
        r#"
[web]
command = "sleep 999"

[worker]
command = "sleep 999"
"#,
    )
    .unwrap();

    pm3(&data_dir, work_dir).arg("start").assert().success();

    // List should show both processes online
    let processes = get_process_list(&data_dir, work_dir);

    let web = processes
        .iter()
        .find(|p| p.name == "web")
        .expect("web should appear in list");
    assert_eq!(web.status, ProcessStatus::Online, "web should be online");

    let worker = processes
        .iter()
        .find(|p| p.name == "worker")
        .expect("worker should appear in list");
    assert_eq!(
        worker.status,
        ProcessStatus::Online,
        "worker should be online"
    );

    kill_daemon(&data_dir, work_dir);
}

#[test]
fn test_e2e_start_named_process_only() {
    let dir = TempDir::new().unwrap();
    let work_dir = dir.path();
    let data_dir = dir.path().join("data");

    std::fs::write(
        work_dir.join("pm3.toml"),
        r#"
[web]
command = "sleep 999"

[worker]
command = "sleep 999"
"#,
    )
    .unwrap();

    // Start only web
    pm3(&data_dir, work_dir)
        .args(["start", "web"])
        .assert()
        .success()
        .stdout(predicate::str::contains("started: web"));

    // List should show web but not worker
    let processes = get_process_list(&data_dir, work_dir);
    assert!(
        processes.iter().any(|p| p.name == "web"),
        "web should appear in list"
    );
    assert!(
        !processes.iter().any(|p| p.name == "worker"),
        "worker should NOT appear in list"
    );

    kill_daemon(&data_dir, work_dir);
}

#[test]
fn test_e2e_start_no_config_file_errors() {
    let dir = TempDir::new().unwrap();
    let work_dir = dir.path();
    let data_dir = dir.path().join("data");

    // No pm3.toml exists — start should fail client-side
    pm3(&data_dir, work_dir).arg("start").assert().failure();
}

#[test]
fn test_e2e_start_nonexistent_name_errors() {
    let dir = TempDir::new().unwrap();
    let work_dir = dir.path();
    let data_dir = dir.path().join("data");

    std::fs::write(
        work_dir.join("pm3.toml"),
        r#"
[web]
command = "sleep 999"
"#,
    )
    .unwrap();

    // Start a nonexistent process name (use --json to get structured error)
    let output = pm3(&data_dir, work_dir)
        .args(["--json", "start", "nonexistent"])
        .output()
        .unwrap();
    let response = parse_json_response(&output);
    match response {
        Response::Error { message } => {
            assert!(
                message.contains("not found"),
                "error should contain 'not found', got: {message}"
            );
        }
        other => panic!("expected Error response, got: {other:?}"),
    }

    kill_daemon(&data_dir, work_dir);
}

// ── Step 9: List command ────────────────────────────────────────────

#[test]
fn test_e2e_list_shows_process_details() {
    let dir = TempDir::new().unwrap();
    let work_dir = dir.path();
    let data_dir = dir.path().join("data");

    std::fs::write(
        work_dir.join("pm3.toml"),
        r#"
[web]
command = "sleep 999"

[worker]
command = "sleep 999"
"#,
    )
    .unwrap();

    pm3(&data_dir, work_dir).arg("start").assert().success();

    let processes = get_process_list(&data_dir, work_dir);

    // Both processes should be listed with PIDs and online status
    for name in &["web", "worker"] {
        let p = processes
            .iter()
            .find(|p| p.name == *name)
            .unwrap_or_else(|| panic!("{name} should appear in list"));
        assert_eq!(p.status, ProcessStatus::Online, "{name} should be online");
        assert!(p.pid.is_some(), "{name} should have a PID");
    }

    kill_daemon(&data_dir, work_dir);
}

#[test]
fn test_e2e_list_no_processes_shows_message() {
    let dir = TempDir::new().unwrap();
    let work_dir = dir.path();
    let data_dir = dir.path().join("data");

    // No pm3.toml needed — list auto-starts the daemon
    let processes = get_process_list(&data_dir, work_dir);
    assert!(
        processes.is_empty(),
        "process list should be empty when no processes running"
    );

    kill_daemon(&data_dir, work_dir);
}

// ── Full lifecycle ──────────────────────────────────────────────────

#[test]
fn test_e2e_full_lifecycle() {
    let dir = TempDir::new().unwrap();
    let work_dir = dir.path();
    let data_dir = dir.path().join("data");

    std::fs::write(
        work_dir.join("pm3.toml"),
        r#"
[web]
command = "sleep 999"

[worker]
command = "sleep 999"
"#,
    )
    .unwrap();

    // 1. Start → 2 processes
    pm3(&data_dir, work_dir)
        .arg("start")
        .assert()
        .success()
        .stdout(predicate::str::contains("started:"));

    // 2. List → both online, record PIDs
    let processes = get_process_list(&data_dir, work_dir);
    let web_pid_before = find_process_pid(&processes, "web");
    let worker_pid_before = find_process_pid(&processes, "worker");
    for p in &processes {
        assert_eq!(
            p.status,
            ProcessStatus::Online,
            "{} should be online",
            p.name
        );
    }

    // 3. Restart web → new PID, worker unchanged
    pm3(&data_dir, work_dir)
        .args(["restart", "web"])
        .assert()
        .success()
        .stdout(predicate::str::contains("restarted: web"));

    let processes = get_process_list(&data_dir, work_dir);
    let web_pid_after = find_process_pid(&processes, "web");
    let worker_pid_after = find_process_pid(&processes, "worker");
    assert_ne!(
        web_pid_before, web_pid_after,
        "web PID should change after restart"
    );
    assert_eq!(
        worker_pid_before, worker_pid_after,
        "worker PID should not change after web restart"
    );

    // 4. Stop worker → worker stopped, web still online
    pm3(&data_dir, work_dir)
        .args(["stop", "worker"])
        .assert()
        .success()
        .stdout(predicate::str::contains("stopped: worker"));

    let processes = get_process_list(&data_dir, work_dir);
    let web = processes.iter().find(|p| p.name == "web").unwrap();
    assert_eq!(
        web.status,
        ProcessStatus::Online,
        "web should still be online after stopping worker"
    );
    let worker = processes.iter().find(|p| p.name == "worker").unwrap();
    assert_eq!(
        worker.status,
        ProcessStatus::Stopped,
        "worker should be stopped"
    );

    // 5. Kill → daemon shuts down, files cleaned up
    pm3(&data_dir, work_dir)
        .arg("kill")
        .assert()
        .success()
        .stdout(predicate::str::contains("daemon shutting down"));
    std::thread::sleep(Duration::from_millis(500));
    assert!(
        !data_dir.join("pm3.sock").exists(),
        "socket should be removed after kill"
    );
    assert!(
        !data_dir.join("pm3.pid").exists(),
        "PID file should be removed after kill"
    );

    // 6. List → auto-starts fresh daemon, no processes
    let processes = get_process_list(&data_dir, work_dir);
    assert!(
        processes.is_empty(),
        "process list should be empty after kill"
    );

    kill_daemon(&data_dir, work_dir);
}

// ── Item 14: Log command ────────────────────────────────────────────

#[test]
fn test_e2e_log_shows_output() {
    let dir = TempDir::new().unwrap();
    let work_dir = dir.path();
    let data_dir = dir.path().join("data");

    std::fs::write(
        work_dir.join("pm3.toml"),
        r#"
[echoer]
command = "sh -c 'echo hello_from_echoer'"
"#,
    )
    .unwrap();

    pm3(&data_dir, work_dir).arg("start").assert().success();
    std::thread::sleep(Duration::from_millis(500));

    pm3(&data_dir, work_dir)
        .args(["log", "echoer"])
        .assert()
        .success()
        .stdout(predicate::str::contains("hello_from_echoer"));

    kill_daemon(&data_dir, work_dir);
}

#[test]
fn test_e2e_log_lines_param() {
    let dir = TempDir::new().unwrap();
    let work_dir = dir.path();
    let data_dir = dir.path().join("data");

    std::fs::write(
        work_dir.join("pm3.toml"),
        r#"
[counter]
command = "sh -c 'for i in 1 2 3 4 5 6 7 8 9 10; do echo line$i; done'"
"#,
    )
    .unwrap();

    pm3(&data_dir, work_dir).arg("start").assert().success();
    std::thread::sleep(Duration::from_millis(500));

    let output = pm3(&data_dir, work_dir)
        .args(["log", "counter", "--lines", "5"])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let lines: Vec<&str> = stdout.lines().filter(|l| !l.is_empty()).collect();

    assert_eq!(
        lines.len(),
        5,
        "should show exactly 5 lines, got: {lines:?}"
    );

    kill_daemon(&data_dir, work_dir);
}

#[test]
fn test_e2e_log_no_name_shows_interleaved() {
    let dir = TempDir::new().unwrap();
    let work_dir = dir.path();
    let data_dir = dir.path().join("data");

    std::fs::write(
        work_dir.join("pm3.toml"),
        r#"
[alpha]
command = "sh -c 'echo alpha_line'"

[beta]
command = "sh -c 'echo beta_line'"
"#,
    )
    .unwrap();

    pm3(&data_dir, work_dir).arg("start").assert().success();
    std::thread::sleep(Duration::from_millis(500));

    let output = pm3(&data_dir, work_dir).arg("log").output().unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(
        stdout.contains("alpha_line"),
        "should contain alpha_line, got: {stdout}"
    );
    assert!(
        stdout.contains("beta_line"),
        "should contain beta_line, got: {stdout}"
    );

    kill_daemon(&data_dir, work_dir);
}
