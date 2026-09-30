//! Dragging files that are still on another computer ("virtual files").
//!
//! The core starts a system drag on this computer with [`start`]. The result
//! comes back on the event channel: where the user dropped, or that the drag
//! was cancelled. Data then arrives over the network and is written by the core.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

/// One file or folder of the batch, relative to the batch, with `/` separators.
#[derive(Debug, Clone)]
pub struct DragItem {
    pub relative_path: String,
    pub size: u64,
    pub mtime_unix: i64,
    pub is_directory: bool,
}

impl DragItem {
    pub fn is_top(&self) -> bool {
        !self.relative_path.contains('/')
    }

    pub fn name(&self) -> &str {
        self.relative_path
            .rsplit('/')
            .next()
            .unwrap_or(&self.relative_path)
    }
}

pub struct DragDesc {
    pub id: u64,
    pub items: Vec<DragItem>,
    /// Windows: files are received here first and read by Explorer as they grow.
    pub staged: Option<Arc<StagedFeed>>,
}

#[derive(Debug)]
pub enum DragEvent {
    /// Explorer wants the data now (it asked for file contents or accepted the drop).
    Staged { id: u64 },
    /// The drop target named a folder (Linux XDS). Conflicts are ours to ask.
    Folder { id: u64, dir: PathBuf },
    /// The file manager copied the placeholder files; write into them where they are.
    InPlace { id: u64, tops: Vec<PathBuf> },
    /// Dropped, but where the files went could not be found.
    Lost { id: u64 },
    Cancelled { id: u64, reason: &'static str },
}

impl DragEvent {
    pub fn id(&self) -> u64 {
        match self {
            Self::Staged { id }
            | Self::Folder { id, .. }
            | Self::InPlace { id, .. }
            | Self::Lost { id }
            | Self::Cancelled { id, .. } => *id,
        }
    }
}

/// Whether this build can start a virtual file drag here.
pub fn supported() -> bool {
    #[cfg(windows)]
    {
        true
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        crate::linux::xdnd::supported()
    }
    #[cfg(not(any(windows, all(unix, not(target_os = "macos")))))]
    {
        false
    }
}

/// Start the drag. The left button must already be held (injected by the core);
/// the drag follows the pointer and ends when the button is released.
pub fn start(desc: DragDesc, events: std::sync::mpsc::Sender<DragEvent>) -> bool {
    #[cfg(windows)]
    {
        crate::win::dragsrc::start(desc, events)
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        crate::linux::xdnd::start(desc, events)
    }
    #[cfg(not(any(windows, all(unix, not(target_os = "macos")))))]
    {
        let _ = (desc, events);
        false
    }
}

/// End a running drag without dropping, for example when the pointer leaves this screen.
pub fn cancel(id: u64) {
    #[cfg(windows)]
    crate::win::dragsrc::cancel(id);
    #[cfg(all(unix, not(target_os = "macos")))]
    crate::linux::xdnd::cancel(id);
    #[cfg(not(any(windows, all(unix, not(target_os = "macos")))))]
    let _ = id;
}

/// Remove placeholder and staging files left by an earlier run.
pub fn cleanup_stale() {
    let root = staging_root();
    let Ok(list) = std::fs::read_dir(&root) else {
        return;
    };
    for entry in list.flatten() {
        let old = entry
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.elapsed().ok())
            .is_some_and(|age| age > Duration::from_secs(3600));
        if old {
            let _ = std::fs::remove_dir_all(entry.path());
        }
    }
}

/// Private folder for staging and placeholder files of drags.
pub fn staging_root() -> PathBuf {
    std::env::temp_dir().join("cross-systemconnect-drag")
}

// ---------------------------------------------------------------- staged feed

/// Files received into a staging folder while a reader (Explorer) consumes
/// them, in any order, before they are complete.
pub struct StagedFeed {
    pub root: PathBuf,
    entries: Vec<StagedEntry>,
    failed: AtomicBool,
    /// Set once the core asked the sender for data.
    pub requested: AtomicBool,
    lock: Mutex<()>,
    cv: Condvar,
}

struct StagedEntry {
    path: PathBuf,
    size: u64,
    written: AtomicU64,
    done: AtomicBool,
}

pub enum Wait {
    /// This many bytes of the file exist now.
    Ready(u64),
    Failed,
    TimedOut,
}

impl StagedFeed {
    pub fn new(root: PathBuf, items: &[DragItem]) -> Self {
        let entries = items
            .iter()
            .map(|it| StagedEntry {
                path: join_rel(&root, &it.relative_path),
                size: it.size,
                written: AtomicU64::new(0),
                done: AtomicBool::new(it.is_directory),
            })
            .collect();
        Self {
            root,
            entries,
            failed: AtomicBool::new(false),
            requested: AtomicBool::new(false),
            lock: Mutex::new(()),
            cv: Condvar::new(),
        }
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn path(&self, index: usize) -> Option<&Path> {
        self.entries.get(index).map(|e| e.path.as_path())
    }

    pub fn size(&self, index: usize) -> u64 {
        self.entries.get(index).map(|e| e.size).unwrap_or(0)
    }

    fn wake(&self) {
        let _guard = self.lock.lock();
        self.cv.notify_all();
    }

    pub fn progress(&self, index: usize, written: u64) {
        if let Some(e) = self.entries.get(index) {
            e.written.store(written, Ordering::Release);
            self.wake();
        }
    }

    pub fn complete(&self, index: usize) {
        if let Some(e) = self.entries.get(index) {
            e.written.store(e.size, Ordering::Release);
            e.done.store(true, Ordering::Release);
            self.wake();
        }
    }

    pub fn fail(&self) {
        self.failed.store(true, Ordering::Release);
        self.wake();
    }

    pub fn is_failed(&self) -> bool {
        self.failed.load(Ordering::Acquire)
    }

    pub fn all_done(&self) -> bool {
        self.entries.iter().all(|e| e.done.load(Ordering::Acquire))
    }

    /// Block until more than `pos` bytes of entry `index` exist, or it is complete.
    pub fn wait(&self, index: usize, pos: u64, timeout: Duration) -> Wait {
        let Some(e) = self.entries.get(index) else {
            return Wait::Failed;
        };
        let deadline = Instant::now() + timeout;
        let mut guard = self.lock.lock().unwrap_or_else(|p| p.into_inner());
        loop {
            if self.is_failed() {
                return Wait::Failed;
            }
            let written = e.written.load(Ordering::Acquire);
            if written > pos || e.done.load(Ordering::Acquire) {
                return Wait::Ready(written);
            }
            let now = Instant::now();
            if now >= deadline {
                return Wait::TimedOut;
            }
            guard = match self.cv.wait_timeout(guard, deadline - now) {
                Ok((g, _)) => g,
                Err(p) => p.into_inner().0,
            };
        }
    }
}

fn join_rel(root: &Path, rel: &str) -> PathBuf {
    let mut p = root.to_path_buf();
    for part in rel.split('/') {
        p.push(part);
    }
    p
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(rel: &str, size: u64, dir: bool) -> DragItem {
        DragItem {
            relative_path: rel.into(),
            size,
            mtime_unix: 0,
            is_directory: dir,
        }
    }

    #[test]
    fn readers_wait_for_bytes_in_any_order() {
        let feed = Arc::new(StagedFeed::new(
            PathBuf::from("/tmp/x"),
            &[item("d", 0, true), item("d/a", 10, false), item("d/b", 5, false)],
        ));
        assert!(matches!(feed.wait(0, 0, Duration::ZERO), Wait::Ready(0)));
        assert!(matches!(feed.wait(2, 0, Duration::ZERO), Wait::TimedOut));
        let f2 = feed.clone();
        let reader = std::thread::spawn(move || f2.wait(2, 0, Duration::from_secs(5)));
        feed.progress(1, 4);
        feed.complete(2);
        assert!(matches!(reader.join().unwrap(), Wait::Ready(5)));
        assert!(!feed.all_done());
        feed.complete(1);
        assert!(feed.all_done());
        feed.fail();
        assert!(matches!(feed.wait(1, 99, Duration::ZERO), Wait::Failed));
        assert_eq!(feed.path(2), Some(Path::new("/tmp/x/d/b")));
    }
}
