pub(crate) mod clip_x11;
pub(crate) mod cursor;
pub(crate) mod drop_folder;
pub(crate) mod fullscreen;
pub mod grab;
pub(crate) mod leds;
pub(crate) mod paste_hold;
pub mod portal;
pub(crate) mod power;
pub(crate) mod uinput;
mod xdo;

use crate::stub::StubInputCapture;
use crate::traits::*;
use std::sync::atomic::{AtomicI32, Ordering};
use tracing::info;

const WHEEL_DELTA: i32 = 120;
static X_WHEEL_V: AtomicI32 = AtomicI32::new(0);
static X_WHEEL_H: AtomicI32 = AtomicI32::new(0);

/// Add 1/120 units to `acc` and take out the whole notches it now holds.
pub(crate) fn wheel_notches(acc: &AtomicI32, units: i32) -> i32 {
    let total = acc.fetch_add(units, Ordering::AcqRel).saturating_add(units);
    let notches = total / WHEEL_DELTA;
    if notches != 0 {
        acc.fetch_sub(notches * WHEEL_DELTA, Ordering::AcqRel);
    }
    notches
}

pub struct LinuxInputCapture {
    inner: StubInputCapture,
}

impl LinuxInputCapture {
    pub fn new() -> Self {
        Self {
            inner: StubInputCapture,
        }
    }
}

impl InputCapture for LinuxInputCapture {
    fn start(
        &mut self,
        on_mouse: Box<dyn FnMut(MouseEvent) -> bool + Send>,
        on_key: Box<dyn FnMut(KeyEvent) -> bool + Send>,
    ) -> anyhow::Result<()> {
        info!("LinuxInputCapture: X11 path stub — extend with XInput2");
        self.inner.start(on_mouse, on_key)
    }
    fn stop(&mut self) {
        self.inner.stop();
    }
    fn set_grab(&mut self, grab: bool) {
        self.inner.set_grab(grab);
    }
    fn warp_cursor(&mut self, x: i32, y: i32) {
        self.inner.warp_cursor(x, y);
    }
}

pub struct LinuxInputInjector;

impl LinuxInputInjector {
    pub fn new() -> Self {
        Self
    }
}

impl InputInjector for LinuxInputInjector {
    fn mouse_move_abs(&mut self, x: i32, y: i32) {
        if wayland_session() {
            if let Some((cx, cy)) = xdo::pointer() {
                uinput::move_rel(x - cx, y - cy);
            }
            return;
        }
        xdo::move_abs(x, y);
    }
    fn mouse_move_rel(&mut self, dx: i32, dy: i32) {
        if wayland_session() {
            uinput::move_rel(dx, dy);
            return;
        }
        xdo::move_rel(dx, dy);
    }
    fn mouse_button(&mut self, button: u32, down: bool) {
        if wayland_session() {
            uinput::button(button, down);
            return;
        }
        xdo::button(button, down);
    }
    fn mouse_wheel(&mut self, v: i32, h: i32) {
        if wayland_session() {
            uinput::wheel(v, h);
            return;
        }
        // X11 scrolls with button clicks: 4 up, 5 down, 6 left, 7 right.
        let clicks = [
            (wheel_notches(&X_WHEEL_V, v), 4, 5),
            (wheel_notches(&X_WHEEL_H, h), 7, 6),
        ];
        for (amount, positive, negative) in clicks {
            let xbtn = if amount > 0 { positive } else { negative };
            for _ in 0..amount.unsigned_abs().min(20) {
                xdo::x_button(xbtn, true);
                xdo::x_button(xbtn, false);
            }
        }
    }
    fn key(&mut self, hid_usage: u32, down: bool) {
        if let Some(code) = crate::keymap::evdev_from_hid(hid_usage) {
            if wayland_session() {
                uinput::key(code, down);
                return;
            }
            xdo::key(code as u32 + 8, down);
        }
    }
}

/// Folder to paste files into, when the focus is a file manager or the desktop.
pub(crate) fn paste_folder() -> Option<std::path::PathBuf> {
    drop_folder::paste_folder()
}

/// Wayland turns XTest into a Remote Desktop prompt. Inject with uinput instead.
pub(crate) fn wayland_session() -> bool {
    match std::env::var("XDG_SESSION_TYPE") {
        Ok(kind) if kind.eq_ignore_ascii_case("wayland") => true,
        Ok(kind) if kind.eq_ignore_ascii_case("x11") => false,
        _ => std::env::var_os("WAYLAND_DISPLAY").is_some(),
    }
}

pub struct LinuxScreenInfo;

impl ScreenInfo for LinuxScreenInfo {
    fn virtual_size(&self) -> ScreenSize {
        xdo::screen_size()
            .map(|(width, height)| ScreenSize { width, height })
            .unwrap_or(ScreenSize {
                width: 1920,
                height: 1080,
            })
    }

    fn monitors(&self) -> Vec<Rect> {
        let list: Vec<Rect> = xdo::monitors()
            .into_iter()
            .map(|(x, y, w, h)| Rect {
                left: x,
                top: y,
                right: x + w,
                bottom: y + h,
            })
            .collect();
        if list.is_empty() {
            let s = self.virtual_size();
            vec![Rect {
                left: 0,
                top: 0,
                right: s.width,
                bottom: s.height,
            }]
        } else {
            list
        }
    }
}

/// On a Wayland host with the portal, XWayland's pointer is stale outside X11
/// windows; edges come from barrier hits instead.
pub(crate) fn pointer_pos() -> Option<(i32, i32)> {
    if wayland_session() && portal::ready() {
        return None;
    }
    xdo::pointer()
}

pub(crate) fn primary_button_down() -> bool {
    if wayland_session() && uinput::left_down() {
        return true;
    }
    const BUTTON1_MASK: u32 = 1 << 8;
    xdo::pointer_state().is_some_and(|(_, _, mask)| mask & BUTTON1_MASK != 0)
}

pub(crate) fn warp_cursor(x: i32, y: i32) {
    if wayland_session() && portal::ready() {
        return;
    }
    if wayland_session() {
        if let Some((cx, cy)) = xdo::pointer() {
            uinput::move_rel(x - cx, y - cy);
        }
        return;
    }
    xdo::warp(x, y);
}
