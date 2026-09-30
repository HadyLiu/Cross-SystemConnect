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
    fn mouse_wheel(&mut self, v: i32, _h: i32) {
        let xbtn = if v > 0 {
            4
        } else if v < 0 {
            5
        } else {
            return;
        };
        xdo::x_button(xbtn, true);
        xdo::x_button(xbtn, false);
    }
    fn key(&mut self, _hid_usage: u32, _down: bool) {}
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
}

pub(crate) fn pointer_pos() -> Option<(i32, i32)> {
    xdo::pointer()
}

pub(crate) fn warp_cursor(x: i32, y: i32) {
    xdo::warp(x, y);
}
