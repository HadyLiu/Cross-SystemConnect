//! Folder a file paste should use on Windows: the Explorer window in front,
//! or the desktop. Anything else leaves the choice to the fallback directory.

use std::path::PathBuf;
use windows::core::{Interface, VARIANT};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CLSCTX_LOCAL_SERVER, COINIT_APARTMENTTHREADED,
};
use windows::Win32::UI::Shell::{
    IShellWindows, IWebBrowser2, SHGetKnownFolderPath, ShellWindows, FOLDERID_Desktop,
    KF_FLAG_DEFAULT,
};
use windows::Win32::UI::WindowsAndMessaging::{FindWindowExW, GetClassNameW, GetForegroundWindow};

pub fn paste_folder() -> Option<PathBuf> {
    unsafe { folder_of_foreground() }
}

unsafe fn folder_of_foreground() -> Option<PathBuf> {
    let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
    let hwnd = GetForegroundWindow();
    if hwnd.0.is_null() {
        return None;
    }
    let mut buf = [0u16; 256];
    let n = GetClassNameW(hwnd, &mut buf);
    if n <= 0 {
        return None;
    }
    let class = String::from_utf16_lossy(&buf[..n as usize]);
    if class == "Progman" || class == "WorkerW" {
        return desktop_dir();
    }
    if class == "CabinetWClass" || class == "ExploreWClass" {
        return explorer_folder(hwnd.0 as isize).or_else(|| desktop_dir());
    }
    None
}

unsafe fn desktop_dir() -> Option<PathBuf> {
    let pw = SHGetKnownFolderPath(&FOLDERID_Desktop, KF_FLAG_DEFAULT, None).ok()?;
    let path = pw.to_string().ok()?;
    let _ = windows::Win32::System::Com::CoTaskMemFree(Some(pw.0 as *const _));
    if path.is_empty() {
        None
    } else {
        Some(PathBuf::from(path))
    }
}

unsafe fn explorer_folder(foreground: isize) -> Option<PathBuf> {
    let shell: IShellWindows =
        CoCreateInstance(&ShellWindows, None, CLSCTX_LOCAL_SERVER).ok()?;
    let count = shell.Count().ok()?;
    let tab = FindWindowExW(
        windows::Win32::Foundation::HWND(foreground as *mut _),
        None,
        windows::core::w!("ShellTabWindowClass"),
        None,
    )
    .ok()
    .map(|h| h.0 as isize)
    .unwrap_or(0);
    for index in 0..count {
        let Ok(item) = shell.Item(&VARIANT::from(index)) else {
            continue;
        };
        let browser: IWebBrowser2 = match item.cast() {
            Ok(b) => b,
            Err(_) => continue,
        };
        let Ok(current) = browser.HWND() else {
            continue;
        };
        if current.0 != foreground && current.0 != tab {
            continue;
        }
        let Ok(url) = browser.LocationURL() else {
            continue;
        };
        if let Some(path) = file_url_to_path(&url.to_string()) {
            return Some(path);
        }
    }
    None
}

fn file_url_to_path(url: &str) -> Option<PathBuf> {
    let rest = url
        .trim()
        .strip_prefix("file:///")
        .or_else(|| url.trim().strip_prefix("file://localhost/"))?;
    let decoded = percent_decode(rest);
    if decoded.len() < 2 || !decoded.as_bytes().get(1).is_some_and(|c| *c == b':') {
        return None;
    }
    Some(PathBuf::from(decoded.replace('/', "\\")))
}

fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(v) = u8::from_str_radix(
                std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or(""),
                16,
            ) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}
