//! X11 drag source for files that are still on another computer.
//!
//! The drag offers two ways to receive:
//! - XDS (`XdndDirectSave0`): the file manager names the folder it was dropped
//!   into and the core writes there.
//! - `text/uri-list` of placeholder files with the real names. The file manager
//!   copies them; the copies are found by their marker content and then filled.

use crate::vdrag::{staging_root, DragDesc, DragEvent, DragItem};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::Sender;
use std::sync::OnceLock;
use std::time::{Duration, Instant, SystemTime};
use tracing::{debug, info, warn};
use x11rb::connection::Connection;
use x11rb::protocol::xproto::{
    AtomEnum, ClientMessageEvent, ConnectionExt as _, CreateWindowAux, EventMask, GrabMode,
    GrabStatus, PropMode, SelectionNotifyEvent, Window, WindowClass, SELECTION_NOTIFY_EVENT,
};
use x11rb::protocol::Event;
use x11rb::rust_connection::RustConnection;
use x11rb::wrapper::ConnectionExt as _;

/// Placeholder marker inside a dragged folder (same name as the core uses).
const FOLDER_MARKER: &str = ".csc-drag";
const XDND_VERSION: u32 = 5;
/// How long file managers get to copy the placeholders after the drop.
const FIND_TIMEOUT: Duration = Duration::from_secs(20);
/// Drop without an answer from the target.
const DROP_TIMEOUT: Duration = Duration::from_secs(30);

static CANCEL: AtomicU64 = AtomicU64::new(0);

pub fn supported() -> bool {
    static OK: OnceLock<bool> = OnceLock::new();
    *OK.get_or_init(|| {
        std::env::var_os("DISPLAY").is_some_and(|d| !d.is_empty())
            && super::xdo::screen_size().is_some()
    })
}

pub fn cancel(id: u64) {
    CANCEL.store(id, Ordering::Release);
}

pub fn start(desc: DragDesc, events: Sender<DragEvent>) -> bool {
    let dir = staging_root().join(desc.id.to_string());
    let tops = match make_placeholders(&dir, desc.id, &desc.items) {
        Ok(t) => t,
        Err(e) => {
            warn!("drag placeholders in {}: {e}", dir.display());
            return false;
        }
    };
    if tops.is_empty() {
        return false;
    }
    CANCEL.store(0, Ordering::Release);
    let (ready_tx, ready_rx) = std::sync::mpsc::channel();
    let id = desc.id;
    let spawned = std::thread::Builder::new().name("csc-xdnd".into()).spawn(move || {
        let atoms = match x11rb::connect(None)
            .map_err(|e| e.to_string())
            .and_then(|(c, screen)| Atoms::new(&c).map(|a| (c, screen, a)))
        {
            Ok(v) => v,
            Err(e) => {
                warn!("drag: X11 connection: {e}");
                let _ = ready_tx.send(false);
                return;
            }
        };
        let (conn, screen, atoms) = atoms;
        let root = conn.setup().roots[screen].root;
        let mut drag = match Drag::open(conn, root, atoms, id, tops) {
            Ok(d) => d,
            Err(e) => {
                warn!("drag: {e}");
                let _ = ready_tx.send(false);
                return;
            }
        };
        let _ = ready_tx.send(true);
        let ev = drag.run().unwrap_or_else(|e| {
            warn!("drag {id}: {e}");
            DragEvent::Cancelled { id, reason: "error" }
        });
        drag.close();
        let cleanup = !matches!(ev, DragEvent::Cancelled { .. } | DragEvent::Lost { .. });
        let _ = events.send(ev);
        if cleanup {
            let _ = std::fs::remove_dir_all(&dir);
        }
    });
    spawned.is_ok() && ready_rx.recv_timeout(Duration::from_secs(3)).unwrap_or(false)
}

/// One placeholder per top-level item, named like the real one.
struct Top {
    index: usize,
    name: String,
    is_dir: bool,
    path: PathBuf,
}

fn marker(id: u64, index: usize) -> String {
    format!("{{\"csc_drag\":{id},\"index\":{index}}}")
}

fn make_placeholders(dir: &Path, id: u64, items: &[DragItem]) -> std::io::Result<Vec<Top>> {
    let _ = std::fs::remove_dir_all(dir);
    std::fs::create_dir_all(dir)?;
    let mut tops = Vec::new();
    for (index, it) in items.iter().enumerate().filter(|(_, it)| it.is_top()) {
        let name = it.name().to_string();
        if name.is_empty() || name == "." || name == ".." || name.contains('/') {
            continue;
        }
        let path = dir.join(&name);
        if it.is_directory {
            std::fs::create_dir_all(&path)?;
            std::fs::write(path.join(FOLDER_MARKER), marker(id, index))?;
        } else {
            std::fs::write(&path, marker(id, index))?;
        }
        tops.push(Top {
            index,
            name,
            is_dir: it.is_directory,
            path,
        });
    }
    Ok(tops)
}

struct Atoms {
    aware: u32,
    selection: u32,
    enter: u32,
    position: u32,
    status: u32,
    leave: u32,
    drop: u32,
    finished: u32,
    action_copy: u32,
    type_list: u32,
    direct_save: u32,
    uri_list: u32,
    text_plain: u32,
    targets: u32,
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
            aware: get("XdndAware")?,
            selection: get("XdndSelection")?,
            enter: get("XdndEnter")?,
            position: get("XdndPosition")?,
            status: get("XdndStatus")?,
            leave: get("XdndLeave")?,
            drop: get("XdndDrop")?,
            finished: get("XdndFinished")?,
            action_copy: get("XdndActionCopy")?,
            type_list: get("XdndTypeList")?,
            direct_save: get("XdndDirectSave0")?,
            uri_list: get("text/uri-list")?,
            text_plain: get("text/plain")?,
            targets: get("TARGETS")?,
        })
    }
}

#[derive(Clone, Copy)]
struct Target {
    window: Window,
    version: u32,
}

/// What the drop target used to take the files.
#[derive(Default)]
struct Taken {
    folder: Option<PathBuf>,
    uri_list: bool,
}

struct Drag {
    conn: RustConnection,
    root: Window,
    window: Window,
    atoms: Atoms,
    id: u64,
    tops: Vec<Top>,
    target: Option<Target>,
    accepted: bool,
    waiting_status: bool,
    pos: (i16, i16),
    sent_pos: Option<(i16, i16)>,
    time: u32,
    taken: Taken,
}

type Res<T> = Result<T, String>;

fn err<E: std::fmt::Display>(e: E) -> String {
    e.to_string()
}

impl Drag {
    fn open(conn: RustConnection, root: Window, atoms: Atoms, id: u64, tops: Vec<Top>) -> Res<Self> {
        let window = conn.generate_id().map_err(err)?;
        conn.create_window(
            0,
            window,
            root,
            -10,
            -10,
            1,
            1,
            0,
            WindowClass::INPUT_ONLY,
            x11rb::COPY_FROM_PARENT,
            &CreateWindowAux::new().override_redirect(1u32),
        )
        .map_err(err)?;
        conn.change_property32(
            PropMode::REPLACE,
            window,
            atoms.type_list,
            AtomEnum::ATOM,
            &[atoms.direct_save, atoms.uri_list],
        )
        .map_err(err)?;
        conn.change_property8(
            PropMode::REPLACE,
            window,
            atoms.direct_save,
            atoms.text_plain,
            tops[0].name.as_bytes(),
        )
        .map_err(err)?;
        conn.set_selection_owner(window, atoms.selection, x11rb::CURRENT_TIME)
            .map_err(err)?;
        let mut drag = Self {
            conn,
            root,
            window,
            atoms,
            id,
            tops,
            target: None,
            accepted: false,
            waiting_status: false,
            pos: (0, 0),
            sent_pos: None,
            time: x11rb::CURRENT_TIME,
            taken: Taken::default(),
        };
        drag.grab()?;
        Ok(drag)
    }

    /// Take the pointer, then press the button so the release comes to us.
    fn grab(&mut self) -> Res<()> {
        let held = self
            .conn
            .query_pointer(self.root)
            .map_err(err)?
            .reply()
            .map_err(err)?;
        if u16::from(held.mask) & 0x100 != 0 {
            super::xdo::x_button(1, false);
            std::thread::sleep(Duration::from_millis(20));
        }
        self.pos = (held.root_x, held.root_y);
        let mask = EventMask::BUTTON_PRESS | EventMask::BUTTON_RELEASE | EventMask::POINTER_MOTION;
        for _ in 0..10 {
            let status = self
                .conn
                .grab_pointer(
                    false,
                    self.root,
                    mask,
                    GrabMode::ASYNC,
                    GrabMode::ASYNC,
                    x11rb::NONE,
                    x11rb::NONE,
                    x11rb::CURRENT_TIME,
                )
                .map_err(err)?
                .reply()
                .map_err(err)?
                .status;
            if status == GrabStatus::SUCCESS {
                self.conn.flush().map_err(err)?;
                super::xdo::x_button(1, true);
                return Ok(());
            }
            std::thread::sleep(Duration::from_millis(30));
        }
        Err("pointer is grabbed by another program".into())
    }

    fn close(&mut self) {
        let _ = self.conn.ungrab_pointer(x11rb::CURRENT_TIME);
        let _ = self.conn.destroy_window(self.window);
        let _ = self.conn.flush();
    }

    fn cancelled(&self) -> bool {
        CANCEL.load(Ordering::Acquire) == self.id
    }

    fn send(&self, to: Window, kind: u32, data: [u32; 5]) -> Res<()> {
        let ev = ClientMessageEvent::new(32, to, kind, data);
        self.conn
            .send_event(false, to, EventMask::NO_EVENT, ev)
            .map_err(err)?;
        Ok(())
    }

    fn aware_version(&self, w: Window) -> Option<u32> {
        let r = self
            .conn
            .get_property(false, w, self.atoms.aware, AtomEnum::ATOM, 0, 1)
            .ok()?
            .reply()
            .ok()?;
        let v = r.value32()?.next()?;
        Some(v)
    }

    /// Deepest XDND-aware window under the point.
    fn target_at(&self, x: i16, y: i16) -> Option<Target> {
        let mut w = self.root;
        let mut found = None;
        for _ in 0..16 {
            let r = self
                .conn
                .translate_coordinates(self.root, w, x, y)
                .ok()?
                .reply()
                .ok()?;
            if r.child == x11rb::NONE || r.child == self.window {
                break;
            }
            w = r.child;
            if let Some(version) = self.aware_version(w) {
                found = Some(Target {
                    window: w,
                    version: version.min(XDND_VERSION),
                });
                break;
            }
        }
        found
    }

    fn leave(&mut self) -> Res<()> {
        if let Some(t) = self.target.take() {
            self.send(t.window, self.atoms.leave, [self.window, 0, 0, 0, 0])?;
        }
        self.accepted = false;
        self.waiting_status = false;
        self.sent_pos = None;
        Ok(())
    }

    fn track(&mut self) -> Res<()> {
        let (x, y) = self.pos;
        let now = self.target_at(x, y);
        if now.map(|t| t.window) != self.target.map(|t| t.window) {
            self.leave()?;
            if let Some(t) = now {
                self.send(
                    t.window,
                    self.atoms.enter,
                    [
                        self.window,
                        t.version << 24,
                        self.atoms.direct_save,
                        self.atoms.uri_list,
                        0,
                    ],
                )?;
                self.target = Some(t);
            }
        }
        if let Some(t) = self.target {
            if !self.waiting_status && self.sent_pos != Some((x, y)) {
                let xy = ((x as u16 as u32) << 16) | (y as u16 as u32);
                self.send(
                    t.window,
                    self.atoms.position,
                    [self.window, 0, xy, self.time, self.atoms.action_copy],
                )?;
                self.waiting_status = true;
                self.sent_pos = Some((x, y));
            }
        }
        self.conn.flush().map_err(err)
    }

    fn run(&mut self) -> Res<DragEvent> {
        let id = self.id;
        self.track()?;
        let mut dropped: Option<Instant> = None;
        loop {
            if self.cancelled() {
                self.leave()?;
                self.conn.flush().map_err(err)?;
                super::xdo::x_button(1, false);
                return Ok(DragEvent::Cancelled { id, reason: "left screen" });
            }
            if let Some(at) = dropped {
                if at.elapsed() > DROP_TIMEOUT {
                    return Ok(self.after_drop());
                }
            }
            let Some(ev) = self.conn.poll_for_event().map_err(err)? else {
                std::thread::sleep(Duration::from_millis(4));
                continue;
            };
            match ev {
                Event::MotionNotify(m) if dropped.is_none() => {
                    self.pos = (m.root_x, m.root_y);
                    self.time = m.time;
                    self.track()?;
                }
                Event::ButtonRelease(b) if dropped.is_none() && b.detail == 1 => {
                    self.pos = (b.root_x, b.root_y);
                    self.time = b.time;
                    let _ = self.conn.ungrab_pointer(b.time);
                    match self.target {
                        Some(t) if self.accepted => {
                            self.send(t.window, self.atoms.drop, [self.window, 0, b.time, 0, 0])?;
                            self.conn.flush().map_err(err)?;
                            dropped = Some(Instant::now());
                            debug!("drag {id}: dropped on window {:#x}", t.window);
                        }
                        _ => {
                            self.leave()?;
                            self.conn.flush().map_err(err)?;
                            return Ok(DragEvent::Cancelled { id, reason: "no target" });
                        }
                    }
                }
                Event::ClientMessage(c) if c.type_ == self.atoms.status => {
                    let d = c.data.as_data32();
                    if self.target.is_some_and(|t| t.window == d[0]) {
                        self.waiting_status = false;
                        self.accepted = d[1] & 1 != 0;
                        if dropped.is_none() {
                            self.track()?;
                        }
                    }
                }
                Event::ClientMessage(c) if c.type_ == self.atoms.finished => {
                    let d = c.data.as_data32();
                    let version = self.target.map(|t| t.version).unwrap_or(0);
                    if version >= 5 && d[1] & 1 == 0 && self.taken.folder.is_none() && !self.taken.uri_list {
                        return Ok(DragEvent::Cancelled { id, reason: "refused" });
                    }
                    return Ok(self.after_drop());
                }
                Event::SelectionRequest(r) => {
                    self.answer(r)?;
                    if dropped.is_some() && self.taken.folder.is_some() {
                        return Ok(self.after_drop());
                    }
                }
                _ => {}
            }
        }
    }

    fn after_drop(&mut self) -> DragEvent {
        let id = self.id;
        if let Some(dir) = self.taken.folder.take() {
            return DragEvent::Folder { id, dir };
        }
        if !self.taken.uri_list {
            return DragEvent::Lost { id };
        }
        info!("drag {id}: looking for the copied placeholders");
        match find_copies(id, &self.tops, SystemTime::now() - Duration::from_secs(60)) {
            Some(found) => {
                let tops = found
                    .into_iter()
                    .zip(&self.tops)
                    .map(|(path, top)| same_style_name(path, &top.name, top.is_dir))
                    .collect();
                DragEvent::InPlace { id, tops }
            }
            None => DragEvent::Lost { id },
        }
    }

    fn answer(&mut self, r: x11rb::protocol::xproto::SelectionRequestEvent) -> Res<()> {
        let a = &self.atoms;
        let property = if r.property == x11rb::NONE { r.target } else { r.property };
        let mut ok = true;
        if r.selection != a.selection {
            ok = false;
        } else if r.target == a.targets {
            self.conn
                .change_property32(
                    PropMode::REPLACE,
                    r.requestor,
                    property,
                    AtomEnum::ATOM,
                    &[a.targets, a.direct_save, a.uri_list],
                )
                .map_err(err)?;
        } else if r.target == a.uri_list {
            let list: String = self
                .tops
                .iter()
                .map(|t| format!("{}\r\n", file_uri(&t.path)))
                .collect();
            self.conn
                .change_property8(PropMode::REPLACE, r.requestor, property, a.uri_list, list.as_bytes())
                .map_err(err)?;
            self.taken.uri_list = true;
        } else if r.target == a.direct_save {
            let named = self
                .conn
                .get_property(false, self.window, a.direct_save, AtomEnum::ANY, 0, 4096)
                .map_err(err)?
                .reply()
                .map_err(err)?;
            let dir = std::str::from_utf8(&named.value)
                .ok()
                .and_then(path_of_uri)
                .and_then(|p| p.parent().map(Path::to_path_buf))
                .filter(|d| d.is_dir());
            let answer: &[u8] = if dir.is_some() { b"S" } else { b"F" };
            if let Some(dir) = dir {
                info!("drag {}: XDS target folder {}", self.id, dir.display());
                self.taken.folder = Some(dir);
            }
            self.conn
                .change_property8(PropMode::REPLACE, r.requestor, property, AtomEnum::STRING, answer)
                .map_err(err)?;
        } else {
            ok = false;
        }
        let reply = SelectionNotifyEvent {
            response_type: SELECTION_NOTIFY_EVENT,
            sequence: 0,
            time: r.time,
            requestor: r.requestor,
            selection: r.selection,
            target: r.target,
            property: if ok { property } else { x11rb::NONE },
        };
        self.conn
            .send_event(false, r.requestor, EventMask::NO_EVENT, reply)
            .map_err(err)?;
        self.conn.flush().map_err(err)
    }
}

fn file_uri(path: &Path) -> String {
    use std::os::unix::ffi::OsStrExt;
    let mut out = String::from("file://");
    for &b in path.as_os_str().as_bytes() {
        if b.is_ascii_alphanumeric() || b"/-._~".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

fn path_of_uri(uri: &str) -> Option<PathBuf> {
    use std::os::unix::ffi::OsStringExt;
    let rest = uri.trim_end_matches(['\0', '\r', '\n']).strip_prefix("file://")?;
    let path = &rest[rest.find('/')?..];
    let bytes = path.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    let hex = |b: u8| (b as char).to_digit(16).map(|d| d as u8);
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let (Some(h), Some(l)) = (hex(bytes[i + 1]), hex(bytes[i + 2])) {
                out.push(h << 4 | l);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    Some(PathBuf::from(std::ffi::OsString::from_vec(out)))
}

// ------------------------------------------------------------ finding copies

fn is_marker(path: &Path, id: u64, index: usize) -> bool {
    std::fs::read_to_string(path).is_ok_and(|s| s.trim() == marker(id, index))
}

/// Search the places a file manager can drop into for copies of the placeholders.
/// Only folders changed since `since` are looked into; the copy may be renamed.
fn find_copies(id: u64, tops: &[Top], since: SystemTime) -> Option<Vec<PathBuf>> {
    let staging = staging_root();
    let since_secs = since
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let deadline = Instant::now() + FIND_TIMEOUT;
    let mut found: Vec<Option<PathBuf>> = vec![None; tops.len()];
    loop {
        for dir in search_dirs(&staging, since_secs) {
            let Ok(list) = std::fs::read_dir(&dir) else { continue };
            for entry in list.flatten() {
                let Ok(meta) = entry.metadata() else { continue };
                if meta.ctime() < since_secs {
                    continue;
                }
                let path = entry.path();
                for (slot, top) in found.iter_mut().zip(tops) {
                    if slot.is_some() || top.is_dir != meta.is_dir() {
                        continue;
                    }
                    let hit = if top.is_dir {
                        is_marker(&path.join(FOLDER_MARKER), id, top.index)
                    } else {
                        meta.len() < 256 && is_marker(&path, id, top.index)
                    };
                    if hit {
                        debug!("drag {id}: {} copied to {}", top.name, path.display());
                        *slot = Some(path.clone());
                        break;
                    }
                }
            }
        }
        if found.iter().all(Option::is_some) {
            return found.into_iter().collect();
        }
        if Instant::now() >= deadline {
            warn!("drag {id}: copied placeholders not found");
            return None;
        }
        std::thread::sleep(Duration::from_millis(500));
    }
}

/// A copy the file manager renamed to keep both ("a (副本).txt", "a (copy).txt")
/// gets the name every other receive uses: "a (1).txt", "a (2).txt".
fn same_style_name(path: PathBuf, name: &str, is_dir: bool) -> PathBuf {
    let Some(dir) = path.parent().map(Path::to_path_buf) else {
        return path;
    };
    if path.file_name().is_some_and(|n| n == name) {
        return path;
    }
    let (stem, ext) = match name.rfind('.') {
        Some(i) if i > 0 && !is_dir => (&name[..i], &name[i..]),
        _ => (name, ""),
    };
    let ours = path
        .file_name()
        .and_then(|n| n.to_str())
        .and_then(|n| n.strip_prefix(stem)?.strip_prefix(" (")?.strip_suffix(ext)?.strip_suffix(')'))
        .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()));
    if ours {
        return path;
    }
    let Some(target) = (1..10_000)
        .map(|n| dir.join(format!("{stem} ({n}){ext}")))
        .find(|p| std::fs::symlink_metadata(p).is_err())
    else {
        return path;
    };
    match std::fs::rename(&path, &target) {
        Ok(()) => target,
        Err(e) => {
            debug!("keep {}: {e}", path.display());
            path
        }
    }
}

/// Folders under the usual roots whose contents changed since `since_secs`.
fn search_dirs(staging: &Path, since_secs: i64) -> Vec<PathBuf> {
    const MAX_DEPTH: usize = 6;
    const MAX_DIRS: usize = 40_000;
    let mut roots: Vec<PathBuf> = Vec::new();
    if let Some(home) = std::env::var_os("HOME").map(PathBuf::from) {
        roots.push(home);
    }
    if let Ok(user) = std::env::var("USER") {
        roots.push(PathBuf::from("/media").join(&user));
        roots.push(PathBuf::from("/run/media").join(&user));
    }
    roots.push(PathBuf::from("/mnt"));
    roots.push(std::env::temp_dir());
    let mut out = Vec::new();
    let mut queue: std::collections::VecDeque<(PathBuf, usize)> =
        roots.into_iter().filter(|r| r.is_dir()).map(|r| (r, 0)).collect();
    let mut seen = 0usize;
    while let Some((dir, depth)) = queue.pop_front() {
        seen += 1;
        if seen > MAX_DIRS {
            break;
        }
        if dir.starts_with(staging) {
            continue;
        }
        if std::fs::metadata(&dir).is_ok_and(|m| m.mtime() >= since_secs) {
            out.push(dir.clone());
        }
        if depth >= MAX_DEPTH {
            continue;
        }
        let Ok(list) = std::fs::read_dir(&dir) else { continue };
        for entry in list.flatten() {
            let hidden = entry.file_name().to_string_lossy().starts_with('.');
            if !hidden && entry.file_type().is_ok_and(|t| t.is_dir()) {
                queue.push_back((entry.path(), depth + 1));
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(rel: &str, dir: bool) -> DragItem {
        DragItem {
            relative_path: rel.into(),
            size: 3,
            mtime_unix: 0,
            is_directory: dir,
        }
    }

    #[test]
    fn uri_round_trip() {
        let p = Path::new("/home/u/桌面/a b%.txt");
        let uri = file_uri(p);
        assert!(uri.starts_with("file:///home/u/%E6"));
        assert_eq!(path_of_uri(&uri).as_deref(), Some(p));
        assert_eq!(
            path_of_uri("file://host/tmp/x%20y\r\n").as_deref(),
            Some(Path::new("/tmp/x y"))
        );
        assert_eq!(path_of_uri("http://x/y"), None);
    }

    #[test]
    fn placeholders_are_found_after_copy() {
        let base = std::env::temp_dir().join(format!("csc-xdnd-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let items = [item("a.txt", false), item("d", true), item("d/x", false)];
        let tops = make_placeholders(&base.join("stage"), 7, &items).unwrap();
        assert_eq!(tops.len(), 2);
        assert_eq!(tops[1].index, 1);
        assert!(is_marker(&base.join("stage/a.txt"), 7, 0));
        assert!(is_marker(&base.join("stage/d").join(FOLDER_MARKER), 7, 1));
        let dest = base.join("dest");
        std::fs::create_dir_all(dest.join("d")).unwrap();
        std::fs::copy(base.join("stage/a.txt"), dest.join("a (1).txt")).unwrap();
        std::fs::copy(base.join("stage/d").join(FOLDER_MARKER), dest.join("d").join(FOLDER_MARKER)).unwrap();
        let since_secs = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64
            - 5;
        let dirs = search_dirs(&base.join("stage"), since_secs);
        assert!(dirs.contains(&dest));
        assert!(!dirs.iter().any(|d| d.starts_with(base.join("stage"))));

        std::fs::write(dest.join("a.txt"), "old").unwrap();
        let renamed = same_style_name(dest.join("a (1).txt"), "a.txt", false);
        assert_eq!(renamed, dest.join("a (1).txt"), "already in our style, (1) is taken by itself");
        std::fs::rename(dest.join("a (1).txt"), dest.join("a (副本).txt")).unwrap();
        let renamed = same_style_name(dest.join("a (副本).txt"), "a.txt", false);
        assert_eq!(renamed, dest.join("a (1).txt"));
        assert!(is_marker(&renamed, 7, 0));
        assert_eq!(same_style_name(dest.join("d"), "d", true), dest.join("d"));
        let _ = std::fs::remove_dir_all(&base);
    }
}
