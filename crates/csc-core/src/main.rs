#![cfg_attr(windows, windows_subsystem = "windows")]

mod clipboard;
mod discover;
mod drag;
mod ipc;
mod layout;
mod net;
mod paste;
mod path_safe;
mod progress;
mod prompt;
mod relay;
mod supervisor;
mod switch;
mod tls;
mod transfer;

use crate::clipboard::{ClipEvent, ClipHandle};
use crate::drag::DragHandle;
use crate::ipc::{claim_ipc, serve_ipc, AppState, IpcBusy};
use crate::layout::ScreenLayout;
use crate::net::{spawn_network, InboundEvent, NetHub};
use crate::paste::{PasteAction, SharedClip};
use crate::prompt::PeerPrompt;
use crate::supervisor::Supervisor;
use crate::switch::{EdgePush, EdgeReport, EnterDecision, SwitchController, SwitchState};
use crate::transfer::TransferHandle;
use anyhow::Context;
use csc_config::{default_config_path, default_log_path, Config};
use csc_platform::{create_input_injector, create_screen_info, query_host_net_info, Captured};
use csc_proto::{
    encode_json_frame, encode_switch_leave_bin, AppStatus, ClipOfferPayload, DragBeginPayload,
    DragCancelPayload, EdgeHitPayload, FileEntry, LocalNic, MsgType, PasteRequestPayload,
    SwitchPayload, CAP_EDGE_REPORT, CAP_VIRTUAL_DRAG,
};
use std::collections::HashMap;
use std::fs::OpenOptions;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{mpsc, watch, RwLock};
use tracing::{debug, info, warn};
use tracing_subscriber::EnvFilter;

const HID_V: u32 = 0x19;
const HID_ESC: u32 = 0x29;
const HID_LCTRL: u32 = 0xE0;
const HID_RCTRL: u32 = 0xE4;
/// Text above this goes on the bulk queue so it never holds up the pointer.
const BULK_TEXT: usize = 64 * 1024;

fn peer_size(
    sizes: &HashMap<String, csc_platform::ScreenSize>,
    name: &str,
    fallback: csc_platform::ScreenSize,
) -> csc_platform::ScreenSize {
    sizes
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case(name))
        .map(|(_, size)| *size)
        .filter(|size| size.width > 1 && size.height > 1)
        .unwrap_or(fallback)
}

/// Where the local pointer reappears after it leaves a remote screen.
/// `leave` is the edge of the remote screen that was crossed, `pct` the
/// position along it, and `m` the local monitor the pointer returns to.
fn reentry_point(leave: csc_proto::Edge, pct: f32, m: csc_platform::Rect) -> (i32, i32) {
    let inset = 12;
    let pct = pct.clamp(0.0, 1.0);
    let lo_x = m.left + inset;
    let hi_x = (m.right - 1 - inset).max(lo_x);
    let lo_y = m.top + inset;
    let hi_y = (m.bottom - 1 - inset).max(lo_y);
    let along_x = (m.left + (m.width() as f32 * pct) as i32).clamp(lo_x, hi_x);
    let along_y = (m.top + (m.height() as f32 * pct) as i32).clamp(lo_y, hi_y);
    match leave.opposite() {
        csc_proto::Edge::Left => (lo_x, along_y),
        csc_proto::Edge::Right => (hi_x, along_y),
        csc_proto::Edge::Top => (along_x, lo_y),
        csc_proto::Edge::Bottom => (along_x, hi_y),
        csc_proto::Edge::TopLeft => (lo_x, lo_y),
        csc_proto::Edge::TopRight => (hi_x, lo_y),
        csc_proto::Edge::BottomLeft => (lo_x, hi_y),
        csc_proto::Edge::BottomRight => (hi_x, hi_y),
    }
}

/// Give input back to this computer. `target` is where the cursor reappears,
/// in desktop coordinates relative to the virtual origin.
fn release_pointer(grab: &mut bool, target: Option<(i32, i32)>) {
    if !*grab {
        return;
    }
    *grab = false;
    csc_platform::capture_set_grab(false, None);
    if let Some((x, y)) = target {
        let (ox, oy) = csc_platform::virtual_origin();
        csc_platform::warp_cursor(ox + x, oy + y);
    }
}

/// Keys and buttons the remote computer currently sees as held down.
#[derive(Default)]
struct HeldInput {
    keys: std::collections::HashSet<u32>,
    buttons: std::collections::HashSet<u32>,
}

impl HeldInput {
    fn track(&mut self, ev: Captured) {
        match ev {
            Captured::Key { hid, down: true } => {
                self.keys.insert(hid);
            }
            Captured::Key { hid, down: false } => {
                self.keys.remove(&hid);
            }
            Captured::Button { button, down: true } => {
                self.buttons.insert(button);
            }
            Captured::Button { button, down: false } => {
                self.buttons.remove(&button);
            }
            _ => {}
        }
    }

    fn ctrl(&self) -> bool {
        self.keys.contains(&HID_LCTRL) || self.keys.contains(&HID_RCTRL)
    }

    /// Release everything on the remote, so no key stays stuck after the pointer leaves.
    async fn release_all(&mut self, net: &NetHub) {
        for hid in self.keys.drain() {
            net.send_captured(Captured::Key { hid, down: false }).await;
        }
        for button in self.buttons.drain() {
            net.send_captured(Captured::Button { button, down: false }).await;
        }
    }

    /// Release everything this computer injected on behalf of a remote host.
    fn release_injected(&mut self, injector: &mut dyn csc_platform::InputInjector) {
        for hid in self.keys.drain() {
            injector.key(hid, false);
        }
        for button in self.buttons.drain() {
            injector.mouse_button(button, false);
        }
    }
}

/// Area against `edge` of monitor `m`, around the pointer, where dragged files
/// are caught. Desktop coordinates relative to the virtual origin. With a
/// virtual drag it is a thin strip: it only has to notice the drag arriving.
fn drop_zone_rect(
    edge: csc_proto::Edge,
    x: i32,
    y: i32,
    m: csc_platform::Rect,
    strip: bool,
) -> csc_platform::Rect {
    use csc_proto::Edge;
    let depth = if strip { 12 } else { 160 };
    const LENGTH: i32 = 260;
    let (w, h) = match edge {
        Edge::Left | Edge::Right => (depth, LENGTH),
        Edge::Top | Edge::Bottom => (LENGTH, depth),
        _ => (depth.max(48), depth.max(48)),
    };
    let w = w.min(m.width());
    let h = h.min(m.height());
    let left = match edge {
        Edge::Left | Edge::TopLeft | Edge::BottomLeft => m.left,
        Edge::Right | Edge::TopRight | Edge::BottomRight => m.right - w,
        Edge::Top | Edge::Bottom => (x - w / 2).clamp(m.left, m.right - w),
    };
    let top = match edge {
        Edge::Top | Edge::TopLeft | Edge::TopRight => m.top,
        Edge::Bottom | Edge::BottomLeft | Edge::BottomRight => m.bottom - h,
        Edge::Left | Edge::Right => (y - h / 2).clamp(m.top, m.bottom - h),
    };
    csc_platform::Rect {
        left,
        top,
        right: left + w,
        bottom: top + h,
    }
}

/// A key combination such as `Ctrl+Alt+Home`, as HID usages.
/// Modifiers are stored left-handed; either hand matches.
#[derive(Debug, Clone, PartialEq)]
struct Hotkey {
    mods: Vec<u32>,
    key: u32,
}

impl Hotkey {
    fn parse(text: &str) -> Option<Self> {
        use csc_platform::keymap::{fold_modifier, hid_from_name};
        let mut usages = text
            .split('+')
            .filter(|p| !p.trim().is_empty())
            .map(|p| hid_from_name(p).map(fold_modifier))
            .collect::<Option<Vec<u32>>>()?;
        let key = usages.pop()?;
        Some(Self { mods: usages, key })
    }

    /// True when `hid` going down completes the combination, given the keys already held.
    fn completes(&self, hid: u32, held: &std::collections::HashSet<u32>) -> bool {
        use csc_platform::keymap::fold_modifier;
        fold_modifier(hid) == self.key
            && self
                .mods
                .iter()
                .all(|m| held.iter().any(|k| fold_modifier(*k) == *m))
    }
}

/// Keep the pointer just inside the edge it crossed, so the physical mouse
/// stays where the hand is and a small move back returns to this computer.
fn park_inside(
    edge: csc_proto::Edge,
    x: i32,
    y: i32,
    screen: csc_platform::ScreenSize,
) -> (i32, i32) {
    let w = screen.width.max(2);
    let h = screen.height.max(2);
    let inset = 24;
    let cx = x.clamp(inset, w - 1 - inset);
    let cy = y.clamp(inset, h - 1 - inset);
    match edge {
        csc_proto::Edge::Left => (inset, cy),
        csc_proto::Edge::Right => (w - 1 - inset, cy),
        csc_proto::Edge::Top => (cx, inset),
        csc_proto::Edge::Bottom => (cx, h - 1 - inset),
        csc_proto::Edge::TopLeft => (inset, inset),
        csc_proto::Edge::TopRight => (w - 1 - inset, inset),
        csc_proto::Edge::BottomLeft => (inset, h - 1 - inset),
        csc_proto::Edge::BottomRight => (w - 1 - inset, h - 1 - inset),
    }
}

fn log_path_of(cfg: &Config) -> PathBuf {
    if !cfg.log.file.is_empty() {
        PathBuf::from(&cfg.log.file)
    } else {
        default_log_path()
    }
}

fn open_log_file(path: &std::path::Path) -> anyhow::Result<std::fs::File> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("create log dir {}", parent.display()))?;
        }
    }
    OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .with_context(|| format!("open log {}", path.display()))
}

/// Open the configured log, or the default path when that file cannot be created.
/// A missing directory must not stop the process before IPC is listening.
fn init_logging(cfg: &Config) -> anyhow::Result<PathBuf> {
    let level = if !cfg.log.level.is_empty() {
        cfg.log.level.as_str()
    } else {
        cfg.log_level.as_str()
    };
    let filter = EnvFilter::try_new(format!(
        "info,Cross_SystemConnect_core={level},csc_platform={level}"
    ))
    .unwrap_or_default();
    let preferred = log_path_of(cfg);
    let fallback = default_log_path();
    let (path, file) = match open_log_file(&preferred) {
        Ok(file) => (preferred, file),
        Err(err) if preferred != fallback => {
            eprintln!("log path {} unavailable: {err:#}", preferred.display());
            (fallback.clone(), open_log_file(&fallback)?)
        }
        Err(err) => return Err(err),
    };
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(file)
        .with_ansi(false)
        .init();
    info!("logging to {}", path.display());
    Ok(path)
}

fn single_instance_lock() -> anyhow::Result<std::fs::File> {
    let dir = default_config_path()
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| PathBuf::from("."));
    std::fs::create_dir_all(&dir)?;
    let lock_path = dir.join("Cross-SystemConnect-core.lock");
    let file = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(&lock_path)?;
    std::fs::write(&lock_path, format!("{}", std::process::id()))?;
    Ok(file)
}

fn nics_of(host: csc_platform::HostNetInfo) -> (String, Vec<String>, Vec<LocalNic>) {
    let nics = host
        .nics
        .into_iter()
        .map(|n| LocalNic {
            address: n.address,
            kind: n.kind,
            name: n.name,
        })
        .collect();
    (host.host_name, host.ipv4_addresses, nics)
}

fn popup(body: &str, error: bool) {
    if error {
        warn!("{body}");
    } else {
        info!("{body}");
    }
    csc_platform::notify("Cross-SystemConnect", body, error);
}

/// Where the edge window came from, and what happens once a file drag enters it.
#[derive(Debug, Clone)]
enum ZoneOrigin {
    /// This computer's own pointer at an edge leading to a neighbor.
    Local { dec: EnterDecision, x: i32, y: i32 },
    /// This computer is controlled; its pointer pushed through an outer edge.
    Controlled(EdgeReport),
}

/// Where the pointer went after a controlled computer reported a drag at its edge.
#[derive(Debug, Clone)]
struct DragRoute {
    from: String,
    /// None: the keyboard host itself.
    to: Option<String>,
    at: Instant,
}

/// Result of checking files dragged into the edge window.
struct DragReady {
    peer: String,
    result: Result<DragBeginPayload, String>,
}

struct Core {
    config: Arc<RwLock<Config>>,
    status: Arc<RwLock<AppStatus>>,
    net: NetHub,
    transfer: TransferHandle,
    drag: DragHandle,
    clip: ClipHandle,
    net_reload: watch::Sender<u64>,
    layout_rx: watch::Receiver<u64>,
    drag_ready_tx: mpsc::Sender<DragReady>,
    injector: Box<dyn csc_platform::InputInjector>,
    screen_info: Box<dyn csc_platform::ScreenInfo>,
    screen: csc_platform::ScreenSize,
    monitors: Vec<csc_platform::Rect>,
    switch: SwitchController,
    hooked: bool,
    remote_x: i32,
    remote_y: i32,
    peer_screens: HashMap<String, csc_platform::ScreenSize>,
    grab: bool,
    grab_armed: bool,
    park_x: i32,
    park_y: i32,
    enter_point: (i32, i32),
    /// Just inside the edge the pointer left through, for returns that cross no edge.
    home_point: (i32, i32),
    held: HeldInput,
    /// Keys and buttons this computer injected for a remote host, released if that host goes away.
    injected: HeldInput,
    return_hotkey: Option<Hotkey>,
    last_miss: Instant,
    next_nic: Instant,
    supervisor: Supervisor,
    drop_zone: Option<csc_proto::DropZone>,
    zone_origin: Option<ZoneOrigin>,
    /// Files in the last drag were refused; no new edge window until the button is released.
    zone_blocked: bool,
    drag_pending: bool,
    drag_route: Option<DragRoute>,
    clip_seq: u64,
    last_remote_clip: u64,
    shared: SharedClip,
    /// Files copied on this computer, by copy sequence number.
    my_files: Option<(u64, Vec<PathBuf>)>,
    /// Controlled computer: push through its own edges, and the last pointer position.
    probe: EdgePush,
    last_real: Option<(i32, i32)>,
    last_edge_sent: Instant,
}

fn hotkey_of(cfg: &Config) -> Option<Hotkey> {
    let text = cfg.hotkeys.return_to_server.clone();
    let parsed = Hotkey::parse(&text);
    if parsed.is_none() && !text.trim().is_empty() {
        warn!("return hotkey {text:?} not understood");
    }
    parsed
}

impl Core {
    async fn is_host(&self) -> bool {
        self.status.read().await.role == "server"
    }

    async fn local_name(&self) -> String {
        self.status.read().await.local_name.clone()
    }

    async fn set_active_screen(&self, name: Option<&str>) {
        let mut s = self.status.write().await;
        s.active_screen = match name {
            Some(n) => n.to_string(),
            None => s.local_name.clone(),
        };
    }

    async fn clear_zone(&mut self) {
        if self.drop_zone.take().is_some() {
            self.status.write().await.drop_zone = None;
        }
        self.zone_origin = None;
    }

    async fn show_zone(&mut self, zone: csc_proto::DropZone, origin: ZoneOrigin) {
        // Keep the first area so the window does not jump under the drag.
        if self.drop_zone.is_some() || self.zone_blocked {
            return;
        }
        debug!("edge window for {} at {},{}", zone.peer, zone.x, zone.y);
        self.status.write().await.drop_zone = Some(zone.clone());
        self.drop_zone = Some(zone);
        self.zone_origin = Some(origin);
    }

    // ------------------------------------------------------------ tick

    async fn tick(&mut self) {
        if self.layout_rx.has_changed().unwrap_or(false) {
            let _ = self.layout_rx.borrow_and_update();
            let cfg = self.config.read().await;
            self.switch.configure(
                cfg.name.clone(),
                self.screen,
                cfg.switch_cfg.clone(),
                ScreenLayout::load(&cfg.layout),
            );
            self.switch.set_screen(self.screen, self.monitors.clone());
            self.return_hotkey = hotkey_of(&cfg);
        }
        if self.grab && !self.net.has_peer(self.switch.active_remote()).await {
            warn!(
                "session to {} is gone; returning input to this computer",
                self.switch.active_remote()
            );
            self.switch.unlock_to_local();
            self.held = HeldInput::default();
            release_pointer(&mut self.grab, Some(self.home_point));
            self.grab_armed = false;
            self.set_active_screen(None).await;
        }
        if Instant::now() >= self.next_nic {
            self.next_nic = Instant::now() + Duration::from_secs(3);
            self.poll_system().await;
        }
        {
            let ns = self.net.status.read().await.clone();
            let mut s = self.status.write().await;
            s.connected = ns.connected;
            s.peer_name = ns.peer_name.clone();
            self.switch.set_enabled(s.sharing_enabled);
            // Being controlled earlier (as a client) leaves the switch in Remote.
            // A keyboard host is only Remote while it holds the grab.
            if !self.grab && s.role == "server" && self.switch.state() == SwitchState::Remote {
                info!("clearing stale remote state; pointer belongs to this computer");
                self.switch.unlock_to_local();
                s.active_screen = s.local_name.clone();
            }
        }
        if !self.grab && csc_platform::paste_hits() > 0 {
            self.paste(String::new()).await;
        }
        self.zone_upkeep().await;

        if self.screen.width <= 1 || self.screen.height <= 1 {
            return;
        }
        if self.grab && self.switch.state() == SwitchState::Remote {
            self.grab_step().await;
        } else if self.grab {
            release_pointer(&mut self.grab, None);
            self.grab_armed = false;
        } else if self.switch.enabled() && self.switch.state() == SwitchState::Local {
            self.local_step().await;
        } else {
            // Keep raw motion from piling up while this computer is controlled.
            let _ = csc_platform::take_raw_motion();
        }
    }

    /// Screens, adapters and the link supervisor, every few seconds.
    async fn poll_system(&mut self) {
        if !self.grab {
            let now_screen = self.screen_info.virtual_size();
            let now_monitors = self.screen_info.monitors();
            if now_monitors != self.monitors
                || now_screen.width != self.screen.width
                || now_screen.height != self.screen.height
            {
                info!(
                    "screen changed to {}x{}, monitors {:?}",
                    now_screen.width, now_screen.height, now_monitors
                );
                self.screen = now_screen;
                self.monitors = now_monitors;
                self.switch.set_screen(self.screen, self.monitors.clone());
            }
        }
        let Ok(host) = tokio::task::spawn_blocking(query_host_net_info).await else {
            return;
        };
        let (host_name, ips, nics) = nics_of(host);
        let search = self.config.read().await.search_address.clone();
        let mut reload = self.supervisor.on_adapters(&search, &ips);
        let (connected, host) = {
            let mut s = self.status.write().await;
            s.host_name = host_name;
            s.local_ipv4 = ips;
            s.local_nics = nics;
            (s.connected, s.role == "server")
        };
        if !host {
            reload = reload.or(self.supervisor.on_link(connected, Instant::now()));
        }
        if let Some(why) = reload {
            info!("restarting the network: {why}");
            self.net_reload.send_modify(|n| *n = n.wrapping_add(1));
        }
    }

    /// Drop the edge window once the drag moves away or ends.
    async fn zone_upkeep(&mut self) {
        let dragging = csc_platform::primary_button_down();
        if !dragging {
            self.zone_blocked = false;
        }
        let Some(zone) = self.drop_zone.as_ref() else {
            return;
        };
        if self.drag_pending {
            return;
        }
        let near = csc_platform::cursor_pos().is_some_and(|(x, y)| {
            x >= zone.x - 48 && x < zone.x + zone.width + 48 && y >= zone.y - 48 && y < zone.y + zone.height + 48
        });
        if !dragging || !near {
            self.clear_zone().await;
        }
    }

    /// Local input while the pointer is on a remote screen.
    async fn grab_step(&mut self) {
        // With hooks, local input arrives already swallowed. Without them,
        // poll the cursor and pull it back to the park point each tick.
        let steps: Vec<Captured> = if self.hooked {
            csc_platform::capture_drain()
        } else {
            let mut polled = Vec::new();
            if let Some((ax, ay)) = csc_platform::cursor_pos() {
                let (ox, oy) = csc_platform::virtual_origin();
                let x = ax - ox;
                let y = ay - oy;
                if !self.grab_armed {
                    // Use the cursor's real position. Forcing it back to a
                    // mismatched point makes the physical mouse feel stuck.
                    self.park_x = x;
                    self.park_y = y;
                    self.grab_armed = true;
                } else if x != self.park_x || y != self.park_y {
                    csc_platform::warp_cursor(ox + self.park_x, oy + self.park_y);
                    polled.push(Captured::Move {
                        dx: x - self.park_x,
                        dy: y - self.park_y,
                    });
                }
            }
            polled
        };
        for step in steps {
            if self.grab_input(step).await {
                break;
            }
        }
    }

    /// One captured event while grabbed. True once input is back on this computer.
    async fn grab_input(&mut self, step: Captured) -> bool {
        if let Captured::Key { hid, down: true } = step {
            if self
                .return_hotkey
                .as_ref()
                .is_some_and(|k| k.completes(hid, &self.held.keys))
            {
                let current = self.switch.active_remote().to_string();
                self.held.release_all(&self.net).await;
                let leave_msg = SwitchPayload {
                    edge: csc_proto::Edge::Left,
                    position_pct: 0.5,
                };
                self.net.send_to(&current, encode_switch_leave_bin(&leave_msg)).await;
                self.switch.unlock_to_local();
                release_pointer(&mut self.grab, Some(self.home_point));
                self.grab_armed = false;
                self.set_active_screen(None).await;
                info!("return hotkey: input back from {current}");
                return true;
            }
            if hid == HID_V && self.held.ctrl() {
                let target = self.switch.active_remote().to_string();
                if self.paste(target).await {
                    // Never forwarded, so its release is dropped below as well.
                    return false;
                }
            }
        }
        if let Captured::Key { hid, down: false } = step {
            if !self.held.keys.contains(&hid) {
                // Release of a key the remote never saw (hotkey, caught Ctrl+V).
                return false;
            }
        }
        let Captured::Move { dx, dy } = step else {
            self.held.track(step);
            self.net.send_captured(step).await;
            return false;
        };
        let dx = dx.clamp(-300, 300);
        let dy = dy.clamp(-300, 300);
        let current = self.switch.active_remote().to_string();
        let remote = peer_size(&self.peer_screens, &current, self.screen);
        let Some(leave) = self.switch.on_remote_relative(
            dx,
            dy,
            &mut self.remote_x,
            &mut self.remote_y,
            remote,
        ) else {
            self.net.send_mouse_move(dx, dy).await;
            return false;
        };
        self.leave_remote(leave, None).await
    }

    /// The pointer leaves the active remote through `leave.edge`: hop to the
    /// next computer, or come back here. `dragging` names the computer a file
    /// drag comes from. True when input is back on this computer.
    async fn leave_remote(&mut self, leave: EnterDecision, dragging: Option<String>) -> bool {
        self.held.release_all(&self.net).await;
        let local_name = self.local_name().await;
        let current = self.switch.active_remote().to_string();
        let next = self.switch.neighbor_from_active(leave.edge);
        let hop = next.as_ref().filter(|name| {
            !name.eq_ignore_ascii_case(&local_name) && !name.eq_ignore_ascii_case(&current)
        });
        let leave_msg = SwitchPayload {
            edge: leave.edge,
            position_pct: leave.position_pct,
        };
        if let Some(next) = hop.cloned() {
            self.net.send_to(&current, encode_switch_leave_bin(&leave_msg)).await;
            let enter = SwitchPayload {
                edge: leave.edge.opposite(),
                position_pct: leave.position_pct,
            };
            self.net.send_to(&next, csc_proto::encode_switch_bin(&enter)).await;
            let remote = peer_size(&self.peer_screens, &next, self.screen);
            self.switch.confirm_entered(
                enter.edge,
                enter.position_pct,
                remote,
                &mut self.remote_x,
                &mut self.remote_y,
                &next,
            );
            let reports = self.net.peer_caps(&next).await & CAP_EDGE_REPORT != 0;
            self.switch.set_remote_reports(reports);
            self.set_active_screen(Some(&next)).await;
            info!(
                "switch hop {current} -> {next} at {:?} {:.2}",
                leave.edge, leave.position_pct
            );
            if let Some(from) = dragging {
                self.drag_route = Some(DragRoute {
                    from,
                    to: Some(next),
                    at: Instant::now(),
                });
            }
            false
        } else {
            if current.is_empty() {
                self.net.send_raw(encode_switch_leave_bin(&leave_msg)).await;
            } else {
                self.net.send_to(&current, encode_switch_leave_bin(&leave_msg)).await;
            }
            self.switch.confirm_returned();
            let m = self.switch.monitor_at(self.enter_point.0, self.enter_point.1);
            let back = reentry_point(leave.edge, leave.position_pct, m);
            release_pointer(&mut self.grab, Some(back));
            self.grab_armed = false;
            self.set_active_screen(None).await;
            info!(
                "switch return from {current} at {:?} {:.2}, cursor to {back:?}",
                leave.edge, leave.position_pct
            );
            if let Some(from) = dragging {
                self.drag_route = Some(DragRoute {
                    from,
                    to: None,
                    at: Instant::now(),
                });
            }
            true
        }
    }

    /// The local pointer, while it is on this computer.
    async fn local_step(&mut self) {
        let raw = csc_platform::take_raw_motion();
        let Some((ax, ay)) = csc_platform::cursor_pos() else {
            return;
        };
        let (ox, oy) = csc_platform::virtual_origin();
        let x = ax - ox;
        let y = ay - oy;
        let dragging = csc_platform::primary_button_down();
        let Some(dec) = self.switch.on_local_cursor(x, y, raw, Instant::now()) else {
            return;
        };
        let known_screen = self.peer_screens.iter().any(|(k, size)| {
            k.eq_ignore_ascii_case(&dec.neighbor) && size.width > 1 && size.height > 1
        });
        let has_peer = self.net.has_peer(&dec.neighbor).await;
        let drops = has_peer && self.config.read().await.file_transfer.enabled;
        if dragging && drops {
            // A held button at the edge may be a file drag: show the edge window
            // instead of switching. Files entering it start the drag across.
            let virtual_drag = self.net.peer_caps(&dec.neighbor).await & CAP_VIRTUAL_DRAG != 0;
            let r = drop_zone_rect(dec.edge, x, y, self.switch.monitor_at(x, y), virtual_drag);
            let zone = csc_proto::DropZone {
                peer: dec.neighbor.clone(),
                x: r.left + ox,
                y: r.top + oy,
                width: r.width(),
                height: r.height(),
                virtual_drag,
            };
            self.show_zone(zone, ZoneOrigin::Local { dec, x, y }).await;
        } else if has_peer && known_screen {
            self.enter_remote(dec, x, y).await;
        } else if self.last_miss.elapsed() > Duration::from_secs(2) {
            self.last_miss = Instant::now();
            if !has_peer {
                warn!(
                    "pointer hit {:?} but {} has no session; place that computer on an edge in the layout",
                    dec.edge, dec.neighbor
                );
            } else {
                warn!(
                    "pointer hit {:?} but {} has not reported its screen size; leaving the local pointer alone",
                    dec.edge, dec.neighbor
                );
            }
        }
    }

    /// Move the pointer from this computer onto `dec.neighbor`.
    async fn enter_remote(&mut self, dec: EnterDecision, x: i32, y: i32) {
        let (ox, oy) = csc_platform::virtual_origin();
        let entry = dec.edge.opposite();
        let payload = SwitchPayload {
            edge: entry,
            position_pct: dec.position_pct,
        };
        self.drag.pointer_left().await;
        self.net
            .send_to(&dec.neighbor, csc_proto::encode_switch_bin(&payload))
            .await;
        let remote = peer_size(&self.peer_screens, &dec.neighbor, self.screen);
        self.switch.confirm_entered(
            entry,
            dec.position_pct,
            remote,
            &mut self.remote_x,
            &mut self.remote_y,
            &dec.neighbor,
        );
        let reports = self.net.peer_caps(&dec.neighbor).await & CAP_EDGE_REPORT != 0;
        self.switch.set_remote_reports(reports);
        self.enter_point = (x, y);
        let m = self.switch.monitor_at(x, y);
        self.home_point = reentry_point(entry, dec.position_pct, m);
        self.held = HeldInput::default();
        if self.hooked {
            // Moves are measured from the park point, and Windows clips the cursor
            // at the desktop edge. Park in the middle of this monitor so no motion is lost.
            let (cx, cy) = m.center();
            csc_platform::capture_set_grab(true, Some((ox + cx, oy + cy)));
        } else {
            let parked = park_inside(dec.edge, x, y, self.screen);
            self.park_x = parked.0;
            self.park_y = parked.1;
            csc_platform::warp_cursor(ox + self.park_x, oy + self.park_y);
        }
        self.grab = true;
        self.grab_armed = false;
        self.set_active_screen(Some(&dec.neighbor)).await;
        info!(
            "switch enter -> {} at {:?} {:.2} from ({x},{y}) on {:?}, pushed {} px{}",
            dec.neighbor,
            dec.edge,
            dec.position_pct,
            m,
            self.switch.edge_push_px(),
            if reports { "; remote reports its edges" } else { "" }
        );
    }

    // ------------------------------------------------------------ clipboard

    async fn on_clip(&mut self, ev: ClipEvent) {
        let peers = self.net.peer_names().await;
        let c = self.config.read().await.clipboard.clone();
        let host = self.is_host().await;
        self.clip_seq += 1;
        let seq = self.clip_seq;
        match ev {
            ClipEvent::Text(text) => {
                if host {
                    self.shared.copied("", seq, false);
                    csc_platform::paste_watch(false);
                }
                if peers.is_empty() || !c.enabled || !c.text {
                    return;
                }
                if text.len() > c.max_inline_bytes() {
                    popup(
                        &format!(
                            "复制的文字有 {:.1} MB，超过同步上限 {} MB，没有同步到其他电脑。",
                            text.len() as f64 / (1024.0 * 1024.0),
                            c.max_inline_mb
                        ),
                        true,
                    );
                    return;
                }
                let offer = ClipOfferPayload {
                    seq,
                    formats: vec!["text".into()],
                    text: Some(text),
                    files: Vec::new(),
                };
                self.send_clip_offer(&offer, &peers, None).await;
            }
            ClipEvent::Files(paths) => {
                self.my_files = Some((seq, paths.clone()));
                if host {
                    self.shared.copied("", seq, true);
                    csc_platform::paste_watch(false);
                    return;
                }
                if peers.is_empty() || !c.files {
                    return;
                }
                // Only names go out; the files are sent when someone pastes them.
                let files = paths
                    .iter()
                    .filter_map(|p| p.file_name().and_then(|n| n.to_str()))
                    .map(|n| FileEntry {
                        relative_path: n.to_string(),
                        size: 0,
                        mtime_unix: 0,
                        is_directory: false,
                    })
                    .collect();
                let offer = ClipOfferPayload {
                    seq,
                    formats: vec!["files".into()],
                    text: None,
                    files,
                };
                self.send_clip_offer(&offer, &peers, None).await;
            }
        }
    }

    async fn send_clip_offer(&self, offer: &ClipOfferPayload, peers: &[String], except: Option<&str>) {
        let Ok(frame) = encode_json_frame(MsgType::ClipOffer, offer) else {
            return;
        };
        for p in peers {
            if except.is_some_and(|e| e.eq_ignore_ascii_case(p)) {
                continue;
            }
            if frame.len() > BULK_TEXT {
                self.net.send_bulk(p, frame.clone()).await;
            } else {
                self.net.send_control(p, frame.clone()).await;
            }
        }
    }

    async fn on_clip_offer(&mut self, peer: &str, offer: ClipOfferPayload) {
        let host = self.is_host().await;
        let c = self.config.read().await.clipboard.clone();
        if offer.formats.iter().any(|f| f == "files") {
            if host && c.files {
                debug!("{peer} copied {} file(s)", offer.files.len());
                self.shared.copied(peer, offer.seq, true);
                csc_platform::paste_watch(true);
            }
            return;
        }
        let Some(text) = offer.text.clone() else {
            return;
        };
        if host {
            self.shared.copied(peer, offer.seq, false);
            csc_platform::paste_watch(false);
            // Pass it on, so every controlled computer can paste it too.
            let peers = self.net.peer_names().await;
            self.send_clip_offer(&offer, &peers, Some(peer)).await;
        }
        if c.enabled && c.text && offer.seq != self.last_remote_clip {
            self.last_remote_clip = offer.seq;
            debug!("clipboard text from {peer} ({} chars)", text.chars().count());
            self.clip.set_text(text);
        }
    }

    /// Ctrl+V on `target` (empty: this computer). True when the key is caught
    /// and must not reach the system there.
    async fn paste(&mut self, target: String) -> bool {
        if !self.is_host().await {
            return false;
        }
        let Some(action) = self.shared.paste(&target, Instant::now()) else {
            return false;
        };
        let local = self.local_name().await;
        let shown = if target.is_empty() { local.clone() } else { target.clone() };
        match action {
            PasteAction::Repeat => debug!("repeated paste on {shown} ignored"),
            PasteAction::SendOwn { to } => {
                let Some((_, paths)) = self.my_files.clone() else {
                    return true;
                };
                info!("paste on {to}: sending {} copied item(s)", paths.len());
                self.transfer
                    .send_files(to, paths, csc_proto::TransferPurpose::Paste)
                    .await;
            }
            PasteAction::Request { from, seq, to } => {
                info!("paste on {shown}: asking {from} for its copied files");
                if !to.is_empty() {
                    self.transfer.allow_relay(from.clone(), to.clone()).await;
                }
                let req = PasteRequestPayload { seq, to };
                if let Ok(frame) = encode_json_frame(MsgType::PasteRequest, &req) {
                    self.net.send_control(&from, frame).await;
                }
            }
        }
        true
    }

    // ------------------------------------------------------------ drags

    /// The edge window saw files dragged into it.
    async fn on_drag_enter(&mut self, peer: String, paths: Vec<PathBuf>) {
        let wanted = self
            .drop_zone
            .as_ref()
            .is_some_and(|z| z.virtual_drag && z.peer.eq_ignore_ascii_case(&peer));
        if !wanted || self.drag_pending || paths.is_empty() {
            debug!("drag into edge window ignored");
            return;
        }
        self.drag_pending = true;
        let transfer = self.transfer.clone();
        let tx = self.drag_ready_tx.clone();
        tokio::spawn(async move {
            let result = transfer.prepare_drag(peer.clone(), paths, String::new()).await;
            let _ = tx.send(DragReady { peer, result }).await;
        });
    }

    async fn on_drag_ready(&mut self, ready: DragReady) {
        self.drag_pending = false;
        let begin = match ready.result {
            Ok(b) => b,
            Err(msg) => {
                popup(&msg, true);
                self.clear_zone().await;
                self.zone_blocked = true;
                return;
            }
        };
        let origin = self.zone_origin.clone();
        self.clear_zone().await;
        let Some(origin) = origin.filter(|_| csc_platform::primary_button_down()) else {
            debug!("drag ended before the files were checked");
            return;
        };
        // Cancel the drag on this computer first, so the program it came from
        // does not also get a drop (or move the files away).
        self.injector.key(HID_ESC, true);
        self.injector.key(HID_ESC, false);
        tokio::time::sleep(Duration::from_millis(60)).await;
        let Ok(begin_frame) = encode_json_frame(MsgType::DragBegin, &begin) else {
            return;
        };
        match origin {
            ZoneOrigin::Local { dec, x, y } => {
                self.enter_remote(dec, x, y).await;
                self.net.send_control(&ready.peer, begin_frame).await;
                info!("dragging {} item(s) onto {}", begin.entries.len(), ready.peer);
            }
            ZoneOrigin::Controlled(report) => {
                let hit = EdgeHitPayload {
                    edge: report.edge,
                    position_pct: report.position_pct,
                    dragging: true,
                    corner: report.corner,
                };
                if let Ok(frame) = encode_json_frame(MsgType::EdgeHit, &hit) {
                    self.net.send_control(&ready.peer, frame).await;
                }
                self.net.send_control(&ready.peer, begin_frame).await;
                self.last_edge_sent = Instant::now();
                info!("dragging {} item(s) off this screen", begin.entries.len());
            }
        }
    }

    /// A drag arrives on this screen, from `peer`.
    async fn start_drag_here(&mut self, peer: &str, begin: DragBeginPayload) {
        let ok = csc_platform::vdrag::supported() && self.config.read().await.file_transfer.enabled;
        if !ok {
            let c = DragCancelPayload {
                transfer_id: begin.transfer_id,
                reason: "unsupported".into(),
            };
            if let Ok(frame) = encode_json_frame(MsgType::DragCancel, &c) {
                self.net.send_control(peer, frame).await;
            }
            return;
        }
        // The system drag needs the button down. The hand is still holding it;
        // its release arrives as usual and drops the files. On X11 the drag
        // source presses it itself once it holds the pointer grab.
        if cfg!(windows) {
            self.injector.mouse_button(0, true);
        }
        if !self.is_host().await {
            self.injected.track(Captured::Button { button: 0, down: true });
        }
        self.drag.begin(peer.to_string(), begin).await;
    }

    async fn on_drag_begin(&mut self, peer: String, begin: DragBeginPayload) {
        if !self.is_host().await {
            self.start_drag_here(&peer, begin).await;
            return;
        }
        let route = self
            .drag_route
            .take()
            .filter(|r| r.from.eq_ignore_ascii_case(&peer) && r.at.elapsed() < Duration::from_secs(5));
        match route {
            Some(DragRoute { to: None, .. }) => self.start_drag_here(&peer, begin).await,
            Some(DragRoute { to: Some(to), .. }) => {
                info!("passing drag from {peer} on to {to}");
                self.transfer.relay_drag(peer, begin, to).await;
            }
            None => {
                info!("drag from {peer} did not lead anywhere");
                let c = DragCancelPayload {
                    transfer_id: begin.transfer_id,
                    reason: "no_target".into(),
                };
                if let Ok(frame) = encode_json_frame(MsgType::DragCancel, &c) {
                    self.net.send_control(&peer, frame).await;
                }
            }
        }
    }

    // ------------------------------------------------------------ controlled side

    /// Input from the keyboard host moved this computer's pointer.
    async fn controlled_move(&mut self, dx: i32, dy: i32) {
        let w = self.screen.width.max(1);
        let h = self.screen.height.max(1);
        self.remote_x = self.remote_x.saturating_add(dx).clamp(0, w - 1);
        self.remote_y = self.remote_y.saturating_add(dy).clamp(0, h - 1);
        self.injector.mouse_move_abs(self.remote_x, self.remote_y);
        let Some((ax, ay)) = csc_platform::cursor_pos() else {
            return;
        };
        let (ox, oy) = csc_platform::virtual_origin();
        let real = (ax - ox, ay - oy);
        let moved = self
            .last_real
            .map(|l| (real.0 - l.0, real.1 - l.1))
            .unwrap_or((0, 0));
        self.last_real = Some(real);
        // Follow the real pointer, which the system keeps on the monitors.
        self.remote_x = real.0;
        self.remote_y = real.1;
        let Some(report) = self.switch.probe_edge(&mut self.probe, real.0, real.1, (dx, dy), moved) else {
            return;
        };
        if self.last_edge_sent.elapsed() < Duration::from_millis(250) {
            return;
        }
        let Some(host) = self.net.peer_names().await.into_iter().next() else {
            return;
        };
        let dragging = self.injected.buttons.contains(&0);
        let host_reads_edges = self.net.peer_caps(&host).await & CAP_EDGE_REPORT != 0;
        if dragging && host_reads_edges && self.config.read().await.file_transfer.enabled {
            let r = drop_zone_rect(report.edge, real.0, real.1, self.switch.monitor_at(real.0, real.1), true);
            let zone = csc_proto::DropZone {
                peer: host,
                x: r.left + ox,
                y: r.top + oy,
                width: r.width(),
                height: r.height(),
                virtual_drag: true,
            };
            self.show_zone(zone, ZoneOrigin::Controlled(report)).await;
            return;
        }
        info!(
            "pointer at {:?} pushed {:?} {:.2} (corner {:?})",
            real, report.edge, report.position_pct, report.corner
        );
        let hit = EdgeHitPayload {
            edge: report.edge,
            position_pct: report.position_pct,
            dragging: false,
            corner: report.corner,
        };
        if let Ok(frame) = encode_json_frame(MsgType::EdgeHit, &hit) {
            self.net.send_control(&host, frame).await;
        }
        self.last_edge_sent = Instant::now();
    }

    /// Keyboard host: the active remote's real pointer pushed through an edge.
    async fn on_edge_hit(&mut self, peer: &str, hit: EdgeHitPayload) {
        if !self.grab || !self.switch.active_remote().eq_ignore_ascii_case(peer) {
            return;
        }
        let report = EdgeReport {
            edge: hit.edge,
            corner: hit.corner,
            position_pct: hit.position_pct,
        };
        let Some(leave) = self.switch.on_remote_edge(&report) else {
            debug!("{peer} pushed {:?}, which leads nowhere", hit.edge);
            return;
        };
        info!(
            "{peer} reports its pointer at {:?} {:.2}; host estimate ({},{})",
            leave.edge, leave.position_pct, self.remote_x, self.remote_y
        );
        let dragging = hit.dragging.then(|| peer.to_string());
        self.leave_remote(leave, dragging).await;
    }

    async fn on_paste_request(&mut self, peer: &str, req: PasteRequestPayload) {
        match self.my_files.clone() {
            Some((seq, paths)) if seq == req.seq => {
                info!("sending copied files for a paste on {}", if req.to.is_empty() { peer } else { &req.to });
                self.transfer
                    .send_via(peer.to_string(), paths, csc_proto::TransferPurpose::Paste, req.to)
                    .await;
            }
            _ => info!("paste request {} from {peer} is for an older copy", req.seq),
        }
    }

    async fn on_from(&mut self, peer: String, msg: MsgType, payload: Vec<u8>) {
        match msg {
            MsgType::ClipOffer => match serde_json::from_slice::<ClipOfferPayload>(&payload) {
                Ok(offer) => self.on_clip_offer(&peer, offer).await,
                Err(e) => warn!("bad clip offer: {e}"),
            },
            MsgType::EdgeHit => {
                if let Ok(hit) = serde_json::from_slice(&payload) {
                    self.on_edge_hit(&peer, hit).await;
                }
            }
            MsgType::PasteRequest => {
                if let Ok(req) = serde_json::from_slice(&payload) {
                    self.on_paste_request(&peer, req).await;
                }
            }
            MsgType::DragBegin => {
                if let Ok(begin) = serde_json::from_slice(&payload) {
                    self.on_drag_begin(peer, begin).await;
                }
            }
            _ => {}
        }
    }

    /// False when the network side has shut down.
    async fn on_inbound(&mut self, ev: Option<InboundEvent>) -> bool {
        match ev {
            None => return false,
            Some(InboundEvent::Disconnected { peer }) => {
                info!("session {peer} disconnected");
                let mut s = self.status.write().await;
                let active = self.grab
                    && (self.switch.active_remote().eq_ignore_ascii_case(&peer)
                        || s.active_screen.eq_ignore_ascii_case(&peer));
                if active {
                    self.switch.unlock_to_local();
                    self.held = HeldInput::default();
                    release_pointer(&mut self.grab, Some(self.home_point));
                    self.grab_armed = false;
                    s.active_screen = s.local_name.clone();
                }
                if !self.grab {
                    // The host that was driving this computer is gone.
                    self.injected.release_injected(self.injector.as_mut());
                    if self.switch.state() == SwitchState::Remote {
                        self.switch.unlock_to_local();
                        s.active_screen = s.local_name.clone();
                    }
                }
                let left: Vec<&str> = s
                    .peer_name
                    .split(',')
                    .map(str::trim)
                    .filter(|n| !n.is_empty() && !n.eq_ignore_ascii_case(&peer))
                    .collect();
                s.peer_name = left.join(", ");
                s.connected = !s.peer_name.is_empty();
                drop(s);
                self.shared.peer_gone(&peer);
                csc_platform::paste_watch(self.shared.remote_files());
                self.transfer.peer_gone(peer).await;
            }
            Some(InboundEvent::Hello(h)) => {
                info!(
                    "HELLO from {} ({}x{}, capabilities {:#x})",
                    h.host_name, h.screen_width, h.screen_height, h.capabilities
                );
                if h.screen_width > 1 && h.screen_height > 1 {
                    self.peer_screens.insert(
                        h.host_name,
                        csc_platform::ScreenSize {
                            width: h.screen_width,
                            height: h.screen_height,
                        },
                    );
                }
            }
            Some(InboundEvent::Heartbeat) => {}
            Some(InboundEvent::SwitchEnter(p)) => {
                let screen = self.screen;
                self.switch.confirm_entered(
                    p.edge,
                    p.position_pct,
                    screen,
                    &mut self.remote_x,
                    &mut self.remote_y,
                    "",
                );
                self.probe.reset();
                self.last_real = None;
                // Land on the real monitor at that position, not in a gap of the bounding box.
                let m = self.switch.monitor_at(self.remote_x, self.remote_y);
                self.remote_x = self.remote_x.clamp(m.left, m.right - 1);
                self.remote_y = self.remote_y.clamp(m.top, m.bottom - 1);
                self.injector.mouse_move_abs(self.remote_x, self.remote_y);
            }
            Some(InboundEvent::SwitchLeave(p)) => {
                debug!(edge = ?p.edge, "switch leave");
                self.drag.pointer_left().await;
                let current = self.switch.active_remote().to_string();
                let local = self.local_name().await;
                let next = self.switch.neighbor_from_active(p.edge);
                if let Some(next) = next {
                    if !next.eq_ignore_ascii_case(&local) && !next.eq_ignore_ascii_case(&current) {
                        let enter = SwitchPayload {
                            edge: p.edge.opposite(),
                            position_pct: p.position_pct,
                        };
                        self.net.send_to(&next, csc_proto::encode_switch_bin(&enter)).await;
                        let screen = self.screen;
                        self.switch.confirm_entered(
                            enter.edge,
                            enter.position_pct,
                            screen,
                            &mut self.remote_x,
                            &mut self.remote_y,
                            &next,
                        );
                        self.set_active_screen(Some(&next)).await;
                        return true;
                    }
                }
                self.switch.confirm_returned();
                if self.grab {
                    self.held.release_all(&self.net).await;
                    let m = self.switch.monitor_at(self.enter_point.0, self.enter_point.1);
                    release_pointer(&mut self.grab, Some(reentry_point(p.edge, p.position_pct, m)));
                }
                self.grab_armed = false;
                self.set_active_screen(None).await;
            }
            Some(InboundEvent::MouseMove { dx, dy }) => {
                if !self.grab {
                    self.controlled_move(dx, dy).await;
                }
            }
            Some(InboundEvent::MouseButton { button, down }) => {
                if !self.grab {
                    self.injected.track(Captured::Button { button, down });
                    self.injector.mouse_button(button, down);
                }
            }
            Some(InboundEvent::MouseWheel { v, h }) => {
                if !self.grab {
                    self.injector.mouse_wheel(v, h);
                }
            }
            Some(InboundEvent::Key { hid, down }) => {
                if !self.grab {
                    self.injected.track(Captured::Key { hid, down });
                    self.injector.key(hid, down);
                }
            }
            Some(InboundEvent::From { peer, msg, payload }) => self.on_from(peer, msg, payload).await,
            Some(InboundEvent::Raw(ty, payload)) => {
                debug!(?ty, len = payload.len(), "unhandled frame");
            }
        }
        true
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let config_path = std::env::args()
        .position(|a| a == "--config")
        .and_then(|i| std::env::args().nth(i + 1))
        .map(PathBuf::from)
        .unwrap_or_else(default_config_path);

    let cfg = Config::load_or_create(&config_path).unwrap_or_else(|e| {
        eprintln!("config load failed, using defaults: {e:#}");
        Config::default()
    });
    let log_path = match init_logging(&cfg) {
        Ok(path) => path,
        Err(e) => {
            eprintln!("logging disabled: {e:#}");
            let _ = tracing_subscriber::fmt()
                .with_env_filter(EnvFilter::try_new("info").unwrap_or_default())
                .with_ansi(false)
                .try_init();
            log_path_of(&cfg)
        }
    };
    let _lock = match single_instance_lock() {
        Ok(file) => Some(file),
        Err(e) => {
            eprintln!("single instance lock skipped: {e:#}");
            None
        }
    };

    info!("Cross-SystemConnect-core starting name={}", cfg.name);

    csc_platform::init_process();
    let screen_info = create_screen_info();
    let screen = screen_info.virtual_size();
    let monitors = screen_info.monitors();
    info!(
        "screen {}x{}, monitors {:?}",
        screen.width, screen.height, monitors
    );
    let mut switch = SwitchController::default();
    switch.configure(
        cfg.name.clone(),
        screen,
        cfg.switch_cfg.clone(),
        ScreenLayout::load(&cfg.layout),
    );
    switch.set_screen(screen, monitors.clone());
    let hooked = csc_platform::capture_start();

    let status = AppStatus {
        connected: false,
        peer_name: String::new(),
        active_screen: cfg.name.clone(),
        sharing_enabled: true,
        role: match cfg.role {
            csc_config::Role::Server => "server".into(),
            csc_config::Role::Client => "client".into(),
        },
        local_name: cfg.name.clone(),
        launch_at_login: cfg.gui.launch_at_login,
        host_name: cfg.name.clone(),
        local_ipv4: Vec::new(),
        local_nics: Vec::new(),
        discovery_port: cfg.control_port,
        link_state: match cfg.role {
            csc_config::Role::Server => "searching".into(),
            csc_config::Role::Client => "announcing".into(),
        },
        pending_peer: None,
        discovered: Vec::new(),
        search_subnet: crate::discover::subnet_label(&cfg.search_address).unwrap_or_default(),
        drop_zone: None,
        transfers: Vec::new(),
    };

    let return_hotkey = hotkey_of(&cfg);
    let config = Arc::new(RwLock::new(cfg));
    let status = Arc::new(RwLock::new(status));
    let (quit_tx, mut quit_rx) = watch::channel(false);
    let (net_reload, net_reload_rx) = watch::channel(0u64);
    let (layout_reload, layout_rx) = watch::channel(0u64);
    let prompt = Arc::new(PeerPrompt::default());
    let (transfer, transfer_rx) = crate::transfer::channel();
    let board = crate::progress::TransferBoard::default();
    let (drag_enter_tx, mut drag_enter_rx) = mpsc::channel::<(String, Vec<PathBuf>)>(4);

    let state = Arc::new(AppState {
        transfer: transfer.clone(),
        board: board.clone(),
        drag_enter: drag_enter_tx,
        config: config.clone(),
        config_path: config_path.clone(),
        status: status.clone(),
        quit_tx,
        prompt: prompt.clone(),
        net_reload: net_reload.clone(),
        layout_reload,
        log_path,
    });

    let (inbound_tx, mut inbound_rx) = mpsc::channel::<InboundEvent>(256);
    // Bind IPC before discovery. A second core must exit so it does not also take port 24900.
    let ipc_bound = match claim_ipc() {
        Ok(bound) => bound,
        Err(err) if err.downcast_ref::<IpcBusy>().is_some() => {
            info!("core already running, exiting");
            std::process::exit(0);
        }
        Err(err) => return Err(err),
    };
    let ipc_state = state.clone();
    tokio::spawn(async move {
        if let Err(e) = serve_ipc(ipc_bound, ipc_state).await {
            warn!("IPC server stopped: {e:#}");
        }
    });
    // GetAdaptersAddresses must not run on a Tokio worker after the pipe is on IOCP.
    let host = tokio::task::spawn_blocking(query_host_net_info)
        .await
        .unwrap_or_else(|e| {
            warn!("host net info: {e}");
            csc_platform::HostNetInfo::default()
        });
    {
        let mut cfg = config.write().await;
        if cfg.search_address.is_empty() {
            if let Some(nic) = host
                .nics
                .iter()
                .find(|n| n.kind == "ethernet")
                .or(host.nics.first())
            {
                cfg.search_address = nic.address.clone();
                if let Err(e) = cfg.save(&config_path) {
                    warn!("save default search address: {e:#}");
                }
            }
        }
        info!("host={} ips={:?}", host.host_name, host.ipv4_addresses);
    }
    let search = config.read().await.search_address.clone();
    let mut supervisor = Supervisor::default();
    {
        let (host_name, ips, nics) = nics_of(host);
        supervisor.on_adapters(&search, &ips);
        let mut s = status.write().await;
        s.host_name = host_name;
        s.local_ipv4 = ips;
        s.local_nics = nics;
        s.search_subnet = crate::discover::subnet_label(&search).unwrap_or_default();
    }

    let net = spawn_network(
        config.clone(),
        config_path.clone(),
        status.clone(),
        prompt,
        net_reload_rx,
        inbound_tx,
        transfer.clone(),
    )
    .await?;

    let (clip_tx, mut clip_rx) = mpsc::channel::<ClipEvent>(16);
    let clip = crate::clipboard::spawn(clip_tx);
    crate::transfer::spawn(transfer_rx, net.clone(), config.clone(), board);
    let drag = crate::drag::spawn(transfer.clone(), config.clone());
    let (drag_ready_tx, mut drag_ready_rx) = mpsc::channel::<DragReady>(4);
    // Sequence numbers start from the clock so a restarted core is never mistaken for a repeat.
    let clip_seq = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(1);

    let mut core = Core {
        config,
        status,
        net,
        transfer,
        drag,
        clip,
        net_reload,
        layout_rx,
        drag_ready_tx,
        injector: create_input_injector(),
        screen_info,
        screen,
        monitors,
        switch,
        hooked,
        remote_x: 0,
        remote_y: 0,
        peer_screens: HashMap::new(),
        grab: false,
        grab_armed: false,
        park_x: 0,
        park_y: 0,
        enter_point: (0, 0),
        home_point: (screen.width / 2, screen.height / 2),
        held: HeldInput::default(),
        injected: HeldInput::default(),
        return_hotkey,
        last_miss: Instant::now() - Duration::from_secs(5),
        next_nic: Instant::now(),
        supervisor,
        drop_zone: None,
        zone_origin: None,
        zone_blocked: false,
        drag_pending: false,
        drag_route: None,
        clip_seq,
        last_remote_clip: 0,
        shared: SharedClip::default(),
        my_files: None,
        probe: EdgePush::default(),
        last_real: None,
        last_edge_sent: Instant::now() - Duration::from_secs(5),
    };
    let mut tick = tokio::time::interval(Duration::from_millis(8));

    loop {
        tokio::select! {
            _ = quit_rx.changed() => {
                if *quit_rx.borrow_and_update() {
                    info!("quit requested");
                    core.held.release_all(&core.net).await;
                    release_pointer(&mut core.grab, None);
                    break;
                }
            }
            _ = tick.tick() => core.tick().await,
            Some(ev) = clip_rx.recv() => core.on_clip(ev).await,
            Some((peer, paths)) = drag_enter_rx.recv() => core.on_drag_enter(peer, paths).await,
            Some(ready) = drag_ready_rx.recv() => core.on_drag_ready(ready).await,
            ev = inbound_rx.recv() => {
                if !core.on_inbound(ev).await {
                    break;
                }
            }
        }
    }

    info!("Cross-SystemConnect-core exiting");
    // Ensure network/IPC tasks die with the process (do not leave a stray kernel).
    std::process::exit(0);
}

#[cfg(test)]
mod tests {
    use super::Hotkey;
    use std::collections::HashSet;

    #[test]
    fn return_hotkey_matches_either_hand() {
        let hk = Hotkey::parse("Ctrl+Alt+Home").expect("parse");
        let home = 0x4A;
        let held: HashSet<u32> = [0xE4, 0xE2].into_iter().collect();
        assert!(hk.completes(home, &held));
        let only_ctrl: HashSet<u32> = [0xE0].into_iter().collect();
        assert!(!hk.completes(home, &only_ctrl));
        assert!(!hk.completes(0x4D, &held));
        assert!(Hotkey::parse("Ctrl+Nope").is_none());
    }

    #[test]
    fn strip_is_thin_but_old_window_is_not() {
        let m = csc_platform::Rect { left: 0, top: 0, right: 1920, bottom: 1080 };
        let strip = super::drop_zone_rect(csc_proto::Edge::Right, 1919, 500, m, true);
        assert_eq!((strip.left, strip.width(), strip.height()), (1908, 12, 260));
        let wide = super::drop_zone_rect(csc_proto::Edge::Right, 1919, 500, m, false);
        assert_eq!(wide.width(), 160);
    }
}
