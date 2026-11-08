//! eframe App shell: settings pages, tray events, status polling, transfer viewport.

use crate::events::{EventBus, UiEvent};
use crate::ui::{self, TransferUi};
use csc_config::Config;
use eframe::egui::{self, ViewportCommand};
use serde_json::Value;
use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};

/// Doc/y.html sidebar tabs (5).
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Page {
    /// 设备与拓扑
    Home,
    /// 共享与文件
    Sharing,
    /// 切换与热键
    Switch,
    /// 系统与网络（网络 / 通知语言 / 隐私 / 高级）
    System,
    /// 关于软件
    About,
}

pub struct App {
    pub cfg: Config,
    pub status: Value,
    pub page: Page,
    pub message: String,
    pub message_ok: bool,
    pub pair_digits: [String; 6],
    pub last_pair_sent: String,
    /// Peer being dragged on the topology desk: name + pointer grab offset.
    pub desk_drag: Option<(String, egui::Vec2)>,
    /// Free-drag center while moving a peer (desk-local coords).
    pub desk_drag_pos: Option<egui::Pos2>,
    pub log_text: String,
    pub transfer: TransferUi,
    /// Sidebar / about logo from `resources/app.png`.
    pub logo: Option<egui::TextureHandle>,
    pub updater: crate::update::Updater,

    events: Receiver<UiEvent>,
    bus: EventBus,
    settings_visible: bool,
    save_pending: bool,
    last_status_at: Instant,
    last_popup_key: String,
    #[cfg(not(target_os = "linux"))]
    tray: Option<TrayParts>,
    #[cfg(target_os = "linux")]
    _linux_tray: Option<ksni::Handle<crate::linux_tray::LinuxTray>>,
    quitting: bool,
}

#[cfg(not(target_os = "linux"))]
struct TrayParts {
    icon: tray_icon::TrayIcon,
    settings: tray_icon::menu::MenuItem,
    quit: tray_icon::menu::MenuItem,
}

impl App {
    pub fn new(
        cc: &eframe::CreationContext<'_>,
        bus: EventBus,
        events: Receiver<UiEvent>,
        open_settings: bool,
    ) -> Self {
        crate::theme::install(&cc.egui_ctx);
        bus.bind_context(cc.egui_ctx.clone());
        crate::status_poller::spawn(bus.clone(), cc.egui_ctx.clone());
        let logo = crate::icon::load_logo_texture(&cc.egui_ctx, 128);

        let cfg = load_config();
        crate::locale::apply(&cfg.gui.language);
        let _ = crate::autostart::apply(cfg.gui.launch_at_login);

        // auto_hide：启动后藏到托盘。未传 --open-settings 且配置允许时隐藏。
        let settings_visible = open_settings || !cfg.gui.auto_hide;
        if !settings_visible {
            cc.egui_ctx
                .send_viewport_cmd(ViewportCommand::Visible(false));
        } else {
            cc.egui_ctx
                .send_viewport_cmd(ViewportCommand::Visible(true));
        }

        #[cfg(not(target_os = "linux"))]
        let tray = build_tray(bus.clone());
        #[cfg(target_os = "linux")]
        let linux_tray = crate::linux_tray::spawn(bus.clone());

        // Bring settings forward when a peer asks to connect or a code must show.
        {
            let bus = bus.clone();
            std::thread::spawn(move || {
                let mut shown_for = String::new();
                loop {
                    std::thread::sleep(Duration::from_millis(500));
                    let key = crate::ipc_client::get_status()
                        .ok()
                        .map(|v| popup_key(&v))
                        .unwrap_or_default();
                    if key.is_empty() {
                        shown_for.clear();
                        continue;
                    }
                    if key != shown_for {
                        shown_for = key;
                        bus.show_settings_now();
                    }
                }
            });
        }

        let mut app = Self {
            cfg,
            status: Value::Null,
            page: Page::Home,
            message: String::new(),
            message_ok: true,
            pair_digits: std::array::from_fn(|_| String::new()),
            last_pair_sent: String::new(),
            desk_drag: None,
            desk_drag_pos: None,
            log_text: String::new(),
            transfer: TransferUi::default(),
            logo,
            events,
            bus,
            settings_visible,
            save_pending: false,
            last_status_at: Instant::now() - Duration::from_secs(2),
            last_popup_key: String::new(),
            #[cfg(not(target_os = "linux"))]
            tray,
            #[cfg(target_os = "linux")]
            _linux_tray: linux_tray,
            quitting: false,
            updater: crate::update::Updater::default(),
        };
        if app.cfg.gui.check_updates {
            app.updater.check(&cc.egui_ctx);
        }
        app.reload_status();
        app.reload_logs();
        app
    }

    pub fn t<'a>(&self, zh: &'a str, en: &'a str) -> &'a str {
        if crate::locale::is_english() {
            en
        } else {
            zh
        }
    }

    pub fn page_title(&self) -> &str {
        match self.page {
            Page::Home => self.t("设备与拓扑", "Devices and topology"),
            Page::Sharing => self.t("共享与文件", "Sharing and files"),
            Page::Switch => self.t("切换与热键", "Switching and hotkeys"),
            Page::System => self.t("系统与网络", "System and network"),
            Page::About => self.t("关于软件", "About"),
        }
    }

    pub fn set_ok(&mut self, msg: impl Into<String>) {
        self.message = msg.into();
        self.message_ok = true;
    }

    pub fn set_err(&mut self, msg: impl Into<String>) {
        self.message = msg.into();
        self.message_ok = false;
    }

    pub fn queue_save(&mut self) {
        self.save_pending = true;
    }

    pub fn reload_status(&mut self) {
        let before = trust_sync_key(&self.status);
        match crate::ipc_client::get_status() {
            Ok(v) => self.status = v,
            Err(e) => {
                self.status = serde_json::json!({
                    "connected": false,
                    "error": e.to_string(),
                    "localName": csc_config::local_computer_name(),
                });
            }
        }
        self.last_status_at = Instant::now();
        // Core writes trusted_peers on pin/remember; refresh so the device list
        // and topology unplaced names update without a manual save.
        let after = trust_sync_key(&self.status);
        if before != after {
            self.sync_trusted_from_core();
        }
    }

    /// Pull trusted peers (and screens when idle) from core after a link change.
    fn sync_trusted_from_core(&mut self) {
        let Ok(v) = crate::ipc_client::get_config() else {
            return;
        };
        let Ok(remote) = serde_json::from_value::<Config>(v) else {
            return;
        };
        self.cfg.trusted_peers = remote.trusted_peers;
        self.cfg.peer_fingerprints = remote.peer_fingerprints;
        if !self.save_pending {
            self.cfg.screens = remote.screens;
            self.cfg.layout = remote.layout;
            if self.cfg.ensure_trusted_screen_places() {
                self.queue_save();
            }
        }
    }

    pub fn reload_config(&mut self) {
        self.cfg = load_config();
        crate::locale::apply(&self.cfg.gui.language);
        self.refresh_tray_labels();
    }

    pub fn reload_logs(&mut self) {
        match crate::ipc_client::get_logs() {
            Ok(v) => {
                self.log_text = v
                    .get("text")
                    .or_else(|| v.get("logs"))
                    .and_then(|t| t.as_str())
                    .unwrap_or("")
                    .to_string();
            }
            Err(e) => self.log_text = e.to_string(),
        }
    }

    fn flush_save(&mut self) {
        if !self.save_pending {
            return;
        }
        self.save_pending = false;
        // Rebuild switch edges from desk placements before writing.
        self.cfg.sync_layout_from_screens();
        let Ok(body) = serde_json::to_value(&self.cfg) else {
            self.set_err("serialize config failed");
            return;
        };
        match crate::ipc_client::save_config(body) {
            Ok(_) => {
                self.reload_config();
                // Do not rescan here — rediscovery after every save flaps dual-dial sessions.
                self.reload_status();
                self.set_ok(self.t("已保存", "Saved"));
                self.refresh_tray_labels();
            }
            Err(e) => self.set_err(e.to_string()),
        }
    }

    pub fn show_settings(&mut self, ctx: &egui::Context) {
        self.settings_visible = true;
        // Unminimize before mapping — GNOME Wayland often ignores Visible after a hide.
        ctx.send_viewport_cmd(ViewportCommand::Minimized(false));
        ctx.send_viewport_cmd(ViewportCommand::Visible(true));
        ctx.send_viewport_cmd(ViewportCommand::Focus);
        // Briefly on top so a pairing prompt / tray restore is not buried.
        ctx.send_viewport_cmd(ViewportCommand::WindowLevel(egui::WindowLevel::AlwaysOnTop));
        ctx.send_viewport_cmd(ViewportCommand::WindowLevel(egui::WindowLevel::Normal));
        ctx.request_repaint();
    }

    pub fn hide_settings(&mut self, ctx: &egui::Context) {
        self.settings_visible = false;
        // A native Wayland window cannot be hidden or unminimized by the app, so
        // only minimize it there. The X11 window (also under XWayland) is unmapped.
        #[cfg(target_os = "linux")]
        {
            if crate::x11_window() {
                ctx.send_viewport_cmd(ViewportCommand::Visible(false));
            } else {
                ctx.send_viewport_cmd(ViewportCommand::Minimized(true));
            }
        }
        #[cfg(not(target_os = "linux"))]
        {
            ctx.send_viewport_cmd(ViewportCommand::Visible(false));
            ctx.send_viewport_cmd(ViewportCommand::Minimized(true));
        }
        ctx.request_repaint();
    }

    pub fn request_quit(&mut self, _ctx: &egui::Context) {
        // Title-bar Quit path: never block on IPC (same as tray / dock Quit).
        self.quitting = true;
        crate::singleton::release();
        crate::ipc_client::force_quit_now();
    }

    fn drain_events(&mut self, ctx: &egui::Context) {
        if crate::singleton::take_show_request() || self.bus.take_show_requested() {
            self.show_settings(ctx);
        }
        while let Ok(ev) = self.events.try_recv() {
            match ev {
                UiEvent::ShowSettings => {
                    self.show_settings(ctx);
                }
                UiEvent::Transfers(list) => {
                    self.transfer.set_transfers(list);
                }
                UiEvent::TrayLink {
                    connected,
                    being_controlled,
                    blink_on,
                } => {
                    self.apply_tray_link(connected, being_controlled, blink_on);
                }
            }
        }
    }

    fn apply_tray_link(&mut self, connected: bool, being_controlled: bool, blink_on: bool) {
        let show_dot = connected && (!being_controlled || blink_on);
        let dot = if show_dot {
            crate::icon::GreenDot::On
        } else {
            crate::icon::GreenDot::Off
        };
        #[cfg(not(target_os = "linux"))]
        if let Some(tray) = self.tray.as_ref() {
            let _ = tray.icon.set_icon(Some(crate::icon::app_tray_icon(dot)));
        }
        #[cfg(target_os = "linux")]
        if let Some(handle) = self._linux_tray.as_ref() {
            handle.update(|t| {
                t.connected = connected;
                t.being_controlled = being_controlled;
                t.blink_on = blink_on;
            });
            let _ = dot;
        }
    }

    fn refresh_tray_labels(&mut self) {
        #[cfg(not(target_os = "linux"))]
        if let Some(tray) = self.tray.as_ref() {
            tray.settings.set_text(crate::locale::settings());
            tray.quit.set_text(crate::locale::quit());
        }
        #[cfg(target_os = "linux")]
        if let Some(handle) = self._linux_tray.as_ref() {
            // Rebuild menu strings (locale) from Tray::menu().
            handle.update(|_| {});
        }
    }
}

impl eframe::App for App {
    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        // Transparent so the painted rounded window chrome shows through.
        [0.0, 0.0, 0.0, 0.0]
    }

    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.drain_events(ctx);

        if self.last_status_at.elapsed() >= Duration::from_millis(500) {
            self.reload_status();
            let key = popup_key(&self.status);
            if !key.is_empty() && key != self.last_popup_key {
                self.last_popup_key = key;
                self.show_settings(ctx);
            } else if key.is_empty() {
                self.last_popup_key.clear();
            }
        }

        self.flush_save();

        if ctx.input(|i| i.viewport().close_requested()) {
            // Title-bar ✕ already calls hide_settings when close_to_tray is on.
            // close_requested is from the shell/taskbar Quit — must actually exit.
            // SIGTERM from the dock is handled in unix_signals (watcher thread).
            if !self.quitting {
                self.quitting = true;
                crate::singleton::release();
                crate::ipc_client::force_quit_now();
            }
        }

        // Child transfer viewport needs a root UI pass; unhide briefly while busy.
        let transfer_busy = self.transfer.is_open();
        if transfer_busy && !self.settings_visible {
            ctx.send_viewport_cmd(ViewportCommand::Visible(true));
            ctx.send_viewport_cmd(ViewportCommand::Minimized(false));
        } else if !self.settings_visible && !transfer_busy {
            // Do not re-assert Minimized every frame — that races tray ShowSettings.
            #[cfg(not(target_os = "linux"))]
            ctx.send_viewport_cmd(ViewportCommand::Visible(false));
        }

        if !self.settings_visible || transfer_busy {
            // Keep polling tray events / ui.show while "hidden".
            ctx.request_repaint_after(Duration::from_millis(200));
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        // 根窗口一旦进入 ui 通道就说明已映射；始终绘制，避免 Visible(false)
        // 在部分桌面未生效时出现纯黑空窗。
        ui::draw(self, ui);
        self.transfer.show(ui.ctx(), !crate::locale::is_english());
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        if !self.quitting {
            let _ = crate::ipc_client::quit_core();
        }
        #[cfg(not(target_os = "linux"))]
        {
            self.tray.take();
        }
    }
}

fn load_config() -> Config {
    match crate::ipc_client::get_config() {
        Ok(v) => serde_json::from_value(v).unwrap_or_default(),
        Err(_) => Config::default(),
    }
}

/// Detect when core likely wrote trust / link state that the UI config cache misses.
fn trust_sync_key(status: &Value) -> String {
    let connected = status
        .get("connected")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let peer = status
        .get("peerName")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let host_state = status
        .get("hostState")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let pending = status
        .get("pendingPeer")
        .and_then(|p| p.get("hostName"))
        .and_then(|s| s.as_str())
        .unwrap_or("");
    let discovered = status
        .get("discovered")
        .and_then(|v| v.as_array())
        .map(|a| a.len())
        .unwrap_or(0);
    format!("{connected}|{peer}|{host_state}|{pending}|{discovered}")
}

/// What should pop the settings window: a client asking to connect, or a
/// pairing code this computer must show. Empty when nothing is waiting.
pub fn popup_key(status: &Value) -> String {
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

#[cfg(not(target_os = "linux"))]
fn build_tray(bus: EventBus) -> Option<TrayParts> {
    use tray_icon::menu::{Menu, MenuEvent, MenuItem};
    use tray_icon::{TrayIconBuilder, TrayIconEvent};

    let tray_menu = Menu::new();
    let item_settings = MenuItem::new(crate::locale::settings(), true, None);
    let item_quit = MenuItem::new(crate::locale::quit(), true, None);
    let _ = tray_menu.append(&item_settings);
    let _ = tray_menu.append(&item_quit);

    let tray_icon = TrayIconBuilder::new()
        .with_menu(Box::new(tray_menu))
        .with_tooltip(crate::app_title())
        .with_icon(crate::icon::app_tray_icon(crate::icon::GreenDot::Off))
        .build()
        .ok()?;

    let settings_id = item_settings.id().clone();
    let quit_id = item_quit.id().clone();

    MenuEvent::set_event_handler(Some({
        let bus = bus.clone();
        move |event: MenuEvent| {
            if event.id == settings_id {
                bus.show_settings_now();
            } else if event.id == quit_id {
                crate::singleton::release();
                crate::ipc_client::force_quit_now();
            }
        }
    }));

    TrayIconEvent::set_event_handler(Some({
        let bus = bus.clone();
        move |event: TrayIconEvent| {
            if let TrayIconEvent::DoubleClick { .. } = event {
                bus.show_settings_now();
            }
        }
    }));

    Some(TrayParts {
        icon: tray_icon,
        settings: item_settings,
        quit: item_quit,
    })
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
}
