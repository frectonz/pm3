use crate::paths::Paths;
use nix::sys::signal;
use nix::unistd::Pid;
use std::fs;
use std::io;

pub fn write_pid_file(paths: &Paths) -> io::Result<()> {
    let pid = std::process::id();
    fs::write(paths.pid_file(), pid.to_string())
}

pub fn read_pid_file(paths: &Paths) -> Option<u32> {
    fs::read_to_string(paths.pid_file())
        .ok()
        .and_then(|s| s.trim().parse().ok())
}

pub fn remove_pid_file(paths: &Paths) {
    let _ = fs::remove_file(paths.pid_file());
}

pub fn is_daemon_running(paths: &Paths) -> io::Result<bool> {
    let pid = match read_pid_file(paths) {
        Some(p) => p,
        None => return Ok(false),
    };

    match signal::kill(Pid::from_raw(pid as i32), None) {
        Ok(()) => Ok(true),
        Err(nix::errno::Errno::ESRCH) => {
            // Process doesn't exist — stale PID file
            remove_pid_file(paths);
            Ok(false)
        }
        Err(nix::errno::Errno::EPERM) => {
            // Process exists but we lack permission to signal it
            Ok(true)
        }
        Err(e) => Err(io::Error::new(io::ErrorKind::Other, e)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn test_write_and_read_pid_file() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::with_base(dir.path().to_path_buf());

        write_pid_file(&paths).unwrap();
        let pid = read_pid_file(&paths);
        assert_eq!(pid, Some(std::process::id()));
    }

    #[test]
    fn test_read_nonexistent_pid_file() {
        let paths = Paths::with_base(PathBuf::from("/tmp/pm3-nonexistent-test-dir"));
        assert_eq!(read_pid_file(&paths), None);
    }

    #[test]
    fn test_is_daemon_running_with_self() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::with_base(dir.path().to_path_buf());

        // Write our own PID — should report running
        write_pid_file(&paths).unwrap();
        assert!(is_daemon_running(&paths).unwrap());
    }

    #[test]
    fn test_is_daemon_running_stale_pid() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::with_base(dir.path().to_path_buf());

        // Write a bogus PID that almost certainly doesn't exist
        fs::write(paths.pid_file(), "4294967").unwrap();
        assert!(!is_daemon_running(&paths).unwrap());
        // Stale PID file should have been cleaned up
        assert!(!paths.pid_file().exists());
    }
}
