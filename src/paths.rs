use color_eyre::eyre::bail;
use std::path::{Path, PathBuf};

#[derive(Clone)]
pub struct Paths {
    data_dir: PathBuf,
}

impl Paths {
    pub fn new() -> color_eyre::Result<Self> {
        let Some(base) = dirs::data_dir() else {
            bail!("could not determine data directory");
        };
        Ok(Self {
            data_dir: base.join("pm3"),
        })
    }

    pub fn with_base(base: PathBuf) -> Self {
        Self { data_dir: base }
    }

    pub fn data_dir(&self) -> &Path {
        &self.data_dir
    }

    pub fn pid_file(&self) -> PathBuf {
        self.data_dir.join("pm3.pid")
    }

    pub fn socket_file(&self) -> PathBuf {
        self.data_dir.join("pm3.sock")
    }

    pub fn dump_file(&self) -> PathBuf {
        self.data_dir.join("dump.json")
    }

    pub fn log_dir(&self) -> PathBuf {
        self.data_dir.join("logs")
    }

    pub fn stdout_log(&self, name: &str) -> PathBuf {
        self.data_dir.join("logs").join(format!("{name}-out.log"))
    }

    pub fn stderr_log(&self, name: &str) -> PathBuf {
        self.data_dir.join("logs").join(format!("{name}-err.log"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(target_os = "macos")]
    #[test]
    fn test_data_dir_macos() {
        let paths = Paths::new().unwrap();
        let data_dir = paths.data_dir().to_str().unwrap();
        assert!(
            data_dir.ends_with("Library/Application Support/pm3"),
            "expected macOS data dir, got: {data_dir}"
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn test_data_dir_linux() {
        let paths = Paths::new().unwrap();
        let data_dir = paths.data_dir().to_str().unwrap();
        assert!(
            data_dir.ends_with(".local/share/pm3") || data_dir.contains("pm3"),
            "expected Linux data dir, got: {data_dir}"
        );
    }

    #[test]
    fn test_pid_file_under_data_dir() {
        let paths = Paths::with_base(PathBuf::from("/tmp/pm3-test"));
        let pid = paths.pid_file();
        assert!(pid.starts_with(paths.data_dir()));
        assert!(pid.ends_with("pm3.pid"));
    }

    #[test]
    fn test_socket_file_under_data_dir() {
        let paths = Paths::with_base(PathBuf::from("/tmp/pm3-test"));
        let sock = paths.socket_file();
        assert!(sock.starts_with(paths.data_dir()));
        assert!(sock.ends_with("pm3.sock"));
    }

    #[test]
    fn test_dump_file_under_data_dir() {
        let paths = Paths::with_base(PathBuf::from("/tmp/pm3-test"));
        let dump = paths.dump_file();
        assert!(dump.starts_with(paths.data_dir()));
        assert!(dump.ends_with("dump.json"));
    }

    #[test]
    fn test_log_dir_under_data_dir() {
        let paths = Paths::with_base(PathBuf::from("/tmp/pm3-test"));
        let log_dir = paths.log_dir();
        assert!(log_dir.starts_with(paths.data_dir()));
        assert!(log_dir.ends_with("logs"));
    }

    #[test]
    fn test_stdout_log_includes_name() {
        let paths = Paths::with_base(PathBuf::from("/tmp/pm3-test"));
        let log = paths.stdout_log("web");
        assert!(log.ends_with("logs/web-out.log"));
    }

    #[test]
    fn test_stderr_log_includes_name() {
        let paths = Paths::with_base(PathBuf::from("/tmp/pm3-test"));
        let log = paths.stderr_log("web");
        assert!(log.ends_with("logs/web-err.log"));
    }
}
