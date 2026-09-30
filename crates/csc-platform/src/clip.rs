//! System clipboard: text and file lists (Windows CF_HDROP, X11 text/uri-list).

use std::path::PathBuf;
use tracing::warn;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClipContent {
    Empty,
    Text(String),
    Files(Vec<PathBuf>),
}

pub struct SystemClipboard {
    inner: arboard::Clipboard,
}

impl SystemClipboard {
    /// None when no clipboard is reachable (for example no X display).
    pub fn open() -> Option<Self> {
        match arboard::Clipboard::new() {
            Ok(inner) => Some(Self { inner }),
            Err(e) => {
                warn!("clipboard unavailable: {e}");
                None
            }
        }
    }

    /// Files take precedence: a file copy often also carries the paths as text.
    pub fn read(&mut self) -> ClipContent {
        if let Ok(files) = self.inner.get().file_list() {
            let files: Vec<PathBuf> = files
                .into_iter()
                .map(tidy_path)
                .filter(|p| !p.as_os_str().is_empty())
                .collect();
            if !files.is_empty() {
                return ClipContent::Files(files);
            }
        }
        match self.inner.get_text() {
            Ok(text) if !text.is_empty() => ClipContent::Text(text),
            _ => ClipContent::Empty,
        }
    }

    pub fn set_text(&mut self, text: &str) -> anyhow::Result<()> {
        self.inner.set_text(text.to_string())?;
        Ok(())
    }

    pub fn set_files(&mut self, files: &[PathBuf]) -> anyhow::Result<()> {
        self.inner.set().file_list(files)?;
        Ok(())
    }
}

/// A path taken from a `text/uri-list`, without the line end left on it.
/// The list is CRLF-separated, and arboard splits on LF only, so every path
/// but the last would otherwise end in '\r'. A real name that ends that way
/// is kept when it exists.
pub fn tidy_path(path: PathBuf) -> PathBuf {
    let Some(s) = path.to_str() else {
        return path;
    };
    let trimmed = s.trim_end_matches(['\r', '\n', '\0']);
    if trimmed.len() == s.len() || path.exists() {
        return path;
    }
    PathBuf::from(trimmed)
}

/// Changes whenever the clipboard content changes, where the platform can tell
/// cheaply. None means "unknown": read the clipboard to find out.
pub fn change_token() -> Option<u64> {
    #[cfg(windows)]
    {
        let n = unsafe { windows::Win32::System::DataExchange::GetClipboardSequenceNumber() };
        Some(n as u64)
    }
    #[cfg(not(windows))]
    {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uri_list_line_ends_are_dropped() {
        let dir = std::env::temp_dir().join(format!("csc-tidy-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("设计方案.md");
        std::fs::write(&file, b"x").unwrap();
        let with_cr = PathBuf::from(format!("{}\r", file.display()));
        assert_eq!(tidy_path(with_cr), file);
        assert_eq!(tidy_path(file.clone()), file);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
