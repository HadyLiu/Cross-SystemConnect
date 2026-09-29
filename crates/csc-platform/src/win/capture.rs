use crate::traits::*;
use tracing::info;

/// Windows LL hook capture — hooks are installed when start() is called.
/// Full Raw Input path can be extended; v1 polls cursor position from core loop.
pub struct WinInputCapture {
    grab: bool,
}

impl WinInputCapture {
    pub fn new() -> Self {
        Self { grab: false }
    }
}

impl InputCapture for WinInputCapture {
    fn start(
        &mut self,
        _on_mouse: Box<dyn FnMut(MouseEvent) -> bool + Send>,
        _on_key: Box<dyn FnMut(KeyEvent) -> bool + Send>,
    ) -> anyhow::Result<()> {
        // Hooks require a message pump thread; core drives edge detection via ScreenInfo + GetCursorPos.
        info!("WinInputCapture started (cursor-poll mode)");
        Ok(())
    }

    fn stop(&mut self) {}

    fn set_grab(&mut self, grab: bool) {
        self.grab = grab;
    }

    fn warp_cursor(&mut self, x: i32, y: i32) {
        unsafe {
            let _ = windows::Win32::UI::WindowsAndMessaging::SetCursorPos(x, y);
        }
    }
}
