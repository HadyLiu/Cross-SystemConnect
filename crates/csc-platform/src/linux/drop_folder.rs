//! Folder under the pointer when a drag is released.
//!
//! GNOME Files publishes each window's location on the session bus. The
//! accessibility tree names the icon under the pointer, so a release on a
//! folder lands in that folder rather than the window's parent.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;
use tracing::{info, warn};

const BUS_WAIT: Duration = Duration::from_millis(250);

pub(crate) fn folder_at(x: i32, y: i32) -> Option<PathBuf> {
    let desktop = desktop_dir();
    let windows = nautilus_windows();
    let hit = accessibility_at(x, y).unwrap_or_default();
    let chosen = decide(&hit, &windows, &desktop);
    info!(
        "drop at ({x},{y}) app={} names={:?} -> {}",
        hit.app,
        hit.names,
        chosen
            .as_ref()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "(none)".into())
    );
    chosen
}

#[derive(Debug, Default)]
pub(crate) struct Hit {
    /// Accessible names from the object under the pointer up to the window.
    pub names: Vec<String>,
    pub uris: Vec<String>,
    pub app: String,
}

fn accessibility_at(x: i32, y: i32) -> Option<Hit> {
    let session = dbus::blocking::Connection::new_session().ok()?;
    let bus = session.with_proxy("org.a11y.Bus", "/org/a11y/bus", BUS_WAIT);
    let (addr,): (String,) = bus
        .method_call("org.a11y.Bus", "GetAddress", ())
        .map_err(|e| warn!("accessibility bus: {e}"))
        .ok()?;
    let a11y = dbus::blocking::Connection::new_address(&addr)
        .map_err(|e| warn!("accessibility connect: {e}"))
        .ok()?;
    let root = a11y.with_proxy(
        "org.a11y.atspi.Registry",
        "/org/a11y/atspi/accessible/root",
        BUS_WAIT,
    );
    let (sx, sy) = scale_point(&root, x, y);
    let ((dest, path),): ((String, dbus::Path),) = root
        .method_call(
            "org.a11y.atspi.Component",
            "GetAccessibleAtPoint",
            (sx, sy, 0u32),
        )
        .map_err(|e| warn!("accessible at point: {e}"))
        .ok()?;
    if dest.is_empty() || path_str(&path) == "/org/a11y/atspi/null" {
        return None;
    }
    let mut hit = Hit::default();
    let mut dest = dest;
    let mut path = path;
    for _ in 0..16 {
        let shown = path_str(&path);
        if shown == "/org/a11y/atspi/null" || shown.ends_with("/root") {
            break;
        }
        let node = a11y.with_proxy(&dest, &path, BUS_WAIT);
        if hit.app.is_empty() {
            if let Ok(((adest, apath),)) =
                node.method_call("org.a11y.atspi.Accessible", "GetApplication", ())
                    as Result<((String, dbus::Path),), dbus::Error>
            {
                let app = a11y.with_proxy(&adest, &apath, BUS_WAIT);
                if let Ok((name,)) =
                    app.method_call("org.a11y.atspi.Accessible", "GetName", ())
                        as Result<(String,), dbus::Error>
                {
                    hit.app = name;
                }
            }
        }
        if let Ok((name,)) = node.method_call("org.a11y.atspi.Accessible", "GetName", ())
            as Result<(String,), dbus::Error>
        {
            let name = name.trim().to_string();
            if !name.is_empty() && hit.names.last().is_none_or(|p| p != &name) {
                hit.names.push(name);
            }
        }
        if let Ok((desc,)) = node.method_call("org.a11y.atspi.Accessible", "GetDescription", ())
            as Result<(String,), dbus::Error>
        {
            remember_uri(&mut hit.uris, desc.trim());
        }
        if let Ok((attrs,)) =
            node.method_call("org.a11y.atspi.Accessible", "GetAttributes", ())
                as Result<(HashMap<String, String>,), dbus::Error>
        {
            for (k, v) in attrs {
                if matches!(
                    k.as_str(),
                    "uri" | "URI" | "url" | "URL" | "docurl" | "DocURL"
                ) {
                    remember_uri(&mut hit.uris, v.trim());
                }
            }
        }
        let Ok(((parent_dest, parent_path),)) =
            node.method_call("org.a11y.atspi.Accessible", "GetParent", ())
                as Result<((String, dbus::Path),), dbus::Error>
        else {
            break;
        };
        if parent_dest.is_empty() {
            break;
        }
        dest = parent_dest;
        path = parent_path;
    }
    Some(hit)
}

fn path_str(path: &dbus::Path<'_>) -> String {
    path.to_string()
}

fn remember_uri(uris: &mut Vec<String>, value: &str) {
    if value.starts_with("file://") || value.starts_with('/') {
        if !uris.iter().any(|u| u == value) {
            uris.push(value.to_string());
        }
    }
}

fn scale_point(root: &dbus::blocking::Proxy<&dbus::blocking::Connection>, x: i32, y: i32) -> (i32, i32) {
    let extents: (i32, i32, i32, i32) = match root.method_call(
        "org.a11y.atspi.Component",
        "GetExtents",
        (0u32,),
    ) {
        Ok(v) => v,
        Err(_) => return (x, y),
    };
    let (_, _, aw, ah) = extents;
    let Some((sw, sh)) = super::xdo::screen_size() else {
        return (x, y);
    };
    if aw <= 0 || ah <= 0 || sw <= 0 || sh <= 0 {
        return (x, y);
    }
    // Accessibility coordinates follow the desktop scale. Our pointer is in
    // the same pixels XWayland reports for the screen.
    let sx = (i64::from(x) * i64::from(aw) / i64::from(sw)) as i32;
    let sy = (i64::from(y) * i64::from(ah) / i64::from(sh)) as i32;
    (sx, sy)
}

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
    let map: HashMap<String, Vec<String>> = match proxy.get(
        "org.freedesktop.FileManager1",
        "OpenWindowsWithLocations",
    ) {
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
        let Ok(((dest, path),)) = node.method_call(
            "org.a11y.atspi.Accessible",
            "GetChildAtIndex",
            (index,),
        ) as Result<((String, dbus::Path<'static>),), dbus::Error>
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

fn accessible_name(
    bus: &dbus::blocking::Connection,
    dest: &str,
    path: &dbus::Path<'_>,
) -> String {
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
            if let Ok(v) = u8::from_str_radix(std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or(""), 16)
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

pub(crate) fn decide(hit: &Hit, windows: &[PathBuf], desktop: &Path) -> Option<PathBuf> {
    if let Some(dir) = uri_target(&hit.uris) {
        return Some(refine(&dir, &hit.names));
    }
    let on_files = is_files(&hit.app);
    let on_desktop = is_desktop(&hit.app, &hit.names);
    let base = if on_files {
        match_window(&hit.names, windows).or_else(|| single(windows))
    } else if on_desktop {
        Some(desktop.to_path_buf())
    } else {
        match_window(&hit.names, windows)
    }?;
    Some(refine(&base, &hit.names))
}

fn uri_target(uris: &[String]) -> Option<PathBuf> {
    let mut best: Option<PathBuf> = None;
    for uri in uris {
        let path = if uri.starts_with("file://") {
            let Some(path) = file_uri_to_path(uri) else {
                continue;
            };
            path
        } else if uri.starts_with('/') {
            PathBuf::from(uri)
        } else {
            continue;
        };
        if path.is_dir() {
            let replace = best.as_ref().is_none_or(|b| path.components().count() > b.components().count());
            if replace {
                best = Some(path);
            }
        } else if path.is_file() {
            if let Some(parent) = path.parent() {
                if parent.is_dir() && best.is_none() {
                    best = Some(parent.to_path_buf());
                }
            }
        }
    }
    best
}

fn single(windows: &[PathBuf]) -> Option<PathBuf> {
    if windows.len() == 1 {
        Some(windows[0].clone())
    } else {
        None
    }
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
        || names.iter().any(|n| n.eq_ignore_ascii_case("desktop") || n == "桌面")
}

pub(crate) fn match_window(names: &[String], windows: &[PathBuf]) -> Option<PathBuf> {
    let mut best: Option<(usize, &Path)> = None;
    for window in windows {
        let Some(base) = window.file_name().and_then(|s| s.to_str()) else {
            continue;
        };
        for (i, name) in names.iter().enumerate() {
            if name == base || name.eq_ignore_ascii_case(base) {
                if best.as_ref().is_none_or(|(score, _)| i >= *score) {
                    best = Some((i, window.as_path()));
                }
            }
        }
    }
    best.map(|(_, p)| p.to_path_buf())
}

pub(crate) fn refine(base: &Path, names: &[String]) -> PathBuf {
    let title = base
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let mut dir = base.to_path_buf();
    for name in names {
        if !title.is_empty() && name == &title {
            break;
        }
        if !valid_child(name) {
            continue;
        }
        let child = dir.join(name);
        if child.is_dir() {
            dir = child;
        } else if child.is_file() {
            break;
        }
    }
    dir
}

fn valid_child(name: &str) -> bool {
    !name.is_empty()
        && name != "."
        && name != ".."
        && !name.contains('/')
        && !name.contains('\\')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_file_manager_window_is_the_paste_folder() {
        let only = PathBuf::from("/home/hady/Projects");
        assert_eq!(pick_window(&[only.clone()], ""), Some(only));
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

    #[test]
    fn release_on_a_child_folder_uses_that_folder() {
        let root = std::env::temp_dir().join(format!("csc-drop-{}", std::process::id()));
        let docs = root.join("Documents");
        let projects = docs.join("Projects");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&projects).unwrap();
        std::fs::write(docs.join("notes.txt"), b"x").unwrap();
        let hit = Hit {
            names: vec!["Projects".into(), "Documents".into(), "Files".into()],
            uris: Vec::new(),
            app: "org.gnome.Nautilus".into(),
        };
        let chosen = decide(&hit, &[docs.clone()], &root.join("Desktop")).unwrap();
        assert_eq!(chosen, projects);

        let hit = Hit {
            names: vec!["notes.txt".into(), "Documents".into()],
            uris: Vec::new(),
            app: "Files".into(),
        };
        let chosen = decide(&hit, &[docs.clone()], &root.join("Desktop")).unwrap();
        assert_eq!(chosen, docs);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn desktop_release_uses_the_desktop_directory() {
        let desktop = std::env::temp_dir().join(format!("csc-desk-{}", std::process::id()));
        let icon = desktop.join("Inbox");
        let _ = std::fs::remove_dir_all(&desktop);
        std::fs::create_dir_all(&icon).unwrap();
        let hit = Hit {
            names: vec!["Inbox".into(), "Desktop".into()],
            uris: Vec::new(),
            app: "gnome-shell".into(),
        };
        let chosen = decide(&hit, &[], &desktop).unwrap();
        assert_eq!(chosen, icon);
        let _ = std::fs::remove_dir_all(&desktop);
    }
}
