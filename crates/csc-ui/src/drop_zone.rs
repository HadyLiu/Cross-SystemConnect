//! Small always-on-top window at a screen edge while files are dragged there.
//! Dropping on it sends the files to the computer beyond that edge.

use crate::{ipc_client, UserEvent};
use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;
use tao::dpi::{PhysicalPosition, PhysicalSize};
use tao::event_loop::{EventLoopProxy, EventLoopWindowTarget};
use tao::window::{Window, WindowBuilder};
use wry::{DragDropEvent, WebView, WebViewBuilder};

#[derive(Debug, Clone, PartialEq)]
pub struct Zone {
    pub peer: String,
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

fn zone_from_status(v: &serde_json::Value) -> Option<Zone> {
    let z = v.get("dropZone")?;
    let int = |k: &str| z.get(k).and_then(|n| n.as_i64()).map(|n| n as i32);
    Some(Zone {
        peer: z.get("peer")?.as_str()?.to_string(),
        x: int("x")?,
        y: int("y")?,
        width: int("width")?.max(40),
        height: int("height")?.max(40),
    })
}

/// Follow the core's drop zone. Polls fast: the zone only matters while a drag
/// is held against the edge.
pub fn watch(proxy: EventLoopProxy<UserEvent>) {
    std::thread::spawn(move || {
        let mut last: Option<Zone> = None;
        loop {
            std::thread::sleep(Duration::from_millis(80));
            let zone = ipc_client::get_status()
                .ok()
                .and_then(|v| zone_from_status(&v));
            if zone != last {
                last = zone.clone();
                if proxy.send_event(UserEvent::DropZone(zone)).is_err() {
                    return;
                }
            }
        }
    });
}

const HTML: &str = r#"<!doctype html><html><head><meta charset="utf-8"><style>
html,body{margin:0;height:100%;background:#16202c;color:#e8f1ff;font:14px system-ui,sans-serif;overflow:hidden}
body{display:flex;align-items:center;justify-content:center;text-align:center;box-sizing:border-box;
border:3px dashed #4da3ff;border-radius:10px}
.big{font-size:30px;line-height:1.2}
b{color:#7cc4ff;word-break:break-all}
</style></head><body><div><div class="big">⇪</div>松开发送到<br><b id="p"></b></div></body></html>"#;

pub struct DropWindow {
    window: Window,
    webview: WebView,
    peer: Rc<RefCell<String>>,
}

impl DropWindow {
    pub fn create(target: &EventLoopWindowTarget<UserEvent>) -> Option<Self> {
        let wb = WindowBuilder::new()
            .with_title("Cross-SystemConnect")
            .with_decorations(false)
            .with_always_on_top(true)
            .with_resizable(false)
            .with_focused(false)
            .with_visible(false)
            .with_inner_size(PhysicalSize::new(160u32, 260u32));
        #[cfg(windows)]
        let wb = {
            use tao::platform::windows::WindowBuilderExtWindows;
            wb.with_skip_taskbar(true)
        };
        #[cfg(target_os = "linux")]
        let wb = {
            use tao::platform::unix::WindowBuilderExtUnix;
            wb.with_skip_taskbar(true)
        };
        let window = match wb.build(target) {
            Ok(w) => w,
            Err(e) => {
                tracing::error!("drop window: {e}");
                return None;
            }
        };
        let peer = Rc::new(RefCell::new(String::new()));
        let target_peer = peer.clone();
        let builder = WebViewBuilder::new()
            .with_html(HTML)
            .with_drag_drop_handler(move |event| {
                if let DragDropEvent::Drop { paths, .. } = event {
                    let peer = target_peer.borrow().clone();
                    let paths: Vec<String> = paths
                        .iter()
                        .map(|p| p.to_string_lossy().into_owned())
                        .collect();
                    if !peer.is_empty() && !paths.is_empty() {
                        std::thread::spawn(move || {
                            if let Err(e) = ipc_client::send_files(&peer, &paths) {
                                tracing::warn!("send dropped files: {e:#}");
                            }
                        });
                    }
                }
                true
            });
        #[cfg(not(target_os = "linux"))]
        let webview = builder.build(&window);
        #[cfg(target_os = "linux")]
        let webview = {
            use tao::platform::unix::WindowExtUnix;
            use wry::WebViewBuilderExtUnix;
            builder.build_gtk(window.default_vbox()?)
        };
        match webview {
            Ok(webview) => Some(Self {
                window,
                webview,
                peer,
            }),
            Err(e) => {
                tracing::error!("drop window webview: {e}");
                None
            }
        }
    }

    pub fn show(&self, zone: &Zone) {
        *self.peer.borrow_mut() = zone.peer.clone();
        let name = serde_json::to_string(&zone.peer).unwrap_or_else(|_| "\"\"".into());
        let _ = self
            .webview
            .evaluate_script(&format!("document.getElementById('p').textContent={name};"));
        self.window
            .set_inner_size(PhysicalSize::new(zone.width as u32, zone.height as u32));
        self.window
            .set_outer_position(PhysicalPosition::new(zone.x, zone.y));
        self.window.set_visible(true);
        self.window.set_always_on_top(true);
    }

    pub fn hide(&self) {
        self.window.set_visible(false);
    }
}
