use assert_cmd::Command;
use assert_cmd::cargo::cargo_bin_cmd;
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
    let output = pm3(&data_dir, work_dir).arg("list").output().unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);

    let web_line = stdout
        .lines()
        .find(|l| l.contains("web"))
        .expect("web should appear in list output");
    assert!(
        web_line.contains("stopped"),
        "web should be stopped, got: {web_line}"
    );

    let worker_line = stdout
        .lines()
        .find(|l| l.contains("worker"))
        .expect("worker should appear in list output");
    assert!(
        worker_line.contains("online"),
        "worker should be online, got: {worker_line}"
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
    let output = pm3(&data_dir, work_dir).arg("list").output().unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);

    // Every process line (skip header) should show "stopped"
    let process_lines: Vec<&str> = stdout
        .lines()
        .skip(1) // skip header row
        .filter(|l| !l.trim().is_empty())
        .collect();
    assert!(
        !process_lines.is_empty(),
        "should have process lines in output"
    );
    for line in &process_lines {
        assert!(
            line.contains("stopped"),
            "all processes should be stopped, got: {line}"
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

    // Try to stop a nonexistent process
    pm3(&data_dir, work_dir)
        .args(["stop", "nonexistent"])
        .assert()
        .stderr(predicate::str::contains("not found"));

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
    let output = pm3(&data_dir, work_dir).arg("list").output().unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let web_pid_before = extract_pid(&stdout, "web");
    let worker_pid_before = extract_pid(&stdout, "worker");

    // Restart only web
    pm3(&data_dir, work_dir)
        .args(["restart", "web"])
        .assert()
        .success()
        .stdout(predicate::str::contains("restarted: web"));

    // Verify: web has new PID, worker unchanged, both online
    let output = pm3(&data_dir, work_dir).arg("list").output().unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);

    let web_pid_after = extract_pid(&stdout, "web");
    let worker_pid_after = extract_pid(&stdout, "worker");

    assert_ne!(
        web_pid_before, web_pid_after,
        "web PID should change after restart"
    );
    assert_eq!(
        worker_pid_before, worker_pid_after,
        "worker PID should not change"
    );

    let web_line = stdout.lines().find(|l| l.contains("web")).unwrap();
    assert!(
        web_line.contains("online"),
        "web should be online, got: {web_line}"
    );
    let worker_line = stdout.lines().find(|l| l.contains("worker")).unwrap();
    assert!(
        worker_line.contains("online"),
        "worker should be online, got: {worker_line}"
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
    let output = pm3(&data_dir, work_dir).arg("list").output().unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let web_pid_before = extract_pid(&stdout, "web");
    let worker_pid_before = extract_pid(&stdout, "worker");

    // Restart all (no args)
    pm3(&data_dir, work_dir)
        .arg("restart")
        .assert()
        .success()
        .stdout(predicate::str::contains("restarted:"));

    // Verify: both have new PIDs, both online
    let output = pm3(&data_dir, work_dir).arg("list").output().unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);

    let web_pid_after = extract_pid(&stdout, "web");
    let worker_pid_after = extract_pid(&stdout, "worker");

    assert_ne!(
        web_pid_before, web_pid_after,
        "web PID should change after restart"
    );
    assert_ne!(
        worker_pid_before, worker_pid_after,
        "worker PID should change after restart"
    );

    let web_line = stdout.lines().find(|l| l.contains("web")).unwrap();
    assert!(
        web_line.contains("online"),
        "web should be online, got: {web_line}"
    );
    let worker_line = stdout.lines().find(|l| l.contains("worker")).unwrap();
    assert!(
        worker_line.contains("online"),
        "worker should be online, got: {worker_line}"
    );

    kill_daemon(&data_dir, work_dir);
}

/// Extract a PID from `pm3 list` output for a given process name.
/// Expects table rows like: "web    12345  online  ..."
fn extract_pid(list_output: &str, name: &str) -> String {
    let line = list_output
        .lines()
        .find(|l| l.contains(name))
        .unwrap_or_else(|| panic!("process '{name}' not found in list output"));
    let fields: Vec<&str> = line.split_whitespace().collect();
    // PID is the second column
    fields[1].to_string()
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
    let output = pm3(&data_dir, work_dir).arg("list").output().unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("web"),
        "web should appear in list before kill"
    );

    // Kill the daemon
    pm3(&data_dir, work_dir).arg("kill").assert().success();
    std::thread::sleep(Duration::from_millis(500));

    // List should auto-start a fresh daemon with no processes
    pm3(&data_dir, work_dir)
        .arg("list")
        .assert()
        .success()
        .stdout(predicate::str::contains("no processes running"));

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
    let output = pm3(&data_dir, work_dir).arg("list").output().unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let web_line = stdout
        .lines()
        .find(|l| l.contains("web"))
        .expect("web should appear in list");
    assert!(
        web_line.contains("online"),
        "web should be online, got: {web_line}"
    );

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
    let output = pm3(&data_dir, work_dir).arg("list").output().unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);

    let web_line = stdout
        .lines()
        .find(|l| l.contains("web"))
        .expect("web should appear in list");
    assert!(
        web_line.contains("online"),
        "web should be online, got: {web_line}"
    );

    let worker_line = stdout
        .lines()
        .find(|l| l.contains("worker"))
        .expect("worker should appear in list");
    assert!(
        worker_line.contains("online"),
        "worker should be online, got: {worker_line}"
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
    let output = pm3(&data_dir, work_dir).arg("list").output().unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(stdout.contains("web"), "web should appear in list");
    assert!(
        !stdout.contains("worker"),
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

    // Start a nonexistent process name
    pm3(&data_dir, work_dir)
        .args(["start", "nonexistent"])
        .assert()
        .stderr(predicate::str::contains("not found"));

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

    let output = pm3(&data_dir, work_dir).arg("list").output().unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);

    // Header row should contain column names
    let header = stdout.lines().next().expect("should have a header line");
    assert!(header.contains("name"), "header should contain 'name'");
    assert!(header.contains("pid"), "header should contain 'pid'");
    assert!(header.contains("status"), "header should contain 'status'");

    // Both processes should be listed with numeric PIDs and online status
    for name in &["web", "worker"] {
        let line = stdout
            .lines()
            .find(|l| l.contains(name))
            .unwrap_or_else(|| panic!("{name} should appear in list"));
        assert!(
            line.contains("online"),
            "{name} should be online, got: {line}"
        );

        // PID column should be a number
        let pid_str = extract_pid(&stdout, name);
        assert!(
            pid_str.parse::<u32>().is_ok(),
            "{name} PID should be numeric, got: {pid_str}"
        );
    }

    kill_daemon(&data_dir, work_dir);
}

#[test]
fn test_e2e_list_no_processes_shows_message() {
    let dir = TempDir::new().unwrap();
    let work_dir = dir.path();
    let data_dir = dir.path().join("data");

    // No pm3.toml needed — list auto-starts the daemon
    pm3(&data_dir, work_dir)
        .arg("list")
        .assert()
        .success()
        .stdout(predicate::str::contains("no processes running"));

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
    let output = pm3(&data_dir, work_dir).arg("list").output().unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let web_pid_before = extract_pid(&stdout, "web");
    let worker_pid_before = extract_pid(&stdout, "worker");
    assert!(stdout.contains("online"), "processes should be online");

    // 3. Restart web → new PID, worker unchanged
    pm3(&data_dir, work_dir)
        .args(["restart", "web"])
        .assert()
        .success()
        .stdout(predicate::str::contains("restarted: web"));

    let output = pm3(&data_dir, work_dir).arg("list").output().unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let web_pid_after = extract_pid(&stdout, "web");
    let worker_pid_after = extract_pid(&stdout, "worker");
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

    let output = pm3(&data_dir, work_dir).arg("list").output().unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let web_line = stdout.lines().find(|l| l.contains("web")).unwrap();
    assert!(
        web_line.contains("online"),
        "web should still be online after stopping worker"
    );
    let worker_line = stdout.lines().find(|l| l.contains("worker")).unwrap();
    assert!(
        worker_line.contains("stopped"),
        "worker should be stopped, got: {worker_line}"
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
    pm3(&data_dir, work_dir)
        .arg("list")
        .assert()
        .success()
        .stdout(predicate::str::contains("no processes running"));

    kill_daemon(&data_dir, work_dir);
}
