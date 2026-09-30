//! Local input capture on X11 while the pointer is on a remote computer.
//! A dedicated thread with its own display connection grabs the pointer and
//! keyboard, turns events into `Captured`, and holds the cursor at a park point.

use super::xdo::{dlopen, sym};
use crate::traits::Captured;
use libc::{c_int, c_long, c_uint, c_ulong, c_void};
use std::sync::Mutex;
use std::time::{Duration, Instant};
use tracing::{info, warn};

type OpenDisplay = unsafe extern "C" fn(*const libc::c_char) -> *mut c_void;
type InitThreads = unsafe extern "C" fn() -> c_int;
type DefaultRoot = unsafe extern "C" fn(*mut c_void) -> c_ulong;
type ConnectionNumber = unsafe extern "C" fn(*mut c_void) -> c_int;
type Flush = unsafe extern "C" fn(*mut c_void) -> c_int;
type Pending = unsafe extern "C" fn(*mut c_void) -> c_int;
type NextEvent = unsafe extern "C" fn(*mut c_void, *mut c_long) -> c_int;
type GrabPointer = unsafe extern "C" fn(
    *mut c_void,
    c_ulong,
    c_int,
    c_uint,
    c_int,
    c_int,
    c_ulong,
    c_ulong,
    c_ulong,
) -> c_int;
type GrabKeyboard =
    unsafe extern "C" fn(*mut c_void, c_ulong, c_int, c_int, c_int, c_ulong) -> c_int;
type Ungrab = unsafe extern "C" fn(*mut c_void, c_ulong) -> c_int;
type WarpPointer = unsafe extern "C" fn(
    *mut c_void,
    c_ulong,
    c_ulong,
    c_int,
    c_int,
    c_uint,
    c_uint,
    c_int,
    c_int,
) -> c_int;

const KEY_PRESS: c_int = 2;
const KEY_RELEASE: c_int = 3;
const BUTTON_PRESS: c_int = 4;
const BUTTON_RELEASE: c_int = 5;
const MOTION_NOTIFY: c_int = 6;
const POINTER_MASK: c_uint = (1 << 2) | (1 << 3) | (1 << 6);
const GRAB_MODE_ASYNC: c_int = 1;
const GRAB_SUCCESS: c_int = 0;
/// Warp back to the park point once the pointer drifts this far from it.
const REPARK_PX: i32 = 64;

/// Layout shared by XKeyEvent, XButtonEvent and XMotionEvent up to `detail`.
#[repr(C)]
struct XInputEvent {
    kind: c_int,
    serial: c_ulong,
    send_event: c_int,
    display: *mut c_void,
    window: c_ulong,
    root: c_ulong,
    subwindow: c_ulong,
    time: c_ulong,
    x: c_int,
    y: c_int,
    x_root: c_int,
    y_root: c_int,
    state: c_uint,
    detail: c_uint,
}

struct Api {
    dpy: *mut c_void,
    root: c_ulong,
    fd: c_int,
    flush: Flush,
    pending: Pending,
    next_event: NextEvent,
    grab_pointer: GrabPointer,
    grab_keyboard: GrabKeyboard,
    ungrab_pointer: Ungrab,
    ungrab_keyboard: Ungrab,
    warp: WarpPointer,
}

struct Shared {
    /// Park point while grabbed, None when input belongs to this computer.
    want: Option<(i32, i32)>,
    queue: Vec<Captured>,
}

static SHARED: Mutex<Shared> = Mutex::new(Shared {
    want: None,
    queue: Vec::new(),
});
static STARTED: Mutex<Option<bool>> = Mutex::new(None);

fn push(ev: Captured) {
    let Ok(mut s) = SHARED.lock() else { return };
    if s.want.is_none() {
        return;
    }
    if let (Captured::Move { dx, dy }, Some(Captured::Move { dx: pdx, dy: pdy })) =
        (ev, s.queue.last_mut())
    {
        *pdx += dx;
        *pdy += dy;
        return;
    }
    s.queue.push(ev);
}

/// Open the X display and start the capture thread. False without X11.
pub fn start() -> bool {
    let mut started = STARTED.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(ok) = *started {
        return ok;
    }
    if std::env::var("XDG_SESSION_TYPE").is_ok_and(|t| t.eq_ignore_ascii_case("wayland")) {
        warn!("Wayland session: X11 grabs do not see native windows; only pointer motion is forwarded");
        *started = Some(false);
        return false;
    }
    let (tx, rx) = std::sync::mpsc::channel();
    let spawned = std::thread::Builder::new()
        .name("x11-grab".into())
        .spawn(move || match unsafe { open() } {
            Some(api) => {
                let _ = tx.send(true);
                run(api);
            }
            None => {
                let _ = tx.send(false);
            }
        });
    let ok = spawned.is_ok() && rx.recv_timeout(Duration::from_secs(3)).unwrap_or(false);
    if ok {
        info!("X11 input capture ready");
    } else {
        warn!("X11 input capture unavailable; only pointer motion is forwarded");
    }
    *started = Some(ok);
    ok
}

pub fn set_grab(grab: bool, park: Option<(i32, i32)>) {
    if let Ok(mut s) = SHARED.lock() {
        s.want = if grab { Some(park.unwrap_or((0, 0))) } else { None };
        s.queue.clear();
    }
}

pub fn drain() -> Vec<Captured> {
    SHARED
        .lock()
        .map(|mut s| std::mem::take(&mut s.queue))
        .unwrap_or_default()
}

unsafe fn open() -> Option<Api> {
    let x11 = dlopen(b"libX11.so.6\0")?;
    let init: InitThreads = sym(x11, "XInitThreads")?;
    let open_display: OpenDisplay = sym(x11, "XOpenDisplay")?;
    let default_root: DefaultRoot = sym(x11, "XDefaultRootWindow")?;
    let connection_number: ConnectionNumber = sym(x11, "XConnectionNumber")?;
    init();
    let dpy = open_display(std::ptr::null());
    if dpy.is_null() {
        return None;
    }
    Some(Api {
        dpy,
        root: default_root(dpy),
        fd: connection_number(dpy),
        flush: sym(x11, "XFlush")?,
        pending: sym(x11, "XPending")?,
        next_event: sym(x11, "XNextEvent")?,
        grab_pointer: sym(x11, "XGrabPointer")?,
        grab_keyboard: sym(x11, "XGrabKeyboard")?,
        ungrab_pointer: sym(x11, "XUngrabPointer")?,
        ungrab_keyboard: sym(x11, "XUngrabKeyboard")?,
        warp: sym(x11, "XWarpPointer")?,
    })
}

fn button_event(detail: c_uint, down: bool) -> Option<Captured> {
    let button = match detail {
        1 => 0,
        3 => 1,
        2 => 2,
        8 => 3,
        9 => 4,
        // Wheel buttons send a press and a release per notch; count the press.
        4..=7 if !down => return None,
        4 => return Some(Captured::Wheel { v: 1, h: 0 }),
        5 => return Some(Captured::Wheel { v: -1, h: 0 }),
        6 => return Some(Captured::Wheel { v: 0, h: -1 }),
        7 => return Some(Captured::Wheel { v: 0, h: 1 }),
        _ => return None,
    };
    Some(Captured::Button { button, down })
}

struct GrabState {
    park: (i32, i32),
    /// Last reported pointer position; unknown right after grabbing.
    last: Option<(i32, i32)>,
    /// When a warp to the park point was requested and its event not yet seen.
    warp_at: Option<Instant>,
    last_attempt: Option<Instant>,
    warned: bool,
}

fn run(api: Api) {
    let mut grabbed: Option<GrabState> = None;
    let mut buf = [0 as c_long; 24];
    loop {
        let want = SHARED.lock().map(|s| s.want).unwrap_or(None);
        unsafe {
            match (want, grabbed.as_mut()) {
                (Some(park), None) => grabbed = Some(try_grab(&api, park, None)),
                (Some(park), Some(g)) if g.last_attempt.is_some() => {
                    let retry = g.last_attempt.is_some_and(|t| t.elapsed() > Duration::from_millis(200));
                    if retry {
                        let warned = g.warned;
                        grabbed = Some(try_grab(&api, park, Some(warned)));
                    }
                }
                (Some(park), Some(g)) if g.park != park => {
                    g.park = park;
                    (api.warp)(api.dpy, 0, api.root, 0, 0, 0, 0, park.0, park.1);
                    (api.flush)(api.dpy);
                    g.warp_at = Some(Instant::now());
                }
                (None, Some(_)) => {
                    (api.ungrab_pointer)(api.dpy, 0);
                    (api.ungrab_keyboard)(api.dpy, 0);
                    (api.flush)(api.dpy);
                    grabbed = None;
                }
                _ => {}
            }

            while (api.pending)(api.dpy) > 0 {
                (api.next_event)(api.dpy, buf.as_mut_ptr());
                let Some(g) = grabbed.as_mut().filter(|g| g.last_attempt.is_none()) else {
                    continue;
                };
                let ev = &*(buf.as_ptr() as *const XInputEvent);
                match ev.kind {
                    MOTION_NOTIFY => {
                        let pos = (ev.x_root, ev.y_root);
                        let expired = g
                            .warp_at
                            .is_some_and(|t| t.elapsed() > Duration::from_millis(150));
                        if g.warp_at.is_some() && (pos == g.park || expired) {
                            // The warp's own motion event, not a hand movement.
                            g.warp_at = None;
                            g.last = Some(pos);
                            continue;
                        }
                        if let Some(last) = g.last {
                            let (dx, dy) = (pos.0 - last.0, pos.1 - last.1);
                            if dx != 0 || dy != 0 {
                                push(Captured::Move { dx, dy });
                            }
                        }
                        g.last = Some(pos);
                        let far = (pos.0 - g.park.0).abs() > REPARK_PX
                            || (pos.1 - g.park.1).abs() > REPARK_PX;
                        if far && g.warp_at.is_none() {
                            (api.warp)(api.dpy, 0, api.root, 0, 0, 0, 0, g.park.0, g.park.1);
                            (api.flush)(api.dpy);
                            g.warp_at = Some(Instant::now());
                        }
                    }
                    BUTTON_PRESS | BUTTON_RELEASE => {
                        if let Some(c) = button_event(ev.detail, ev.kind == BUTTON_PRESS) {
                            push(c);
                        }
                    }
                    KEY_PRESS | KEY_RELEASE => {
                        let evdev = ev.detail.saturating_sub(8) as u16;
                        if let Some(hid) = crate::keymap::hid_from_evdev(evdev) {
                            push(Captured::Key {
                                hid,
                                down: ev.kind == KEY_PRESS,
                            });
                        }
                    }
                    _ => {}
                }
            }

            let wait_ms = if grabbed.is_some() { 4 } else { 15 };
            let mut pfd = libc::pollfd {
                fd: api.fd,
                events: libc::POLLIN,
                revents: 0,
            };
            libc::poll(&mut pfd, 1, wait_ms);
        }
    }
}

/// Grab pointer and keyboard. On failure (another client holds a grab, such
/// as an open menu) the state records the attempt so it is retried.
unsafe fn try_grab(api: &Api, park: (i32, i32), warned: Option<bool>) -> GrabState {
    let p = (api.grab_pointer)(
        api.dpy,
        api.root,
        0,
        POINTER_MASK,
        GRAB_MODE_ASYNC,
        GRAB_MODE_ASYNC,
        0,
        0,
        0,
    );
    let k = if p == GRAB_SUCCESS {
        (api.grab_keyboard)(api.dpy, api.root, 0, GRAB_MODE_ASYNC, GRAB_MODE_ASYNC, 0)
    } else {
        -1
    };
    let ok = p == GRAB_SUCCESS && k == GRAB_SUCCESS;
    if !ok && p == GRAB_SUCCESS {
        (api.ungrab_pointer)(api.dpy, 0);
    }
    if ok {
        (api.warp)(api.dpy, 0, api.root, 0, 0, 0, 0, park.0, park.1);
    }
    (api.flush)(api.dpy);
    let already_warned = warned.unwrap_or(false);
    if !ok && !already_warned {
        warn!("X11 grab failed (pointer {p}, keyboard {k}); retrying");
    }
    GrabState {
        park,
        last: None,
        warp_at: ok.then(Instant::now),
        last_attempt: (!ok).then(Instant::now),
        warned: already_warned || !ok,
    }
}
