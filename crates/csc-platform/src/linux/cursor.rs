//! Hide the X11 cursor while this computer is controlling another one.

use x11rb::connection::Connection;
use x11rb::protocol::xfixes::ConnectionExt as _;

pub(crate) fn set_hidden(hidden: bool) {
    if super::wayland_session() {
        return;
    }
    let Ok((conn, screen)) = x11rb::connect(None) else {
        return;
    };
    let root = conn.setup().roots[screen].root;
    if conn.xfixes_query_version(5, 0).is_err() {
        return;
    }
    let result = if hidden {
        conn.xfixes_hide_cursor(root)
    } else {
        conn.xfixes_show_cursor(root)
    };
    if result.is_ok() {
        let _ = conn.flush();
    }
}
