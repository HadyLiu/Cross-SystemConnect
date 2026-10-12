//! Own the X11 CLIPBOARD so file managers see both `text/uri-list` and
//! `x-special/gnome-copied-files` (Doc §4.4). arboard alone only sets uri-list,
//! which GNOME Desktop Icons / Nautilus treat as a no-op for paste.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::OnceLock;
use std::time::{Duration, Instant};
use tracing::{debug, warn};
use x11rb::connection::Connection;
use x11rb::protocol::xproto::{
    AtomEnum, ConnectionExt as _, CreateWindowAux, EventMask, PropMode, SelectionNotifyEvent,
    WindowClass, SELECTION_NOTIFY_EVENT,
};
use x11rb::protocol::Event;
use x11rb::rust_connection::RustConnection;
use x11rb::wrapper::ConnectionExt as _;

const GNOME_COPIED: &str = "x-special/gnome-copied-files";
const URI_LIST: &str = "text/uri-list";

enum Cmd {
    SetFiles(Vec<PathBuf>),
}

struct Atoms {
    clipboard: u32,
    targets: u32,
    uri_list: u32,
    gnome: u32,
    utf8: u32,
}

impl Atoms {
    fn new(c: &RustConnection) -> Result<Self, String> {
        let get = |name: &str| -> Result<u32, String> {
            Ok(c.intern_atom(false, name.as_bytes())
                .map_err(|e| e.to_string())?
                .reply()
                .map_err(|e| e.to_string())?
                .atom)
        };
        Ok(Self {
            clipboard: get("CLIPBOARD")?,
            targets: get("TARGETS")?,
            uri_list: get(URI_LIST)?,
            gnome: get(GNOME_COPIED)?,
            utf8: get("UTF8_STRING")?,
        })
    }
}

static TX: OnceLock<Sender<Cmd>> = OnceLock::new();
static ALIVE: AtomicBool = AtomicBool::new(false);

fn sender() -> Option<&'static Sender<Cmd>> {
    if let Some(tx) = TX.get() {
        return Some(tx);
    }
    let (tx, rx) = mpsc::channel();
    if std::thread::Builder::new()
        .name("csc-clip-x11".into())
        .spawn(move || worker(rx))
        .is_err()
    {
        return None;
    }
    for _ in 0..50 {
        if ALIVE.load(Ordering::Acquire) {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let _ = TX.set(tx);
    TX.get()
}

fn worker(rx: Receiver<Cmd>) {
    let Ok((conn, screen)) = x11rb::connect(None) else {
        warn!("clipboard: no X11 display");
        return;
    };
    let Ok(atoms) = Atoms::new(&conn) else {
        warn!("clipboard: intern atoms failed");
        return;
    };
    let root = conn.setup().roots[screen].root;
    let win = match conn.generate_id() {
        Ok(id) => id,
        Err(e) => {
            warn!("clipboard: generate_id: {e}");
            return;
        }
    };
    if let Err(e) = conn.create_window(
        x11rb::COPY_DEPTH_FROM_PARENT,
        win,
        root,
        0,
        0,
        1,
        1,
        0,
        WindowClass::INPUT_ONLY,
        x11rb::COPY_FROM_PARENT,
        &CreateWindowAux::new().event_mask(EventMask::PROPERTY_CHANGE),
    ) {
        warn!("clipboard: create_window: {e}");
        return;
    }
    let _ = conn.flush();
    ALIVE.store(true, Ordering::Release);

    let mut uri_bytes = Vec::new();
    let mut gnome_bytes = Vec::new();
    let mut own = false;

    loop {
        while let Ok(cmd) = rx.try_recv() {
            match cmd {
                Cmd::SetFiles(paths) => {
                    let uris: Vec<String> = paths.iter().map(|p| file_uri(p)).collect();
                    uri_bytes = uris
                        .iter()
                        .flat_map(|u| format!("{u}\r\n").into_bytes())
                        .collect();
                    let mut gnome = String::from("copy\n");
                    gnome.push_str(&uris.join("\n"));
                    gnome_bytes = gnome.into_bytes();
                    if let Err(e) = conn.set_selection_owner(win, atoms.clipboard, x11rb::CURRENT_TIME)
                    {
                        warn!("clipboard: set_selection_owner: {e}");
                        continue;
                    }
                    let _ = conn.flush();
                    own = conn
                        .get_selection_owner(atoms.clipboard)
                        .ok()
                        .and_then(|c| c.reply().ok())
                        .is_some_and(|r| r.owner == win);
                    if own {
                        debug!(
                            "clipboard: offering {} file(s) as uri-list + gnome-copied-files",
                            paths.len()
                        );
                    } else {
                        warn!("clipboard: failed to own CLIPBOARD");
                    }
                }
            }
        }

        match conn.poll_for_event() {
            Ok(Some(Event::SelectionRequest(r))) if own && r.owner == win => {
                let property = if r.property == x11rb::NONE {
                    r.target
                } else {
                    r.property
                };
                let ok = if r.target == atoms.targets {
                    let list = [atoms.targets, atoms.uri_list, atoms.gnome, atoms.utf8];
                    conn.change_property32(
                        PropMode::REPLACE,
                        r.requestor,
                        property,
                        AtomEnum::ATOM,
                        &list,
                    )
                    .is_ok()
                } else if r.target == atoms.uri_list {
                    conn.change_property8(
                        PropMode::REPLACE,
                        r.requestor,
                        property,
                        atoms.uri_list,
                        &uri_bytes,
                    )
                    .is_ok()
                } else if r.target == atoms.gnome {
                    conn.change_property8(
                        PropMode::REPLACE,
                        r.requestor,
                        property,
                        atoms.gnome,
                        &gnome_bytes,
                    )
                    .is_ok()
                } else {
                    false
                };
                let reply = SelectionNotifyEvent {
                    response_type: SELECTION_NOTIFY_EVENT,
                    sequence: 0,
                    time: r.time,
                    requestor: r.requestor,
                    selection: r.selection,
                    target: r.target,
                    property: if ok { property } else { x11rb::NONE },
                };
                let _ = conn.send_event(false, r.requestor, EventMask::NO_EVENT, reply);
                let _ = conn.flush();
            }
            Ok(Some(Event::SelectionClear(c))) if c.selection == atoms.clipboard => {
                own = false;
            }
            Ok(Some(_)) => {}
            Ok(None) => {
                // Block for the next clipboard command, waking often for SelectionRequest.
                match rx.recv_timeout(Duration::from_millis(20)) {
                    Ok(cmd) => {
                        // Re-queue via handling on next loop iteration.
                        match cmd {
                            Cmd::SetFiles(paths) => {
                                let uris: Vec<String> = paths.iter().map(|p| file_uri(p)).collect();
                                uri_bytes = uris
                                    .iter()
                                    .flat_map(|u| format!("{u}\r\n").into_bytes())
                                    .collect();
                                let mut gnome = String::from("copy\n");
                                gnome.push_str(&uris.join("\n"));
                                gnome_bytes = gnome.into_bytes();
                                if conn
                                    .set_selection_owner(win, atoms.clipboard, x11rb::CURRENT_TIME)
                                    .is_ok()
                                {
                                    let _ = conn.flush();
                                    own = conn
                                        .get_selection_owner(atoms.clipboard)
                                        .ok()
                                        .and_then(|c| c.reply().ok())
                                        .is_some_and(|r| r.owner == win);
                                    if own {
                                        debug!(
                                            "clipboard: offering {} file(s) as uri-list + gnome-copied-files",
                                            paths.len()
                                        );
                                    }
                                }
                            }
                        }
                    }
                    Err(mpsc::RecvTimeoutError::Timeout) => {}
                    Err(mpsc::RecvTimeoutError::Disconnected) => return,
                }
            }
            Err(_) => return,
        }
    }
}

/// Advertise placeholders with both GNOME and uri-list MIME types.
pub fn set_files(paths: &[PathBuf]) -> bool {
    if paths.is_empty() {
        return false;
    }
    let Some(tx) = sender() else {
        return false;
    };
    tx.send(Cmd::SetFiles(paths.to_vec())).is_ok()
}

/// Read `x-special/gnome-copied-files` from the current CLIPBOARD owner.
pub fn read_gnome_files() -> Option<Vec<PathBuf>> {
    let (conn, _) = x11rb::connect(None).ok()?;
    let atoms = Atoms::new(&conn).ok()?;
    let root = conn.setup().roots.first()?.root;
    let win = conn.generate_id().ok()?;
    conn.create_window(
        x11rb::COPY_DEPTH_FROM_PARENT,
        win,
        root,
        0,
        0,
        1,
        1,
        0,
        WindowClass::INPUT_ONLY,
        x11rb::COPY_FROM_PARENT,
        &CreateWindowAux::new().event_mask(EventMask::PROPERTY_CHANGE),
    )
    .ok()?;
    let prop = conn
        .intern_atom(false, b"CSC_GNOME_CLIP")
        .ok()?
        .reply()
        .ok()?
        .atom;
    conn.convert_selection(
        win,
        atoms.clipboard,
        atoms.gnome,
        prop,
        x11rb::CURRENT_TIME,
    )
    .ok()?;
    conn.flush().ok()?;
    let deadline = Instant::now() + Duration::from_millis(400);
    while Instant::now() < deadline {
        match conn.poll_for_event() {
            Ok(Some(Event::SelectionNotify(n))) if n.requestor == win => {
                if n.property == x11rb::NONE {
                    return None;
                }
                let reply = conn
                    .get_property(false, win, prop, AtomEnum::ANY, 0, 64 * 1024)
                    .ok()?
                    .reply()
                    .ok()?;
                if reply.type_ == x11rb::NONE {
                    return None;
                }
                let text = String::from_utf8_lossy(&reply.value);
                return parse_gnome_copied(&text);
            }
            Ok(Some(_)) => {}
            Ok(None) => std::thread::sleep(Duration::from_millis(10)),
            Err(_) => return None,
        }
    }
    None
}

fn parse_gnome_copied(text: &str) -> Option<Vec<PathBuf>> {
    let mut lines = text.lines();
    let action = lines.next()?.trim();
    if action != "copy" && action != "cut" {
        return None;
    }
    let files: Vec<PathBuf> = lines
        .filter_map(|line| {
            let line = line.trim();
            if line.is_empty() {
                return None;
            }
            path_of_uri(line).or_else(|| Some(PathBuf::from(line)))
        })
        .filter(|p| !p.as_os_str().is_empty())
        .collect();
    if files.is_empty() {
        None
    } else {
        Some(files)
    }
}

fn file_uri(path: &Path) -> String {
    super::paste_hold::file_uri(path)
}

fn path_of_uri(uri: &str) -> Option<PathBuf> {
    use std::os::unix::ffi::OsStringExt;
    let rest = uri
        .trim_end_matches(['\0', '\r', '\n'])
        .strip_prefix("file://")?;
    let path = &rest[rest.find('/')?..];
    let bytes = path.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let decoded = (|| -> Option<u8> {
                let a = (bytes[i + 1] as char).to_digit(16)? as u8;
                let b = (bytes[i + 2] as char).to_digit(16)? as u8;
                Some(a * 16 + b)
            })();
            if let Some(v) = decoded {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    Some(PathBuf::from(std::ffi::OsString::from_vec(out)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gnome_copy_lines_parse() {
        let text = "copy\nfile:///tmp/a.txt\nfile:///tmp/b%20c.txt\n";
        let files = parse_gnome_copied(text).unwrap();
        assert_eq!(files.len(), 2);
        assert_eq!(files[0], PathBuf::from("/tmp/a.txt"));
        assert_eq!(files[1], PathBuf::from("/tmp/b c.txt"));
    }
}
