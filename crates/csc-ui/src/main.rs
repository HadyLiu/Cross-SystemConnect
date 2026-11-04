#![cfg_attr(windows, windows_subsystem = "windows")]

mod autostart;
mod icon;
mod ipc_client;
#[cfg(target_os = "linux")]
mod linux_tray;
mod locale;
mod log_picker;
mod status_poller;
mod transfer_window;

#[cfg(not(target_os = "linux"))]
use anyhow::Context;
use serde_json::json;
use std::borrow::Cow;
use tao::{
    event::{Event, StartCause, WindowEvent},
    event_loop::{ControlFlow, EventLoopBuilder},
    window::WindowBuilder,
};
#[cfg(not(target_os = "linux"))]
use tray_icon::{
    menu::{Menu, MenuEvent, MenuItem},
    TrayIconBuilder, TrayIconEvent,
};
use wry::{
    http::{header::CONTENT_TYPE, Method, Request, Response, StatusCode},
    WebViewBuilder,
};

const APP_VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Clone)]
pub(crate) enum UserEvent {
    ShowSettings,
    PasteHere,
    ToggleSharing,
    Quit,
    /// Settings language changed; Windows tray labels are updated on this thread.
    TrayLanguage,
    /// Incoming batches from the core status.
    Transfers(Vec<serde_json::Value>),
    /// Settings page asked for the log file dialog, starting from this path.
    PickLogFile(String),
    /// Tray green-dot: connected / being-controlled blink phase.
    TrayLink {
        connected: bool,
        being_controlled: bool,
        blink_on: bool,
    },
}

/// What should pop the settings window: a client asking to connect, or a
/// pairing code this computer must show. Empty when nothing is waiting.
fn popup_key(status: &serde_json::Value) -> String {
    let host = status
        .get("pendingPeer")
        .and_then(|p| p.get("hostName"))
        .and_then(|s| s.as_str())
        .unwrap_or_default();
    let code = status
        .get("pairingCode")
        .and_then(|s| s.as_str())
        .unwrap_or_default();
    if host.is_empty() && code.is_empty() {
        return String::new();
    }
    format!("{host}\u{1f}{code}")
}

pub(crate) fn app_title() -> String {
    format!("Cross-SystemConnect v{APP_VERSION}")
}

fn index_html() -> Vec<u8> {
    include_str!("../ui/index.html")
        .replace("__CSC_VERSION__", APP_VERSION)
        .into_bytes()
}

fn cors_headers(builder: wry::http::response::Builder) -> wry::http::response::Builder {
    builder
        .header("Access-Control-Allow-Origin", "*")
        .header("Access-Control-Allow-Methods", "GET, POST, OPTIONS")
        .header("Access-Control-Allow-Headers", "content-type")
        .header("Cache-Control", "no-store")
}

fn handle_http(req: Request<Vec<u8>>) -> anyhow::Result<Response<Cow<'static, [u8]>>> {
    let path = req.uri().path();
    let method = req.method().clone();

    if method == Method::OPTIONS {
        return Ok(cors_headers(Response::builder())
            .status(StatusCode::NO_CONTENT)
            .body(Cow::Borrowed(&[][..]))?);
    }

    if path == "/icon.png" && method == Method::GET {
        return Ok(cors_headers(Response::builder())
            .header(CONTENT_TYPE, "image/png")
            .body(Cow::Borrowed(icon::app_png_bytes()))?);
    }

    if path == "/" || path == "/index.html" {
        return Ok(cors_headers(Response::builder())
            .header(CONTENT_TYPE, "text/html; charset=utf-8")
            .body(Cow::Owned(index_html()))?);
    }

    if path == "/status" && method == Method::GET {
        let v = ipc_client::get_status().unwrap_or_else(|e| {
            json!({
                "connected": false,
                "peerName": "",
                "activeScreen": "",
                "beingControlled": false,
                "sharingEnabled": false,
                "role": "",
                "localName": csc_config::local_computer_name(),
                "launchAtLogin": false,
                "hostName": csc_config::local_computer_name(),
                "localIpv4": [],
                "localNics": [],
                "discoveryPort": 24900,
                "linkState": "",
                "pendingPeer": null,
                "discovered": [],
                "searchSubnet": "",
                "error": e.to_string()
            })
        });
        let body = serde_json::to_vec(&v)?;
        return Ok(cors_headers(Response::builder())
            .header(CONTENT_TYPE, "application/json")
            .body(Cow::Owned(body))?);
    }

    if path == "/pair-open" && method == Method::POST {
        let (status, v) = match ipc_client::open_pairing() {
            Ok(v) => (StatusCode::OK, v),
            Err(e) => (
                StatusCode::SERVICE_UNAVAILABLE,
                json!({"ok": false, "error": e.to_string()}),
            ),
        };
        let body = serde_json::to_vec(&v)?;
        return Ok(cors_headers(Response::builder())
            .status(status)
            .header(CONTENT_TYPE, "application/json")
            .body(Cow::Owned(body))?);
    }

    if path == "/reset-data" && method == Method::POST {
        let parsed: serde_json::Value =
            serde_json::from_slice(req.body()).unwrap_or(serde_json::Value::Null);
        let keep = parsed
            .get("keepPairing")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let (status, v) = match ipc_client::reset_data(keep) {
            Ok(v) => (StatusCode::OK, v),
            Err(e) => (
                StatusCode::SERVICE_UNAVAILABLE,
                json!({"ok": false, "error": e.to_string()}),
            ),
        };
        let body = serde_json::to_vec(&v)?;
        return Ok(cors_headers(Response::builder())
            .status(status)
            .header(CONTENT_TYPE, "application/json")
            .body(Cow::Owned(body))?);
    }

    if path == "/sharing" && method == Method::POST {
        let body: serde_json::Value =
            serde_json::from_slice(req.body()).unwrap_or(serde_json::Value::Null);
        let enabled = body
            .get("enabled")
            .and_then(|v| v.as_bool())
            .unwrap_or(true);
        let (status, v) = match ipc_client::set_sharing_enabled(enabled) {
            Ok(v) => (StatusCode::OK, v),
            Err(e) => (
                StatusCode::SERVICE_UNAVAILABLE,
                json!({"ok": false, "error": e.to_string()}),
            ),
        };
        let body = serde_json::to_vec(&v)?;
        return Ok(cors_headers(Response::builder())
            .status(status)
            .header(CONTENT_TYPE, "application/json")
            .body(Cow::Owned(body))?);
    }

    if path == "/rescan" && method == Method::POST {
        let (status, v) = match ipc_client::rescan() {
            Ok(v) => (StatusCode::OK, v),
            Err(e) => (
                StatusCode::SERVICE_UNAVAILABLE,
                json!({"ok": false, "error": e.to_string()}),
            ),
        };
        let body = serde_json::to_vec(&v)?;
        return Ok(cors_headers(Response::builder())
            .status(status)
            .header(CONTENT_TYPE, "application/json")
            .body(Cow::Owned(body))?);
    }

    if path == "/logs" && method == Method::GET {
        match ipc_client::get_logs() {
            Ok(v) => {
                let body = serde_json::to_vec(&v)?;
                return Ok(cors_headers(Response::builder())
                    .header(CONTENT_TYPE, "application/json")
                    .body(Cow::Owned(body))?);
            }
            Err(e) => {
                // Keep HTTP 200 so the log page can show the IPC reason. A 503
                // was only the status code and hid why the read failed.
                let body = serde_json::to_vec(&json!({"text": "", "error": e.to_string()}))?;
                return Ok(cors_headers(Response::builder())
                    .header(CONTENT_TYPE, "application/json")
                    .body(Cow::Owned(body))?);
            }
        }
    }

    if path == "/config" && method == Method::GET {
        match ipc_client::get_config() {
            Ok(v) => {
                let body = serde_json::to_vec(&v)?;
                Ok(cors_headers(Response::builder())
                    .header(CONTENT_TYPE, "application/json")
                    .body(Cow::Owned(body))?)
            }
            Err(e) => {
                let body = serde_json::to_vec(&json!({"error": e.to_string()}))?;
                Ok(cors_headers(Response::builder())
                    .status(StatusCode::SERVICE_UNAVAILABLE)
                    .header(CONTENT_TYPE, "application/json")
                    .body(Cow::Owned(body))?)
            }
        }
    } else if path == "/forget-peer" && method == Method::POST {
        let body: serde_json::Value =
            serde_json::from_slice(req.body()).unwrap_or(serde_json::Value::Null);
        let host = body.get("hostName").and_then(|v| v.as_str()).unwrap_or("");
        match ipc_client::forget_peer(host) {
            Ok(v) => {
                let out = serde_json::to_vec(&v)?;
                Ok(cors_headers(Response::builder())
                    .header(CONTENT_TYPE, "application/json")
                    .body(Cow::Owned(out))?)
            }
            Err(e) => {
                let out = serde_json::to_vec(&json!({"ok": false, "error": e.to_string()}))?;
                Ok(cors_headers(Response::builder())
                    .status(StatusCode::BAD_REQUEST)
                    .header(CONTENT_TYPE, "application/json")
                    .body(Cow::Owned(out))?)
            }
        }
    } else if path == "/peer-decision" && method == Method::POST {
        let body: serde_json::Value =
            serde_json::from_slice(req.body()).unwrap_or(serde_json::Value::Null);
        let host = body.get("hostName").and_then(|v| v.as_str()).unwrap_or("");
        let decision = body.get("decision").and_then(|v| v.as_str()).unwrap_or("");
        let code = body.get("code").and_then(|v| v.as_str()).unwrap_or("");
        match ipc_client::resolve_peer(host, decision, code) {
            Ok(v) => {
                let out = serde_json::to_vec(&v)?;
                Ok(cors_headers(Response::builder())
                    .header(CONTENT_TYPE, "application/json")
                    .body(Cow::Owned(out))?)
            }
            Err(e) => {
                let out = serde_json::to_vec(&json!({"ok": false, "error": e.to_string()}))?;
                Ok(cors_headers(Response::builder())
                    .status(StatusCode::BAD_REQUEST)
                    .header(CONTENT_TYPE, "application/json")
                    .body(Cow::Owned(out))?)
            }
        }
    } else if path == "/diagnostics" && method == Method::POST {
        match ipc_client::export_diagnostics() {
            Ok(v) => {
                let out = serde_json::to_vec(&v)?;
                Ok(cors_headers(Response::builder())
                    .header(CONTENT_TYPE, "application/json")
                    .body(Cow::Owned(out))?)
            }
            Err(e) => {
                let out = serde_json::to_vec(&json!({"ok": false, "error": e.to_string()}))?;
                Ok(cors_headers(Response::builder())
                    .status(StatusCode::BAD_REQUEST)
                    .header(CONTENT_TYPE, "application/json")
                    .body(Cow::Owned(out))?)
            }
        }
    } else if path == "/config" && method == Method::POST {
        let body: serde_json::Value = serde_json::from_slice(req.body())?;
        let launch = body
            .pointer("/gui/launch_at_login")
            .and_then(|v| v.as_bool());
        match ipc_client::save_config(body) {
            Ok(v) => {
                if let Some(enabled) = launch {
                    if let Err(e) = autostart::apply(enabled) {
                        tracing::warn!("launch at login: {e:#}");
                    }
                }
                let out = serde_json::to_vec(&v)?;
                Ok(cors_headers(Response::builder())
                    .header(CONTENT_TYPE, "application/json")
                    .body(Cow::Owned(out))?)
            }
            Err(e) => {
                let out = serde_json::to_vec(&json!({"ok": false, "error": e.to_string()}))?;
                Ok(cors_headers(Response::builder())
                    .status(StatusCode::BAD_REQUEST)
                    .header(CONTENT_TYPE, "application/json")
                    .body(Cow::Owned(out))?)
            }
        }
    } else {
        Ok(cors_headers(Response::builder())
            .status(StatusCode::NOT_FOUND)
            .body(Cow::Borrowed(&b"not found"[..]))?)
    }
}

fn start_url() -> &'static str {
    #[cfg(windows)]
    {
        "http://csc.localhost/"
    }
    #[cfg(not(windows))]
    {
        "csc://localhost/"
    }
}

fn build_webview(
    window: &tao::window::Window,
    proxy: tao::event_loop::EventLoopProxy<UserEvent>,
) -> wry::Result<wry::WebView> {
    let builder = WebViewBuilder::new()
        .with_ipc_handler(move |req| {
            if let Some(current) = req.body().strip_prefix("pick-log-file:") {
                let _ = proxy.send_event(UserEvent::PickLogFile(current.to_string()));
            } else if let Some(code) = req.body().strip_prefix("lang:") {
                locale::apply(code);
                let _ = proxy.send_event(UserEvent::TrayLanguage);
            }
        })
        .with_custom_protocol(
            "csc".into(),
            move |_webview_id, request| match handle_http(request) {
                Ok(r) => r.map(Into::into),
                Err(e) => {
                    let body = format!("{e:#}").into_bytes();
                    cors_headers(Response::builder())
                        .status(StatusCode::INTERNAL_SERVER_ERROR)
                        .header(CONTENT_TYPE, "text/plain")
                        .body(Cow::Owned(body))
                        .unwrap()
                        .map(Into::into)
                }
            },
        )
        // Windows WebView2 only intercepts http://{scheme}.localhost.
        // Linux WebKit serves the custom scheme itself; http://csc.localhost
        // is a real connection and fails with "Connection refused".
        .with_url(start_url())
        .with_devtools(true);

    #[cfg(any(
        target_os = "windows",
        target_os = "macos",
        target_os = "ios",
        target_os = "android"
    ))]
    {
        builder.build(window)
    }
    #[cfg(not(any(
        target_os = "windows",
        target_os = "macos",
        target_os = "ios",
        target_os = "android"
    )))]
    {
        use tao::platform::unix::WindowExtUnix;
        use wry::WebViewBuilderExtUnix;
        let vbox = window.default_vbox().unwrap();
        builder.build_gtk(vbox)
    }
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

fn main() -> anyhow::Result<()> {
    // Smart App Control blocks a second program, even this same file launched
    // again. The kernel runs in this process, so nothing else is started.
    #[cfg(windows)]
    start_core_in_process();

    // WebKitGTK draws <select> popups with the GTK theme, which CSS cannot reach.
    // Must be set before GTK starts. A theme the user chose still wins.
    #[cfg(target_os = "linux")]
    if std::env::var_os("GTK_THEME").is_none() {
        std::env::set_var("GTK_THEME", "Adwaita:dark");
    }

    // Log to file so GUI subsystem still has diagnostics; capped like the core log.
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

    let open_settings = std::env::args().any(|a| a == "--open-settings");

    if let Err(e) = ipc_client::ensure_core_running() {
        tracing::error!("start core failed: {e:#}");
        // Continue so tray still appears; settings will show IPC errors
    }
    // The login entry follows the saved setting, which is off by default.
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

    let event_loop = EventLoopBuilder::<UserEvent>::with_user_event().build();
    let proxy = event_loop.create_proxy();

    #[cfg(not(target_os = "linux"))]
    let (mut tray_icon, tray_settings, tray_paste, tray_share, tray_quit) = {
        use tray_icon::menu::CheckMenuItem;
        let tray_menu = Menu::new();
        let item_settings = MenuItem::new(locale::settings(), true, None);
        let item_paste = MenuItem::new(locale::paste(), true, None);
        let sharing_on = ipc_client::get_status()
            .ok()
            .and_then(|s| s.get("sharingEnabled").and_then(|v| v.as_bool()))
            .unwrap_or(true);
        let item_share = CheckMenuItem::new(
            if sharing_on {
                locale::pause_sharing()
            } else {
                locale::resume_sharing()
            },
            true,
            !sharing_on,
            None,
        );
        let item_quit = MenuItem::new(locale::quit(), true, None);
        tray_menu.append(&item_settings)?;
        tray_menu.append(&item_paste)?;
        tray_menu.append(&item_share)?;
        tray_menu.append(&item_quit)?;

        let tray_icon_img = icon::app_tray_icon(icon::GreenDot::Off);
        let tray_icon = TrayIconBuilder::new()
            .with_menu(Box::new(tray_menu))
            .with_tooltip(app_title())
            .with_icon(tray_icon_img)
            .build()
            .context("create tray icon")?;

        let settings_id = item_settings.id().clone();
        let paste_id = item_paste.id().clone();
        let share_id = item_share.id().clone();
        let quit_id = item_quit.id().clone();

        MenuEvent::set_event_handler(Some({
            let proxy = proxy.clone();
            move |event: MenuEvent| {
                if event.id == settings_id {
                    let _ = proxy.send_event(UserEvent::ShowSettings);
                } else if event.id == paste_id {
                    let _ = proxy.send_event(UserEvent::PasteHere);
                } else if event.id == share_id {
                    let _ = proxy.send_event(UserEvent::ToggleSharing);
                } else if event.id == quit_id {
                    let _ = proxy.send_event(UserEvent::Quit);
                }
            }
        }));

        TrayIconEvent::set_event_handler(Some({
            let proxy = proxy.clone();
            move |event: TrayIconEvent| {
                if let TrayIconEvent::DoubleClick { .. } = event {
                    let _ = proxy.send_event(UserEvent::ShowSettings);
                }
            }
        }));
        (
            Some(tray_icon),
            item_settings,
            item_paste,
            item_share,
            item_quit,
        )
    };
    #[cfg(target_os = "linux")]
    let linux_tray = {
        // Linux panels do not deliver a double-click. Left click is Activate.
        let service = ksni::TrayService::new(linux_tray::LinuxTray {
            proxy: proxy.clone(),
            connected: false,
            being_controlled: false,
            blink_on: true,
        });
        let handle = service.handle();
        service.spawn();
        handle
    };

    // Bring the settings window forward when a client asks to connect, or when
    // this computer has a pairing code to show. The page skips status polls while
    // hidden in the tray, so a code would otherwise never appear.
    {
        let proxy = proxy.clone();
        std::thread::spawn(move || {
            let mut shown_for = String::new();
            loop {
                std::thread::sleep(std::time::Duration::from_millis(500));
                let key = ipc_client::get_status()
                    .ok()
                    .map(|v| popup_key(&v))
                    .unwrap_or_default();
                if key.is_empty() {
                    shown_for.clear();
                    continue;
                }
                if key != shown_for {
                    shown_for = key;
                    let _ = proxy.send_event(UserEvent::ShowSettings);
                }
            }
        });
    }

    status_poller::spawn(proxy.clone());

    let mut window = None::<tao::window::Window>;
    let mut webview = None::<wry::WebView>;
    let mut transfer_window = None::<transfer_window::TransferWindow>;
    let mut transfers: Vec<serde_json::Value> = Vec::new();
    let mut warmed = false;

    if open_settings {
        let _ = proxy.send_event(UserEvent::ShowSettings);
    }

    event_loop.run(move |event, event_loop, control_flow| {
        *control_flow = ControlFlow::Wait;

        match event {
            Event::NewEvents(StartCause::Init) => {
                // Warm WebView2 in a hidden window so first open is faster
                if !warmed && !open_settings {
                    warmed = true;
                    let mut wb = WindowBuilder::new()
                        .with_title(app_title())
                        .with_inner_size(tao::dpi::LogicalSize::new(1100.0, 720.0))
                        .with_visible(false);
                    if let Some(ic) = icon::app_window_icon() {
                        wb = wb.with_window_icon(Some(ic));
                    }
                    let w = wb.build(event_loop).expect("window");
                    match build_webview(&w, proxy.clone()) {
                        Ok(wv) => {
                            webview = Some(wv);
                            window = Some(w);
                            tracing::info!("webview warmed (hidden)");
                        }
                        Err(e) => tracing::error!("webview warm failed: {e}"),
                    }
                }
            }
            Event::UserEvent(UserEvent::ShowSettings) => {
                if window.is_none() {
                    let mut wb = WindowBuilder::new()
                        .with_title(app_title())
                        .with_inner_size(tao::dpi::LogicalSize::new(1100.0, 720.0));
                    if let Some(ic) = icon::app_window_icon() {
                        wb = wb.with_window_icon(Some(ic));
                    }
                    let w = wb.build(event_loop).expect("window");
                    match build_webview(&w, proxy.clone()) {
                        Ok(wv) => {
                            webview = Some(wv);
                            // Wayland CSD: heal dead close button after first map.
                            #[cfg(target_os = "linux")]
                            {
                                w.set_resizable(false);
                                w.set_resizable(true);
                            }
                            window = Some(w);
                        }
                        Err(e) => {
                            tracing::error!("webview create failed: {e}");
                        }
                    }
                } else if let Some(w) = window.as_ref() {
                    w.set_visible(true);
                    w.set_minimized(false);
                    // Briefly on top so a pairing prompt is not buried under other windows.
                    w.set_always_on_top(true);
                    w.set_focus();
                    w.set_always_on_top(false);
                    // Hide→show leaves GTK CSD close dead until a configure
                    // (e.g. maximize). Toggle resizable to force that heal.
                    #[cfg(target_os = "linux")]
                    {
                        w.set_resizable(false);
                        w.set_resizable(true);
                    }
                }
            }
            Event::UserEvent(UserEvent::Transfers(list)) => {
                if transfer_window.is_none() && !list.is_empty() {
                    transfer_window = transfer_window::TransferWindow::create(event_loop);
                }
                if let Some(tw) = transfer_window.as_mut() {
                    tw.update(&list);
                }
                transfers = list;
            }
            Event::UserEvent(UserEvent::TrayLink {
                connected,
                being_controlled,
                blink_on,
            }) => {
                let show_dot = connected && (!being_controlled || blink_on);
                let dot = if show_dot {
                    icon::GreenDot::On
                } else {
                    icon::GreenDot::Off
                };
                #[cfg(not(target_os = "linux"))]
                if let Some(tray) = tray_icon.as_ref() {
                    let _ = tray.set_icon(Some(icon::app_tray_icon(dot)));
                }
                #[cfg(target_os = "linux")]
                {
                    linux_tray.update(|t| {
                        t.connected = connected;
                        t.being_controlled = being_controlled;
                        t.blink_on = blink_on;
                    });
                    let _ = dot;
                }
            }
            Event::UserEvent(UserEvent::PickLogFile(current)) => {
                if let (Some(w), Some(wv)) = (window.as_ref(), webview.as_ref()) {
                    let start = if current.trim().is_empty() {
                        csc_config::default_log_path()
                    } else {
                        std::path::PathBuf::from(current.trim())
                    };
                    if let Some(path) = log_picker::pick(w, &start) {
                        let js = json!(path.display().to_string());
                        let _ = wv.evaluate_script(&format!("window.onLogFilePicked({js});"));
                    }
                }
            }
            Event::UserEvent(UserEvent::TrayLanguage) => {
                #[cfg(not(target_os = "linux"))]
                {
                    tray_settings.set_text(locale::settings());
                    tray_paste.set_text(locale::paste());
                    tray_quit.set_text(locale::quit());
                    let sharing_on = ipc_client::get_status()
                        .ok()
                        .and_then(|s| s.get("sharingEnabled").and_then(|v| v.as_bool()))
                        .unwrap_or(true);
                    tray_share.set_text(if sharing_on {
                        locale::pause_sharing()
                    } else {
                        locale::resume_sharing()
                    });
                    tray_share.set_checked(!sharing_on);
                }
            }
            Event::UserEvent(UserEvent::PasteHere) => {
                if let Err(e) = ipc_client::call("paste_remote_files", serde_json::json!({})) {
                    tracing::warn!("paste remote files: {e:#}");
                }
            }
            Event::UserEvent(UserEvent::ToggleSharing) => {
                let sharing_on = ipc_client::get_status()
                    .ok()
                    .and_then(|s| s.get("sharingEnabled").and_then(|v| v.as_bool()))
                    .unwrap_or(true);
                if let Err(e) = ipc_client::set_sharing_enabled(!sharing_on) {
                    tracing::warn!("toggle sharing: {e:#}");
                }
                #[cfg(not(target_os = "linux"))]
                {
                    let now = !sharing_on;
                    tray_share.set_text(if now {
                        locale::pause_sharing()
                    } else {
                        locale::resume_sharing()
                    });
                    tray_share.set_checked(!now);
                }
            }
            Event::UserEvent(UserEvent::Quit) => {
                let _ = ipc_client::quit_core();
                #[cfg(not(target_os = "linux"))]
                tray_icon.take();
                webview.take();
                window.take();
                *control_flow = ControlFlow::Exit;
            }
            Event::WindowEvent {
                event: WindowEvent::CloseRequested,
                window_id,
                ..
            } => {
                if let Some(w) = window.as_ref() {
                    if w.id() == window_id {
                        w.set_visible(false);
                    }
                }
                if let Some(tw) = transfer_window.as_mut() {
                    if tw.id() == window_id {
                        tw.close(&transfers);
                    }
                }
            }
            _ => {}
        }
    });
}

#[cfg(test)]
mod tests {
    #[test]
    fn pairing_code_pops_the_window() {
        let none = serde_json::json!({"pairingCode": "", "pendingPeer": null});
        assert!(super::popup_key(&none).is_empty());
        let code = serde_json::json!({"pairingCode": "123456"});
        let other = serde_json::json!({"pairingCode": "654321"});
        assert!(!super::popup_key(&code).is_empty());
        assert_ne!(super::popup_key(&code), super::popup_key(&other));
        let peer = serde_json::json!({"pendingPeer": {"hostName": "Desk"}});
        assert!(!super::popup_key(&peer).is_empty());
    }

    #[test]
    fn index_html_embeds_package_version() {
        let html = String::from_utf8(super::index_html()).expect("utf-8");
        assert!(html.contains(&format!("v{}", super::APP_VERSION)));
        assert!(!html.contains("__CSC_VERSION__"));
    }
}
