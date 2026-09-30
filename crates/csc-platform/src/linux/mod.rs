pub mod grab;
pub mod xdnd;
mod xdo;

use crate::stub::StubInputCapture;
use crate::traits::*;
use tracing::info;

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
        xdo::move_abs(x, y);
    }
    fn mouse_move_rel(&mut self, dx: i32, dy: i32) {
        xdo::move_rel(dx, dy);
    }
    fn mouse_button(&mut self, button: u32, down: bool) {
        xdo::button(button, down);
    }
    fn mouse_wheel(&mut self, v: i32, h: i32) {
        // X11 scrolls with button clicks: 4 up, 5 down, 6 left, 7 right.
        let clicks = [(v, 4, 5), (h, 7, 6)];
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
            xdo::key(code as u32 + 8, down);
        }
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

pub(crate) fn pointer_pos() -> Option<(i32, i32)> {
    xdo::pointer()
}

pub(crate) fn primary_button_down() -> bool {
    const BUTTON1_MASK: u32 = 1 << 8;
    xdo::pointer_state().is_some_and(|(_, _, mask)| mask & BUTTON1_MASK != 0)
}

pub(crate) fn warp_cursor(x: i32, y: i32) {
    xdo::warp(x, y);
}
