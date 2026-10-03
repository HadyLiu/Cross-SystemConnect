//! A small label in the corner while another computer is moving this pointer.

use std::sync::mpsc::{self, Sender};
use std::sync::OnceLock;
use x11rb::connection::Connection;
use x11rb::protocol::xproto::{
    ConnectionExt as _, CreateGCAux, CreateWindowAux, WindowClass,
};
use x11rb::rust_connection::RustConnection;

pub(crate) fn set_controlled_hint(text: Option<String>) {
    if let Some(tx) = sender() {
        let _ = tx.send(text);
    }
}

fn sender() -> Option<Sender<Option<String>>> {
    static TX: OnceLock<Sender<Option<String>>> = OnceLock::new();
    Some(
        TX.get_or_init(|| {
            let (tx, rx) = mpsc::channel();
            let _ = std::thread::Builder::new()
                .name("csc-hint".into())
                .spawn(move || run(rx));
            tx
        })
        .clone(),
    )
}

fn run(rx: mpsc::Receiver<Option<String>>) {
    let mut shown: Option<Hint> = None;
    while let Ok(next) = rx.recv() {
        match next {
            Some(text) => {
                if shown.is_none() {
                    shown = Hint::open();
                }
                if let Some(hint) = shown.as_mut() {
                    hint.paint(&text);
                }
            }
            None => {
                if let Some(hint) = shown.take() {
                    hint.close();
                }
            }
        }
    }
}

struct Hint {
    conn: RustConnection,
    window: u32,
    gc: u32,
}

impl Hint {
    fn open() -> Option<Self> {
        let (conn, screen) = x11rb::connect(None).ok()?;
        let setup = conn.setup();
        let root = setup.roots[screen].root;
        let width = setup.roots[screen].width_in_pixels;
        let window = conn.generate_id().ok()?;
        let w = 280u16;
        let h = 28u16;
        let x = width.saturating_sub(w + 16) as i16;
        conn.create_window(
            x11rb::COPY_DEPTH_FROM_PARENT,
            window,
            root,
            x,
            16,
            w,
            h,
            0,
            WindowClass::INPUT_OUTPUT,
            x11rb::COPY_FROM_PARENT,
            &CreateWindowAux::new()
                .override_redirect(1)
                .background_pixel(0x2a2620),
        )
        .ok()?;
        conn.map_window(window).ok()?;
        let gc = conn.generate_id().ok()?;
        conn.create_gc(gc, window, &CreateGCAux::new().foreground(0xf7f4ec))
            .ok()?;
        let _ = conn.flush();
        Some(Self { conn, window, gc })
    }

    fn paint(&mut self, text: &str) {
        let bytes: Vec<u8> = text
            .chars()
            .map(|c| if c.is_ascii() { c as u8 } else { b'?' })
            .take(40)
            .collect();
        let _ = self.conn.image_text8(self.window, self.gc, 8, 18, &bytes);
        let _ = self.conn.flush();
    }

    fn close(self) {
        let _ = self.conn.destroy_window(self.window);
        let _ = self.conn.flush();
    }
}
