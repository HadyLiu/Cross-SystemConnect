//! Log files that never grow past `LOG_CAP_BYTES`. Once full, the current file
//! is copied to `stem.1.ext` (for `log.txt` → `log.1.txt`) and the live file starts over.

use std::fs::{File, OpenOptions};
use std::io::{self, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

pub const LOG_CAP_BYTES: u64 = 1024 * 1024;

pub struct CappedLog {
    path: PathBuf,
    file: File,
    size: u64,
}

impl CappedLog {
    /// Open `path` for appending, creating it and its folder. A file already
    /// over the cap is emptied first.
    pub fn open(path: &Path) -> io::Result<Self> {
        if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
            std::fs::create_dir_all(dir)?;
        }
        // Not append mode: emptying the file needs plain write access on Windows.
        let mut file = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(false)
            .open(path)?;
        let size = file.seek(SeekFrom::End(0))?;
        let mut log = Self {
            path: path.to_path_buf(),
            file,
            size,
        };
        if log.size >= LOG_CAP_BYTES {
            log.rotate()?;
        }
        Ok(log)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    fn clear(&mut self) -> io::Result<()> {
        self.file.set_len(0)?;
        self.file.seek(SeekFrom::Start(0))?;
        self.size = 0;
        Ok(())
    }

    /// Keep one previous file as `stem.1.ext` (e.g. `log.1.txt`), then empty the live file.
    fn rotate(&mut self) -> io::Result<()> {
        let previous = std::fs::read(&self.path).unwrap_or_default();
        let bak = backup_path(&self.path);
        let _ = std::fs::remove_file(&bak);
        if !previous.is_empty() {
            std::fs::write(&bak, previous)?;
        }
        self.clear()?;
        let note = b"-- previous log moved aside --\n";
        self.file.write_all(note)?;
        self.size = note.len() as u64;
        Ok(())
    }

    pub fn write(&mut self, buf: &[u8]) -> io::Result<()> {
        if self.size + buf.len() as u64 > LOG_CAP_BYTES {
            self.rotate()?;
        }
        self.file.write_all(buf)?;
        self.size += buf.len() as u64;
        Ok(())
    }
}

fn backup_path(path: &Path) -> PathBuf {
    let stem = path
        .file_stem()
        .and_then(|n| n.to_str())
        .unwrap_or("log");
    match path.extension().and_then(|e| e.to_str()) {
        Some(ext) => path.with_file_name(format!("{stem}.1.{ext}")),
        None => path.with_file_name(format!("{stem}.1")),
    }
}

/// A log file shared by every writer of a `tracing` subscriber. `None` writes nothing.
#[derive(Clone, Default)]
pub struct SharedLog(Arc<Mutex<Option<CappedLog>>>);

impl SharedLog {
    pub fn set(&self, log: Option<CappedLog>) {
        if let Ok(mut cur) = self.0.lock() {
            *cur = log;
        }
    }

    pub fn path(&self) -> Option<PathBuf> {
        self.0.lock().ok()?.as_ref().map(|l| l.path.clone())
    }
}

impl Write for SharedLog {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if let Ok(mut cur) = self.0.lock() {
            if let Some(log) = cur.as_mut() {
                // A full disk must not turn every log call into an error.
                let _ = log.write(buf);
            }
        }
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn full_log_starts_over() {
        let dir = std::env::temp_dir().join(format!("csc-log-{}", std::process::id()));
        let path = dir.join("log.txt");
        let _ = std::fs::remove_dir_all(&dir);
        let mut log = CappedLog::open(&path).unwrap();
        let line = vec![b'x'; 1000];
        log.write(&line).unwrap();
        assert_eq!(std::fs::metadata(&path).unwrap().len(), 1000);
        drop(log);

        std::fs::write(&path, vec![b'y'; LOG_CAP_BYTES as usize + 10]).unwrap();
        let mut log = CappedLog::open(&path).unwrap();
        log.write(b"new\n").unwrap();
        let bak = backup_path(&path);
        assert!(std::fs::metadata(&bak).unwrap().len() > LOG_CAP_BYTES);
        assert_eq!(bak.file_name().and_then(|n| n.to_str()), Some("log.1.txt"));
        let live = std::fs::read(&path).unwrap();
        assert!(live.ends_with(b"new\n"), "{live:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
