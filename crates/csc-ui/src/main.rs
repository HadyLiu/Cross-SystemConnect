#![cfg_attr(windows, windows_subsystem = "windows")]

mod admin_service;
mod app;
mod autostart;
mod events;
mod icon;
mod ipc_client;
#[cfg(target_os = "linux")]
mod linux_tray;
mod locale;
mod log_picker;
mod singleton;
mod status_poller;
mod theme;
mod ui;
#[cfg(target_os = "linux")]
mod unix_signals;
mod update;

use crate::events::EventBus;
use eframe::egui;

const APP_VERSION: &str = env!("CARGO_PKG_VERSION");

pub(crate) fn app_title() -> String {
    format!("Cross-SystemConnect v{APP_VERSION}")
}

/// Kernel thread inside the window process. A second executable is what
/// Smart App Control reports as a blocked unsafe app.
#[cfg(windows)]
fn start_core_in_process() {
    let _ = std::thread::Builder::new()
        .name("csc-core".into())
        .spawn(|| {
            let Ok(rt) = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
            else {
                return;
            };
            if let Err(e) = rt.block_on(csc_core::run()) {
                eprintln!("core: {e:#}");
            }
        });
}

fn main() -> eframe::Result {
    // Elevated mode: the core runs in the Windows service; connect to it.
    #[cfg(windows)]
    if admin_service::running() {
        admin_service::set_active(true);
    } else {
        start_core_in_process();
    }

    {
        use csc_config::logfile::{CappedLog, SharedLog};
        let filter = tracing_subscriber::EnvFilter::try_new("info").unwrap_or_default();
        let file = SharedLog::default();
        if let Some(dir) = dirs::config_dir() {
            file.set(CappedLog::open(&dir.join("Cross-SystemConnect").join("ui.log")).ok());
        }
        let _ = tracing_subscriber::fmt()
            .with_env_filter(filter)
            .with_writer(move || file.clone())
            .with_ansi(false)
            .try_init();
    }

    // --open-settings：强制开设置；--background：强制托盘（覆盖配置里的 auto_hide）。
    let args: Vec<String> = std::env::args().collect();
    let open_settings =
        args.iter().any(|a| a == "--open-settings") || !args.iter().any(|a| a == "--background");

    // One tray/settings process only. Extra launches wake the existing one.
    if !singleton::try_claim() {
        // Give the running instance a moment to notice ui.show.
        std::thread::sleep(std::time::Duration::from_millis(200));
        return Ok(());
    }
    let _singleton_guard = SingletonGuard;

    // Dock 「退出」 on GNOME/KDE usually sends SIGTERM; handle it even when minimized.
    #[cfg(target_os = "linux")]
    unix_signals::install();

    if let Err(e) = ipc_client::ensure_core_running() {
        tracing::error!("start core failed: {e:#}");
    }

    let saved = ipc_client::get_config().ok();
    if let Some(lang) = saved
        .as_ref()
        .and_then(|c| c.pointer("/gui/language"))
        .and_then(|v| v.as_str())
    {
        locale::apply(lang);
    }
    let launch = saved
        .as_ref()
        .and_then(|c| c.pointer("/gui/launch_at_login"))
        .and_then(|v| v.as_bool());
    if let Some(Err(e)) = launch.map(autostart::apply) {
        tracing::warn!("launch at login: {e:#}");
    }

    let (bus, events) = EventBus::new();

    let mut viewport = egui::ViewportBuilder::default()
        .with_title(app_title())
        .with_inner_size([880.0, 580.0])
        .with_min_inner_size([760.0, 500.0])
        .with_decorations(false)
        .with_transparent(true)
        .with_app_id("cross-systemconnect");
    if let Some(icon) = icon::egui_icon_data() {
        viewport = viewport.with_icon(icon);
    }

    let options = eframe::NativeOptions {
        viewport,
        event_loop_builder: x11_event_loop(),
        ..Default::default()
    };

    eframe::run_native(
        &app_title(),
        options,
        Box::new(move |cc| Ok(Box::new(app::App::new(cc, bus, events, open_settings)))),
    )
}

/// On a Wayland desktop, run the window through XWayland. A Wayland client
/// cannot unminimize itself, and winit cannot hide/show a Wayland window, so
/// the tray could never bring settings back. X11 maps and unmaps it reliably.
#[cfg(target_os = "linux")]
fn x11_event_loop() -> Option<eframe::EventLoopBuilderHook> {
    if !x11_window() {
        return None;
    }
    Some(Box::new(|builder| {
        use winit::platform::x11::EventLoopBuilderExtX11;
        builder.with_x11();
    }))
}

#[cfg(not(target_os = "linux"))]
fn x11_event_loop() -> Option<eframe::EventLoopBuilderHook> {
    None
}

/// The settings window is an X11 window (X11 session or XWayland).
#[cfg(target_os = "linux")]
pub fn x11_window() -> bool {
    std::env::var_os("DISPLAY").is_some()
}

struct SingletonGuard;

impl Drop for SingletonGuard {
    fn drop(&mut self) {
        singleton::release();
    }
}
