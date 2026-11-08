//! Wayland keyboard host through the xdg-desktop-portal InputCapture interface
//! (GNOME 45+, KDE Plasma 6.1+). The compositor owns the pointer: this module
//! places pointer barriers on the screen edges that lead to another computer.
//! When the pointer crosses one, the compositor hands every local input event
//! to us over libei until `Release` gives the pointer back at a chosen point.

use crate::traits::Captured;
use dbus::arg::{PropMap, RefArg, Variant};
use dbus::blocking::Connection;
use dbus::message::MatchRule;
use reis::ei;
use reis::event::{DeviceCapability, EiEvent};
use std::os::unix::net::UnixStream;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tracing::{debug, info, warn};

pub(super) const DEST: &str = "org.freedesktop.portal.Desktop";
pub(super) const PATH: &str = "/org/freedesktop/portal/desktop";
const IFACE: &str = "org.freedesktop.portal.InputCapture";
const CAP_KEYBOARD: u32 = 1;
const CAP_POINTER: u32 = 2;
/// `persist_mode` 2: the permission lasts until the user revokes it.
const PERSIST_UNTIL_REVOKED: u32 = 2;
/// Smooth (touchpad) scroll pixels per wheel notch.
const SCROLL_PX_PER_NOTCH: f32 = 15.0;

/// Desktop edges, in the order of `set_edges`.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Edges {
    pub left: bool,
    pub right: bool,
    pub top: bool,
    pub bottom: bool,
}

enum Cmd {
    Edges(Edges),
    /// Give the pointer back at this point (core desktop pixels).
    Release(Option<(i32, i32)>),
}

/// Zone geometry in portal (logical) pixels and the core's desktop size.
#[derive(Clone, Copy, Default)]
struct Frame {
    x: i32,
    y: i32,
    w: i32,
    h: i32,
    core_w: i32,
    core_h: i32,
}

impl Frame {
    fn core_point(&self, x: f64, y: f64) -> (i32, i32) {
        let sx = f64::from(self.core_w.max(1)) / f64::from(self.w.max(1));
        let sy = f64::from(self.core_h.max(1)) / f64::from(self.h.max(1));
        let cx = ((x - f64::from(self.x)) * sx).round() as i32;
        let cy = ((y - f64::from(self.y)) * sy).round() as i32;
        (cx.clamp(0, self.core_w - 1), cy.clamp(0, self.core_h - 1))
    }

    fn portal_point(&self, x: i32, y: i32) -> (f64, f64) {
        let sx = f64::from(self.w.max(1)) / f64::from(self.core_w.max(1));
        let sy = f64::from(self.h.max(1)) / f64::from(self.core_h.max(1));
        (
            f64::from(self.x) + f64::from(x) * sx,
            f64::from(self.y) + f64::from(y) * sy,
        )
    }
}

static READY: AtomicBool = AtomicBool::new(false);
static ACTIVE: AtomicBool = AtomicBool::new(false);
static TOKEN: AtomicU32 = AtomicU32::new(0);
static HIT: Mutex<Option<(i32, i32)>> = Mutex::new(None);
static CMD: Mutex<Option<Sender<Cmd>>> = Mutex::new(None);

/// True once the portal session is enabled and barriers can fire.
pub fn ready() -> bool {
    READY.load(Ordering::Acquire)
}

/// True while the compositor routes local input to us.
pub fn active() -> bool {
    ACTIVE.load(Ordering::Acquire)
}

/// Where the pointer crossed a barrier (core desktop pixels), once.
pub fn take_hit() -> Option<(i32, i32)> {
    HIT.lock().ok()?.take()
}

pub fn set_edges(edges: Edges) {
    send(Cmd::Edges(edges));
}

/// Hand the pointer back to this computer, placing it at `at` when given.
pub fn release(at: Option<(i32, i32)>) {
    if ACTIVE.swap(false, Ordering::AcqRel) {
        send(Cmd::Release(at));
    }
}

fn send(cmd: Cmd) {
    if let Ok(guard) = CMD.lock() {
        if let Some(tx) = guard.as_ref() {
            let _ = tx.send(cmd);
        }
    }
}

/// True when the session bus offers InputCapture. The session itself starts in
/// the background because the compositor first asks the user for permission.
pub fn start(core_size: (i32, i32)) -> bool {
    let Ok(conn) = Connection::new_session() else {
        return false;
    };
    let version = {
        use dbus::blocking::stdintf::org_freedesktop_dbus::Properties;
        let proxy = conn.with_proxy(DEST, PATH, Duration::from_millis(800));
        proxy.get::<u32>(IFACE, "version")
    };
    let version = match version {
        Ok(v) => v,
        Err(e) => {
            info!("InputCapture portal unavailable: {e}");
            return false;
        }
    };
    let (tx, rx) = mpsc::channel();
    if let Ok(mut guard) = CMD.lock() {
        *guard = Some(tx);
    }
    info!("InputCapture portal version {version}; asking the desktop for input capture");
    std::thread::Builder::new()
        .name("input-capture".into())
        .spawn(move || {
            if let Err(e) = run(conn, rx, core_size, version) {
                warn!("InputCapture session ended: {e}");
            }
            READY.store(false, Ordering::Release);
            ACTIVE.store(false, Ordering::Release);
        })
        .is_ok()
}

pub(super) type BoxErr = Box<dyn std::error::Error>;

pub(super) fn next_token() -> String {
    format!("csc{}", TOKEN.fetch_add(1, Ordering::AcqRel))
}

pub(super) fn var<T: RefArg + 'static>(v: T) -> Variant<Box<dyn RefArg>> {
    Variant(Box::new(v))
}

fn request<A: dbus::arg::AppendAll>(
    conn: &Connection,
    method: &str,
    token: &str,
    args: A,
    wait: Duration,
) -> Result<PropMap, BoxErr> {
    request_on(conn, IFACE, method, token, args, wait)
}

/// Call a portal method that answers through a Request object, and wait for it.
pub(super) fn request_on<A: dbus::arg::AppendAll>(
    conn: &Connection,
    iface: &str,
    method: &str,
    token: &str,
    args: A,
    wait: Duration,
) -> Result<PropMap, BoxErr> {
    let sender = conn.unique_name().trim_start_matches(':').replace('.', "_");
    let path = format!("{PATH}/request/{sender}/{token}");
    let answer: Arc<Mutex<Option<(u32, PropMap)>>> = Arc::new(Mutex::new(None));
    let slot = answer.clone();
    let rule = MatchRule::new_signal("org.freedesktop.portal.Request", "Response")
        .with_path(dbus::Path::new(path.clone())?);
    let id = conn.add_match(rule, move |(code, results): (u32, PropMap), _, _| {
        if let Ok(mut s) = slot.lock() {
            *s = Some((code, results));
        }
        false
    })?;
    let proxy = conn.with_proxy(DEST, PATH, Duration::from_secs(5));
    let (_handle,): (dbus::Path,) = proxy.method_call(iface, method, args)?;
    let until = Instant::now() + wait;
    loop {
        if let Some((code, results)) = answer.lock().ok().and_then(|mut s| s.take()) {
            if code != 0 {
                return Err(format!("{method} refused (response {code})").into());
            }
            return Ok(results);
        }
        if Instant::now() > until {
            let _ = conn.remove_match(id);
            return Err(format!("{method} timed out").into());
        }
        conn.process(Duration::from_millis(100))?;
    }
}

fn zones(conn: &Connection, session: &dbus::Path<'static>) -> Result<(Vec<[i32; 4]>, u32), BoxErr> {
    let token = next_token();
    let mut opts = PropMap::new();
    opts.insert("handle_token".into(), var(token.clone()));
    let results = request(
        conn,
        "GetZones",
        &token,
        (session.clone(), opts),
        Duration::from_secs(5),
    )?;
    let zone_set = dbus::arg::prop_cast::<u32>(&results, "zone_set")
        .copied()
        .unwrap_or(0);
    let mut out = Vec::new();
    if let Some(list) = results.get("zones").and_then(|v| v.0.as_iter()) {
        for zone in list {
            let Some(mut it) = zone.as_iter() else {
                continue;
            };
            let w = it.next().and_then(|v| v.as_i64()).unwrap_or(0) as i32;
            let h = it.next().and_then(|v| v.as_i64()).unwrap_or(0) as i32;
            let x = it.next().and_then(|v| v.as_i64()).unwrap_or(0) as i32;
            let y = it.next().and_then(|v| v.as_i64()).unwrap_or(0) as i32;
            if w > 0 && h > 0 {
                out.push([x, y, w, h]);
            }
        }
    }
    Ok((out, zone_set))
}

/// One barrier per zone side that lies on the outer box of the desktop.
fn barriers(zones: &[[i32; 4]], edges: Edges) -> Vec<(u32, (i32, i32, i32, i32))> {
    let left = zones.iter().map(|z| z[0]).min().unwrap_or(0);
    let top = zones.iter().map(|z| z[1]).min().unwrap_or(0);
    let right = zones.iter().map(|z| z[0] + z[2]).max().unwrap_or(0);
    let bottom = zones.iter().map(|z| z[1] + z[3]).max().unwrap_or(0);
    let mut out = Vec::new();
    for &[x, y, w, h] in zones {
        let mut add = |line| out.push((out.len() as u32 + 1, line));
        if edges.left && x == left {
            add((x, y, x, y + h - 1));
        }
        if edges.right && x + w == right {
            add((x + w, y, x + w, y + h - 1));
        }
        if edges.top && y == top {
            add((x, y, x + w - 1, y));
        }
        if edges.bottom && y + h == bottom {
            add((x, y + h, x + w - 1, y + h));
        }
    }
    out
}

/// libei deltas are in portal (logical) pixels; the core counts this desktop
/// in its own pixels, like `Frame::core_point`.
static SCALE_X: AtomicU32 = AtomicU32::new(0x3f80_0000);
static SCALE_Y: AtomicU32 = AtomicU32::new(0x3f80_0000);

fn motion_scale() -> (f32, f32) {
    (
        f32::from_bits(SCALE_X.load(Ordering::Relaxed)),
        f32::from_bits(SCALE_Y.load(Ordering::Relaxed)),
    )
}

fn bounding(zones: &[[i32; 4]], core: (i32, i32)) -> Frame {
    let x = zones.iter().map(|z| z[0]).min().unwrap_or(0);
    let y = zones.iter().map(|z| z[1]).min().unwrap_or(0);
    let r = zones.iter().map(|z| z[0] + z[2]).max().unwrap_or(core.0);
    let b = zones.iter().map(|z| z[1] + z[3]).max().unwrap_or(core.1);
    let ratio = |c: i32, p: i32| (c.max(1) as f32 / p.max(1) as f32).to_bits();
    SCALE_X.store(ratio(core.0, r - x), Ordering::Relaxed);
    SCALE_Y.store(ratio(core.1, b - y), Ordering::Relaxed);
    Frame {
        x,
        y,
        w: r - x,
        h: b - y,
        core_w: core.0,
        core_h: core.1,
    }
}

fn set_barriers(
    conn: &Connection,
    session: &dbus::Path<'static>,
    zones: &[[i32; 4]],
    zone_set: u32,
    edges: Edges,
) -> Result<(), BoxErr> {
    let list = barriers(zones, edges);
    let maps: Vec<PropMap> = list
        .iter()
        .map(|(id, line)| {
            let mut m = PropMap::new();
            m.insert("barrier_id".into(), var(*id));
            m.insert("position".into(), var(*line));
            m
        })
        .collect();
    let token = next_token();
    let mut opts = PropMap::new();
    opts.insert("handle_token".into(), var(token.clone()));
    let results = request(
        conn,
        "SetPointerBarriers",
        &token,
        (session.clone(), opts, maps, zone_set),
        Duration::from_secs(5),
    )?;
    let failed: Vec<u64> = results
        .get("failed_barriers")
        .and_then(|v| v.0.as_iter())
        .map(|it| it.filter_map(|b| b.as_u64()).collect())
        .unwrap_or_default();
    for (id, line) in &list {
        if failed.contains(&u64::from(*id)) {
            warn!(
                id,
                ?line,
                "InputCapture rejected a barrier; that edge is skipped"
            );
        } else {
            debug!(id, ?line, "InputCapture barrier set");
        }
    }
    info!(
        ?edges,
        set = list.len() - failed.len(),
        "InputCapture barriers applied"
    );
    Ok(())
}

fn token_file() -> Option<std::path::PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(std::path::PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| std::path::Path::new(&h).join(".config")))?;
    Some(base.join("cross-systemconnect").join("input-capture-token"))
}

fn load_restore_token() -> Option<String> {
    let token = std::fs::read_to_string(token_file()?).ok()?;
    let token = token.trim();
    (!token.is_empty()).then(|| token.to_string())
}

fn save_restore_token(token: Option<&str>) {
    let Some(path) = token_file() else { return };
    let result = match token {
        Some(t) => path
            .parent()
            .map_or(Ok(()), std::fs::create_dir_all)
            .and_then(|()| std::fs::write(&path, t)),
        None => std::fs::remove_file(&path).or(Ok(())),
    };
    if let Err(e) = result {
        warn!("InputCapture restore token not saved: {e}");
    }
}

/// Version 1: the deprecated CreateSession, which asks the user every time.
fn create_session_v1(conn: &Connection) -> Result<dbus::Path<'static>, BoxErr> {
    let token = next_token();
    let mut opts = PropMap::new();
    opts.insert("handle_token".into(), var(token.clone()));
    opts.insert("session_handle_token".into(), var(next_token()));
    opts.insert("capabilities".into(), var(CAP_KEYBOARD | CAP_POINTER));
    // The compositor may show a permission dialog; give the user time to answer.
    let results = request(
        conn,
        "CreateSession",
        &token,
        (String::new(), opts),
        Duration::from_secs(600),
    )?;
    session_path(&results, "CreateSession")
}

/// Version 2: CreateSession2 + Start with a persistent permission, so the
/// desktop asks once and later starts are restored from the saved token.
fn create_session_v2(conn: &Connection) -> Result<dbus::Path<'static>, BoxErr> {
    let mut opts = PropMap::new();
    opts.insert("session_handle_token".into(), var(next_token()));
    let proxy = conn.with_proxy(DEST, PATH, Duration::from_secs(5));
    let (results,): (PropMap,) = proxy.method_call(IFACE, "CreateSession2", (opts,))?;
    let session = session_path(&results, "CreateSession2")?;

    let token = next_token();
    let mut opts = PropMap::new();
    opts.insert("handle_token".into(), var(token.clone()));
    opts.insert("capabilities".into(), var(CAP_KEYBOARD | CAP_POINTER));
    opts.insert("persist_mode".into(), var(PERSIST_UNTIL_REVOKED));
    let restoring = load_restore_token();
    if let Some(t) = &restoring {
        opts.insert("restore_token".into(), var(t.clone()));
    }
    let started = request(
        conn,
        "Start",
        &token,
        (session.clone(), String::new(), opts),
        Duration::from_secs(600),
    );
    let results = match started {
        Ok(r) => r,
        Err(e) => {
            if restoring.is_some() {
                save_restore_token(None);
            }
            return Err(e);
        }
    };
    let next = results.get("restore_token").and_then(|v| v.0.as_str());
    save_restore_token(next);
    info!(
        restored = restoring.is_some(),
        remembered = next.is_some(),
        "InputCapture session started"
    );
    Ok(session)
}

fn session_path(results: &PropMap, method: &str) -> Result<dbus::Path<'static>, BoxErr> {
    let handle = results
        .get("session_handle")
        .and_then(|v| v.0.as_str().map(str::to_string))
        .ok_or_else(|| format!("{method} returned no session_handle"))?;
    Ok(dbus::Path::new(handle)?)
}

/// Mutter only accepts barriers while the session is disabled, so replacing
/// them means Disable, SetPointerBarriers, Enable.
fn rebarrier(
    conn: &Connection,
    session: &dbus::Path<'static>,
    zones: &[[i32; 4]],
    zone_set: u32,
    edges: Edges,
) -> Result<(), BoxErr> {
    READY.store(false, Ordering::Release);
    ACTIVE.store(false, Ordering::Release);
    plain(conn, "Disable", session, PropMap::new());
    set_barriers(conn, session, zones, zone_set, edges)?;
    plain(conn, "Enable", session, PropMap::new());
    READY.store(true, Ordering::Release);
    Ok(())
}

fn plain(conn: &Connection, method: &str, session: &dbus::Path<'static>, opts: PropMap) {
    let proxy = conn.with_proxy(DEST, PATH, Duration::from_secs(2));
    let r: Result<(), dbus::Error> = proxy.method_call(IFACE, method, (session.clone(), opts));
    if let Err(e) = r {
        warn!("InputCapture {method}: {e}");
    }
}

#[derive(Default)]
struct Signals {
    activated: Option<(u32, Option<(f64, f64)>)>,
    deactivated: bool,
    disabled: bool,
    zones_changed: bool,
}

fn run(conn: Connection, rx: Receiver<Cmd>, core: (i32, i32), version: u32) -> Result<(), BoxErr> {
    let session = if version >= 2 {
        create_session_v2(&conn)?
    } else {
        create_session_v1(&conn)?
    };

    let signals: Arc<Mutex<Signals>> = Arc::new(Mutex::new(Signals::default()));
    for member in ["Activated", "Deactivated", "Disabled", "ZonesChanged"] {
        let sink = signals.clone();
        let mine = session.to_string();
        conn.add_match(
            MatchRule::new_signal(IFACE, member),
            move |(from, opts): (dbus::Path, PropMap), _, _| {
                if from.to_string() != mine {
                    return true;
                }
                let Ok(mut s) = sink.lock() else { return true };
                match member {
                    "Activated" => {
                        let id = dbus::arg::prop_cast::<u32>(&opts, "activation_id")
                            .copied()
                            .unwrap_or(0);
                        let pos = opts.get("cursor_position").and_then(|v| {
                            let mut it = v.0.as_iter()?;
                            Some((it.next()?.as_f64()?, it.next()?.as_f64()?))
                        });
                        s.activated = Some((id, pos));
                    }
                    "Deactivated" => s.deactivated = true,
                    "Disabled" => s.disabled = true,
                    _ => s.zones_changed = true,
                }
                true
            },
        )?;
    }

    let fd: (dbus::arg::OwnedFd,) = conn
        .with_proxy(DEST, PATH, Duration::from_secs(5))
        .method_call(IFACE, "ConnectToEIS", (session.clone(), PropMap::new()))?;
    // SAFETY: `into_fd` hands over sole ownership of the descriptor the portal sent.
    let stream = unsafe {
        use std::os::fd::FromRawFd;
        UnixStream::from_raw_fd(fd.0.into_fd())
    };
    let context = ei::Context::new(stream)?;
    std::thread::Builder::new()
        .name("libei".into())
        .spawn(move || {
            if let Err(e) = read_ei(context) {
                warn!("libei stream ended: {e}");
            }
        })?;

    let (mut zone_list, mut zone_set) = zones(&conn, &session)?;
    let mut frame = bounding(&zone_list, core);
    let mut edges = Edges::default();
    set_barriers(&conn, &session, &zone_list, zone_set, edges)?;
    plain(&conn, "Enable", &session, PropMap::new());
    READY.store(true, Ordering::Release);
    info!("InputCapture enabled; zones {zone_list:?}");

    let mut activation = 0u32;
    let mut quiet_until = Instant::now();
    loop {
        conn.process(Duration::from_millis(5))?;
        while let Ok(cmd) = rx.try_recv() {
            match cmd {
                Cmd::Edges(next) if next != edges => {
                    edges = next;
                    rebarrier(&conn, &session, &zone_list, zone_set, edges)?;
                    quiet_until = Instant::now() + Duration::from_secs(1);
                }
                Cmd::Edges(_) => {}
                Cmd::Release(at) => {
                    let mut opts = PropMap::new();
                    opts.insert("activation_id".into(), var(activation));
                    if let Some((x, y)) = at {
                        opts.insert("cursor_position".into(), var(frame.portal_point(x, y)));
                    }
                    plain(&conn, "Release", &session, opts);
                }
            }
        }
        let got = std::mem::take(&mut *signals.lock().map_err(|_| "signal lock poisoned")?);
        if let Some((id, pos)) = got.activated {
            activation = id;
            ACTIVE.store(true, Ordering::Release);
            let at = pos.map(|(x, y)| frame.core_point(x, y)).unwrap_or((0, 0));
            debug!(id, ?pos, ?at, "InputCapture activated");
            if let Ok(mut hit) = HIT.lock() {
                *hit = Some(at);
            }
        }
        if got.deactivated {
            debug!("InputCapture deactivated");
            ACTIVE.store(false, Ordering::Release);
        }
        if got.zones_changed {
            (zone_list, zone_set) = zones(&conn, &session)?;
            frame = bounding(&zone_list, core);
            rebarrier(&conn, &session, &zone_list, zone_set, edges)?;
            quiet_until = Instant::now() + Duration::from_secs(1);
        }
        // Our own Disable during rebarrier also raises Disabled; skip it.
        if got.disabled && Instant::now() > quiet_until {
            warn!("InputCapture disabled by the desktop; enabling again");
            READY.store(false, Ordering::Release);
            ACTIVE.store(false, Ordering::Release);
            std::thread::sleep(Duration::from_millis(500));
            plain(&conn, "Enable", &session, PropMap::new());
            READY.store(true, Ordering::Release);
        }
    }
}

fn button_index(code: u32) -> Option<u32> {
    match code {
        0x110 => Some(0),
        0x111 => Some(1),
        0x112 => Some(2),
        0x113 | 0x116 => Some(3),
        0x114 | 0x115 => Some(4),
        _ => None,
    }
}

fn read_ei(context: ei::Context) -> Result<(), BoxErr> {
    let (_conn, events) =
        context.handshake_blocking("Cross-SystemConnect", ei::handshake::ContextType::Receiver)?;
    let mut frac = (0f32, 0f32);
    let mut scroll = (0f32, 0f32);
    let mut stats = crate::RateStats::new("libei motion");
    for event in events {
        let event = event?;
        if matches!(event, EiEvent::PointerMotion(_)) {
            stats.note();
        }
        match event {
            EiEvent::SeatAdded(e) => {
                e.seat.bind_capabilities(
                    DeviceCapability::Pointer
                        | DeviceCapability::Keyboard
                        | DeviceCapability::Scroll
                        | DeviceCapability::Button,
                );
                let _ = context.flush();
            }
            EiEvent::PointerMotion(m) => {
                let (sx, sy) = motion_scale();
                frac.0 += m.dx * sx;
                frac.1 += m.dy * sy;
                let (dx, dy) = (frac.0.trunc(), frac.1.trunc());
                frac = (frac.0 - dx, frac.1 - dy);
                if dx != 0.0 || dy != 0.0 {
                    super::grab::push(Captured::Move {
                        dx: dx as i32,
                        dy: dy as i32,
                    });
                }
            }
            EiEvent::Button(b) => {
                if let Some(button) = button_index(b.button) {
                    super::grab::push(Captured::Button {
                        button,
                        down: b.state == ei::button::ButtonState::Press,
                    });
                }
            }
            EiEvent::ScrollDiscrete(s) => {
                super::grab::push(Captured::Wheel {
                    v: -s.discrete_dy,
                    h: s.discrete_dx,
                });
            }
            EiEvent::ScrollDelta(s) => {
                scroll.0 += s.dx;
                scroll.1 += s.dy;
                let per = SCROLL_PX_PER_NOTCH / 120.0;
                let (h, v) = ((scroll.0 / per).trunc(), (scroll.1 / per).trunc());
                scroll = (scroll.0 - h * per, scroll.1 - v * per);
                if h != 0.0 || v != 0.0 {
                    super::grab::push(Captured::Wheel {
                        v: -(v as i32),
                        h: h as i32,
                    });
                }
            }
            EiEvent::KeyboardKey(k) => {
                let Ok(evdev) = u16::try_from(k.key) else {
                    continue;
                };
                if let Some(hid) = crate::keymap::hid_from_evdev(evdev) {
                    let down = k.state == ei::keyboard::KeyState::Press;
                    if crate::keyrepeat::forward(hid, down) {
                        super::grab::push(Captured::Key { hid, down });
                    }
                }
            }
            EiEvent::Disconnected(d) => {
                return Err(format!("libei disconnected: {:?}", d.reason).into());
            }
            _ => {}
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn barriers_sit_on_outer_sides_only() {
        let zones = [[0, 0, 1920, 1080], [1920, 0, 1920, 1080]];
        let edges = Edges {
            right: true,
            left: true,
            ..Default::default()
        };
        let list = barriers(&zones, edges);
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].1, (0, 0, 0, 1079));
        assert_eq!(list[1].1, (3840, 0, 3840, 1079));
    }

    #[test]
    fn frame_maps_scaled_zones_to_core_pixels() {
        let f = Frame {
            x: 0,
            y: 0,
            w: 2560,
            h: 1600,
            core_w: 3840,
            core_h: 2400,
        };
        assert_eq!(f.core_point(2560.0, 800.0), (3839, 1200));
        assert_eq!(f.portal_point(1920, 1200), (1280.0, 800.0));
    }
}
