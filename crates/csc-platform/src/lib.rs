//! Platform abstractions for input, clipboard, screen, and host info.

pub mod clip;
mod host;
pub mod keymap;
mod lock;
mod notify;
mod traits;
pub mod vdrag;

pub use lock::{lock_hits, lock_screen, lock_watch};
pub use notify::notify;

#[cfg(all(unix, not(target_os = "macos")))]
mod linux;
mod stub;
#[cfg(windows)]
pub mod win;

pub use host::*;
pub use traits::*;

pub fn create_input_capture() -> Box<dyn InputCapture> {
    #[cfg(windows)]
    {
        Box::new(win::WinInputCapture::new())
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        Box::new(linux::LinuxInputCapture::new())
    }
    #[cfg(target_os = "macos")]
    {
        Box::new(stub::StubInputCapture)
    }
}

pub fn create_input_injector() -> Box<dyn InputInjector> {
    #[cfg(windows)]
    {
        Box::new(win::WinInputInjector::new())
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        Box::new(linux::LinuxInputInjector::new())
    }
    #[cfg(target_os = "macos")]
    {
        Box::new(stub::StubInputInjector)
    }
}

pub fn create_clipboard() -> Box<dyn Clipboard> {
    Box::new(stub::StubClipboard::default())
}

pub fn create_drag_drop() -> Box<dyn DragDrop> {
    Box::new(stub::StubDragDrop)
}

pub fn create_screen_info() -> Box<dyn ScreenInfo> {
    #[cfg(windows)]
    {
        Box::new(win::WinScreenInfo)
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        Box::new(linux::LinuxScreenInfo)
    }
    #[cfg(not(any(windows, all(unix, not(target_os = "macos")))))]
    {
        Box::new(stub::StubScreenInfo)
    }
}

/// Cursor position in virtual-desktop pixels. `None` when this platform cannot read it.
#[cfg(windows)]
pub fn cursor_pos() -> Option<(i32, i32)> {
    Some(win::cursor_pos())
}

#[cfg(all(unix, not(target_os = "macos")))]
pub fn cursor_pos() -> Option<(i32, i32)> {
    linux::pointer_pos()
}

#[cfg(not(any(windows, all(unix, not(target_os = "macos")))))]
pub fn cursor_pos() -> Option<(i32, i32)> {
    None
}

#[cfg(windows)]
pub fn virtual_origin() -> (i32, i32) {
    win::virtual_origin()
}

#[cfg(not(windows))]
pub fn virtual_origin() -> (i32, i32) {
    (0, 0)
}

/// Wayland moves the pointer with a virtual device. The X11 cursor position
/// does not follow that device, so callers must not treat it as the real pointer.
pub fn wayland_session() -> bool {
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        linux::wayland_session()
    }
    #[cfg(not(all(unix, not(target_os = "macos"))))]
    {
        false
    }
}

#[cfg(windows)]
pub fn warp_cursor(x: i32, y: i32) {
    win::warp_cursor(x, y);
}

#[cfg(all(unix, not(target_os = "macos")))]
pub fn warp_cursor(x: i32, y: i32) {
    linux::warp_cursor(x, y);
}

#[cfg(not(any(windows, all(unix, not(target_os = "macos")))))]
pub fn warp_cursor(_x: i32, _y: i32) {}

#[cfg(windows)]
pub fn set_cursor_visible(visible: bool) {
    win::set_cursor_visible(visible);
}

#[cfg(not(windows))]
pub fn set_cursor_visible(_visible: bool) {}

/// True while the primary (left) mouse button is held, for example during a drag.
pub fn primary_button_down() -> bool {
    #[cfg(windows)]
    {
        win::primary_button_down()
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        linux::primary_button_down()
    }
    #[cfg(not(any(windows, all(unix, not(target_os = "macos")))))]
    {
        false
    }
}

/// Process-wide setup that must run before any screen or cursor call.
pub fn init_process() {
    #[cfg(windows)]
    win::init_dpi_awareness();
}

/// Start capturing local input for forwarding. Returns false where the
/// platform has no capture; the core then falls back to polling the cursor
/// and cannot forward buttons or keys.
pub fn capture_start() -> bool {
    #[cfg(windows)]
    {
        win::hook::start()
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        linux::grab::start()
    }
    #[cfg(not(any(windows, all(unix, not(target_os = "macos")))))]
    {
        false
    }
}

/// Swallow local input and hold the cursor at `park` (absolute screen pixels),
/// or give input back to this computer.
pub fn capture_set_grab(grab: bool, park: Option<(i32, i32)>) {
    #[cfg(windows)]
    win::hook::set_grab(grab, park);
    #[cfg(all(unix, not(target_os = "macos")))]
    linux::grab::set_grab(grab, park);
    #[cfg(not(any(windows, all(unix, not(target_os = "macos")))))]
    let _ = (grab, park);
}

/// Input captured since the last call.
pub fn capture_drain() -> Vec<Captured> {
    #[cfg(windows)]
    {
        win::hook::drain()
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        linux::grab::drain()
    }
    #[cfg(not(any(windows, all(unix, not(target_os = "macos")))))]
    {
        Vec::new()
    }
}

/// Hand movement of the local mouse since the last call, before the system
/// clips the cursor at the screen edge. None where it cannot be measured.
pub fn take_raw_motion() -> Option<(i32, i32)> {
    #[cfg(windows)]
    {
        win::hook::take_raw_motion()
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        linux::grab::take_raw_motion()
    }
    #[cfg(not(any(windows, all(unix, not(target_os = "macos")))))]
    {
        None
    }
}

/// Catch Ctrl+V on this computer (while it is not grabbed) instead of letting
/// the focused program paste. Used while the last copy was files on another computer.
pub fn paste_watch(enable: bool) {
    #[cfg(windows)]
    win::hook::paste_watch(enable);
    #[cfg(all(unix, not(target_os = "macos")))]
    linux::grab::paste_watch(enable);
    #[cfg(not(any(windows, all(unix, not(target_os = "macos")))))]
    let _ = enable;
}

/// Ctrl+V presses caught since the last call.
pub fn paste_hits() -> u32 {
    #[cfg(windows)]
    {
        win::hook::paste_hits()
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        linux::grab::paste_hits()
    }
    #[cfg(not(any(windows, all(unix, not(target_os = "macos")))))]
    {
        0
    }
}

pub fn create_auto_start() -> Box<dyn AutoStart> {
    Box::new(stub::StubAutoStart::default())
}

/// Windows UDP sockets report ICMP unreachable as a connection reset and then
/// drop the socket's next read. Discovery scans a whole subnet, so that reset
/// has to be turned off. Other systems ignore the call.
#[cfg(windows)]
pub fn tune_udp_socket(raw: std::os::windows::io::RawSocket) {
    win::disable_udp_connreset(raw);
}
