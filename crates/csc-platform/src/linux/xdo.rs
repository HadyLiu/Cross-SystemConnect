//! X11 pointer injection via libXtst. Loaded at runtime so a missing library
//! does not stop the process. Wayland sessions without XWayland cannot be driven.
//! On Wayland, absolute warps are rejected after the remote-interaction prompt;
//! callers send relative motion instead.

use libc::{c_int, c_uint, c_ulong, c_void};
use std::sync::Mutex;
use tracing::warn;

type OpenDisplay = unsafe extern "C" fn(*const libc::c_char) -> *mut c_void;
type InitThreads = unsafe extern "C" fn() -> c_int;
type DefaultScreen = unsafe extern "C" fn(*mut c_void) -> c_int;
type DisplayWidth = unsafe extern "C" fn(*mut c_void, c_int) -> c_int;
type DisplayHeight = unsafe extern "C" fn(*mut c_void, c_int) -> c_int;
type RootWindow = unsafe extern "C" fn(*mut c_void, c_int) -> c_ulong;
type Flush = unsafe extern "C" fn(*mut c_void) -> c_int;
type QueryPointer = unsafe extern "C" fn(
    *mut c_void,
    c_ulong,
    *mut c_ulong,
    *mut c_ulong,
    *mut c_int,
    *mut c_int,
    *mut c_int,
    *mut c_int,
    *mut c_uint,
) -> c_int;
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
type FakeMotion = unsafe extern "C" fn(*mut c_void, c_int, c_int, c_int, c_ulong) -> c_int;
type FakeButton = unsafe extern "C" fn(*mut c_void, c_uint, c_int, c_ulong) -> c_int;
type FakeKey = unsafe extern "C" fn(*mut c_void, c_uint, c_int, c_ulong) -> c_int;

struct Conn {
    dpy: *mut c_void,
    screen: c_int,
    root: c_ulong,
    width: i32,
    height: i32,
    flush: Flush,
    query: QueryPointer,
    warp: WarpPointer,
    fake_motion: FakeMotion,
    fake_rel: FakeMotion,
    fake_button: FakeButton,
    fake_key: FakeKey,
}

unsafe impl Send for Conn {}

struct Slot {
    conn: Option<Conn>,
    failed: bool,
}

static SLOT: Mutex<Slot> = Mutex::new(Slot {
    conn: None,
    failed: false,
});

fn with<R>(f: impl FnOnce(&mut Conn) -> R) -> Option<R> {
    let mut slot = SLOT.lock().ok()?;
    if slot.conn.is_none() && !slot.failed {
        match open() {
            Some(conn) => slot.conn = Some(conn),
            None => slot.failed = true,
        }
    }
    slot.conn.as_mut().map(f)
}

pub fn screen_size() -> Option<(i32, i32)> {
    with(|c| (c.width, c.height))
}

/// Pointer position and the X button/modifier mask.
pub fn pointer_state() -> Option<(i32, i32, u32)> {
    with(|c| unsafe {
        let (mut root, mut child) = (0, 0);
        let (mut rx, mut ry, mut wx, mut wy) = (0, 0, 0, 0);
        let mut mask = 0;
        let ok = (c.query)(
            c.dpy, c.root, &mut root, &mut child, &mut rx, &mut ry, &mut wx, &mut wy, &mut mask,
        );
        (ok != 0).then_some((rx, ry, mask))
    })
    .flatten()
}

pub fn pointer() -> Option<(i32, i32)> {
    with(|c| unsafe {
        let mut root = 0;
        let mut child = 0;
        let mut rx = 0;
        let mut ry = 0;
        let mut wx = 0;
        let mut wy = 0;
        let mut mask = 0;
        let ok = (c.query)(
            c.dpy, c.root, &mut root, &mut child, &mut rx, &mut ry, &mut wx, &mut wy, &mut mask,
        );
        if ok == 0 {
            None
        } else {
            Some((rx, ry))
        }
    })
    .flatten()
}

pub fn warp(x: i32, y: i32) {
    let _ = with(|c| unsafe {
        (c.warp)(c.dpy, 0, c.root, 0, 0, 0, 0, x, y);
        (c.flush)(c.dpy);
    });
}

pub fn move_abs(x: i32, y: i32) {
    let _ = with(|c| unsafe {
        (c.fake_motion)(c.dpy, c.screen, x, y, 0);
        (c.flush)(c.dpy);
    });
}

pub fn move_rel(dx: i32, dy: i32) {
    // One huge relative event is dropped by some compositors. Step across.
    const STEP: i32 = 400;
    let (mut left_x, mut left_y) = (dx, dy);
    while left_x != 0 || left_y != 0 {
        let sx = left_x.clamp(-STEP, STEP);
        let sy = left_y.clamp(-STEP, STEP);
        let _ = with(|c| unsafe {
            (c.fake_rel)(c.dpy, c.screen, sx, sy, 0);
            (c.flush)(c.dpy);
        });
        left_x -= sx;
        left_y -= sy;
    }
}

pub fn button(button: u32, down: bool) {
    let xbtn = match button {
        0 => 1,
        1 => 3,
        2 => 2,
        3 => 8,
        4 => 9,
        _ => return,
    };
    x_button(xbtn, down);
}

/// Press or release an X11 keycode (evdev code + 8).
pub fn key(keycode: u32, down: bool) {
    let _ = with(|c| unsafe {
        (c.fake_key)(c.dpy, keycode, if down { 1 } else { 0 }, 0);
        (c.flush)(c.dpy);
    });
}

pub fn x_button(xbtn: u32, down: bool) {
    let _ = with(|c| unsafe {
        (c.fake_button)(c.dpy, xbtn, if down { 1 } else { 0 }, 0);
        (c.flush)(c.dpy);
    });
}

fn open() -> Option<Conn> {
    unsafe {
        let x11 = dlopen(b"libX11.so.6\0")?;
        let xtst = dlopen(b"libXtst.so.6\0")?;
        let init: InitThreads = sym(x11, "XInitThreads")?;
        let open_display: OpenDisplay = sym(x11, "XOpenDisplay")?;
        let default_screen: DefaultScreen = sym(x11, "XDefaultScreen")?;
        let display_width: DisplayWidth = sym(x11, "XDisplayWidth")?;
        let display_height: DisplayHeight = sym(x11, "XDisplayHeight")?;
        let root_window: RootWindow = sym(x11, "XRootWindow")?;
        let flush: Flush = sym(x11, "XFlush")?;
        let query: QueryPointer = sym(x11, "XQueryPointer")?;
        let warp: WarpPointer = sym(x11, "XWarpPointer")?;
        let fake_motion: FakeMotion = sym(xtst, "XTestFakeMotionEvent")?;
        let fake_rel: FakeMotion = sym(xtst, "XTestFakeRelativeMotionEvent")?;
        let fake_button: FakeButton = sym(xtst, "XTestFakeButtonEvent")?;
        let fake_key: FakeKey = sym(xtst, "XTestFakeKeyEvent")?;
        init();
        let dpy = open_display(std::ptr::null());
        if dpy.is_null() {
            warn!("XOpenDisplay failed; pointer injection needs X11 or XWayland (DISPLAY)");
            return None;
        }
        let screen = default_screen(dpy);
        let width = display_width(dpy, screen);
        let height = display_height(dpy, screen);
        let root = root_window(dpy, screen);
        if width <= 1 || height <= 1 {
            warn!("X11 reported an empty screen");
            return None;
        }
        Some(Conn {
            dpy,
            screen,
            root,
            width,
            height,
            flush,
            query,
            warp,
            fake_motion,
            fake_rel,
            fake_button,
            fake_key,
        })
    }
}

#[repr(C)]
struct XineramaScreenInfo {
    screen_number: c_int,
    x_org: libc::c_short,
    y_org: libc::c_short,
    width: libc::c_short,
    height: libc::c_short,
}

type XineramaIsActive = unsafe extern "C" fn(*mut c_void) -> c_int;
type XineramaQueryScreens =
    unsafe extern "C" fn(*mut c_void, *mut c_int) -> *mut XineramaScreenInfo;
type XFree = unsafe extern "C" fn(*mut c_void) -> c_int;

/// Physical monitors as `(x, y, width, height)`, from Xinerama. Empty when
/// Xinerama is missing or inactive.
pub fn monitors() -> Vec<(i32, i32, i32, i32)> {
    with(|c| unsafe {
        let mut out = Vec::new();
        let Some(lib) = dlopen_quiet(b"libXinerama.so.1\0") else {
            return out;
        };
        let (Some(active), Some(query), Some(x11)) = (
            sym::<XineramaIsActive>(lib, "XineramaIsActive"),
            sym::<XineramaQueryScreens>(lib, "XineramaQueryScreens"),
            dlopen_quiet(b"libX11.so.6\0"),
        ) else {
            return out;
        };
        let Some(free) = sym::<XFree>(x11, "XFree") else {
            return out;
        };
        if active(c.dpy) == 0 {
            return out;
        }
        let mut n = 0;
        let list = query(c.dpy, &mut n);
        if list.is_null() {
            return out;
        }
        for i in 0..n.max(0) as usize {
            let s = &*list.add(i);
            if s.width > 0 && s.height > 0 {
                out.push((
                    s.x_org as i32,
                    s.y_org as i32,
                    s.width as i32,
                    s.height as i32,
                ));
            }
        }
        free(list as *mut c_void);
        out
    })
    .unwrap_or_default()
}

pub(crate) unsafe fn dlopen_quiet(name: &[u8]) -> Option<*mut c_void> {
    let handle = libc::dlopen(
        name.as_ptr() as *const libc::c_char,
        libc::RTLD_NOW | libc::RTLD_GLOBAL,
    );
    (!handle.is_null()).then_some(handle)
}

pub(crate) unsafe fn dlopen(name: &[u8]) -> Option<*mut c_void> {
    let handle = libc::dlopen(
        name.as_ptr() as *const libc::c_char,
        libc::RTLD_NOW | libc::RTLD_GLOBAL,
    );
    if handle.is_null() {
        let err = libc::dlerror();
        let msg = if err.is_null() {
            "unknown".to_string()
        } else {
            std::ffi::CStr::from_ptr(err).to_string_lossy().into_owned()
        };
        warn!(
            "cannot load {}: {msg}",
            String::from_utf8_lossy(name).trim_end_matches('\0')
        );
        None
    } else {
        Some(handle)
    }
}

pub(crate) unsafe fn sym<T: Copy>(handle: *mut c_void, name: &str) -> Option<T> {
    let cstr = std::ffi::CString::new(name).ok()?;
    let ptr = libc::dlsym(handle, cstr.as_ptr());
    if ptr.is_null() {
        warn!("missing symbol {name}");
        None
    } else {
        Some(std::mem::transmute_copy(&ptr))
    }
}
