use serde::{Deserialize, Serialize};
use std::io::{self, BufRead};
use std::path::Path;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader as TokioBufReader};
use tokio::sync::broadcast;

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// 10 MB rotation threshold
pub const LOG_ROTATION_SIZE: u64 = 10 * 1024 * 1024;

/// Keep up to 3 rotated files (.1, .2, .3)
pub const LOG_ROTATION_KEEP: u32 = 3;

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum LogStream {
    Stdout,
    Stderr,
}

#[derive(Debug, Clone)]
pub struct LogEntry {
    pub stream: LogStream,
    pub line: String,
}

// ---------------------------------------------------------------------------
// tail_file — read last N lines from a file
// ---------------------------------------------------------------------------

pub fn tail_file(path: &Path, n: usize) -> io::Result<Vec<String>> {
    if n == 0 {
        return Ok(Vec::new());
    }

    let file = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e),
    };

    let reader = io::BufReader::new(file);
    let all_lines: Vec<String> = reader.lines().collect::<io::Result<Vec<_>>>()?;

    if all_lines.len() <= n {
        Ok(all_lines)
    } else {
        Ok(all_lines[all_lines.len() - n..].to_vec())
    }
}

// ---------------------------------------------------------------------------
// rotate_log — shift rotated files and rename current to .1
// ---------------------------------------------------------------------------

pub fn rotate_log(path: &Path, max_rotations: u32) -> io::Result<()> {
    // Delete the oldest rotated file if it exists
    let oldest = rotated_path(path, max_rotations);
    if oldest.exists() {
        std::fs::remove_file(&oldest)?;
    }

    // Shift .2 -> .3, .1 -> .2, etc.
    for i in (1..max_rotations).rev() {
        let from = rotated_path(path, i);
        let to = rotated_path(path, i + 1);
        if from.exists() {
            std::fs::rename(&from, &to)?;
        }
    }

    // Rename current to .1
    if path.exists() {
        std::fs::rename(path, rotated_path(path, 1))?;
    }

    Ok(())
}

fn rotated_path(path: &Path, n: u32) -> std::path::PathBuf {
    let mut p = path.as_os_str().to_owned();
    p.push(format!(".{n}"));
    p.into()
}

// ---------------------------------------------------------------------------
// spawn_log_copier — tokio task that reads piped child output
// ---------------------------------------------------------------------------

pub fn spawn_log_copier(
    name: String,
    stream: LogStream,
    reader: impl tokio::io::AsyncRead + Unpin + Send + 'static,
    log_path: std::path::PathBuf,
    log_date_format: Option<String>,
    broadcaster: broadcast::Sender<LogEntry>,
) {
    tokio::spawn(async move {
        if let Err(e) =
            run_log_copier(name, stream, reader, log_path, log_date_format, broadcaster).await
        {
            eprintln!("log copier error: {e}");
        }
    });
}

async fn run_log_copier(
    _name: String,
    stream: LogStream,
    reader: impl tokio::io::AsyncRead + Unpin + Send + 'static,
    log_path: std::path::PathBuf,
    log_date_format: Option<String>,
    broadcaster: broadcast::Sender<LogEntry>,
) -> io::Result<()> {
    let mut buf_reader = TokioBufReader::new(reader);
    let mut file = tokio::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .await?;

    let mut byte_count: u64 = {
        let meta = tokio::fs::metadata(&log_path).await?;
        meta.len()
    };

    let mut line = String::new();
    loop {
        line.clear();
        let n = buf_reader.read_line(&mut line).await?;
        if n == 0 {
            break; // EOF — child exited
        }

        let formatted = if let Some(ref fmt) = log_date_format {
            let ts = chrono::Local::now().format(fmt);
            format!("{ts} | {line}")
        } else {
            line.clone()
        };

        // Check rotation before writing
        let line_bytes = formatted.as_bytes();
        if byte_count + line_bytes.len() as u64 > LOG_ROTATION_SIZE {
            // Flush and close current file, rotate, reopen
            file.flush().await?;
            drop(file);
            rotate_log(&log_path, LOG_ROTATION_KEEP)?;
            file = tokio::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&log_path)
                .await?;
            byte_count = 0;
        }

        file.write_all(line_bytes).await?;
        byte_count += line_bytes.len() as u64;

        // Broadcast to any follow subscribers (ignore if no receivers)
        let _ = broadcaster.send(LogEntry {
            stream: stream.clone(),
            line: line.trim_end().to_string(),
        });
    }

    file.flush().await?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn test_tail_file_empty() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("empty.log");
        std::fs::File::create(&path).unwrap();
        let lines = tail_file(&path, 10).unwrap();
        assert!(lines.is_empty());
    }

    #[test]
    fn test_tail_file_fewer_than_n() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("few.log");
        let mut f = std::fs::File::create(&path).unwrap();
        writeln!(f, "line1").unwrap();
        writeln!(f, "line2").unwrap();
        let lines = tail_file(&path, 10).unwrap();
        assert_eq!(lines, vec!["line1", "line2"]);
    }

    #[test]
    fn test_tail_file_exact_n() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("exact.log");
        let mut f = std::fs::File::create(&path).unwrap();
        for i in 1..=5 {
            writeln!(f, "line{i}").unwrap();
        }
        let lines = tail_file(&path, 5).unwrap();
        assert_eq!(lines.len(), 5);
        assert_eq!(lines[0], "line1");
        assert_eq!(lines[4], "line5");
    }

    #[test]
    fn test_tail_file_last_n() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("many.log");
        let mut f = std::fs::File::create(&path).unwrap();
        for i in 1..=20 {
            writeln!(f, "line{i}").unwrap();
        }
        let lines = tail_file(&path, 3).unwrap();
        assert_eq!(lines, vec!["line18", "line19", "line20"]);
    }

    #[test]
    fn test_tail_file_nonexistent() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nope.log");
        let lines = tail_file(&path, 10).unwrap();
        assert!(lines.is_empty());
    }

    #[test]
    fn test_tail_file_zero_lines() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("zero.log");
        let mut f = std::fs::File::create(&path).unwrap();
        writeln!(f, "line1").unwrap();
        let lines = tail_file(&path, 0).unwrap();
        assert!(lines.is_empty());
    }

    #[test]
    fn test_rotate_log_creates_dot1() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("app.log");
        std::fs::write(&path, "data").unwrap();

        rotate_log(&path, 3).unwrap();

        assert!(!path.exists());
        assert!(rotated_path(&path, 1).exists());
        assert_eq!(
            std::fs::read_to_string(rotated_path(&path, 1)).unwrap(),
            "data"
        );
    }

    #[test]
    fn test_rotate_log_shifts_existing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("app.log");
        std::fs::write(rotated_path(&path, 1), "old1").unwrap();
        std::fs::write(&path, "current").unwrap();

        rotate_log(&path, 3).unwrap();

        assert!(!path.exists());
        assert_eq!(
            std::fs::read_to_string(rotated_path(&path, 1)).unwrap(),
            "current"
        );
        assert_eq!(
            std::fs::read_to_string(rotated_path(&path, 2)).unwrap(),
            "old1"
        );
    }

    #[test]
    fn test_rotate_log_deletes_oldest() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("app.log");
        std::fs::write(rotated_path(&path, 1), "r1").unwrap();
        std::fs::write(rotated_path(&path, 2), "r2").unwrap();
        std::fs::write(rotated_path(&path, 3), "r3").unwrap();
        std::fs::write(&path, "current").unwrap();

        rotate_log(&path, 3).unwrap();

        assert_eq!(
            std::fs::read_to_string(rotated_path(&path, 1)).unwrap(),
            "current"
        );
        assert_eq!(
            std::fs::read_to_string(rotated_path(&path, 2)).unwrap(),
            "r1"
        );
        assert_eq!(
            std::fs::read_to_string(rotated_path(&path, 3)).unwrap(),
            "r2"
        );
        // r3 was deleted
    }

    #[test]
    fn test_rotation_size_constant() {
        assert_eq!(LOG_ROTATION_SIZE, 10 * 1024 * 1024);
    }

    #[test]
    fn test_rotation_keep_constant() {
        assert_eq!(LOG_ROTATION_KEEP, 3);
    }

    // ── Item 16: Log timestamp tests ────────────────────────────────

    /// Helper: pipe `lines` through `run_log_copier` with the given format,
    /// return the resulting log file contents.
    async fn run_copier_with_format(fmt: Option<&str>, lines: &[&str]) -> String {
        let dir = tempfile::tempdir().unwrap();
        let log_path = dir.path().join("test.log");
        let (tx, _rx) = broadcast::channel(16);

        // Build input bytes
        let mut input = String::new();
        for line in lines {
            input.push_str(line);
            input.push('\n');
        }

        let reader = tokio::io::BufReader::new(std::io::Cursor::new(input.into_bytes()));

        run_log_copier(
            "test".to_string(),
            LogStream::Stdout,
            reader,
            log_path.clone(),
            fmt.map(|s| s.to_string()),
            tx,
        )
        .await
        .unwrap();

        tokio::fs::read_to_string(&log_path).await.unwrap()
    }

    #[tokio::test]
    async fn test_timestamp_format_ymd_hms() {
        let content =
            run_copier_with_format(Some("%Y-%m-%d %H:%M:%S"), &["hello world", "second line"])
                .await;

        let re = regex::Regex::new(r"^\d{4}-\d{2}-\d{2} \d{2}:\d{2}:\d{2} \| .+$").unwrap();
        for line in content.lines() {
            assert!(re.is_match(line), "line did not match pattern: {line}");
        }
        assert!(
            content.contains("hello world"),
            "content should be preserved"
        );
        assert!(
            content.contains("second line"),
            "content should be preserved"
        );
    }

    #[tokio::test]
    async fn test_timestamp_format_hm_only() {
        let content = run_copier_with_format(Some("%H:%M"), &["test line"]).await;

        let re = regex::Regex::new(r"^\d{2}:\d{2} \| .+$").unwrap();
        for line in content.lines() {
            assert!(re.is_match(line), "line did not match pattern: {line}");
        }
    }

    #[tokio::test]
    async fn test_timestamp_format_epoch() {
        let content = run_copier_with_format(Some("%s"), &["epoch line"]).await;

        let re = regex::Regex::new(r"^\d+ \| .+$").unwrap();
        for line in content.lines() {
            assert!(re.is_match(line), "line did not match pattern: {line}");
        }
    }

    #[tokio::test]
    async fn test_timestamp_format_date_only() {
        let content = run_copier_with_format(Some("%Y-%m-%d"), &["date only"]).await;

        let re = regex::Regex::new(r"^\d{4}-\d{2}-\d{2} \| .+$").unwrap();
        for line in content.lines() {
            assert!(re.is_match(line), "line did not match pattern: {line}");
        }
    }

    // ── Item 17: Log rotation threshold tests ─────────────────────────

    /// Helper: pipe `data` through `run_log_copier`, return `(TempDir, PathBuf)`
    /// so callers can inspect rotated sibling files.
    async fn run_copier_to_dir(data: Vec<u8>) -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let log_path = dir.path().join("test.log");
        let (tx, _rx) = broadcast::channel(16);
        let reader = tokio::io::BufReader::new(std::io::Cursor::new(data));
        run_log_copier(
            "test".into(),
            LogStream::Stdout,
            reader,
            log_path.clone(),
            None,
            tx,
        )
        .await
        .unwrap();
        (dir, log_path)
    }

    #[tokio::test]
    async fn test_rotation_triggered_at_threshold() {
        // 12,000 lines × 1000 bytes = 12MB (> 10MB threshold)
        let line = "A".repeat(999) + "\n";
        let data: Vec<u8> = line.repeat(12_000).into_bytes();
        let (_dir, log_path) = run_copier_to_dir(data).await;

        // .1 should exist after rotation
        assert!(
            rotated_path(&log_path, 1).exists(),
            "rotated file .1 should exist after exceeding threshold"
        );

        // Current file should be smaller than the rotation threshold
        let current_size = std::fs::metadata(&log_path).unwrap().len();
        assert!(
            current_size < LOG_ROTATION_SIZE,
            "current file size ({current_size}) should be < LOG_ROTATION_SIZE ({LOG_ROTATION_SIZE})"
        );
    }

    #[tokio::test]
    async fn test_rotation_keeps_only_three_rotated_files() {
        // 45,000 lines × 1000 bytes = 45MB (triggers 4 rotations)
        let line = "A".repeat(999) + "\n";
        let data: Vec<u8> = line.repeat(45_000).into_bytes();
        let (_dir, log_path) = run_copier_to_dir(data).await;

        // .1, .2, .3 should exist
        for i in 1..=3 {
            assert!(
                rotated_path(&log_path, i).exists(),
                "rotated file .{i} should exist"
            );
        }

        // .4 should NOT exist
        assert!(
            !rotated_path(&log_path, 4).exists(),
            "rotated file .4 should NOT exist (max keep is 3)"
        );
    }

    #[tokio::test]
    async fn test_no_rotation_below_threshold() {
        // 5,000 lines × 1000 bytes = 5MB (< 10MB threshold)
        let line = "A".repeat(999) + "\n";
        let data: Vec<u8> = line.repeat(5_000).into_bytes();
        let (_dir, log_path) = run_copier_to_dir(data).await;

        // No rotation should have occurred
        assert!(
            !rotated_path(&log_path, 1).exists(),
            "rotated file .1 should NOT exist when below threshold"
        );

        // Current file should contain all data
        let current_size = std::fs::metadata(&log_path).unwrap().len();
        assert_eq!(
            current_size, 5_000_000,
            "current file size should be exactly 5,000,000 bytes"
        );
    }

    #[tokio::test]
    async fn test_no_timestamp_when_format_is_none() {
        let content = run_copier_with_format(None, &["raw line one", "raw line two"]).await;

        let lines: Vec<&str> = content.lines().collect();
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0], "raw line one");
        assert_eq!(lines[1], "raw line two");
        // Verify no separator is present
        for line in &lines {
            assert!(
                !line.contains(" | "),
                "line should not contain ' | ': {line}"
            );
        }
    }
}
