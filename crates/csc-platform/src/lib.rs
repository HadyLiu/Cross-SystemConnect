//! Platform abstractions for input, clipboard, screen, and host info.

pub mod clip;
mod host;
pub mod keymap;
#[cfg(any(windows, all(unix, not(target_os = "macos"))))]
mod keyrepeat;
mod lock;
mod notify;
mod traits;
pub mod vdrag;

pub use lock::{lock_hits, lock_screen, lock_watch, mods_held, pause_hits, pause_watch, stay_hits, stay_watch};
pub use notify::notify;

/// The machine is about to sleep, shutting down, or has woken.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PowerEvent {
    Suspending,
    ShuttingDown,
    Resumed,
    /// The session lock screen is up. Display only; input can still be injected.
    Locked,
    Unlocked,
    /// Windows lock, UAC, or the secure attention screen. The host must take the pointer back.
    SecureDesktop,
}

/// Ask the OS not to idle-sleep while connected computers are in use.
pub fn set_keep_awake(on: bool) {
    #[cfg(all(unix, not(target_os = "macos")))]
    linux::power::set_keep_awake(on);
    #[cfg(windows)]
    {
        use windows::Win32::System::Power::{
            SetThreadExecutionState, ES_CONTINUOUS, ES_DISPLAY_REQUIRED, ES_SYSTEM_REQUIRED,
        };
        unsafe {
            if on {
                let _ = SetThreadExecutionState(
                    ES_CONTINUOUS | ES_SYSTEM_REQUIRED | ES_DISPLAY_REQUIRED,
                );
            } else {
                let _ = SetThreadExecutionState(ES_CONTINUOUS);
            }
        }
    }
    #[cfg(not(any(windows, all(unix, not(target_os = "macos")))))]
    {
        let _ = on;
    }
}

/// Ask the OS to treat this computer as recently used (anti-idle / anti-blank).
pub fn note_user_activity() {
    #[cfg(all(unix, not(target_os = "macos")))]
    linux::power::note_user_activity();
    #[cfg(windows)]
    {
        use windows::Win32::UI::Input::KeyboardAndMouse::{
            SendInput, INPUT, INPUT_0, INPUT_MOUSE, MOUSEINPUT, MOUSEEVENTF_MOVE,
        };
        use windows::Win32::System::Power::{
            SetThreadExecutionState, ES_CONTINUOUS, ES_DISPLAY_REQUIRED, ES_SYSTEM_REQUIRED,
        };
        unsafe {
            let input = INPUT {
                r#type: INPUT_MOUSE,
                Anonymous: INPUT_0 {
                    mi: MOUSEINPUT {
                        dx: 0,
                        dy: 0,
                        mouseData: 0,
                        dwFlags: MOUSEEVENTF_MOVE,
                        time: 0,
                        dwExtraInfo: 0,
                    },
                },
            };
            let _ = SendInput(&[input], std::mem::size_of::<INPUT>() as i32);
            let _ = SetThreadExecutionState(
                ES_CONTINUOUS | ES_SYSTEM_REQUIRED | ES_DISPLAY_REQUIRED,
            );
        }
    }
}

/// Free bytes on the filesystem that holds `path`.
pub fn free_bytes(path: &std::path::Path) -> Option<u64> {
    #[cfg(unix)]
    {
        let c = std::ffi::CString::new(path.to_str()?).ok()?;
        let mut st: libc::statvfs = unsafe { std::mem::zeroed() };
        if unsafe { libc::statvfs(c.as_ptr(), &mut st) } != 0 {
            return None;
        }
        return Some(st.f_bavail as u64 * st.f_frsize as u64);
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        None
    }
}

/// Caps Lock, Num Lock, Scroll Lock as reported by the keyboard lights.
pub fn lock_leds() -> (bool, bool, bool) {
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        linux::leds::lock_leds()
    }
    #[cfg(not(all(unix, not(target_os = "macos"))))]
    {
        (false, false, false)
    }
}

/// Hide or show this computer's cursor while it is controlling another one.
/// Wayland leaves the cursor to the compositor.
pub fn set_host_cursor_hidden(hidden: bool) {
    #[cfg(all(unix, not(target_os = "macos")))]
    linux::cursor::set_hidden(hidden);
    #[cfg(windows)]
    win::set_cursor_visible(!hidden);
    #[cfg(not(any(windows, all(unix, not(target_os = "macos")))))]
    {
        let _ = hidden;
    }
}

/// True when the foreground X11 window is fullscreen. Wayland-native windows report false.
pub fn foreground_fullscreen() -> bool {
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        linux::fullscreen::foreground_fullscreen()
    }
    #[cfg(not(all(unix, not(target_os = "macos"))))]
    {
        false
    }
}

/// Former on-screen “Controlled” badge. Kept as a no-op; link state is the tray green dot.
pub fn set_controlled_hint(_text: Option<String>) {}

/// A background watch. The receiver stays open; events arrive when the OS reports them.
pub fn spawn_power_watch() -> std::sync::mpsc::Receiver<PowerEvent> {
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        linux::power::spawn()
    }
    #[cfg(windows)]
    {
        win::power::spawn()
    }
    #[cfg(not(any(windows, all(unix, not(target_os = "macos")))))]
    {
        let (_tx, rx) = std::sync::mpsc::channel();
        rx
    }
}

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

/// Ensure `/dev/uinput` is open before the first controlled pointer jump.
pub fn warm_uinput() -> bool {
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        linux::uinput::warm()
    }
    #[cfg(not(all(unix, not(target_os = "macos"))))]
    {
        true
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

/// Resident memory of this process in MiB, when the platform reports it.
pub fn resident_mb() -> Option<u64> {
    #[cfg(windows)]
    unsafe {
        use windows::Win32::System::ProcessStatus::{
            K32GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS,
        };
        use windows::Win32::System::Threading::GetCurrentProcess;
        let mut counters = PROCESS_MEMORY_COUNTERS::default();
        let size = std::mem::size_of::<PROCESS_MEMORY_COUNTERS>() as u32;
        if !K32GetProcessMemoryInfo(GetCurrentProcess(), &mut counters, size).as_bool() {
            return None;
        }
        Some(counters.WorkingSetSize as u64 / (1024 * 1024))
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let statm = std::fs::read_to_string("/proc/self/statm").ok()?;
        let pages: u64 = statm.split_whitespace().nth(1)?.parse().ok()?;
        let page = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
        Some(pages * u64::try_from(page).ok()? / (1024 * 1024))
    }
    #[cfg(not(any(windows, all(unix, not(target_os = "macos")))))]
    {
        None
    }
}

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

/// Put copied names on this computer as placeholder files. The file manager
/// pastes those; [`wait_paste`] finds the copies. `None` on Windows.
pub fn prepare_paste(id: u64, items: &[vdrag::DragItem]) -> Option<(u64, Vec<std::path::PathBuf>)> {
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        linux::paste_hold::prepare_paste(id, items)
    }
    #[cfg(not(all(unix, not(target_os = "macos"))))]
    {
        let _ = (id, items);
        None
    }
}

/// Wait until placeholders from [`prepare_paste`] have been copied.
pub fn wait_paste(gen: u64, wait: std::time::Duration) -> Option<Vec<std::path::PathBuf>> {
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        linux::paste_hold::wait_paste(gen, wait)
    }
    #[cfg(not(all(unix, not(target_os = "macos"))))]
    {
        let _ = (gen, wait);
        None
    }
}

/// Where a file paste should be written when this computer's focus is a file
/// manager or the desktop. `None` means use the fallback directory.
pub fn paste_folder() -> Option<std::path::PathBuf> {
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        linux::paste_folder()
    }
    #[cfg(windows)]
    {
        win::paste_folder()
    }
    #[cfg(not(any(windows, all(unix, not(target_os = "macos")))))]
    {
        None
    }
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
