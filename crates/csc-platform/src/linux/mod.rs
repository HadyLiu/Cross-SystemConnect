use crate::stub::{StubInputCapture, StubInputInjector};
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

pub struct LinuxInputInjector {
    inner: StubInputInjector,
}

impl LinuxInputInjector {
    pub fn new() -> Self {
        Self {
            inner: StubInputInjector,
        }
    }
}

impl InputInjector for LinuxInputInjector {
    fn mouse_move_abs(&mut self, x: i32, y: i32) {
        self.inner.mouse_move_abs(x, y);
    }
    fn mouse_move_rel(&mut self, dx: i32, dy: i32) {
        self.inner.mouse_move_rel(dx, dy);
    }
    fn mouse_button(&mut self, button: u32, down: bool) {
        self.inner.mouse_button(button, down);
    }
    fn mouse_wheel(&mut self, v: i32, h: i32) {
        self.inner.mouse_wheel(v, h);
    }
    fn key(&mut self, hid_usage: u32, down: bool) {
        self.inner.key(hid_usage, down);
    }
}
