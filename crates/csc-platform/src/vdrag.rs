//! Types shared by clipboard file paste placeholders.
//!
//! Cross-screen virtual drag was removed; only [`DragItem`] remains for
//! building paste placeholders on Linux.

use std::path::PathBuf;

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

/// Latest pointer position on this screen (no-op after drag removal; kept for call sites).
pub fn note_pointer(_x: i32, _y: i32) {}

/// The left button was released on this computer (no-op after drag removal).
pub fn note_local_release() {}

/// Whether this build can start a virtual file drag here (always false; feature removed).
pub fn supported() -> bool {
    false
}

/// Join `root` / relative path with `/` separators.
pub fn join_rel(root: &std::path::Path, rel: &str) -> PathBuf {
    let mut out = root.to_path_buf();
    for part in rel.split('/').filter(|p| !p.is_empty() && *p != ".") {
        if part == ".." {
            continue;
        }
        out.push(part);
    }
    out
}
