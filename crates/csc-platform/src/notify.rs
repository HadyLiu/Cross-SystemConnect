//! User-visible messages from the background core.
//! Errors may open a dialog. Connect and disconnect messages are desktop
//! notifications on Windows and Linux, and never a popup window.

/// Show `body` to the user without blocking the caller.
pub fn notify(title: &str, body: &str, error: bool) {
    let title = title.to_string();
    let body = body.to_string();
    let _ = std::thread::Builder::new()
        .name("notify".into())
        .spawn(move || show(&title, &body, error));
}

#[cfg(windows)]
fn show(title: &str, body: &str, error: bool) {
    if error {
        message_box(title, body, true);
        return;
    }
    if !balloon(title, body) {
        tracing::info!("{title}: {body}");
    }
}

#[cfg(windows)]
fn message_box(title: &str, body: &str, error: bool) {
    use windows::core::PCWSTR;
    use windows::Win32::UI::WindowsAndMessaging::{
        MessageBoxW, MB_ICONINFORMATION, MB_ICONWARNING, MB_OK, MB_SETFOREGROUND, MB_TOPMOST,
    };
    let wide = |s: &str| s.encode_utf16().chain(Some(0)).collect::<Vec<u16>>();
    let (t, b) = (wide(title), wide(body));
    let icon = if error {
        MB_ICONWARNING
    } else {
        MB_ICONINFORMATION
    };
    unsafe {
        MessageBoxW(
            None,
            PCWSTR(b.as_ptr()),
            PCWSTR(t.as_ptr()),
            MB_OK | icon | MB_TOPMOST | MB_SETFOREGROUND,
        );
    }
}

/// A tray balloon. It is a notification, not a window the user must dismiss.
#[cfg(windows)]
fn balloon(title: &str, body: &str) -> bool {
    use std::time::{Duration, Instant};
    use windows::core::w;
    use windows::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows::Win32::UI::Shell::{
        Shell_NotifyIconW, NIF_ICON, NIF_INFO, NIF_TIP, NIIF_INFO, NIM_ADD, NIM_DELETE,
        NOTIFYICONDATAW,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, DestroyWindow, DispatchMessageW, LoadIconW, PeekMessageW, RegisterClassW,
        TranslateMessage, HWND_MESSAGE, IDI_INFORMATION, MSG, PM_REMOVE, WINDOW_EX_STYLE,
        WINDOW_STYLE, WNDCLASSW,
    };

    unsafe {
        let Ok(module) = GetModuleHandleW(None) else {
            return false;
        };
        let module = windows::Win32::Foundation::HINSTANCE(module.0);
        let class = w!("CscNotify");
        let wc = WNDCLASSW {
            lpfnWndProc: Some(notify_proc),
            hInstance: module,
            lpszClassName: class,
            ..Default::default()
        };
        RegisterClassW(&wc);
        let Ok(hwnd) = CreateWindowExW(
            WINDOW_EX_STYLE(0x08000080),
            class,
            w!(""),
            WINDOW_STYLE(0x80000000),
            0,
            0,
            0,
            0,
            HWND_MESSAGE,
            None,
            module,
            None,
        ) else {
            return false;
        };
        let icon = LoadIconW(None, IDI_INFORMATION).unwrap_or_default();
        let mut data = NOTIFYICONDATAW {
            cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
            hWnd: hwnd,
            uID: 1,
            uFlags: NIF_INFO | NIF_ICON | NIF_TIP,
            hIcon: icon,
            dwInfoFlags: NIIF_INFO,
            ..Default::default()
        };
        data.Anonymous.uTimeout = 10_000;
        copy_utf16(&mut data.szTip, "Cross-SystemConnect");
        copy_utf16(&mut data.szInfoTitle, title);
        copy_utf16(&mut data.szInfo, body);
        let added = Shell_NotifyIconW(NIM_ADD, &data).as_bool();
        if added {
            let deadline = Instant::now() + Duration::from_secs(8);
            while Instant::now() < deadline {
                let mut msg = MSG::default();
                while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                    let _ = TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            let _ = Shell_NotifyIconW(NIM_DELETE, &data);
        }
        let _ = DestroyWindow(hwnd);
        added
    }
}

#[cfg(windows)]
unsafe extern "system" fn notify_proc(
    hwnd: windows::Win32::Foundation::HWND,
    msg: u32,
    wparam: windows::Win32::Foundation::WPARAM,
    lparam: windows::Win32::Foundation::LPARAM,
) -> windows::Win32::Foundation::LRESULT {
    use windows::Win32::UI::WindowsAndMessaging::DefWindowProcW;
    DefWindowProcW(hwnd, msg, wparam, lparam)
}

#[cfg(windows)]
fn copy_utf16(dst: &mut [u16], text: &str) {
    dst.fill(0);
    let limit = dst.len().saturating_sub(1);
    for (slot, unit) in dst.iter_mut().zip(text.encode_utf16().take(limit)) {
        *slot = unit;
    }
}

#[cfg(all(unix, not(target_os = "macos")))]
fn show(title: &str, body: &str, error: bool) {
    use std::process::{Command, Stdio};
    let quiet = |c: &mut Command| {
        c.stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
    };
    // Connect and disconnect stay on the notification bus. A dialog is only
    // for an error, and only when that bus is missing.
    if dbus_notify(title, body, error) {
        return;
    }
    if !error {
        let mut c = Command::new("notify-send");
        c.args(["-a", "Cross-SystemConnect", title, body]);
        quiet(&mut c);
        if c.status().is_err() {
            tracing::info!("{title}: {body}");
        }
        return;
    }
    let mut zenity = Command::new("zenity");
    zenity.args(["--warning", "--no-wrap", "--title", title, "--text", body]);
    quiet(&mut zenity);
    if zenity.status().is_ok_and(|s| s.code().is_some()) {
        return;
    }
    let mut c = Command::new("notify-send");
    c.args(["-a", "Cross-SystemConnect", "-u", "critical", title, body]);
    quiet(&mut c);
    if c.status().is_err() {
        tracing::warn!("{title}: {body}");
    }
}

#[cfg(all(unix, not(target_os = "macos")))]
fn dbus_notify(title: &str, body: &str, error: bool) -> bool {
    use dbus::arg::PropMap;
    use dbus::blocking::Connection;
    use std::time::Duration;
    let Ok(conn) = Connection::new_session() else {
        return false;
    };
    let proxy = conn.with_proxy(
        "org.freedesktop.Notifications",
        "/org/freedesktop/Notifications",
        Duration::from_secs(2),
    );
    let mut hints = PropMap::new();
    if error {
        hints.insert(
            "urgency".into(),
            dbus::arg::Variant(Box::new(2u8) as Box<dyn dbus::arg::RefArg>),
        );
    }
    let result: Result<(u32,), dbus::Error> = proxy.method_call(
        "org.freedesktop.Notifications",
        "Notify",
        (
            "Cross-SystemConnect",
            0u32,
            "",
            title,
            body,
            Vec::<String>::new(),
            hints,
            5000i32,
        ),
    );
    result.is_ok()
}

#[cfg(not(any(windows, all(unix, not(target_os = "macos")))))]
fn show(title: &str, body: &str, _error: bool) {
    tracing::warn!("{title}: {body}");
}
