//! Platform abstractions for input, clipboard, screen, and host info.

mod host;
mod traits;

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
    #[cfg(not(windows))]
    {
        Box::new(stub::StubScreenInfo)
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
