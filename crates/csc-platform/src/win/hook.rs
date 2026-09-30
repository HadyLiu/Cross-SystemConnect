//! Low-level mouse and keyboard hooks for the keyboard host.
//!
//! While grabbed, every physical mouse and key event is swallowed and queued,
//! so it reaches only the remote computer. The cursor stays on the park point;
//! each move is reported as the distance from that point.
//! Windows drops a hook whose callback is slow, so callbacks only queue.

use crate::keymap::hid_from_scan;
use crate::traits::Captured;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::{Mutex, Once};
use tracing::{info, warn};
use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, GetMessageW, SetCursorPos, SetWindowsHookExW, HHOOK, KBDLLHOOKSTRUCT,
    MSG, MSLLHOOKSTRUCT, WH_KEYBOARD_LL, WH_MOUSE_LL,
};

const WM_MOUSEMOVE: u32 = 0x0200;
const WM_LBUTTONDOWN: u32 = 0x0201;
const WM_LBUTTONUP: u32 = 0x0202;
const WM_RBUTTONDOWN: u32 = 0x0204;
const WM_RBUTTONUP: u32 = 0x0205;
const WM_MBUTTONDOWN: u32 = 0x0207;
const WM_MBUTTONUP: u32 = 0x0208;
const WM_MOUSEWHEEL: u32 = 0x020A;
const WM_XBUTTONDOWN: u32 = 0x020B;
const WM_XBUTTONUP: u32 = 0x020C;
const WM_MOUSEHWHEEL: u32 = 0x020E;
const LLMHF_INJECTED: u32 = 0x01;
const LLKHF_EXTENDED: u32 = 0x01;
const LLKHF_INJECTED: u32 = 0x10;
const LLKHF_UP: u32 = 0x80;
const WHEEL_DELTA: i32 = 120;

static START: Once = Once::new();
static HOOKED: AtomicBool = AtomicBool::new(false);
static GRAB: AtomicBool = AtomicBool::new(false);
static PARK_X: AtomicI32 = AtomicI32::new(0);
static PARK_Y: AtomicI32 = AtomicI32::new(0);
static WHEEL_V: AtomicI32 = AtomicI32::new(0);
static WHEEL_H: AtomicI32 = AtomicI32::new(0);
static QUEUE: Mutex<VecDeque<Captured>> = Mutex::new(VecDeque::new());

/// Install the hooks on their own message-loop thread. Returns whether they are active.
pub fn start() -> bool {
    START.call_once(|| {
        let (tx, rx) = std::sync::mpsc::channel();
        let spawned = std::thread::Builder::new()
            .name("csc-input-hook".into())
            .spawn(move || hook_thread(tx));
        if spawned.is_ok() {
            if let Ok(ok) = rx.recv() {
                HOOKED.store(ok, Ordering::SeqCst);
            }
        }
    });
    HOOKED.load(Ordering::SeqCst)
}

fn hook_thread(ready: std::sync::mpsc::Sender<bool>) {
    unsafe {
        let module: HINSTANCE = match GetModuleHandleW(None) {
            Ok(m) => m.into(),
            Err(e) => {
                warn!("GetModuleHandleW: {e}");
                let _ = ready.send(false);
                return;
            }
        };
        let mouse = SetWindowsHookExW(WH_MOUSE_LL, Some(mouse_proc), module, 0);
        let keyboard = SetWindowsHookExW(WH_KEYBOARD_LL, Some(keyboard_proc), module, 0);
        match (&mouse, &keyboard) {
            (Ok(_), Ok(_)) => info!("input hooks installed"),
            _ => {
                warn!(
                    "input hooks failed (mouse: {:?}, keyboard: {:?}); buttons and keys will not reach remote screens",
                    mouse.as_ref().err(),
                    keyboard.as_ref().err()
                );
                let _ = ready.send(false);
                return;
            }
        }
        let _ = ready.send(true);
        let mut msg = MSG::default();
        while GetMessageW(&mut msg, HWND::default(), 0, 0).as_bool() {}
    }
}

/// Start or stop swallowing input. `park` is the absolute screen point the
/// cursor is held at while grabbed.
pub fn set_grab(grab: bool, park: Option<(i32, i32)>) {
    if let Some((x, y)) = park {
        PARK_X.store(x, Ordering::SeqCst);
        PARK_Y.store(y, Ordering::SeqCst);
        unsafe {
            let _ = SetCursorPos(x, y);
        }
    }
    WHEEL_V.store(0, Ordering::SeqCst);
    WHEEL_H.store(0, Ordering::SeqCst);
    GRAB.store(grab, Ordering::SeqCst);
    if !grab {
        if let Ok(mut q) = QUEUE.lock() {
            q.clear();
        }
    }
}

pub fn drain() -> Vec<Captured> {
    match QUEUE.lock() {
        Ok(mut q) => q.drain(..).collect(),
        Err(_) => Vec::new(),
    }
}

fn push(ev: Captured) {
    if let Ok(mut q) = QUEUE.lock() {
        // Coalesce moves so a slow consumer never sees a long backlog.
        if let (Captured::Move { dx, dy }, Some(Captured::Move { dx: pdx, dy: pdy })) =
            (ev, q.back_mut())
        {
            *pdx += dx;
            *pdy += dy;
            return;
        }
        q.push_back(ev);
    }
}

fn wheel_notches(acc: &AtomicI32, delta: i32) -> i32 {
    let total = acc.fetch_add(delta, Ordering::SeqCst) + delta;
    let notches = total / WHEEL_DELTA;
    if notches != 0 {
        acc.fetch_sub(notches * WHEEL_DELTA, Ordering::SeqCst);
    }
    notches
}

unsafe extern "system" fn mouse_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code >= 0 && GRAB.load(Ordering::Relaxed) {
        let info = &*(lparam.0 as *const MSLLHOOKSTRUCT);
        if info.flags & LLMHF_INJECTED == 0 {
            let high = (info.mouseData >> 16) as u16;
            let ev = match wparam.0 as u32 {
                WM_MOUSEMOVE => {
                    let dx = info.pt.x - PARK_X.load(Ordering::Relaxed);
                    let dy = info.pt.y - PARK_Y.load(Ordering::Relaxed);
                    (dx != 0 || dy != 0).then_some(Captured::Move { dx, dy })
                }
                WM_LBUTTONDOWN => Some(Captured::Button { button: 0, down: true }),
                WM_LBUTTONUP => Some(Captured::Button { button: 0, down: false }),
                WM_RBUTTONDOWN => Some(Captured::Button { button: 1, down: true }),
                WM_RBUTTONUP => Some(Captured::Button { button: 1, down: false }),
                WM_MBUTTONDOWN => Some(Captured::Button { button: 2, down: true }),
                WM_MBUTTONUP => Some(Captured::Button { button: 2, down: false }),
                WM_XBUTTONDOWN | WM_XBUTTONUP => Some(Captured::Button {
                    button: if high == 1 { 3 } else { 4 },
                    down: wparam.0 as u32 == WM_XBUTTONDOWN,
                }),
                WM_MOUSEWHEEL => {
                    let v = wheel_notches(&WHEEL_V, high as i16 as i32);
                    (v != 0).then_some(Captured::Wheel { v, h: 0 })
                }
                WM_MOUSEHWHEEL => {
                    let h = wheel_notches(&WHEEL_H, high as i16 as i32);
                    (h != 0).then_some(Captured::Wheel { v: 0, h })
                }
                _ => None,
            };
            if let Some(ev) = ev {
                push(ev);
            }
            return LRESULT(1);
        }
    }
    CallNextHookEx(HHOOK::default(), code, wparam, lparam)
}

unsafe extern "system" fn keyboard_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code >= 0 && GRAB.load(Ordering::Relaxed) {
        let info = &*(lparam.0 as *const KBDLLHOOKSTRUCT);
        let flags = info.flags.0;
        if flags & LLKHF_INJECTED == 0 {
            let extended = flags & LLKHF_EXTENDED != 0;
            if let Some(hid) = hid_from_scan(info.scanCode as u16, extended) {
                push(Captured::Key {
                    hid,
                    down: flags & LLKHF_UP == 0,
                });
            }
            return LRESULT(1);
        }
    }
    CallNextHookEx(HHOOK::default(), code, wparam, lparam)
}
