//! Log files that never grow past `LOG_CAP_BYTES`: once full, the file is emptied and starts over.

use std::fs::{File, OpenOptions};
use std::io::{self, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

pub const LOG_CAP_BYTES: u64 = 100 * 1024;

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
            log.clear()?;
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

    pub fn write(&mut self, buf: &[u8]) -> io::Result<()> {
        if self.size + buf.len() as u64 > LOG_CAP_BYTES {
            self.clear()?;
            let note = b"-- log reached 100 KB and was cleared --\n";
            self.file.write_all(note)?;
            self.size = note.len() as u64;
        }
        self.file.write_all(buf)?;
        self.size += buf.len() as u64;
        Ok(())
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
        for _ in 0..102 {
            log.write(&line).unwrap();
        }
        assert_eq!(std::fs::metadata(&path).unwrap().len(), 102_000);
        log.write(&line).unwrap();
        let len = std::fs::metadata(&path).unwrap().len();
        assert!(len < 2_000, "log was not cleared: {len}");
        drop(log);

        std::fs::write(&path, vec![b'y'; LOG_CAP_BYTES as usize + 10]).unwrap();
        let mut log = CappedLog::open(&path).unwrap();
        log.write(b"new\n").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"new\n");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
