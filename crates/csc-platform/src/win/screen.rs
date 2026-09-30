use crate::traits::{ScreenInfo, ScreenSize};
use windows::Win32::UI::WindowsAndMessaging::{
    GetSystemMetrics, ShowCursor, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN,
    SM_YVIRTUALSCREEN,
};

pub struct WinScreenInfo;

impl ScreenInfo for WinScreenInfo {
    fn virtual_size(&self) -> ScreenSize {
        unsafe {
            ScreenSize {
                width: GetSystemMetrics(SM_CXVIRTUALSCREEN),
                height: GetSystemMetrics(SM_CYVIRTUALSCREEN),
            }
        }
    }
}

pub fn cursor_pos() -> (i32, i32) {
    use windows::Win32::Foundation::POINT;
    use windows::Win32::UI::WindowsAndMessaging::GetCursorPos;
    unsafe {
        let mut pt = POINT::default();
        let _ = GetCursorPos(&mut pt);
        (pt.x, pt.y)
    }
}

pub fn virtual_origin() -> (i32, i32) {
    unsafe {
        (
            GetSystemMetrics(SM_XVIRTUALSCREEN),
            GetSystemMetrics(SM_YVIRTUALSCREEN),
        )
    }
}

pub fn warp_cursor(x: i32, y: i32) {
    unsafe {
        let _ = windows::Win32::UI::WindowsAndMessaging::SetCursorPos(x, y);
    }
}

/// `ShowCursor` is a counter. Call this once when hiding and once when showing.
pub fn set_cursor_visible(visible: bool) {
    unsafe {
        let _ = ShowCursor(visible);
    }
}
