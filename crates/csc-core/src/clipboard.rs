//! Watches the system clipboard on its own thread. Clipboard calls can block
//! (an X11 owner that answers slowly), so they never run on the input loop.
//! Copied files are only reported; they are sent when someone pastes them on
//! another computer, and never put on a clipboard here.

use csc_platform::clip::{ChangeWatch, ClipContent, ClipImage, SystemClipboard};
use std::path::PathBuf;
use std::sync::mpsc as std_mpsc;
use std::time::Duration;
use tokio::sync::mpsc;
use tracing::{info, warn};

const POLL: Duration = Duration::from_millis(400);

#[derive(Debug)]
pub enum ClipEvent {
    Text(String),
    Files(Vec<PathBuf>),
    /// PNG.
    Image(ClipImage),
}

enum ClipCmd {
    SetText(String),
    SetImage(Vec<u8>),
}

/// Sets the local clipboard from remote content. Setting does not echo back as an event.
#[derive(Clone)]
pub struct ClipHandle {
    tx: std_mpsc::Sender<ClipCmd>,
}

impl ClipHandle {
    pub fn set_text(&self, text: String) {
        let _ = self.tx.send(ClipCmd::SetText(text));
    }

    pub fn set_image(&self, png: Vec<u8>) {
        let _ = self.tx.send(ClipCmd::SetImage(png));
    }
}

pub fn spawn(events: mpsc::Sender<ClipEvent>) -> ClipHandle {
    let (tx, rx) = std_mpsc::channel();
    let _ = std::thread::Builder::new()
        .name("clipboard".into())
        .spawn(move || run(rx, events));
    ClipHandle { tx }
}

fn run(rx: std_mpsc::Receiver<ClipCmd>, events: mpsc::Sender<ClipEvent>) {
    let mut clip = loop {
        if let Some(c) = SystemClipboard::open() {
            break c;
        }
        std::thread::sleep(Duration::from_secs(5));
    };
    let mut watch = ChangeWatch::new();
    // Without a cheap change signal the clipboard is read on every poll, and
    // converting an image each time would cost too much.
    let images = watch.token().is_some();
    // What is on the clipboard now does not count as a new copy.
    let mut last = clip.read(images);
    let mut token = watch.token();
    info!(
        "clipboard watcher ready ({})",
        if images {
            "text, files and images"
        } else {
            "text and files; no change signal, so no images"
        }
    );
    loop {
        match rx.recv_timeout(POLL) {
            Ok(ClipCmd::SetText(text)) => {
                match clip.set_text(&text) {
                    Ok(()) => last = ClipContent::Text(text),
                    Err(e) => warn!("set clipboard text: {e:#}"),
                }
                token = watch.token();
                continue;
            }
            Ok(ClipCmd::SetImage(png)) => {
                match ClipImage::from_png(png) {
                    Ok((img, rgba)) => {
                        let (w, h) = (img.width, img.height);
                        match clip.set_image(w, h, rgba) {
                            Ok(()) => {
                                info!("clipboard image set ({w}x{h})");
                                last = ClipContent::Image(img);
                            }
                            Err(e) => warn!("set clipboard image: {e:#}"),
                        }
                    }
                    Err(e) => warn!("received clipboard image is not a PNG: {e:#}"),
                }
                token = watch.token();
                continue;
            }
            Err(std_mpsc::RecvTimeoutError::Disconnected) => return,
            Err(std_mpsc::RecvTimeoutError::Timeout) => {}
        }
        let now = watch.token();
        if now.is_some() && now == token {
            continue;
        }
        token = now;
        let content = clip.read(images);
        if content == last || content == ClipContent::Empty {
            continue;
        }
        last = content.clone();
        let ev = match content {
            ClipContent::Text(t) => ClipEvent::Text(t),
            ClipContent::Files(f) => ClipEvent::Files(f),
            ClipContent::Image(i) => ClipEvent::Image(i),
            ClipContent::Empty => continue,
        };
        if events.blocking_send(ev).is_err() {
            return;
        }
    }
}
