//! System clipboard: text, file lists (Windows CF_HDROP, X11 text/uri-list) and images.
//! Images travel as PNG; the clipboard itself holds RGBA pixels.

use std::borrow::Cow;
use std::hash::{Hash, Hasher};
use std::path::PathBuf;
use tracing::warn;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClipContent {
    Empty,
    Text(String),
    Files(Vec<PathBuf>),
    Image(ClipImage),
}

/// A clipboard image as PNG. Two images are equal when their pixels are, even if
/// the PNG bytes differ (a clipboard round trip re-encodes it).
#[derive(Clone)]
pub struct ClipImage {
    pub png: Vec<u8>,
    pub width: u32,
    pub height: u32,
    pixels: u64,
}

impl PartialEq for ClipImage {
    fn eq(&self, other: &Self) -> bool {
        (self.width, self.height, self.pixels) == (other.width, other.height, other.pixels)
    }
}

impl Eq for ClipImage {}

impl std::fmt::Debug for ClipImage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "ClipImage({}x{}, {} bytes PNG)",
            self.width,
            self.height,
            self.png.len()
        )
    }
}

fn pixel_hash(rgba: &[u8]) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    rgba.hash(&mut h);
    h.finish()
}

impl ClipImage {
    /// Encode RGBA pixels. Fast compression: screenshots are sent right away.
    pub fn from_rgba(width: u32, height: u32, rgba: &[u8]) -> anyhow::Result<Self> {
        use image::codecs::png::{CompressionType, FilterType, PngEncoder};
        use image::ImageEncoder;
        let mut png = Vec::new();
        PngEncoder::new_with_quality(&mut png, CompressionType::Fast, FilterType::Adaptive)
            .write_image(rgba, width, height, image::ExtendedColorType::Rgba8)?;
        Ok(Self {
            png,
            width,
            height,
            pixels: pixel_hash(rgba),
        })
    }

    /// Decode a PNG received from another computer. Returns the RGBA pixels too.
    pub fn from_png(png: Vec<u8>) -> anyhow::Result<(Self, Vec<u8>)> {
        let img = image::load_from_memory_with_format(&png, image::ImageFormat::Png)?.to_rgba8();
        let (width, height) = img.dimensions();
        let rgba = img.into_raw();
        let me = Self {
            png,
            width,
            height,
            pixels: pixel_hash(&rgba),
        };
        Ok((me, rgba))
    }
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
    /// Text comes before images (office programs put a picture of copied cells next
    /// to the text). Images are only read when `images` is set: converting one is
    /// too slow to repeat on every poll.
    pub fn read(&mut self, images: bool) -> ClipContent {
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
        // GNOME Desktop Icons / Nautilus often expose only gnome-copied-files.
        #[cfg(all(unix, not(target_os = "macos")))]
        if let Some(files) = crate::linux::clip_x11::read_gnome_files() {
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
            Ok(text) if !text.is_empty() => return ClipContent::Text(text),
            _ => {}
        }
        if !images {
            return ClipContent::Empty;
        }
        match self.inner.get_image() {
            Ok(img) => {
                match ClipImage::from_rgba(img.width as u32, img.height as u32, &img.bytes) {
                    Ok(img) => ClipContent::Image(img),
                    Err(e) => {
                        warn!("encode clipboard image: {e:#}");
                        ClipContent::Empty
                    }
                }
            }
            Err(_) => ClipContent::Empty,
        }
    }

    pub fn set_text(&mut self, text: &str) -> anyhow::Result<()> {
        self.inner.set_text(text.to_string())?;
        Ok(())
    }

    pub fn set_image(&mut self, width: u32, height: u32, rgba: Vec<u8>) -> anyhow::Result<()> {
        self.inner.set_image(arboard::ImageData {
            width: width as usize,
            height: height as usize,
            bytes: Cow::Owned(rgba),
        })?;
        Ok(())
    }

    pub fn set_files(&mut self, files: &[PathBuf]) -> anyhow::Result<()> {
        // Prefer owning CLIPBOARD with both uri-list and gnome-copied-files so
        // Desktop Icons / Nautilus can paste placeholders (Doc §4.4).
        #[cfg(all(unix, not(target_os = "macos")))]
        {
            if crate::linux::clip_x11::set_files(files) {
                return Ok(());
            }
        }
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

/// Tells cheaply whether the clipboard changed. Windows has a sequence number;
/// X11 reports new clipboard owners through XFixes.
pub struct ChangeWatch {
    #[cfg(all(unix, not(target_os = "macos")))]
    x: Option<xwatch::Watch>,
}

impl ChangeWatch {
    pub fn new() -> Self {
        Self {
            #[cfg(all(unix, not(target_os = "macos")))]
            x: xwatch::Watch::open(),
        }
    }

    /// Changes whenever the clipboard content changes. None means "unknown":
    /// read the clipboard to find out.
    pub fn token(&mut self) -> Option<u64> {
        #[cfg(windows)]
        {
            let n = unsafe { windows::Win32::System::DataExchange::GetClipboardSequenceNumber() };
            Some(n as u64)
        }
        #[cfg(all(unix, not(target_os = "macos")))]
        {
            self.x.as_mut().map(|w| w.token())
        }
        #[cfg(not(any(windows, all(unix, not(target_os = "macos")))))]
        {
            None
        }
    }
}

impl Default for ChangeWatch {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(all(unix, not(target_os = "macos")))]
mod xwatch {
    use x11rb::connection::Connection;
    use x11rb::protocol::xfixes::{ConnectionExt as _, SelectionEventMask};
    use x11rb::protocol::xproto::ConnectionExt as _;
    use x11rb::protocol::Event;
    use x11rb::rust_connection::RustConnection;

    pub struct Watch {
        conn: RustConnection,
        count: u64,
    }

    impl Watch {
        pub fn open() -> Option<Self> {
            let (conn, screen) = x11rb::connect(None).ok()?;
            let root = conn.setup().roots.get(screen)?.root;
            conn.xfixes_query_version(5, 0).ok()?.reply().ok()?;
            let clipboard = conn
                .intern_atom(false, b"CLIPBOARD")
                .ok()?
                .reply()
                .ok()?
                .atom;
            conn.xfixes_select_selection_input(
                root,
                clipboard,
                SelectionEventMask::SET_SELECTION_OWNER
                    | SelectionEventMask::SELECTION_WINDOW_DESTROY
                    | SelectionEventMask::SELECTION_CLIENT_CLOSE,
            )
            .ok()?;
            conn.flush().ok()?;
            Some(Self { conn, count: 0 })
        }

        pub fn token(&mut self) -> u64 {
            while let Ok(Some(ev)) = self.conn.poll_for_event() {
                if matches!(ev, Event::XfixesSelectionNotify(_)) {
                    self.count += 1;
                }
            }
            self.count
        }
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

    #[test]
    fn image_survives_png_round_trip() {
        let rgba: Vec<u8> = (0..4 * 3 * 2).map(|i| (i * 37) as u8).collect();
        let sent = ClipImage::from_rgba(3, 2, &rgba).unwrap();
        let (got, pixels) = ClipImage::from_png(sent.png.clone()).unwrap();
        assert_eq!(pixels, rgba);
        assert_eq!(got, sent);
        let other = ClipImage::from_rgba(3, 2, &[0u8; 24]).unwrap();
        assert_ne!(other, sent);
    }
}
