use crate::traits::{Rect, ScreenInfo, ScreenSize};
use windows::Win32::Foundation::{BOOL, LPARAM, RECT};
use windows::Win32::Graphics::Gdi::{
    EnumDisplayMonitors, GetMonitorInfoW, HDC, HMONITOR, MONITORINFO,
};
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

    fn monitors(&self) -> Vec<Rect> {
        let (ox, oy) = virtual_origin();
        let mut out: Vec<Rect> = Vec::new();
        unsafe {
            let _ = EnumDisplayMonitors(
                HDC::default(),
                None,
                Some(collect_monitor),
                LPARAM(&mut out as *mut Vec<Rect> as isize),
            );
        }
        for r in &mut out {
            r.left -= ox;
            r.right -= ox;
            r.top -= oy;
            r.bottom -= oy;
        }
        if out.is_empty() {
            let s = self.virtual_size();
            out.push(Rect {
                left: 0,
                top: 0,
                right: s.width,
                bottom: s.height,
            });
        }
        out
    }
}

unsafe extern "system" fn collect_monitor(
    monitor: HMONITOR,
    _hdc: HDC,
    _clip: *mut RECT,
    data: LPARAM,
) -> BOOL {
    let out = &mut *(data.0 as *mut Vec<Rect>);
    let mut info = MONITORINFO {
        cbSize: std::mem::size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    if GetMonitorInfoW(monitor, &mut info).as_bool() {
        let r = info.rcMonitor;
        out.push(Rect {
            left: r.left,
            top: r.top,
            right: r.right,
            bottom: r.bottom,
        });
    }
    BOOL(1)
}

/// Use physical pixels everywhere. Without this, a scaled monitor reports
/// logical coordinates and cursor, monitor, and injected positions disagree.
pub fn init_dpi_awareness() {
    use windows::Win32::UI::HiDpi::{
        SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
    };
    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
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

pub fn primary_button_down() -> bool {
    use windows::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, VK_LBUTTON, VK_RBUTTON};
    use windows::Win32::UI::WindowsAndMessaging::SM_SWAPBUTTON;
    unsafe {
        // GetAsyncKeyState reports physical buttons; the primary one may be swapped.
        let key = if GetSystemMetrics(SM_SWAPBUTTON) != 0 {
            VK_RBUTTON
        } else {
            VK_LBUTTON
        };
        (GetAsyncKeyState(key.0 as i32) as u16 & 0x8000) != 0
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
