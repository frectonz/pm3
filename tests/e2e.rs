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
