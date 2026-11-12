//! Integrity levels, UIPI detection and "run as administrator".

use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use windows::core::{HSTRING, PCWSTR, PWSTR};
use windows::Win32::Foundation::{CloseHandle, GetLastError, ERROR_ACCESS_DENIED, HANDLE};
use windows::Win32::Security::{
    GetSidSubAuthority, GetSidSubAuthorityCount, GetTokenInformation, TokenIntegrityLevel,
    TOKEN_MANDATORY_LABEL, TOKEN_QUERY,
};
use windows::Win32::System::Threading::{
    GetCurrentProcess, GetExitCodeProcess, OpenProcess, OpenProcessToken,
    QueryFullProcessImageNameW, WaitForSingleObject, INFINITE, PROCESS_NAME_WIN32,
    PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows::Win32::UI::Shell::{
    ShellExecuteExW, SEE_MASK_NOASYNC, SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    GetForegroundWindow, GetWindowThreadProcessId, SW_HIDE,
};

use crate::uipi;

/// Integrity RID of a token, e.g. [`uipi::MEDIUM`].
pub(crate) fn token_integrity(token: HANDLE) -> Option<u32> {
    unsafe {
        let mut len = 0u32;
        let _ = GetTokenInformation(token, TokenIntegrityLevel, None, 0, &mut len);
        if len == 0 {
            return None;
        }
        // u64 storage keeps the SID pointer inside aligned.
        let mut buf = vec![0u64; (len as usize).div_ceil(8)];
        GetTokenInformation(
            token,
            TokenIntegrityLevel,
            Some(buf.as_mut_ptr().cast()),
            len,
            &mut len,
        )
        .ok()?;
        let label = &*(buf.as_ptr() as *const TOKEN_MANDATORY_LABEL);
        let sid = label.Label.Sid;
        let count = *GetSidSubAuthorityCount(sid);
        if count == 0 {
            return None;
        }
        Some(*GetSidSubAuthority(sid, u32::from(count) - 1))
    }
}

enum Level {
    Known(u32),
    /// The token could not be opened: Windows hides it from us, which for a
    /// normal app window means it runs elevated.
    Hidden,
}

fn process_integrity(process: HANDLE) -> Option<Level> {
    unsafe {
        let mut token = HANDLE::default();
        if OpenProcessToken(process, TOKEN_QUERY, &mut token).is_err() {
            return (GetLastError() == ERROR_ACCESS_DENIED).then_some(Level::Hidden);
        }
        let level = token_integrity(token);
        let _ = CloseHandle(token);
        level.map(Level::Known)
    }
}

/// Integrity level of this process.
pub fn own_integrity() -> u32 {
    static OWN: OnceLock<u32> = OnceLock::new();
    *OWN.get_or_init(|| match process_integrity(unsafe { GetCurrentProcess() }) {
        Some(Level::Known(rid)) => rid,
        _ => uipi::MEDIUM,
    })
}

/// True when this process runs as administrator (or SYSTEM).
pub fn is_elevated() -> bool {
    own_integrity() >= uipi::HIGH
}

fn image_name(process: HANDLE) -> String {
    let mut buf = [0u16; 1024];
    let mut len = buf.len() as u32;
    unsafe {
        if QueryFullProcessImageNameW(
            process,
            PROCESS_NAME_WIN32,
            PWSTR(buf.as_mut_ptr()),
            &mut len,
        )
        .is_ok()
        {
            return uipi::app_name(&String::from_utf16_lossy(&buf[..len as usize]));
        }
    }
    String::new()
}

/// Integrity level and program name of the foreground window's process.
/// `None` when there is no foreground window or it cannot be inspected.
fn foreground_integrity() -> Option<(u32, String)> {
    unsafe {
        let hwnd = GetForegroundWindow();
        if hwnd.0.is_null() {
            return None;
        }
        let mut pid = 0u32;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        if pid == 0 || pid == std::process::id() {
            return None;
        }
        let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
        let level = process_integrity(process);
        let name = image_name(process);
        let _ = CloseHandle(process);
        let rid = match level? {
            Level::Known(rid) => rid,
            Level::Hidden => uipi::HIGH,
        };
        Some((
            rid,
            if name.is_empty() {
                format!("PID {pid}")
            } else {
                name
            },
        ))
    }
}

const CHECK_EVERY: Duration = Duration::from_millis(500);

struct Checker {
    last: Option<Instant>,
    warned: uipi::WarnOnce,
}

/// Called before injecting a click or key: if the foreground window runs at a
/// higher integrity level, Windows will silently drop the input. Warn once.
pub fn check_foreground() {
    static STATE: Mutex<Checker> = Mutex::new(Checker {
        last: None,
        warned: uipi::WarnOnce { last: None },
    });
    let now = Instant::now();
    let mut state = STATE.lock().unwrap_or_else(|e| e.into_inner());
    if state
        .last
        .is_some_and(|t| now.saturating_duration_since(t) < CHECK_EVERY)
    {
        return;
    }
    state.last = Some(now);
    let ours = own_integrity();
    if ours >= uipi::SYSTEM {
        return;
    }
    let Some((theirs, app)) = foreground_integrity() else {
        return;
    };
    if uipi::blocks_input(ours, theirs) && state.warned.should_warn(&app, now) {
        tracing::warn!(
            "前台窗口 {app} 以更高权限运行（完整性 {theirs:#x} > 本程序 {ours:#x}），Windows 会丢弃注入的鼠标/键盘输入（UIPI）"
        );
        uipi::report(&app);
    }
}

/// Run `exe args` through the UAC prompt and wait for it. Returns its exit code.
pub fn run_elevated(exe: &std::path::Path, args: &str) -> std::io::Result<u32> {
    let file = HSTRING::from(exe.as_os_str());
    let params = HSTRING::from(args);
    let verb = HSTRING::from("runas");
    let mut info = SHELLEXECUTEINFOW {
        cbSize: size_of::<SHELLEXECUTEINFOW>() as u32,
        fMask: SEE_MASK_NOCLOSEPROCESS | SEE_MASK_NOASYNC,
        lpVerb: PCWSTR(verb.as_ptr()),
        lpFile: PCWSTR(file.as_ptr()),
        lpParameters: PCWSTR(params.as_ptr()),
        nShow: SW_HIDE.0,
        ..Default::default()
    };
    unsafe {
        ShellExecuteExW(&mut info).map_err(|e| std::io::Error::other(e.message()))?;
        if info.hProcess.is_invalid() {
            return Ok(0);
        }
        WaitForSingleObject(info.hProcess, INFINITE);
        let mut code = 0u32;
        let got = GetExitCodeProcess(info.hProcess, &mut code);
        let _ = CloseHandle(info.hProcess);
        got.map_err(|e| std::io::Error::other(e.message()))?;
        Ok(code)
    }
}
