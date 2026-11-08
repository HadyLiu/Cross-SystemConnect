//! Keep a single settings/tray process. Extra launches ask the running one to show.

use std::fs;
use std::path::PathBuf;

fn runtime_dir() -> PathBuf {
    csc_config::default_config_path()
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| PathBuf::from("."))
}

fn pid_path() -> PathBuf {
    runtime_dir().join("ui.pid")
}

fn show_path() -> PathBuf {
    runtime_dir().join("ui.show")
}

fn pid_alive(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }
    #[cfg(target_os = "linux")]
    {
        PathBuf::from(format!("/proc/{pid}")).exists()
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = pid;
        false
    }
}

/// Returns `true` if this process owns the UI slot.
/// If another live UI exists, requests it to show settings and returns `false`.
pub fn try_claim() -> bool {
    let dir = runtime_dir();
    let _ = fs::create_dir_all(&dir);
    let path = pid_path();

    if let Ok(text) = fs::read_to_string(&path) {
        if let Ok(pid) = text.trim().parse::<u32>() {
            if pid != std::process::id() && pid_alive(pid) {
                request_show();
                tracing::info!("another UI is running (pid {pid}); asked it to show settings");
                return false;
            }
        }
    }

    if let Err(e) = fs::write(&path, std::process::id().to_string()) {
        tracing::warn!("ui.pid write failed: {e:#}");
    }
    true
}

pub fn request_show() {
    let path = show_path();
    if let Some(dir) = path.parent() {
        let _ = fs::create_dir_all(dir);
    }
    let _ = fs::write(path, b"1");
}

/// True when a second instance asked us to open settings.
pub fn take_show_request() -> bool {
    let path = show_path();
    if !path.exists() {
        return false;
    }
    let _ = fs::remove_file(&path);
    true
}

pub fn release() {
    let path = pid_path();
    if let Ok(text) = fs::read_to_string(&path) {
        if text.trim() == std::process::id().to_string() {
            let _ = fs::remove_file(path);
        }
    }
    let _ = fs::remove_file(show_path());
}
