//! Folder for a clipboard file paste: the focused file manager or the desktop.
//!
//! GNOME Files publishes each window's location on the session bus. Accessibility
//! names the focused window so paste lands in that folder.

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;
use tracing::{info, warn};

const BUS_WAIT: Duration = Duration::from_millis(250);

fn nautilus_windows() -> Vec<PathBuf> {
    for (dest, path) in [
        (
            "org.freedesktop.FileManager1",
            "/org/freedesktop/FileManager1",
        ),
        ("org.gnome.Nautilus", "/org/gnome/Nautilus"),
    ] {
        let found = windows_of(dest, path);
        if !found.is_empty() {
            return found;
        }
    }
    Vec::new()
}

fn windows_of(dest: &str, path: &str) -> Vec<PathBuf> {
    let Ok(conn) = dbus::blocking::Connection::new_session() else {
        return Vec::new();
    };
    let proxy = conn.with_proxy(dest, path, BUS_WAIT);
    use dbus::blocking::stdintf::org_freedesktop_dbus::Properties;
    let map: HashMap<String, Vec<String>> =
        match proxy.get("org.freedesktop.FileManager1", "OpenWindowsWithLocations") {
            Ok(v) => v,
            Err(e) => {
                warn!("file manager windows ({dest}): {e}");
                return Vec::new();
            }
        };
    map.into_values()
        .filter_map(|uris| uris.into_iter().next())
        .filter_map(|uri| file_uri_to_path(&uri))
        .collect()
}

/// Directory a file paste should use: the focused file manager's folder, or
/// the desktop. `None` when the focus is not a file target (the caller then
/// uses the fallback directory).
pub(crate) fn paste_folder() -> Option<PathBuf> {
    let focus = focused_accessible();
    let app = focus.as_ref().map(|f| f.app.as_str()).unwrap_or("");
    let name = focus.as_ref().map(|f| f.name.as_str()).unwrap_or("");
    let names = vec![name.to_string()];
    if !app.is_empty() && is_desktop(app, &names) && !is_files(app) {
        let dir = desktop_dir();
        info!("paste target: desktop {}", dir.display());
        return Some(dir);
    }
    if !app.is_empty() && !is_files(app) {
        info!("paste target: {app} is not a file manager");
        return None;
    }
    let windows = nautilus_windows();
    let chosen = pick_window(&windows, name);
    if let Some(dir) = &chosen {
        info!("paste target: {}", dir.display());
    }
    chosen
}

struct FocusHit {
    app: String,
    name: String,
}

fn focused_accessible() -> Option<FocusHit> {
    let session = dbus::blocking::Connection::new_session().ok()?;
    let bus = session.with_proxy("org.a11y.Bus", "/org/a11y/bus", BUS_WAIT);
    let (addr,): (String,) = bus.method_call("org.a11y.Bus", "GetAddress", ()).ok()?;
    let a11y = dbus::blocking::Connection::new_address(&addr).ok()?;
    let root = a11y.with_proxy(
        "org.a11y.atspi.Registry",
        "/org/a11y/atspi/accessible/root",
        BUS_WAIT,
    );
    let mut seen = 0usize;
    for (dest, path) in children(&root) {
        let app = accessible_name(&a11y, &dest, &path);
        let mut queue = vec![(dest, path, 0u8)];
        while let Some((dest, path, depth)) = queue.pop() {
            seen += 1;
            if seen > 160 {
                return None;
            }
            let node = a11y.with_proxy(&dest, &path, Duration::from_millis(80));
            if is_focused(&node) {
                return Some(FocusHit {
                    app,
                    name: proxy_name(&node),
                });
            }
            if depth < 5 {
                for (d, p) in children(&node) {
                    queue.push((d, p, depth + 1));
                }
            }
        }
    }
    None
}

fn children(
    node: &dbus::blocking::Proxy<&dbus::blocking::Connection>,
) -> Vec<(String, dbus::Path<'static>)> {
    if let Ok((kids,)) = node.method_call("org.a11y.atspi.Accessible", "GetChildren", ())
        as Result<(Vec<(String, dbus::Path<'static>)>,), dbus::Error>
    {
        return kids;
    }
    let mut out = Vec::new();
    for index in 0..32 {
        let Ok(((dest, path),)) =
            node.method_call("org.a11y.atspi.Accessible", "GetChildAtIndex", (index,))
                as Result<((String, dbus::Path<'static>),), dbus::Error>
        else {
            break;
        };
        if dest.is_empty() {
            break;
        }
        out.push((dest, path));
    }
    out
}

fn is_focused(node: &dbus::blocking::Proxy<&dbus::blocking::Connection>) -> bool {
    let Ok((states,)) = node.method_call("org.a11y.atspi.Accessible", "GetState", ())
        as Result<(Vec<u32>,), dbus::Error>
    else {
        return false;
    };
    // AT-SPI FOCUSED is bit 12.
    states.first().is_some_and(|word| word & (1 << 12) != 0)
}

fn accessible_name(bus: &dbus::blocking::Connection, dest: &str, path: &dbus::Path<'_>) -> String {
    let node = bus.with_proxy(dest, path, Duration::from_millis(80));
    proxy_name(&node)
}

fn proxy_name(node: &dbus::blocking::Proxy<&dbus::blocking::Connection>) -> String {
    node.method_call("org.a11y.atspi.Accessible", "GetName", ())
        .map(|(name,): (String,)| name)
        .unwrap_or_default()
}

fn pick_window(windows: &[PathBuf], focused_name: &str) -> Option<PathBuf> {
    if windows.len() == 1 {
        return Some(windows[0].clone());
    }
    let needle = focused_name.trim();
    if needle.is_empty() {
        return None;
    }
    windows
        .iter()
        .find(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n == needle || needle.contains(n) || n.contains(needle))
        })
        .cloned()
}

fn desktop_dir() -> PathBuf {
    user_dir("XDG_DESKTOP_DIR").unwrap_or_else(|| home().join("Desktop"))
}

fn home() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/tmp"))
}

fn user_dir(key: &str) -> Option<PathBuf> {
    let path = home().join(".config/user-dirs.dirs");
    let text = std::fs::read_to_string(path).ok()?;
    for line in text.lines() {
        let line = line.trim();
        let rest = line.strip_prefix(key)?.trim().strip_prefix('=')?.trim();
        let rest = rest.trim_matches('"');
        let expanded = rest.replace("$HOME", &home().to_string_lossy());
        if !expanded.is_empty() {
            return Some(PathBuf::from(expanded));
        }
    }
    None
}

pub(crate) fn file_uri_to_path(uri: &str) -> Option<PathBuf> {
    let rest = uri.trim().strip_prefix("file://")?;
    let rest = rest.split(['?', '#']).next().unwrap_or(rest);
    let path = rest
        .strip_prefix("localhost")
        .filter(|p| p.starts_with('/'))
        .unwrap_or(rest);
    if !path.starts_with('/') {
        return None;
    }
    let decoded = percent_decode(path);
    if decoded.is_empty() {
        None
    } else {
        Some(PathBuf::from(decoded))
    }
}

fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(v) =
                u8::from_str_radix(std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or(""), 16)
            {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn is_files(app: &str) -> bool {
    let app = app.to_lowercase();
    app.contains("nautilus")
        || app.contains("files")
        || app.contains("文件")
        || app == "org.gnome.nautilus"
}

fn is_desktop(app: &str, names: &[String]) -> bool {
    let app = app.to_lowercase();
    app.contains("gnome-shell")
        || app.contains("mutter")
        || app.contains("ding")
        || app.contains("desktop icons")
        || names
            .iter()
            .any(|n| n.eq_ignore_ascii_case("desktop") || n == "桌面")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_file_manager_window_is_the_paste_folder() {
        let only = PathBuf::from("/home/hady/Projects");
        assert_eq!(pick_window(std::slice::from_ref(&only), ""), Some(only));
        assert!(is_files("org.gnome.Nautilus"));
        assert!(is_files("文件"));
        assert!(!is_files("gnome-terminal"));
    }

    #[test]
    fn file_uri_decodes_spaces() {
        assert_eq!(
            file_uri_to_path("file:///home/hady/My%20Files"),
            Some(PathBuf::from("/home/hady/My Files"))
        );
        assert_eq!(
            file_uri_to_path("file://localhost/tmp/a"),
            Some(PathBuf::from("/tmp/a"))
        );
        assert!(file_uri_to_path("https://example.com").is_none());
    }
}
