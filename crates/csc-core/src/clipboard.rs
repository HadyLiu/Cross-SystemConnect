//! Clipboard sync orchestration (offer / request / data).

use csc_platform::{Clipboard, FileEntry};
use csc_proto::{ClipOfferPayload, MsgType};
use tracing::debug;

#[derive(Default)]
pub struct ClipboardSync {
    seq: u64,
    last_remote_seq: u64,
    ignore_next: bool,
}

impl ClipboardSync {
    #[allow(dead_code)]
    pub fn on_local_changed(&mut self, clip: &mut dyn Clipboard) -> Option<ClipOfferPayload> {
        if self.ignore_next {
            self.ignore_next = false;
            return None;
        }
        self.seq += 1;
        let text = clip.get_text();
        let files = clip.get_files().map(|paths| {
            paths
                .into_iter()
                .map(|p| FileEntry {
                    relative_path: p
                        .file_name()
                        .map(|s| s.to_string_lossy().into_owned())
                        .unwrap_or_else(|| "file".into()),
                    size: std::fs::metadata(&p).map(|m| m.len()).unwrap_or(0),
                    mtime_unix: 0,
                    is_directory: p.is_dir(),
                })
                .collect::<Vec<_>>()
        });
        let mut formats = Vec::new();
        if text.is_some() {
            formats.push("text".into());
        }
        if files.as_ref().map(|f| !f.is_empty()).unwrap_or(false) {
            formats.push("files".into());
        }
        if formats.is_empty() {
            return None;
        }
        Some(ClipOfferPayload {
            seq: self.seq,
            formats,
            text,
            files: files
                .unwrap_or_default()
                .into_iter()
                .map(|f| csc_proto::FileEntry {
                    relative_path: f.relative_path,
                    size: f.size,
                    mtime_unix: f.mtime_unix,
                    is_directory: f.is_directory,
                })
                .collect(),
        })
    }

    pub fn apply_remote_offer(&mut self, clip: &mut dyn Clipboard, offer: &ClipOfferPayload) {
        if offer.seq <= self.last_remote_seq {
            return;
        }
        self.last_remote_seq = offer.seq;
        self.ignore_next = true;
        if let Some(t) = &offer.text {
            clip.set_text(t);
        }
        if !offer.files.is_empty() {
            let entries: Vec<_> = offer
                .files
                .iter()
                .map(|f| FileEntry {
                    relative_path: f.relative_path.clone(),
                    size: f.size,
                    mtime_unix: f.mtime_unix,
                    is_directory: f.is_directory,
                })
                .collect();
            clip.set_files_lazy(&entries);
        }
        debug!(seq = offer.seq, "applied remote clip offer");
    }

    pub fn offer_msg_type() -> MsgType {
        MsgType::ClipOffer
    }
}
