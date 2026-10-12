//! Clipboard placeholders for remote file paste (Doc §4.4 / §14.6).
//!
//! Offer empty marker files via CLIPBOARD; when the file manager pastes them,
//! find the copies and report their paths so the core can fill them.

use crate::vdrag::DragItem;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime};
use tracing::{debug, warn};

/// Placeholder marker inside a pasted folder (Doc §14.6).
const FOLDER_MARKER_PREFIX: &str = ".csc-paste-";
/// 16-byte magic including the trailing NUL.
const PLACEHOLDER_MAGIC: &[u8] = b"CSC-PLACEHOLDER\0";
#[derive(Clone)]
struct Top {
    #[allow(dead_code)]
    index: usize,
    name: String,
    is_dir: bool,
    path: PathBuf,
    token: [u8; 32],
}

fn folder_marker(id: u64) -> String {
    format!("{FOLDER_MARKER_PREFIX}{id}")
}

fn marker_bytes(token: &[u8; 32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(PLACEHOLDER_MAGIC.len() + 32);
    out.extend_from_slice(PLACEHOLDER_MAGIC);
    out.extend_from_slice(token);
    out
}

fn fresh_token() -> [u8; 32] {
    let mut token = [0u8; 32];
    if let Ok(mut f) = std::fs::File::open("/dev/urandom") {
        use std::io::Read;
        let _ = f.read_exact(&mut token);
    }
    token
}

fn make_placeholders(dir: &Path, id: u64, items: &[DragItem]) -> std::io::Result<Vec<Top>> {
    let _ = std::fs::remove_dir_all(dir);
    std::fs::create_dir_all(dir)?;
    let mut tops = Vec::new();
    for (index, it) in items.iter().enumerate().filter(|(_, it)| it.is_top()) {
        let name = it.name().to_string();
        if name.is_empty() || name == "." || name == ".." || name.contains('/') {
            continue;
        }
        let path = dir.join(&name);
        let token = fresh_token();
        let body = marker_bytes(&token);
        if it.is_directory {
            std::fs::create_dir_all(&path)?;
            std::fs::write(path.join(folder_marker(id)), &body)?;
        } else {
            std::fs::write(&path, &body)?;
        }
        tops.push(Top {
            index,
            name,
            is_dir: it.is_directory,
            path,
            token,
        });
    }
    Ok(tops)
}

pub(crate) fn file_uri(path: &Path) -> String {
    use std::os::unix::ffi::OsStrExt;
    let mut out = String::from("file://");
    for &b in path.as_os_str().as_bytes() {
        if b.is_ascii_alphanumeric() || b"/-._~".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

fn is_marker(path: &Path, top: &Top) -> bool {
    std::fs::read(path).is_ok_and(|b| b == marker_bytes(&top.token))
}

fn find_copies_until(
    id: u64,
    tops: &[Top],
    since: SystemTime,
    deadline: Instant,
) -> Option<Vec<PathBuf>> {
    let staging = paste_root();
    let since_secs = since
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let mut found: Vec<Option<PathBuf>> = vec![None; tops.len()];
    loop {
        for dir in search_dirs(&staging, since_secs) {
            let Ok(list) = std::fs::read_dir(&dir) else {
                continue;
            };
            for entry in list.flatten() {
                let Ok(meta) = entry.metadata() else { continue };
                if meta.ctime() < since_secs {
                    continue;
                }
                let path = entry.path();
                for (slot, top) in found.iter_mut().zip(tops) {
                    if slot.is_some() || top.is_dir != meta.is_dir() {
                        continue;
                    }
                    let hit = if top.is_dir {
                        is_marker(&path.join(folder_marker(id)), top)
                    } else {
                        meta.len() < 256 && is_marker(&path, top)
                    };
                    if hit {
                        debug!("paste {id}: {} copied to {}", top.name, path.display());
                        *slot = Some(path.clone());
                        break;
                    }
                }
            }
        }
        if found.iter().all(Option::is_some) {
            return found.into_iter().collect();
        }
        if Instant::now() >= deadline {
            warn!("paste {id}: copied placeholders not found");
            return None;
        }
        std::thread::sleep(Duration::from_millis(500));
    }
}

fn same_style_name(path: PathBuf, name: &str, is_dir: bool) -> PathBuf {
    let Some(dir) = path.parent().map(Path::to_path_buf) else {
        return path;
    };
    if path.file_name().is_some_and(|n| n == name) {
        return path;
    }
    let (stem, ext) = match name.rfind('.') {
        Some(i) if i > 0 && !is_dir => (&name[..i], &name[i..]),
        _ => (name, ""),
    };
    let ours = path
        .file_name()
        .and_then(|n| n.to_str())
        .and_then(|n| {
            n.strip_prefix(stem)?
                .strip_prefix(" (")?
                .strip_suffix(ext)?
                .strip_suffix(')')
        })
        .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()));
    if ours {
        return path;
    }
    let Some(target) = (1..10_000)
        .map(|n| dir.join(format!("{stem} ({n}){ext}")))
        .find(|p| std::fs::symlink_metadata(p).is_err())
    else {
        return path;
    };
    match std::fs::rename(&path, &target) {
        Ok(()) => target,
        Err(e) => {
            debug!("keep {}: {e}", path.display());
            path
        }
    }
}

fn search_dirs(staging: &Path, since_secs: i64) -> Vec<PathBuf> {
    const MAX_DEPTH: usize = 6;
    const MAX_DIRS: usize = 40_000;
    let mut roots: Vec<PathBuf> = Vec::new();
    if let Some(home) = std::env::var_os("HOME").map(PathBuf::from) {
        roots.push(home);
    }
    if let Ok(user) = std::env::var("USER") {
        roots.push(PathBuf::from("/media").join(&user));
        roots.push(PathBuf::from("/run/media").join(&user));
    }
    roots.push(PathBuf::from("/mnt"));
    roots.push(std::env::temp_dir());
    let mut out = Vec::new();
    let mut queue: std::collections::VecDeque<(PathBuf, usize)> = roots
        .into_iter()
        .filter(|r| r.is_dir())
        .map(|r| (r, 0))
        .collect();
    let mut seen = 0usize;
    while let Some((dir, depth)) = queue.pop_front() {
        seen += 1;
        if seen > MAX_DIRS {
            break;
        }
        if dir.starts_with(staging)
            || dir
                .components()
                .any(|c| c.as_os_str() == "csc-paste")
        {
            continue;
        }
        if std::fs::metadata(&dir).is_ok_and(|m| m.mtime() >= since_secs) {
            out.push(dir.clone());
        }
        if depth >= MAX_DEPTH {
            continue;
        }
        let Ok(list) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in list.flatten() {
            let hidden = entry.file_name().to_string_lossy().starts_with('.');
            if !hidden && entry.file_type().is_ok_and(|t| t.is_dir()) {
                queue.push_back((entry.path(), depth + 1));
            }
        }
    }
    out
}

fn paste_root() -> PathBuf {
    let base = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(std::env::temp_dir);
    base.join("csc-paste")
}

struct PasteHold {
    gen: u64,
    id: u64,
    tops: Vec<Top>,
}

static PASTE_GEN: AtomicU64 = AtomicU64::new(1);
static PASTE: std::sync::Mutex<Option<PasteHold>> = std::sync::Mutex::new(None);

/// Placeholder files for a remote copy, so a file manager can paste them.
/// Returns a generation and the placeholder paths to put on the clipboard.
pub(crate) fn prepare_paste(id: u64, items: &[DragItem]) -> Option<(u64, Vec<PathBuf>)> {
    let dir = paste_root().join(id.to_string());
    let tops = make_placeholders(&dir, id, items).ok()?;
    if tops.is_empty() {
        return None;
    }
    let paths = tops.iter().map(|t| t.path.clone()).collect();
    let gen = PASTE_GEN.fetch_add(1, Ordering::AcqRel) + 1;
    if let Ok(mut slot) = PASTE.lock() {
        *slot = Some(PasteHold { gen, id, tops });
    }
    Some((gen, paths))
}

/// Block until the file manager has copied the placeholders, or `wait` elapses.
/// A newer [`prepare_paste`] cancels this wait.
pub(crate) fn wait_paste(gen: u64, wait: Duration) -> Option<Vec<PathBuf>> {
    let hold = PASTE.lock().ok()?.as_ref().filter(|h| h.gen == gen)?.clone();
    let since = SystemTime::now() - Duration::from_secs(2);
    let deadline = Instant::now() + wait;
    loop {
        let current = PASTE.lock().ok()?.as_ref().map(|h| h.gen);
        if current != Some(gen) {
            return None;
        }
        if let Some(found) = find_copies_until(
            hold.id,
            &hold.tops,
            since,
            Instant::now() + Duration::from_secs(1),
        ) {
            return Some(
                found
                    .into_iter()
                    .zip(&hold.tops)
                    .map(|(path, top)| same_style_name(path, &top.name, top.is_dir))
                    .collect(),
            );
        }
        if Instant::now() >= deadline {
            return None;
        }
    }
}

impl Clone for PasteHold {
    fn clone(&self) -> Self {
        Self {
            gen: self.gen,
            id: self.id,
            tops: self.tops.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(rel: &str, dir: bool) -> DragItem {
        DragItem {
            relative_path: rel.into(),
            size: 3,
            mtime_unix: 0,
            is_directory: dir,
        }
    }

    #[test]
    fn placeholders_are_found_after_copy() {
        let base = std::env::temp_dir().join(format!("csc-paste-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let items = [item("a.txt", false), item("d", true), item("d/x", false)];
        let tops = make_placeholders(&base.join("stage"), 7, &items).unwrap();
        assert_eq!(tops.len(), 2);
        assert!(is_marker(&base.join("stage/a.txt"), &tops[0]));
        assert!(is_marker(
            &base.join("stage/d").join(folder_marker(7)),
            &tops[1]
        ));
        let dest = base.join("dest");
        std::fs::create_dir_all(dest.join("d")).unwrap();
        std::fs::copy(base.join("stage/a.txt"), dest.join("a (1).txt")).unwrap();
        std::fs::copy(
            base.join("stage/d").join(folder_marker(7)),
            dest.join("d").join(folder_marker(7)),
        )
        .unwrap();
        let since = SystemTime::now() - Duration::from_secs(5);
        let found =
            find_copies_until(7, &tops, since, Instant::now() + Duration::from_secs(20)).unwrap();
        assert_eq!(found.len(), 2);
        let _ = std::fs::remove_dir_all(&base);
    }
}
