//! User-visible messages from the background core. Errors pop up a dialog;
//! other messages use a desktop notification where one exists.

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
    use windows::core::PCWSTR;
    use windows::Win32::UI::WindowsAndMessaging::{
        MessageBoxW, MB_ICONINFORMATION, MB_ICONWARNING, MB_OK, MB_SETFOREGROUND, MB_TOPMOST,
    };
    let wide = |s: &str| s.encode_utf16().chain(Some(0)).collect::<Vec<u16>>();
    let (t, b) = (wide(title), wide(body));
    let icon = if error { MB_ICONWARNING } else { MB_ICONINFORMATION };
    unsafe {
        MessageBoxW(
            None,
            PCWSTR(b.as_ptr()),
            PCWSTR(t.as_ptr()),
            MB_OK | icon | MB_TOPMOST | MB_SETFOREGROUND,
        );
    }
}

#[cfg(all(unix, not(target_os = "macos")))]
fn show(title: &str, body: &str, error: bool) {
    use std::process::{Command, Stdio};
    let quiet = |c: &mut Command| {
        c.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    };
    if error {
        let mut c = Command::new("zenity");
        c.args(["--warning", "--no-wrap", "--title", title, "--text", body]);
        quiet(&mut c);
        if c.status().is_ok_and(|s| s.code().is_some()) {
            return;
        }
    }
    let mut c = Command::new("notify-send");
    c.args(["-a", "Cross-SystemConnect"]);
    if error {
        c.args(["-u", "critical"]);
    }
    c.args([title, body]);
    quiet(&mut c);
    if c.status().is_err() {
        tracing::warn!("{title}: {body}");
    }
}

#[cfg(not(any(windows, all(unix, not(target_os = "macos")))))]
fn show(title: &str, body: &str, _error: bool) {
    tracing::warn!("{title}: {body}");
}
