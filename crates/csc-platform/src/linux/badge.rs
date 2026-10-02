//! Drag image that follows the pointer while a file is held over this screen.
//!
//! Wayland will not let this process position a normal window, and a real
//! file-manager drag does not reach GNOME Files. An XWayland override-redirect
//! window still composites above the desktop, offset from the hotspot so the
//! folder underneath stays the drop target.

use super::uinput;
use crate::vdrag::{self, pointed};
use std::sync::atomic::AtomicBool;
use std::thread;
use std::time::{Duration, Instant};
use x11rb::connection::Connection;
use x11rb::protocol::xproto::{
    AtomEnum, ChangeGCAux, ChangeWindowAttributesAux, ConfigureWindowAux, ConnectionExt as _,
    CreateGCAux, CreateWindowAux, EventMask, PropMode, Rectangle, StackMode, WindowClass,
};
use x11rb::wrapper::ConnectionExt as _;
use x11rb::rust_connection::RustConnection;

pub(crate) fn watch_drop(label: &str, is_dir: bool, stacked: bool, cancel: &AtomicBool) -> Option<(i32, i32)> {
    let mut badge = Badge::open(label, is_dir, stacked);
    let start = Instant::now();
    let mut saw_down = uinput::left_down();
    loop {
        if cancel.load(std::sync::atomic::Ordering::Acquire) || vdrag::follow_cancelled() {
            if let Some(b) = badge.as_mut() {
                b.close();
            }
            return None;
        }
        let down = uinput::left_down();
        if down {
            saw_down = true;
        }
        if saw_down && !down {
            if let Some(b) = badge.as_mut() {
                b.close();
            }
            // A screen leave releases the button too. Give that path time to
            // mark the drag cancelled before this is treated as a drop.
            thread::sleep(Duration::from_millis(40));
            if cancel.load(std::sync::atomic::Ordering::Acquire) || vdrag::follow_cancelled() {
                return None;
            }
            return Some(pointed());
        }
        if start.elapsed() > Duration::from_secs(180) {
            if let Some(b) = badge.as_mut() {
                b.close();
            }
            return None;
        }
        if let Some(b) = badge.as_mut() {
            let (x, y) = pointed();
            b.place(x, y);
        }
        thread::sleep(Duration::from_millis(16));
    }
}

struct Badge {
    conn: RustConnection,
    window: u32,
    gc_bg: u32,
    gc_draw: u32,
    gc_text: u32,
    width: u16,
    height: u16,
    label: Vec<u8>,
    is_dir: bool,
    stacked: bool,
    depth: u8,
}

impl Badge {
    fn open(label: &str, is_dir: bool, stacked: bool) -> Option<Self> {
        let label = latin1(label);
        let (conn, screen) = x11rb::connect(None).ok()?;
        let root = conn.setup().roots[screen].root;
        let depth = conn.setup().roots[screen].root_depth;
        let window = conn.generate_id().ok()?;
        let width = 36 + (label.len() as u16).saturating_mul(7).min(160);
        let height = 40u16;
        let (x, y) = pointed();
        let (x, y) = offset(x, y);
        let (x, y) = (x as i16, y as i16);
        conn.create_window(
            x11rb::COPY_DEPTH_FROM_PARENT,
            window,
            root,
            x,
            y,
            width,
            height,
            1,
            WindowClass::INPUT_OUTPUT,
            x11rb::COPY_FROM_PARENT,
            &CreateWindowAux::new()
                .override_redirect(1)
                .background_pixel(rgb(depth, 0xF7, 0xF4, 0xEC))
                .border_pixel(rgb(depth, 0x5C, 0x53, 0x46))
                .event_mask(EventMask::EXPOSURE),
        )
        .ok()?;
        mark_as_drag(&conn, window);
        conn.map_window(window).ok()?;
        let gc_bg = gc(&conn, window, rgb(depth, 0xF7, 0xF4, 0xEC))?;
        let gc_draw = gc(&conn, window, rgb(depth, 0xE0, 0xA0, 0x20))?;
        let gc_text = text_gc(&conn, window, rgb(depth, 0x2A, 0x26, 0x20))?;
        let mut badge = Self {
            conn,
            window,
            gc_bg,
            gc_draw,
            gc_text,
            width,
            height,
            label,
            is_dir,
            stacked,
            depth,
        };
        badge.paint();
        Some(badge)
    }

    fn place(&mut self, x: i32, y: i32) {
        let (x, y) = offset(x, y);
        let _ = self.conn.configure_window(
            self.window,
            &ConfigureWindowAux::new()
                .x(x)
                .y(y)
                .stack_mode(StackMode::ABOVE),
        );
        self.paint();
    }

    fn paint(&mut self) {
        let _ = self.conn.poly_fill_rectangle(
            self.window,
            self.gc_bg,
            &[Rectangle {
                x: 0,
                y: 0,
                width: self.width,
                height: self.height,
            }],
        );
        if self.stacked {
            paint_file(&self.conn, self.window, self.gc_draw, self.depth, 10, 8);
        }
        if self.is_dir && !self.stacked {
            paint_folder(&self.conn, self.window, self.gc_draw, self.depth);
        } else {
            paint_file(&self.conn, self.window, self.gc_draw, self.depth, 6, 6);
        }
        if !self.label.is_empty() {
            let _ = self
                .conn
                .image_text8(self.window, self.gc_text, 34, 25, &self.label);
        }
        let _ = self.conn.flush();
    }

    fn close(&mut self) {
        let _ = self.conn.destroy_window(self.window);
        let _ = self.conn.flush();
    }
}

fn offset(x: i32, y: i32) -> (i32, i32) {
    (
        x.saturating_add(14).max(0),
        y.saturating_add(18).max(0),
    )
}

fn gc(conn: &RustConnection, drawable: u32, pixel: u32) -> Option<u32> {
    let id = conn.generate_id().ok()?;
    conn.create_gc(id, drawable, &CreateGCAux::new().foreground(pixel))
        .ok()?;
    Some(id)
}

fn text_gc(conn: &RustConnection, drawable: u32, pixel: u32) -> Option<u32> {
    let font = conn.generate_id().ok()?;
    if conn.open_font(font, b"fixed").is_err() {
        return gc(conn, drawable, pixel);
    }
    let id = conn.generate_id().ok()?;
    conn.create_gc(
        id,
        drawable,
        &CreateGCAux::new().foreground(pixel).font(font),
    )
    .ok()?;
    Some(id)
}

fn paint_folder(conn: &RustConnection, window: u32, gc: u32, depth: u8) {
    let _ = conn.change_gc(gc, &ChangeGCAux::new().foreground(rgb(depth, 0xC4, 0x86, 0x12)));
    let _ = conn.poly_fill_rectangle(
        window,
        gc,
        &[
            Rectangle {
                x: 6,
                y: 12,
                width: 12,
                height: 6,
            },
            Rectangle {
                x: 6,
                y: 16,
                width: 22,
                height: 16,
            },
        ],
    );
}

fn paint_file(conn: &RustConnection, window: u32, gc: u32, depth: u8, x: i16, y: i16) {
    let _ = conn.change_gc(gc, &ChangeGCAux::new().foreground(rgb(depth, 0xFF, 0xFF, 0xFF)));
    let _ = conn.poly_fill_rectangle(
        window,
        gc,
        &[Rectangle {
            x,
            y,
            width: 16,
            height: 22,
        }],
    );
    let _ = conn.change_gc(gc, &ChangeGCAux::new().foreground(rgb(depth, 0x3D, 0x7E, 0xC9)));
    let _ = conn.poly_fill_rectangle(
        window,
        gc,
        &[Rectangle {
            x,
            y,
            width: 16,
            height: 5,
        }],
    );
}

fn mark_as_drag(conn: &RustConnection, window: u32) {
    let Ok(kind) = conn.intern_atom(false, b"_NET_WM_WINDOW_TYPE") else {
        return;
    };
    let Ok(dnd) = conn.intern_atom(false, b"_NET_WM_WINDOW_TYPE_DND") else {
        return;
    };
    let Ok(above) = conn.intern_atom(false, b"_NET_WM_STATE_ABOVE") else {
        return;
    };
    let Ok(state) = conn.intern_atom(false, b"_NET_WM_STATE") else {
        return;
    };
    let Ok(kind) = kind.reply() else { return };
    let Ok(dnd) = dnd.reply() else { return };
    let Ok(above) = above.reply() else { return };
    let Ok(state) = state.reply() else { return };
    let _ = conn.change_property32(PropMode::REPLACE, window, kind.atom, AtomEnum::ATOM, &[dnd.atom]);
    let _ = conn.change_property32(
        PropMode::REPLACE,
        window,
        state.atom,
        AtomEnum::ATOM,
        &[above.atom],
    );
    let _ = conn.change_window_attributes(
        window,
        &ChangeWindowAttributesAux::new().override_redirect(1),
    );
}

fn rgb(depth: u8, r: u32, g: u32, b: u32) -> u32 {
    if depth >= 24 {
        (r << 16) | (g << 8) | b
    } else {
        0
    }
}

fn latin1(text: &str) -> Vec<u8> {
    let bytes: Vec<u8> = text
        .chars()
        .take(22)
        .map(|c| {
            let u = c as u32;
            if (32..127).contains(&u) {
                u as u8
            } else {
                b'?'
            }
        })
        .collect();
    let marks = bytes.iter().filter(|b| **b == b'?').count();
    if !bytes.is_empty() && marks * 2 >= bytes.len() {
        Vec::new()
    } else {
        bytes
    }
}

impl Drop for Badge {
    fn drop(&mut self) {
        self.close();
    }
}
