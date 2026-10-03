//! Whether the foreground X11 window covers its monitor.
//! Native Wayland windows are not visible here.

use std::sync::Mutex;
use std::time::{Duration, Instant};
use x11rb::connection::Connection;
use x11rb::protocol::xproto::{AtomEnum, ConnectionExt as _};

pub(crate) fn foreground_fullscreen() -> bool {
    static CACHE: Mutex<Option<(Instant, bool)>> = Mutex::new(None);
    if let Ok(slot) = CACHE.lock() {
        if let Some((at, value)) = *slot {
            if at.elapsed() < Duration::from_millis(300) {
                return value;
            }
        }
    }
    let value = query().unwrap_or(false);
    if let Ok(mut slot) = CACHE.lock() {
        *slot = Some((Instant::now(), value));
    }
    value
}

fn query() -> Option<bool> {
    let (conn, screen) = x11rb::connect(None).ok()?;
    let root = conn.setup().roots[screen].root;
    let active = atom(&conn, b"_NET_ACTIVE_WINDOW")?;
    let state = atom(&conn, b"_NET_WM_STATE")?;
    let full = atom(&conn, b"_NET_WM_STATE_FULLSCREEN")?;
    let window = conn
        .get_property(false, root, active, AtomEnum::WINDOW, 0, 1)
        .ok()?
        .reply()
        .ok()?
        .value32()?
        .next()?;
    if window == 0 {
        return Some(false);
    }
    let states = conn
        .get_property(false, window, state, AtomEnum::ATOM, 0, 32)
        .ok()?
        .reply()
        .ok()?;
    let found = states.value32()?.any(|a| a == full);
    Some(found)
}

fn atom(conn: &x11rb::rust_connection::RustConnection, name: &[u8]) -> Option<u32> {
    let reply = conn.intern_atom(false, name).ok()?.reply().ok()?;
    Some(reply.atom)
}
