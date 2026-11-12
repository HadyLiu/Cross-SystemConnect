//! 「以管理员服务运行」开关（仅 Windows）。
//!
//! 打开：经 UAC 运行 `Cross-SystemConnect-core.exe --install-service`，装好
//! 并启动 LocalSystem 服务，然后重启界面，界面不再自带内核，改连服务里的内核。
//! 关闭：经 UAC 运行 `--uninstall-service`，再重启界面，恢复界面自带内核。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

static BUSY: AtomicBool = AtomicBool::new(false);
static ERROR: Mutex<Option<String>> = Mutex::new(None);
/// The core of this UI lives in the service (set at startup).
static ACTIVE: AtomicBool = AtomicBool::new(false);

#[cfg(windows)]
pub fn set_active(on: bool) {
    ACTIVE.store(on, Ordering::SeqCst);
}

/// This UI talks to the service's core: quitting the UI must not stop it.
pub fn active() -> bool {
    ACTIVE.load(Ordering::SeqCst)
}

pub fn busy() -> bool {
    BUSY.load(Ordering::SeqCst)
}

/// Error from the last toggle, once.
pub fn take_error() -> Option<String> {
    ERROR.lock().unwrap_or_else(|e| e.into_inner()).take()
}

#[cfg(windows)]
pub fn installed() -> bool {
    use csc_platform::win::service::{state, ServiceState};
    use std::time::{Duration, Instant};
    static CACHE: Mutex<Option<(Instant, bool)>> = Mutex::new(None);
    let mut cache = CACHE.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((at, v)) = *cache {
        if at.elapsed() < Duration::from_secs(2) {
            return v;
        }
    }
    let v = state() != ServiceState::NotInstalled;
    *cache = Some((Instant::now(), v));
    v
}

#[cfg(not(windows))]
pub fn installed() -> bool {
    false
}

/// The service is installed and running now.
#[cfg(windows)]
pub fn running() -> bool {
    csc_platform::win::service::state() == csc_platform::win::service::ServiceState::Running
}

/// Turn the mode on or off in the background (the UAC prompt blocks).
/// On success the UI restarts itself.
#[cfg(windows)]
pub fn toggle(on: bool) {
    if BUSY.swap(true, Ordering::SeqCst) {
        return;
    }
    let _ = std::thread::Builder::new()
        .name("csc-admin-service".into())
        .spawn(move || match apply(on) {
            Ok(()) => relaunch_and_exit(),
            Err(e) => {
                tracing::warn!("admin service toggle: {e}");
                *ERROR.lock().unwrap_or_else(|e| e.into_inner()) = Some(e);
                BUSY.store(false, Ordering::SeqCst);
            }
        });
}

#[cfg(not(windows))]
pub fn toggle(_on: bool) {}

#[cfg(windows)]
fn apply(on: bool) -> Result<(), String> {
    use csc_platform::elevated::{install_params, UNINSTALL_FLAG};
    let exe = std::env::current_exe()
        .map_err(|e| e.to_string())?
        .with_file_name("Cross-SystemConnect-core.exe");
    if !exe.exists() {
        return Err(format!("找不到 {}", exe.display()));
    }
    let params = if on {
        install_params(&csc_config::default_config_path())
    } else {
        UNINSTALL_FLAG.to_string()
    };
    match csc_platform::win::elevation::run_elevated(&exe, &params) {
        Ok(0) => Ok(()),
        Ok(code) => Err(format!(
            "{}管理员服务失败（退出码 {code}），详见配置目录里的 service.log",
            if on { "安装" } else { "卸载" }
        )),
        Err(e) => Err(format!("没有获得管理员权限：{e}")),
    }
}

#[cfg(windows)]
fn relaunch_and_exit() -> ! {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    if let Ok(exe) = std::env::current_exe() {
        // Start the new window once this one (and its core) has gone.
        let _ = std::process::Command::new("cmd")
            .raw_arg(format!(
                "/C ping -n 3 127.0.0.1 >NUL & start \"\" \"{}\"",
                exe.display()
            ))
            .creation_flags(CREATE_NO_WINDOW)
            .spawn();
    }
    crate::singleton::release();
    crate::ipc_client::force_quit_now();
}
