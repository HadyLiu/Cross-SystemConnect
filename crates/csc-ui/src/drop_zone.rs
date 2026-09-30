//! Always-on-top window at a screen edge while files are dragged there.
//!
//! When the computer beyond the edge can take a virtual drag, the window is a
//! thin strip: files entering it hand the drag to that computer, where the
//! pointer carries on and the user drops them. Otherwise it is a larger
//! target, and dropping on it sends the files to that computer's receive folder.

use crate::{ipc_client, UserEvent};
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use tao::dpi::{PhysicalPosition, PhysicalSize};
use tao::event_loop::EventLoopWindowTarget;
use tao::window::{Window, WindowBuilder};
use wry::{DragDropEvent, WebView, WebViewBuilder};

#[derive(Debug, Clone, PartialEq)]
pub struct Zone {
    pub peer: String,
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
    pub virtual_drag: bool,
}

pub fn zone_from_status(v: &serde_json::Value) -> Option<Zone> {
    let z = v.get("dropZone")?;
    let int = |k: &str| z.get(k).and_then(|n| n.as_i64()).map(|n| n as i32);
    let virtual_drag = z.get("virtualDrag").and_then(|b| b.as_bool()).unwrap_or(false);
    let min = if virtual_drag { 4 } else { 40 };
    Some(Zone {
        peer: z.get("peer")?.as_str()?.to_string(),
        x: int("x")?,
        y: int("y")?,
        width: int("width")?.max(min),
        height: int("height")?.max(min),
        virtual_drag,
    })
}

const HTML: &str = r#"<!doctype html><html><head><meta charset="utf-8"><style>
html,body{margin:0;height:100%;background:#16202c;color:#e8f1ff;font:14px system-ui,sans-serif;overflow:hidden}
body{display:flex;align-items:center;justify-content:center;text-align:center;box-sizing:border-box;
border:3px dashed #4da3ff;border-radius:10px}
body.strip{border:0;border-radius:0;background:#3b8cff}
body.strip>div{display:none}
.big{font-size:30px;line-height:1.2}
b{color:#7cc4ff;word-break:break-all}
</style></head><body><div><div class="big">⇪</div>松开发送到<br><b id="p"></b></div></body></html>"#;

fn to_strings(paths: &[std::path::PathBuf]) -> Vec<String> {
    paths
        .iter()
        .map(|p| p.to_string_lossy().into_owned())
        .collect()
}

pub struct DropWindow {
    window: Window,
    webview: WebView,
    zone: Rc<RefCell<Option<Zone>>>,
    /// The current drag was already handed to the other computer.
    handed: Rc<Cell<bool>>,
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
        let zone: Rc<RefCell<Option<Zone>>> = Rc::new(RefCell::new(None));
        let handed = Rc::new(Cell::new(false));
        let (h_zone, h_handed) = (zone.clone(), handed.clone());
        let builder = WebViewBuilder::new()
            .with_html(HTML)
            .with_drag_drop_handler(move |event| {
                let Some(z) = h_zone.borrow().clone() else {
                    return true;
                };
                match event {
                    DragDropEvent::Enter { paths, .. } if z.virtual_drag && !h_handed.get() => {
                        let paths = to_strings(&paths);
                        if !paths.is_empty() {
                            h_handed.set(true);
                            std::thread::spawn(move || {
                                if let Err(e) = ipc_client::drag_enter(&z.peer, &paths) {
                                    tracing::warn!("hand drag to {}: {e:#}", z.peer);
                                }
                            });
                        }
                    }
                    DragDropEvent::Drop { paths, .. } if !h_handed.get() => {
                        let paths = to_strings(&paths);
                        if !paths.is_empty() {
                            std::thread::spawn(move || {
                                if let Err(e) = ipc_client::send_files(&z.peer, &paths) {
                                    tracing::warn!("send dropped files: {e:#}");
                                }
                            });
                        }
                    }
                    _ => {}
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
                zone,
                handed,
            }),
            Err(e) => {
                tracing::error!("drop window webview: {e}");
                None
            }
        }
    }

    pub fn show(&self, zone: &Zone) {
        let same_edge = self
            .zone
            .borrow()
            .as_ref()
            .is_some_and(|z| z.peer == zone.peer && z.virtual_drag == zone.virtual_drag);
        if !same_edge {
            self.handed.set(false);
        }
        *self.zone.borrow_mut() = Some(zone.clone());
        let name = serde_json::to_string(&zone.peer).unwrap_or_else(|_| "\"\"".into());
        let _ = self.webview.evaluate_script(&format!(
            "document.getElementById('p').textContent={name};document.body.className={};",
            if zone.virtual_drag { "'strip'" } else { "''" }
        ));
        self.window
            .set_inner_size(PhysicalSize::new(zone.width as u32, zone.height as u32));
        self.window
            .set_outer_position(PhysicalPosition::new(zone.x, zone.y));
        self.window.set_visible(true);
        self.window.set_always_on_top(true);
    }

    pub fn hide(&self) {
        self.window.set_visible(false);
        self.handed.set(false);
        *self.zone.borrow_mut() = None;
    }
}
