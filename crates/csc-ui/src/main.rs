#![cfg_attr(windows, windows_subsystem = "windows")]

mod icon;
mod ipc_client;

use anyhow::Context;
use serde_json::json;
use std::borrow::Cow;
use tao::{
    event::{Event, StartCause, WindowEvent},
    event_loop::{ControlFlow, EventLoopBuilder},
    window::WindowBuilder,
};
use tray_icon::{
    menu::{Menu, MenuEvent, MenuItem},
    TrayIconBuilder, TrayIconEvent,
};
use wry::{
    http::{header::CONTENT_TYPE, Method, Request, Response, StatusCode},
    WebViewBuilder,
};

enum UserEvent {
    ShowSettings,
    Quit,
    ToggleShare,
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
        let html = include_str!("../ui/index.html");
        return Ok(cors_headers(Response::builder())
            .header(CONTENT_TYPE, "text/html; charset=utf-8")
            .body(Cow::Borrowed(html.as_bytes()))?);
    }

    if path == "/status" && method == Method::GET {
        let v = ipc_client::get_status().unwrap_or_else(|e| {
            json!({
                "connected": false,
                "peerName": "",
                "activeScreen": "",
                "sharingEnabled": false,
                "role": "",
                "localName": "",
                "launchAtLogin": false,
                "hostName": "",
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

    if path == "/logs" && method == Method::GET {
        match ipc_client::get_logs() {
            Ok(v) => {
                let body = serde_json::to_vec(&v)?;
                return Ok(cors_headers(Response::builder())
                    .header(CONTENT_TYPE, "application/json")
                    .body(Cow::Owned(body))?);
            }
            Err(e) => {
                let body = serde_json::to_vec(&json!({"text": "", "error": e.to_string()}))?;
                return Ok(cors_headers(Response::builder())
                    .status(StatusCode::SERVICE_UNAVAILABLE)
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
    } else if path == "/peer-decision" && method == Method::POST {
        let body: serde_json::Value = serde_json::from_slice(req.body()).unwrap_or(serde_json::Value::Null);
        let host = body.get("hostName").and_then(|v| v.as_str()).unwrap_or("");
        let decision = body.get("decision").and_then(|v| v.as_str()).unwrap_or("");
        match ipc_client::resolve_peer(host, decision) {
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
        match ipc_client::save_config(body) {
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
    } else {
        Ok(cors_headers(Response::builder())
            .status(StatusCode::NOT_FOUND)
            .body(Cow::Borrowed(&b"not found"[..]))?)
    }
}

fn build_webview(window: &tao::window::Window) -> wry::Result<wry::WebView> {
    let builder = WebViewBuilder::new()
        .with_custom_protocol("csc".into(), move |_webview_id, request| {
            match handle_http(request) {
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
            }
        })
        // WebView2: use http://{scheme}.localhost so fetch() works
        .with_url("http://csc.localhost/")
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

fn main() -> anyhow::Result<()> {
    // Log to file so GUI subsystem still has diagnostics
    {
        use tracing_subscriber::fmt;
        let filter = tracing_subscriber::EnvFilter::try_new("info").unwrap_or_default();
        if let Some(dir) = dirs::config_dir() {
            let log_dir = dir.join("Cross-SystemConnect");
            let _ = std::fs::create_dir_all(&log_dir);
            let path = log_dir.join("ui.log");
            if let Ok(file) = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&path)
            {
                fmt().with_env_filter(filter).with_writer(file).with_ansi(false).init();
            } else {
                fmt().with_env_filter(filter).with_ansi(false).init();
            }
        } else {
            fmt().with_env_filter(filter).with_ansi(false).init();
        }
    }

    let open_settings = std::env::args().any(|a| a == "--open-settings");

    if let Err(e) = ipc_client::ensure_core_running() {
        tracing::error!("start core failed: {e:#}");
        // Continue so tray still appears; settings will show IPC errors
    }

    let event_loop = EventLoopBuilder::<UserEvent>::with_user_event().build();
    let proxy = event_loop.create_proxy();

    let tray_menu = Menu::new();
    let item_settings = MenuItem::new("打开设置", true, None);
    let item_share = MenuItem::new("暂停/启用共享", true, None);
    let item_quit = MenuItem::new("退出", true, None);
    tray_menu.append(&item_settings)?;
    tray_menu.append(&item_share)?;
    tray_menu.append(&item_quit)?;

    let tray_icon_img = icon::app_tray_icon();
    let mut tray_icon = Some(
        TrayIconBuilder::new()
            .with_menu(Box::new(tray_menu))
            .with_tooltip("Cross-SystemConnect")
            .with_icon(tray_icon_img)
            .build()
            .context("create tray icon")?,
    );

    let settings_id = item_settings.id().clone();
    let share_id = item_share.id().clone();
    let quit_id = item_quit.id().clone();

    MenuEvent::set_event_handler(Some({
        let proxy = proxy.clone();
        move |event: MenuEvent| {
            if event.id == settings_id {
                let _ = proxy.send_event(UserEvent::ShowSettings);
            } else if event.id == share_id {
                let _ = proxy.send_event(UserEvent::ToggleShare);
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

    // Bring the settings window forward when a client asks to connect.
    {
        let proxy = proxy.clone();
        std::thread::spawn(move || {
            let mut shown_for = String::new();
            loop {
                std::thread::sleep(std::time::Duration::from_millis(800));
                let host = ipc_client::get_status()
                    .ok()
                    .and_then(|v| {
                        v.get("pendingPeer")
                            .and_then(|p| p.get("hostName"))
                            .and_then(|s| s.as_str())
                            .map(|s| s.to_string())
                    })
                    .unwrap_or_default();
                if host.is_empty() {
                    shown_for.clear();
                    continue;
                }
                if host != shown_for {
                    shown_for = host;
                    let _ = proxy.send_event(UserEvent::ShowSettings);
                }
            }
        });
    }

    let mut window = None::<tao::window::Window>;
    let mut webview = None::<wry::WebView>;
    let mut sharing = true;
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
                        .with_title("Cross-SystemConnect")
                        .with_inner_size(tao::dpi::LogicalSize::new(1100.0, 720.0))
                        .with_visible(false);
                    if let Some(ic) = icon::app_window_icon() {
                        wb = wb.with_window_icon(Some(ic));
                    }
                    let w = wb.build(event_loop).expect("window");
                    match build_webview(&w) {
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
                        .with_title("Cross-SystemConnect")
                        .with_inner_size(tao::dpi::LogicalSize::new(1100.0, 720.0));
                    if let Some(ic) = icon::app_window_icon() {
                        wb = wb.with_window_icon(Some(ic));
                    }
                    let w = wb.build(event_loop).expect("window");
                    match build_webview(&w) {
                        Ok(wv) => {
                            webview = Some(wv);
                            window = Some(w);
                        }
                        Err(e) => {
                            tracing::error!("webview create failed: {e}");
                        }
                    }
                } else if let Some(w) = window.as_ref() {
                    w.set_visible(true);
                    w.set_focus();
                }
            }
            Event::UserEvent(UserEvent::ToggleShare) => {
                sharing = !sharing;
                let _ = ipc_client::set_sharing_enabled(sharing);
            }
            Event::UserEvent(UserEvent::Quit) => {
                let _ = ipc_client::quit_core();
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
            }
            _ => {}
        }
    });
}
