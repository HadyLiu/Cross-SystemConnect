//! Local input capture on X11 while the pointer is on a remote computer.
//! A dedicated thread with its own display connection grabs the pointer and
//! keyboard, turns events into `Captured`, and holds the cursor at a park point.
//! The same thread measures raw mouse motion (XInput2), for pushing through a
//! screen edge, notices the lock key from raw key events, and catches Ctrl+V
//! while files copied elsewhere wait to be pasted.

use super::xdo::{dlopen, dlopen_quiet, sym};
use crate::keymap::modifier_bit;
use crate::lock::lock_key_down;
use crate::traits::Captured;
use libc::{c_int, c_long, c_uint, c_ulong, c_void};
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU32, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};
use tracing::{debug, info, warn};

type ErrorHandler = unsafe extern "C" fn(*mut c_void, *mut c_void) -> c_int;
type SetErrorHandler = unsafe extern "C" fn(Option<ErrorHandler>) -> Option<ErrorHandler>;
type KeysymToKeycode = unsafe extern "C" fn(*mut c_void, c_ulong) -> u8;
type GrabKey =
    unsafe extern "C" fn(*mut c_void, c_int, c_uint, c_ulong, c_int, c_int, c_int) -> c_int;
type UngrabKey = unsafe extern "C" fn(*mut c_void, c_int, c_uint, c_ulong) -> c_int;
type QueryExtension = unsafe extern "C" fn(
    *mut c_void,
    *const libc::c_char,
    *mut c_int,
    *mut c_int,
    *mut c_int,
) -> c_int;
type GetEventData = unsafe extern "C" fn(*mut c_void, *mut c_void) -> c_int;
type FreeEventData = unsafe extern "C" fn(*mut c_void, *mut c_void);
type XiSelectEvents = unsafe extern "C" fn(*mut c_void, c_ulong, *mut XiEventMask, c_int) -> c_int;
type XiQueryVersion = unsafe extern "C" fn(*mut c_void, *mut c_int, *mut c_int) -> c_int;

#[repr(C)]
struct XiEventMask {
    deviceid: c_int,
    mask_len: c_int,
    mask: *mut u8,
}

#[repr(C)]
struct GenericCookie {
    kind: c_int,
    serial: c_ulong,
    send_event: c_int,
    display: *mut c_void,
    extension: c_int,
    evtype: c_int,
    cookie: c_uint,
    data: *mut c_void,
}

#[repr(C)]
struct XiRawEvent {
    kind: c_int,
    serial: c_ulong,
    send_event: c_int,
    display: *mut c_void,
    extension: c_int,
    evtype: c_int,
    time: c_ulong,
    deviceid: c_int,
    sourceid: c_int,
    detail: c_int,
    flags: c_int,
    mask_len: c_int,
    mask: *const u8,
    values: *const f64,
    raw_values: *const f64,
}

const GENERIC_EVENT: c_int = 35;
const XI_RAW_KEY_PRESS: c_int = 13;
const XI_RAW_KEY_RELEASE: c_int = 14;
const XI_RAW_MOTION: c_int = 17;
const XI_ALL_MASTER_DEVICES: c_int = 1;
const XK_V: c_ulong = 0x76;
const CONTROL_MASK: c_uint = 1 << 2;
const LOCK_MASK: c_uint = 1 << 1;
const MOD2_MASK: c_uint = 1 << 4;

static RAW_OK: AtomicBool = AtomicBool::new(false);
static RAW_DX: AtomicI32 = AtomicI32::new(0);
static RAW_DY: AtomicI32 = AtomicI32::new(0);
static PASTE_WANT: AtomicBool = AtomicBool::new(false);
static PASTE_HITS: AtomicU32 = AtomicU32::new(0);

unsafe extern "C" fn ignore_x_error(_dpy: *mut c_void, _ev: *mut c_void) -> c_int {
    0
}

pub fn take_raw_motion() -> Option<(i32, i32)> {
    if !RAW_OK.load(Ordering::Relaxed) {
        return None;
    }
    Some((
        RAW_DX.swap(0, Ordering::AcqRel),
        RAW_DY.swap(0, Ordering::AcqRel),
    ))
}

pub fn paste_watch(enable: bool) {
    if PASTE_WANT.swap(enable, Ordering::AcqRel) != enable {
        debug!("Ctrl+V watch {}", if enable { "on" } else { "off" });
    }
    PASTE_HITS.store(0, Ordering::Release);
}

pub fn paste_hits() -> u32 {
    PASTE_HITS.swap(0, Ordering::AcqRel)
}

/// XInput2 pieces for raw motion; absent on servers without XI 2.
struct Xi {
    opcode: c_int,
    get_data: GetEventData,
    free_data: FreeEventData,
}

/// Passive grab of Ctrl+V on the root window.
struct PasteKey {
    keycode: c_int,
    grab: GrabKey,
    ungrab: UngrabKey,
    active: bool,
}

impl PasteKey {
    const MODS: [c_uint; 4] = [
        CONTROL_MASK,
        CONTROL_MASK | LOCK_MASK,
        CONTROL_MASK | MOD2_MASK,
        CONTROL_MASK | LOCK_MASK | MOD2_MASK,
    ];

    unsafe fn set(&mut self, dpy: *mut c_void, root: c_ulong, flush: Flush, on: bool) {
        if on == self.active || self.keycode == 0 {
            return;
        }
        for m in Self::MODS {
            if on {
                (self.grab)(
                    dpy,
                    self.keycode,
                    m,
                    root,
                    0,
                    GRAB_MODE_ASYNC,
                    GRAB_MODE_ASYNC,
                );
            } else {
                (self.ungrab)(dpy, self.keycode, m, root);
            }
        }
        flush(dpy);
        self.active = on;
    }
}

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
    xi: Option<Xi>,
    paste: PasteKey,
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
        s.want = if grab {
            Some(park.unwrap_or((0, 0)))
        } else {
            None
        };
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
    // A refused key grab must not end the process (Xlib's default handler exits).
    if let Some(set_handler) = sym::<SetErrorHandler>(x11, "XSetErrorHandler") {
        set_handler(Some(ignore_x_error));
    }
    let root = default_root(dpy);
    let keysym_to_keycode: KeysymToKeycode = sym(x11, "XKeysymToKeycode")?;
    let xi = open_xi(x11, dpy, root);
    RAW_OK.store(xi.is_some(), Ordering::Release);
    if xi.is_none() {
        info!("XInput2 raw motion unavailable; screen edges switch on contact");
    }
    Some(Api {
        dpy,
        root,
        fd: connection_number(dpy),
        flush: sym(x11, "XFlush")?,
        pending: sym(x11, "XPending")?,
        next_event: sym(x11, "XNextEvent")?,
        grab_pointer: sym(x11, "XGrabPointer")?,
        grab_keyboard: sym(x11, "XGrabKeyboard")?,
        ungrab_pointer: sym(x11, "XUngrabPointer")?,
        ungrab_keyboard: sym(x11, "XUngrabKeyboard")?,
        warp: sym(x11, "XWarpPointer")?,
        xi,
        paste: PasteKey {
            keycode: keysym_to_keycode(dpy, XK_V) as c_int,
            grab: sym(x11, "XGrabKey")?,
            ungrab: sym(x11, "XUngrabKey")?,
            active: false,
        },
    })
}

/// Select raw motion from all master devices on the root window.
unsafe fn open_xi(x11: *mut c_void, dpy: *mut c_void, root: c_ulong) -> Option<Xi> {
    let query_ext: QueryExtension = sym(x11, "XQueryExtension")?;
    let (mut opcode, mut event, mut error) = (0, 0, 0);
    let name = b"XInputExtension\0";
    if query_ext(
        dpy,
        name.as_ptr() as *const libc::c_char,
        &mut opcode,
        &mut event,
        &mut error,
    ) == 0
    {
        return None;
    }
    let xi = dlopen_quiet(b"libXi.so.6\0")?;
    let query_version: XiQueryVersion = sym(xi, "XIQueryVersion")?;
    let select: XiSelectEvents = sym(xi, "XISelectEvents")?;
    // From 2.1 on, raw events keep arriving while another client holds a grab,
    // such as the desktop's own grab of the Win key.
    let (mut major, mut minor) = (2, 2);
    if query_version(dpy, &mut major, &mut minor) != 0 || major < 2 {
        return None;
    }
    let mut bits = [0u8; 3];
    for ev in [XI_RAW_KEY_PRESS, XI_RAW_KEY_RELEASE, XI_RAW_MOTION] {
        bits[(ev >> 3) as usize] |= 1 << (ev & 7);
    }
    let mut mask = XiEventMask {
        deviceid: XI_ALL_MASTER_DEVICES,
        mask_len: bits.len() as c_int,
        mask: bits.as_mut_ptr(),
    };
    if select(dpy, root, &mut mask, 1) != 0 {
        return None;
    }
    Some(Xi {
        opcode,
        get_data: sym(x11, "XGetEventData")?,
        free_data: sym(x11, "XFreeEventData")?,
    })
}

/// Movement of x and y (valuators 0 and 1) in one raw motion event.
unsafe fn raw_delta(ev: &XiRawEvent) -> (f64, f64) {
    let (mut dx, mut dy) = (0.0, 0.0);
    if ev.mask.is_null() || ev.values.is_null() {
        return (dx, dy);
    }
    let mut value = 0usize;
    for bit in 0..(ev.mask_len.max(0) as usize * 8) {
        if *ev.mask.add(bit / 8) & (1 << (bit % 8)) == 0 {
            continue;
        }
        let v = *ev.values.add(value);
        match bit {
            0 => dx = v,
            1 => dy = v,
            _ => {}
        }
        value += 1;
        if bit >= 1 {
            break;
        }
    }
    (dx, dy)
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

/// The eight modifier keys (HID 0xE0..=0xE7) currently held.
#[derive(Default)]
struct HeldMods(u8);

impl HeldMods {
    fn set(&mut self, hid: u32, down: bool) {
        if let Some(i) = hid.checked_sub(0xE0).filter(|i| *i < 8) {
            if down {
                self.0 |= 1 << i;
            } else {
                self.0 &= !(1 << i);
            }
        }
    }

    /// As `keymap::modifier_bit` flags, either hand.
    fn bits(&self) -> u32 {
        (0..8)
            .filter(|i| self.0 & (1 << i) != 0)
            .fold(0, |bits, i| bits | modifier_bit(0xE0 + i))
    }
}

fn run(mut api: Api) {
    let mut grabbed: Option<GrabState> = None;
    let mut buf = [0 as c_long; 24];
    let mut frac = (0.0f64, 0.0f64);
    let mut held_mods = HeldMods::default();
    loop {
        let want = SHARED.lock().map(|s| s.want).unwrap_or(None);
        unsafe {
            let catch_paste = want.is_none() && PASTE_WANT.load(Ordering::Acquire);
            api.paste.set(api.dpy, api.root, api.flush, catch_paste);
            match (want, grabbed.as_mut()) {
                (Some(park), None) => grabbed = Some(try_grab(&api, park, None)),
                (Some(park), Some(g)) if g.last_attempt.is_some() => {
                    let retry = g
                        .last_attempt
                        .is_some_and(|t| t.elapsed() > Duration::from_millis(200));
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
                let kind = *(buf.as_ptr() as *const c_int);
                if kind == GENERIC_EVENT {
                    if let Some(xi) = api.xi.as_ref() {
                        let cookie = buf.as_mut_ptr() as *mut GenericCookie;
                        if (*cookie).extension == xi.opcode
                            && (xi.get_data)(api.dpy, cookie as *mut c_void) != 0
                        {
                            let evtype = (*cookie).evtype;
                            let data = (*cookie).data as *const XiRawEvent;
                            if evtype == XI_RAW_MOTION && !data.is_null() {
                                let (dx, dy) = raw_delta(&*data);
                                frac.0 += dx;
                                frac.1 += dy;
                                let (ix, iy) = (frac.0.trunc(), frac.1.trunc());
                                frac = (frac.0 - ix, frac.1 - iy);
                                RAW_DX.fetch_add(ix as i32, Ordering::AcqRel);
                                RAW_DY.fetch_add(iy as i32, Ordering::AcqRel);
                            } else if (evtype == XI_RAW_KEY_PRESS || evtype == XI_RAW_KEY_RELEASE)
                                && !data.is_null()
                            {
                                let evdev = ((*data).detail as u32).saturating_sub(8) as u16;
                                if let Some(hid) = crate::keymap::hid_from_evdev(evdev) {
                                    let down = evtype == XI_RAW_KEY_PRESS;
                                    held_mods.set(hid, down);
                                    // While grabbed the core sees the keys itself.
                                    if down && grabbed.is_none() {
                                        lock_key_down(hid, held_mods.bits());
                                    }
                                }
                            }
                            (xi.free_data)(api.dpy, cookie as *mut c_void);
                        }
                    }
                    continue;
                }
                if grabbed.is_none() && kind == KEY_PRESS {
                    let ev = &*(buf.as_ptr() as *const XInputEvent);
                    if api.paste.active && ev.detail as c_int == api.paste.keycode {
                        PASTE_HITS.fetch_add(1, Ordering::AcqRel);
                    }
                    continue;
                }
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
