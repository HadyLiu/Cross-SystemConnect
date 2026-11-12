mod clipboard;
mod discover;
mod ipc;
mod layout;
mod logging;
mod net;
mod notice;
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
use crate::ipc::{claim_ipc, serve_ipc, AppState, IpcBusy};
use crate::layout::ScreenLayout;
use crate::logging::LogControl;
use crate::net::{spawn_network, InboundEvent, NetHub};
use crate::paste::{PasteAction, SharedClip};
use crate::prompt::PeerPrompt;
use crate::supervisor::Supervisor;
use crate::switch::{EdgePush, EdgeReport, EnterDecision, SwitchController, SwitchState};
use crate::transfer::{PasteSink, TransferHandle};
use csc_config::{default_config_path, Config};
use csc_platform::{create_input_injector, create_screen_info, query_host_net_info, Captured};
use csc_proto::{
    encode_activity, encode_frame, encode_json_frame, encode_switch_leave_bin, AppStatus,
    ClipOfferPayload, ControlPausePayload, EdgeHitPayload, FileEntry, LocalNic, LockState, MsgType,
    PasteLocatedPayload, PasteRequestPayload, PeerStatePayload, SwitchPayload,
    CAP_CLIP_PLACEHOLDER, CAP_EDGE_REPORT,
};
use std::collections::{HashMap, HashSet};
use std::fs::OpenOptions;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{mpsc, watch, RwLock};
use tracing::{debug, info, warn};

const HID_V: u32 = 0x19;
const HID_L: u32 = 0x0F;
const HID_LCTRL: u32 = 0xE0;
const HID_RCTRL: u32 = 0xE4;
const HID_LGUI: u32 = 0xE3;
const HID_RGUI: u32 = 0xE7;
/// Text above this goes on the bulk queue so it never holds up the pointer.
const BULK_TEXT: usize = 64 * 1024;
/// A held lock key repeats, and every computer passes the lock on once.
const LOCK_REPEAT: Duration = Duration::from_secs(3);

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
/// The pointer stays on that edge (one pixel in, so the crossing itself does not
/// immediately switch back).
#[cfg(test)]
fn reentry_point(leave: csc_proto::Edge, pct: f32, m: csc_platform::Rect) -> (i32, i32) {
    let inset = 1;
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

/// Move the controlled pointer by a relative step. On Linux Wayland the
/// platform crate sends this through a virtual device, not XTest.
fn inject_controlled_delta(
    injector: &mut dyn csc_platform::InputInjector,
    x: i32,
    y: i32,
    dx: i32,
    dy: i32,
) {
    if cfg!(target_os = "linux") {
        if dx != 0 || dy != 0 {
            injector.mouse_move_rel(dx, dy);
        }
    } else {
        injector.mouse_move_abs(x, y);
    }
}

/// Place the pointer on the entry edge when control arrives. One relative jump,
/// not a slide through the desktop and not a warp to the screen center.
/// On Wayland the compositor cursor cannot be read, so the first placement pins
/// to the top-left and then to the edge. Each step is a single event.
fn inject_controlled_jump(
    injector: &mut dyn csc_platform::InputInjector,
    commanded: &mut Option<(i32, i32)>,
    x: i32,
    y: i32,
    screen: csc_platform::ScreenSize,
) {
    #[cfg(target_os = "linux")]
    {
        if csc_platform::wayland_session() {
            let (pin_x, pin_y) = wayland_pin_delta(screen);
            injector.mouse_move_rel(pin_x, pin_y);
            injector.mouse_move_rel(pin_x, pin_y);
            if x != 0 || y != 0 {
                injector.mouse_move_rel(x, y);
            }
            *commanded = Some((x, y));
            return;
        }
        if let Some((cx, cy)) = x11_pointer() {
            let dx = x.saturating_sub(cx);
            let dy = y.saturating_sub(cy);
            if dx != 0 || dy != 0 {
                injector.mouse_move_rel(dx, dy);
            }
        } else {
            injector.mouse_move_abs(x, y);
        }
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (commanded, screen);
        injector.mouse_move_abs(x, y);
    }
}

/// Large enough to hit the top-left of any desktop from an unknown cursor.
#[cfg(target_os = "linux")]
fn wayland_pin_delta(screen: csc_platform::ScreenSize) -> (i32, i32) {
    (
        -screen.width.max(1).saturating_mul(2).max(4096),
        -screen.height.max(1).saturating_mul(2).max(4096),
    )
}

#[cfg(target_os = "linux")]
fn x11_pointer() -> Option<(i32, i32)> {
    let (ax, ay) = csc_platform::cursor_pos()?;
    let (ox, oy) = csc_platform::virtual_origin();
    Some((ax - ox, ay - oy))
}

/// Follow the compositor pointer only when it actually moved.
fn follow_real_pointer(logical: (i32, i32), real: (i32, i32), real_moved: bool) -> (i32, i32) {
    if real_moved {
        real
    } else {
        logical
    }
}

/// A local session state that must hand the pointer back to this computer.
fn local_lock_releases(state: &str) -> bool {
    matches!(state, "locked" | "secure_desktop")
}

/// Result of trying to leave this computer through a local edge / barrier.
enum LocalEdge {
    Idle,
    Entered,
    /// Barrier fired but the neighbor is offline or has no screen size yet.
    Missed {
        edge: csc_proto::Edge,
        pct: f32,
    },
}

/// A button, wheel, or key the hook swallowed in the same batch as a screen
/// change. Play it on this computer once the pointer is back here.
fn replay_swallowed(injector: &mut dyn csc_platform::InputInjector, step: Captured) {
    match step {
        Captured::Button { button, down } => injector.mouse_button(button, down),
        Captured::Wheel { v, h } => injector.mouse_wheel(v, h),
        Captured::Key { hid, down } => injector.key(hid, down),
        Captured::Move { .. } => {}
    }
}

/// Give input back to this computer. `target` is where the cursor reappears,
/// in desktop coordinates relative to the virtual origin.
fn release_pointer(grab: &mut bool, target: Option<(i32, i32)>) {
    if !*grab {
        return;
    }
    *grab = false;
    csc_platform::set_host_cursor_hidden(false);
    let (ox, oy) = csc_platform::virtual_origin();
    csc_platform::capture_return_to(target.map(|(x, y)| (ox + x, oy + y)));
    csc_platform::capture_set_grab(false, None);
    if let Some((x, y)) = target {
        let (ox, oy) = csc_platform::virtual_origin();
        csc_platform::warp_cursor(ox + x, oy + y);
    }
}

/// On the shared edge pixel, drop keys, buttons, and wheels. A release of a
/// key or button the remote already has is kept, so it does not stay down.
/// Ctrl+C and Ctrl+V work only after the pointer moves off this pixel.
fn shared_pixel_drops(held: &HeldInput, step: &Captured) -> bool {
    match step {
        Captured::Move { .. } => false,
        Captured::Key { hid, down: false } => !held.keys.contains(hid),
        Captured::Button {
            button,
            down: false,
        } => !held.buttons.contains(button),
        _ => true,
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
            Captured::Button {
                button,
                down: false,
            } => {
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
            net.send_captured(Captured::Button {
                button,
                down: false,
            })
            .await;
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

/// A key combination such as `Ctrl+Shift+/`, as HID usages.
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

fn single_instance_lock() -> anyhow::Result<std::fs::File> {
    let lock_path = csc_config::core_lock_path();
    if let Some(dir) = lock_path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut file = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(&lock_path)?;
    use std::io::Write;
    write!(file, "{}", std::process::id())?;
    file.flush()?;
    Ok(file)
}

pub(crate) fn nics_of(host: csc_platform::HostNetInfo) -> (String, Vec<String>, Vec<LocalNic>) {
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

/// Ethernet first, then whatever else `HostNetInfo` ordered. Used whenever a
/// search address must be chosen automatically.
pub(crate) fn preferred_search(nics: &[LocalNic]) -> Option<String> {
    nics.iter()
        .find(|n| n.kind == "ethernet")
        .or(nics.first())
        .map(|n| n.address.clone())
}

/// True when `search` was empty or no longer on a local adapter and was replaced.
pub(crate) fn ensure_search_address(cfg: &mut Config, nics: &[LocalNic]) -> bool {
    if nics.is_empty() {
        return false;
    }
    let known = nics.iter().any(|n| n.address == cfg.search_address);
    if !cfg.search_address.is_empty() && known {
        return false;
    }
    let Some(next) = preferred_search(nics) else {
        return false;
    };
    if cfg.search_address == next {
        return false;
    }
    info!(
        "using {} for the keyboard link (was {})",
        next,
        if cfg.search_address.is_empty() {
            "unset"
        } else {
            cfg.search_address.as_str()
        }
    );
    cfg.search_address = next;
    true
}

fn popup(body: &str, error: bool) {
    notice::tell(notice::Kind::Error, "Cross-SystemConnect", body, error);
}

/// Files copied on another computer up to this size are fetched as soon as
/// they are copied and put on the clipboard as real files. Placeholders are
/// only for bigger batches: any program that reads a pasted file at once
/// (chat apps, browsers, editors) would get the 48-byte marker instead.
const PREFETCH_MAX_BYTES: u64 = 512 * 1024 * 1024;

/// Private folder for files fetched ahead for the clipboard.
fn clip_stage_root() -> PathBuf {
    csc_config::user_dirs::cache_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join("cross-systemconnect")
        .join("clip")
}

/// Fetch ahead instead of placeholders when the whole batch is small enough.
fn prefetch_copied(files: &[csc_proto::FileEntry]) -> bool {
    !files.is_empty()
        && files
            .iter()
            .try_fold(0u64, |sum, e| sum.checked_add(e.size))
            .is_some_and(|total| total <= PREFETCH_MAX_BYTES)
}

/// Another computer's copied files arrived complete (`paths`), or failed (`None`).
struct StagedPaste {
    peer: String,
    seq: u64,
    paths: Option<Vec<PathBuf>>,
    /// Filled placeholder copies rather than files fetched ahead.
    in_place: bool,
}

/// A file manager pasted the placeholder copies of another computer's files.
struct LocatedPaste {
    peer: String,
    seq: u64,
    gen: u64,
    paths: Vec<PathBuf>,
}

struct Core {
    config: Arc<RwLock<Config>>,
    status: Arc<RwLock<AppStatus>>,
    net: NetHub,
    transfer: TransferHandle,
    clip: ClipHandle,
    net_reload: watch::Sender<u64>,
    layout_rx: watch::Receiver<u64>,
    injector: Box<dyn csc_platform::InputInjector>,
    screen_info: Box<dyn csc_platform::ScreenInfo>,
    screen: csc_platform::ScreenSize,
    monitors: Vec<csc_platform::Rect>,
    work_areas: Vec<csc_platform::Rect>,
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
    /// Local edge that `home_point` sits inside of.
    home_edge: Option<csc_proto::Edge>,
    held: HeldInput,
    /// Keys and buttons this computer injected for a remote host, released if that host goes away.
    injected: HeldInput,
    return_hotkey: Option<Hotkey>,
    lock_hotkey: Option<Hotkey>,
    stay_hotkey: Option<Hotkey>,
    pause_hotkey: Option<Hotkey>,
    stay_on: bool,
    /// A hotkey handled while grabbed; the desktop may report the same press.
    last_grab_hotkey: Instant,
    last_lock: Instant,
    last_miss: Instant,
    /// Wayland keyboard host still waiting for the input capture permission.
    wayland_host_noted: bool,
    sent_moves: csc_platform::RateStats,
    next_nic: Instant,
    supervisor: Supervisor,
    paste_gen: u64,
    paste_tx: mpsc::Sender<LocatedPaste>,
    staged_tx: mpsc::Sender<StagedPaste>,
    /// Remote copy whose files are on their way here, by peer and copy number.
    staging: Option<(String, u64)>,
    clip_seq: u64,
    last_remote_clip: u64,
    shared: SharedClip,
    /// Files copied on this computer, by copy sequence number.
    my_files: Option<(u64, Vec<PathBuf>)>,
    /// Files copied on `peer`, still available to paste here.
    remote_files: Option<(String, u64)>,
    /// Controlled computer: push through its own edges, and the last pointer position.
    probe: EdgePush,
    last_real: Option<(i32, i32)>,
    /// Wayland: last position sent to the virtual pointer. The X cursor is not that position.
    commanded: Option<(i32, i32)>,
    /// Last Wayland barrier sides applied (skip portal work when unchanged).
    /// `None` until the first apply, so all-false still starts the portal.
    last_capture_edges: Option<(bool, bool, bool, bool)>,
    last_edge_sent: Instant,
    power_rx: std::sync::mpsc::Receiver<csc_platform::PowerEvent>,
    /// Computers that paused sharing, or are on a secure desktop.
    paused_peers: HashSet<String>,
    /// Peers whose HELLO arrived in the current session, lower-cased.
    greeted: HashSet<String>,
    sharing_announced: bool,
    remap: Vec<(u32, u32)>,
    sync_locks: bool,
    block_fullscreen: bool,
    pointer_speed: f32,
    pointer_peers: HashMap<String, f32>,
    carry_x: f32,
    carry_y: f32,
    /// Lock lights the host remembers while its own keys are grabbed.
    virtual_locks: Option<(bool, bool, bool)>,
    /// Last moment this computer had user input (for ACTIVITY / keep-awake).
    last_local_activity: Instant,
    last_activity_sent: Instant,
    /// Latest local clipboard offer when `push_mode=on_switch`.
    pending_clip: Option<ClipOfferPayload>,
}

fn file_entries(paths: &[PathBuf]) -> Vec<FileEntry> {
    paths
        .iter()
        .filter_map(|p| {
            let name = p.file_name().and_then(|n| n.to_str())?;
            Some(FileEntry {
                relative_path: name.to_string(),
                size: std::fs::metadata(p).map(|m| m.len()).unwrap_or(0),
                mtime_unix: 0,
                is_directory: p.is_dir(),
                mode: 0,
            })
        })
        .collect()
}

fn hotkey_of(text: &str, what: &str) -> Option<Hotkey> {
    let parsed = Hotkey::parse(text);
    if parsed.is_none() && !text.trim().is_empty() {
        warn!("{what} hotkey {text:?} not understood");
    }
    parsed
}

/// Return and lock hotkeys from the config. The lock key is also watched
/// while input belongs to this computer.
fn hotkeys_of(cfg: &Config) -> (Option<Hotkey>, Option<Hotkey>) {
    let back = hotkey_of(&cfg.hotkeys.return_to_server, "return");
    let lock = hotkey_of(&cfg.hotkeys.lock_screen, "lock");
    match &lock {
        Some(k) if !csc_platform::lock_watch(&k.mods, k.key) => {
            warn!(
                "lock hotkey {:?} is only noticed while controlling another computer",
                cfg.hotkeys.lock_screen
            );
        }
        Some(_) => {}
        None => {
            csc_platform::lock_watch(&[], 0);
        }
    }
    (back, lock)
}

fn watch_local(cfg: &Config) -> (Option<Hotkey>, Option<Hotkey>) {
    let stay = hotkey_of(&cfg.hotkeys.lock_to_screen, "stay");
    match &stay {
        Some(k) => csc_platform::stay_watch(&k.mods, k.key),
        None => csc_platform::stay_watch(&[], 0),
    }
    let pause = hotkey_of(&cfg.hotkeys.emergency_pause, "pause");
    match &pause {
        Some(k) => csc_platform::pause_watch(&k.mods, k.key),
        None => csc_platform::pause_watch(&[], 0),
    }
    csc_platform::sync_global_shortcuts();
    (stay, pause)
}

fn block_bit(name: &str) -> u32 {
    match name.trim().to_ascii_lowercase().as_str() {
        "ctrl" | "control" => 1,
        "shift" => 2,
        "alt" => 4,
        _ => 0,
    }
}

impl Core {
    async fn is_host(&self) -> bool {
        self.status.read().await.role == "server"
    }

    async fn local_name(&self) -> String {
        self.status.read().await.local_name.clone()
    }

    fn load_input_prefs(&mut self, cfg: &Config) {
        self.remap = cfg.keyboard.remap.iter().map(|r| (r.from, r.to)).collect();
        self.sync_locks = cfg.keyboard.sync_locks;
        self.block_fullscreen = cfg.switch_cfg.block_when_fullscreen;
        self.pointer_speed = cfg
            .pointer
            .speed
            .clamp(csc_config::POINTER_SPEED_MIN, csc_config::POINTER_SPEED_MAX);
        self.pointer_peers = cfg
            .pointer
            .peers
            .iter()
            .map(|(k, v)| {
                (
                    k.clone(),
                    v.clamp(csc_config::POINTER_SPEED_MIN, csc_config::POINTER_SPEED_MAX),
                )
            })
            .collect();
        notice::set_language(&cfg.gui.language);
    }

    fn mapped(&self, hid: u32) -> u32 {
        self.remap
            .iter()
            .find(|(from, _)| *from == hid)
            .map(|(_, to)| *to)
            .unwrap_or(hid)
    }

    fn scale_delta(&mut self, dx: i32, dy: i32) -> (i32, i32) {
        let peer = self.switch.active_remote();
        let factor = self
            .pointer_peers
            .get(peer)
            .copied()
            .unwrap_or(self.pointer_speed);
        self.carry_x += dx as f32 * factor;
        self.carry_y += dy as f32 * factor;
        let ox = self.carry_x.trunc();
        let oy = self.carry_y.trunc();
        self.carry_x -= ox;
        self.carry_y -= oy;
        (ox as i32, oy as i32)
    }

    fn enter_payload(&self, edge: csc_proto::Edge, position_pct: f32) -> SwitchPayload {
        let locks = if self.sync_locks {
            let (caps, num, scroll) = self.virtual_locks.unwrap_or_else(csc_platform::lock_leds);
            Some(LockState { caps, num, scroll })
        } else {
            None
        };
        SwitchPayload {
            edge,
            position_pct,
            locks,
        }
    }

    fn note_virtual_lock(&mut self, hid: u32, down: bool) {
        if !down {
            return;
        }
        let Some(locks) = self.virtual_locks.as_mut() else {
            return;
        };
        match hid {
            0x39 => locks.0 = !locks.0,
            0x53 => locks.1 = !locks.1,
            0x47 => locks.2 = !locks.2,
            _ => {}
        }
    }

    fn match_locks(&mut self, want: (bool, bool, bool)) {
        if !self.sync_locks {
            return;
        }
        let have = csc_platform::lock_leds();
        for (hid, on, now) in [
            (0x39u32, want.0, have.0),
            (0x53, want.1, have.1),
            (0x47, want.2, have.2),
        ] {
            if on != now {
                self.injector.key(hid, true);
                self.injector.key(hid, false);
            }
        }
    }

    async fn announce_sharing(&mut self) {
        let (sharing, role, peers) = {
            let s = self.status.read().await;
            (s.sharing_enabled, s.role.clone(), s.peer_name.clone())
        };
        if sharing == self.sharing_announced {
            return;
        }
        self.sharing_announced = sharing;
        if !sharing && self.grab {
            self.return_home("sharing paused").await;
        }
        if role == "server" {
            return;
        }
        let Ok(frame) = encode_json_frame(
            MsgType::ControlPause,
            &ControlPausePayload {
                paused: !sharing,
                reason: "user".into(),
            },
        ) else {
            return;
        };
        for peer in peers.split(',').map(str::trim).filter(|n| !n.is_empty()) {
            self.net.send_control(peer, frame.clone()).await;
        }
    }

    async fn activity_tick(&mut self) {
        let keep = self.config.read().await.power.keep_peers_awake;
        if !keep {
            return;
        }
        if self.grab || csc_platform::primary_button_down() {
            self.last_local_activity = Instant::now();
        }
        if self.last_local_activity.elapsed() > Duration::from_secs(60) {
            return;
        }
        if self.last_activity_sent.elapsed() < Duration::from_secs(30) {
            return;
        }
        self.last_activity_sent = Instant::now();
        csc_platform::note_user_activity();
        let frame = encode_activity();
        let peers = self.share_peers().await;
        for peer in peers {
            self.net.send_control(&peer, frame.clone()).await;
        }
    }

    async fn set_active_screen(&self, name: Option<&str>) {
        let mut s = self.status.write().await;
        s.active_screen = match name {
            Some(n) => n.to_string(),
            None => s.local_name.clone(),
        };
    }

    // ------------------------------------------------------------ tick

    async fn tick(&mut self) {
        while let Ok(ev) = self.power_rx.try_recv() {
            self.on_power(ev).await;
        }
        if self.layout_rx.has_changed().unwrap_or(false) {
            let _ = self.layout_rx.borrow_and_update();
            let cfg = self.config.read().await.clone();
            self.switch.configure(
                cfg.name.clone(),
                self.screen,
                cfg.switch_cfg.clone(),
                ScreenLayout::load(&cfg.layout),
            );
            self.publish_screen();
            (self.return_hotkey, self.lock_hotkey) = hotkeys_of(&cfg);
            (self.stay_hotkey, self.pause_hotkey) = watch_local(&cfg);
            self.load_input_prefs(&cfg);
        }
        // While grabbed, `grab_input` sees the keys itself; a desktop shortcut
        // reported for the same press must not act a second time.
        let local = !self.grab && self.last_grab_hotkey.elapsed() > Duration::from_millis(500);
        let (lock, stay, pause) = (
            csc_platform::lock_hits() > 0 && local,
            csc_platform::stay_hits() > 0 && local,
            csc_platform::pause_hits() > 0 && local,
        );
        if lock {
            self.lock_all(None).await;
        }
        if stay {
            self.stay_on = !self.stay_on;
            info!(stay = self.stay_on, "stay on this screen");
        }
        if pause {
            let on = {
                let mut s = self.status.write().await;
                s.sharing_enabled = !s.sharing_enabled;
                if !s.sharing_enabled {
                    s.being_controlled = false;
                }
                s.sharing_enabled
            };
            info!(sharing = on, "emergency pause");
        }
        let block = block_bit(&self.config.read().await.switch_cfg.block_modifier);
        let held = csc_platform::mods_held();
        self.switch
            .set_blocked(self.stay_on || (block != 0 && held & block != 0));
        csc_platform::set_keep_awake(self.config.read().await.power.keep_peers_awake);
        self.announce_sharing().await;
        self.activity_tick().await;
        if self.grab && !self.net.has_peer(self.switch.active_remote()).await {
            warn!(
                "session to {} is gone; returning input to this computer",
                self.switch.active_remote()
            );
            self.switch.unlock_to_local();
            self.held = HeldInput::default();
            release_pointer(&mut self.grab, Some(self.home_point));
            self.grab_armed = false;
            self.settle_local(self.home_point, self.home_edge);
            self.set_active_screen(None).await;
            self.refresh_capture_edges().await;
        }
        if Instant::now() >= self.next_nic {
            self.next_nic = Instant::now() + Duration::from_secs(3);
            crate::net::set_bench_enabled(self.config.read().await.diagnostics.bench_enabled);
            self.poll_system().await;
        }
        {
            // Prefer HELLO'd hostnames for the label; any live TCP session
            // counts as connected (3010df5 used NetStatus on insert — same idea).
            // Don't accumulate greeted keys across IP→hostname rekeys.
            let up = self.net.greeted_names().await;
            let all = self.net.peer_names().await;
            self.greeted = up.iter().map(|n| n.to_lowercase()).collect();
            let mut s = self.status.write().await;
            s.connected = !all.is_empty();
            s.peer_name = if !up.is_empty() {
                up.join(", ")
            } else {
                all.join(", ")
            };
            self.switch.set_enabled(s.sharing_enabled);
            // Being controlled earlier (as a client) leaves the switch in Remote.
            // A keyboard host is only Remote while it holds the grab.
            if !self.grab && s.role == "server" && self.switch.state() == SwitchState::Remote {
                info!("clearing stale remote state; pointer belongs to this computer");
                self.switch.unlock_to_local();
                s.active_screen = s.local_name.clone();
            }
        }
        // Keep barriers in sync with live sessions (connect / disconnect / pause).
        self.refresh_capture_edges().await;
        if !self.grab && csc_platform::paste_hits() > 0 {
            if self.is_host().await {
                self.paste(String::new()).await;
            } else {
                // Windows client: paste_watch caught Ctrl+V; tell the host.
                self.paste_remote_here().await;
            }
        }

        if self.screen.width <= 1 || self.screen.height <= 1 {
            return;
        }
        // Wayland InputCapture becomes ready after the portal prompt; barriers
        // can fire in the same tick. Refresh every loop, not only the 3s NIC poll.
        if csc_platform::wayland_session() && !self.grab {
            self.hooked = csc_platform::capture_start();
        }
        let local =
            !self.grab && self.switch.enabled() && self.switch.state() == SwitchState::Local;
        if !local {
            // Raw motion only measures pushes at local edges; what piles up while
            // the pointer is elsewhere would count as a push right after returning.
            let _ = csc_platform::take_raw_motion();
        }
        if self.grab && self.switch.state() == SwitchState::Remote {
            // Enter may have raced ahead of `hooked`. Without set_grab, libei
            // events are dropped and the compositor keeps the pointer trapped.
            self.ensure_capture_grab();
            self.grab_step().await;
        } else if self.grab {
            release_pointer(&mut self.grab, None);
            self.grab_armed = false;
        } else if local {
            self.local_step().await;
        }
    }

    /// Arm portal capture if enter raced ahead of `hooked`. Do not call
    /// `set_grab` every tick: it drops queued motion.
    fn ensure_capture_grab(&mut self) {
        if !csc_platform::wayland_session() || self.hooked {
            return;
        }
        if !csc_platform::capture_start() {
            return;
        }
        self.hooked = true;
        let (ox, oy) = csc_platform::virtual_origin();
        csc_platform::capture_set_grab(true, Some((ox + self.park_x, oy + self.park_y)));
    }

    fn publish_screen(&mut self) {
        self.switch.set_screen(self.screen, self.monitors.clone());
        self.switch.set_work_areas(self.work_areas.clone());
    }

    /// Screens, adapters and the link supervisor, every few seconds.
    async fn poll_system(&mut self) {
        if !self.grab {
            let now_screen = self.screen_info.virtual_size();
            let now_monitors = self.screen_info.monitors();
            let now_work = self.screen_info.work_areas();
            if now_monitors != self.monitors
                || now_work != self.work_areas
                || now_screen.width != self.screen.width
                || now_screen.height != self.screen.height
            {
                info!(
                    "screen changed to {}x{}, monitors {:?}",
                    now_screen.width, now_screen.height, now_monitors
                );
                self.screen = now_screen;
                self.monitors = now_monitors;
                self.work_areas = now_work;
                self.publish_screen();
            }
        }
        let Ok(host) = tokio::task::spawn_blocking(query_host_net_info).await else {
            return;
        };
        let (host_name, ips, nics) = nics_of(host);
        let search = {
            let mut cfg = self.config.write().await;
            if ensure_search_address(&mut cfg, &nics) {
                if let Err(e) = cfg.save(&default_config_path()) {
                    warn!("save search address after adapter change: {e:#}");
                }
                self.net_reload.send_modify(|n| *n = n.wrapping_add(1));
            }
            cfg.search_address.clone()
        };
        let mut reload = self.supervisor.on_adapters(&search, &ips);
        let (connected, host) = {
            let mut s = self.status.write().await;
            s.host_name = host_name;
            s.local_ipv4 = ips;
            s.local_nics = nics;
            s.search_subnet = crate::discover::subnet_label(&search).unwrap_or_default();
            (s.connected, s.role == "server")
        };
        if !host {
            reload = reload.or(self.supervisor.on_link(connected, Instant::now()));
        }
        if host {
            self.refresh_capture_edges().await;
        }
        let waiting = host && csc_platform::wayland_session() && !self.hooked;
        if waiting && !self.wayland_host_noted {
            self.status.write().await.peer_note =
                "请在系统弹窗中允许本应用「捕获输入」，否则指针无法穿过屏幕边缘。".into();
        } else if !waiting && self.wayland_host_noted {
            let mut s = self.status.write().await;
            if s.peer_note.contains("捕获输入") {
                s.peer_note.clear();
            }
        }
        self.wayland_host_noted = waiting;
        if let Some(why) = reload {
            info!("restarting the network: {why}");
            self.net_reload.send_modify(|n| *n = n.wrapping_add(1));
        }
    }

    /// Local input while the pointer is on a remote screen.
    async fn grab_step(&mut self) {
        if self.hooked && csc_platform::capture_released_by_desktop() {
            self.return_home("the desktop ended input capture").await;
            return;
        }
        // With hooks, local input arrives already swallowed. Without them,
        // poll the cursor and pull it back to the park point each tick.
        let steps: Vec<Captured> = if self.hooked {
            let mut steps = csc_platform::capture_drain();
            // Raw motion is not stopped by the edge of this screen. The cursor
            // deltas are, so they only cover about half of this monitor.
            if let Some((dx, dy)) = csc_platform::take_raw_motion() {
                if dx != 0 || dy != 0 {
                    steps.retain(|step| !matches!(step, Captured::Move { .. }));
                    steps.push(Captured::Move { dx, dy });
                }
            }
            steps
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
        let mut returned = false;
        for step in steps {
            if returned {
                // A click queued in the same batch as the crossing must not be
                // thrown away. The hook already swallowed it.
                if matches!(step, Captured::Move { .. }) {
                    continue;
                }
                if self.grab {
                    self.held.track(step);
                    self.net.send_captured(step).await;
                } else {
                    replay_swallowed(self.injector.as_mut(), step);
                }
                continue;
            }
            if self.grab_input(step).await {
                returned = true;
            }
        }
    }

    /// One captured event while grabbed. True once input is back on this computer.
    async fn grab_input(&mut self, step: Captured) -> bool {
        // The edge pixel belongs to both computers. Keys and buttons are
        // dropped there; a release of a key already held inside still goes
        // out, so it does not stick. Mouse movement is the only way off it.
        if self.on_shared_pixel() && shared_pixel_drops(&self.held, &step) {
            return false;
        }
        if let Captured::Key { hid, down: true } = step {
            // Win+L must never reach the remote: GNOME treats Super+L as lock,
            // then further L presses land in the password field as "lll".
            let win_held = self.held.keys.contains(&HID_LGUI) || self.held.keys.contains(&HID_RGUI);
            if hid == HID_L && win_held {
                self.lock_all(None).await;
                return true;
            }
            let hit = |k: &Option<Hotkey>| {
                k.as_ref()
                    .is_some_and(|k| k.completes(hid, &self.held.keys))
            };
            let (lock, back) = (hit(&self.lock_hotkey), hit(&self.return_hotkey));
            if lock || back || hit(&self.stay_hotkey) || hit(&self.pause_hotkey) {
                self.last_grab_hotkey = Instant::now();
            }
            if lock {
                self.lock_all(None).await;
                return true;
            }
            if back {
                self.return_home("return hotkey").await;
                return true;
            }
            if hit(&self.stay_hotkey) {
                self.stay_on = true;
                self.return_home("stay on this screen").await;
                return true;
            }
            if hit(&self.pause_hotkey) {
                self.status.write().await.sharing_enabled = false;
                self.return_home("emergency pause").await;
                return true;
            }
            if hid == HID_V && self.held.ctrl() {
                let target = self.switch.active_remote().to_string();
                // Linux pastes by copying placeholders itself. Swallowing Ctrl+V
                // would steal the key from the file manager and from other windows.
                let own_paste = self.net.peer_caps(&target).await & CAP_CLIP_PLACEHOLDER != 0;
                if !own_paste && self.paste(target).await {
                    // Never forwarded, so its release is dropped below as well.
                    return false;
                }
            }
        }
        let step = match step {
            Captured::Key { hid, down } => {
                let hid = self.mapped(hid);
                self.note_virtual_lock(hid, down);
                Captured::Key { hid, down }
            }
            other => other,
        };
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
        let (dx, dy) = self.scale_delta(dx, dy);
        let dx = dx.clamp(-300, 300);
        let dy = dy.clamp(-300, 300);
        let current = self.switch.active_remote().to_string();
        let remote = peer_size(&self.peer_screens, &current, self.screen);
        let Some(leave) =
            self.switch
                .on_remote_relative(dx, dy, &mut self.remote_x, &mut self.remote_y, remote)
        else {
            self.sent_moves.note();
            self.net.send_mouse_move(dx, dy).await;
            return false;
        };
        self.leave_remote(leave).await
    }

    /// True while the remote pointer sits on a pixel shared with a neighbor.
    fn on_shared_pixel(&self) -> bool {
        if !self.grab {
            return false;
        }
        let remote = peer_size(&self.peer_screens, self.switch.active_remote(), self.screen);
        self.switch
            .cursor_on_shared_pixel(self.remote_x, self.remote_y, remote)
    }

    /// Bring input back to this computer from the active remote, crossing no edge.
    async fn return_home(&mut self, why: &str) {
        let current = self.switch.active_remote().to_string();
        // Hand the pointer back before any network I/O. A stalled session
        // must not keep this computer's cursor frozen.
        self.switch.unlock_to_local();
        release_pointer(&mut self.grab, Some(self.home_point));
        self.grab_armed = false;
        self.held.release_all(&self.net).await;
        if let Some(want) = self.virtual_locks.take() {
            self.match_locks(want);
        }
        let leave_msg = SwitchPayload {
            edge: csc_proto::Edge::Left,
            position_pct: 0.5,
            locks: None,
        };
        self.net
            .send_to(&current, encode_switch_leave_bin(&leave_msg))
            .await;
        // Barrier still covers the seam; ignore hits until the pointer settles inside.
        self.settle_local(self.home_point, self.home_edge);
        self.set_active_screen(None).await;
        info!("{why}: input back from {current}");
    }

    /// Lock this computer and the connected ones. `from` is the computer that
    /// asked, None when the lock key was pressed here. The keyboard host passes
    /// the lock on to its other computers; a controlled computer only tells its host.
    async fn lock_all(&mut self, from: Option<&str>) {
        if self.last_lock.elapsed() < LOCK_REPEAT {
            return;
        }
        self.last_lock = Instant::now();
        // Release remote keys first (especially Super) so a following LockScreen
        // does not leave modifiers held into the password field.
        if self.grab {
            self.return_home("lock screen").await;
        } else {
            self.injected.release_injected(self.injector.as_mut());
        }
        if from.is_none() || self.is_host().await {
            for peer in self.net.peer_names().await {
                if from.is_some_and(|f| f.eq_ignore_ascii_case(&peer)) {
                    continue;
                }
                self.net
                    .send_control(&peer, csc_proto::encode_lock_screen_bin())
                    .await;
            }
        }
        info!(
            "locking the screen (asked by {})",
            from.unwrap_or("this computer")
        );
        csc_platform::lock_screen();
    }

    /// The pointer leaves the active remote through `leave.edge`: hop to the
    /// next computer, or come back here. True when input is back on this computer.
    async fn leave_remote(&mut self, leave: EnterDecision) -> bool {
        let local_name = self.local_name().await;
        let current = self.switch.active_remote().to_string();
        let next = self.switch.neighbor_from_active(leave.edge);
        let hop = next.as_ref().filter(|name| {
            !name.eq_ignore_ascii_case(&local_name) && !name.eq_ignore_ascii_case(&current)
        });
        let leave_msg = SwitchPayload {
            edge: leave.edge,
            position_pct: leave.position_pct,
            locks: None,
        };
        if let Some(next) = hop.cloned() {
            self.held.release_all(&self.net).await;
            self.net
                .send_to(&current, encode_switch_leave_bin(&leave_msg))
                .await;
            let enter = self.enter_payload(leave.edge.opposite(), leave.position_pct);
            self.net
                .send_to(&next, csc_proto::encode_switch_bin(&enter))
                .await;
            let remote = peer_size(&self.peer_screens, &next, self.screen);
            self.switch.confirm_entered(
                enter.edge,
                enter.position_pct,
                remote,
                &mut self.remote_x,
                &mut self.remote_y,
                &next,
            );
            self.switch.inset_remote_estimate(
                enter.edge,
                &mut self.remote_x,
                &mut self.remote_y,
                remote,
            );
            self.switch
                .arm_arrival_guard(enter.edge, (self.remote_x, self.remote_y));
            let reports = self.net.peer_caps(&next).await & CAP_EDGE_REPORT != 0;
            self.switch.set_remote_reports(reports);
            self.set_active_screen(Some(&next)).await;
            info!(
                "switch hop {current} -> {next} at {:?} {:.2}",
                leave.edge, leave.position_pct
            );
            false
        } else {
            self.switch.confirm_returned();
            let back = self.switch.local_reentry(leave.edge, leave.position_pct);
            release_pointer(&mut self.grab, Some(back));
            self.grab_armed = false;
            self.home_point = back;
            self.held.release_all(&self.net).await;
            if current.is_empty() {
                self.net.send_raw(encode_switch_leave_bin(&leave_msg)).await;
            } else {
                self.net
                    .send_to(&current, encode_switch_leave_bin(&leave_msg))
                    .await;
            }
            self.settle_local(back, Some(leave.edge.opposite()));
            self.set_active_screen(None).await;
            info!(
                "switch return from {current} at {:?} {:.2}, cursor to {back:?}",
                leave.edge, leave.position_pct
            );
            true
        }
    }

    /// Wayland barriers only on sides that currently have a live peer session.
    /// Layout alone is not enough: an offline neighbor would trap the pointer.
    async fn refresh_capture_edges(&mut self) {
        if !self.is_host().await {
            return;
        }
        let peers = self.net.peer_names().await;
        let paused = &self.paused_peers;
        let online = |name: &str| {
            peers.iter().any(|p| p.eq_ignore_ascii_case(name))
                && !paused.iter().any(|p| p.eq_ignore_ascii_case(name))
        };
        let next = self.switch.local_sides_reachable(online);
        if self.last_capture_edges == Some(next) {
            return;
        }
        self.last_capture_edges = Some(next);
        let (left, right, top, bottom) = next;
        csc_platform::capture_set_edges(left, right, top, bottom);
    }

    /// Hand compositor capture back inside the screen, never on the barrier pixel.
    fn release_capture_inside(&mut self, local_edge: Option<(csc_proto::Edge, f32)>) {
        let (ox, oy) = csc_platform::virtual_origin();
        let (x, y) = if let Some((edge, pct)) = local_edge {
            // local_reentry takes the remote edge; opposite of the local hit.
            self.switch.local_reentry(edge.opposite(), pct)
        } else {
            self.home_point
        };
        csc_platform::capture_return_to(Some((ox + x, oy + y)));
        self.settle_local((x, y), local_edge.map(|(edge, _)| edge));
    }

    /// Release from a barrier hit when the edge decision is unknown: nudge inward.
    fn release_capture_from_hit(&mut self, ax: i32, ay: i32) {
        let (ox, oy) = csc_platform::virtual_origin();
        let (x, y) = self.switch.nudge_inside(ax - ox, ay - oy);
        csc_platform::capture_return_to(Some((ox + x, oy + y)));
        self.settle_local((x, y), self.home_edge);
    }

    /// Ignore the seam for a moment and require an inward move before recrossing.
    fn settle_local(&mut self, at: (i32, i32), local_edge: Option<csc_proto::Edge>) {
        let edge = local_edge.or(self.home_edge);
        if let Some(edge) = edge {
            self.home_edge = Some(edge);
            self.switch.arm_reentry_guard(edge, at);
        }
    }

    /// The local pointer, while it is on this computer.
    async fn local_step(&mut self) {
        let hit = csc_platform::capture_edge_hit();
        if let Some(at) = hit {
            // We always release capture inset from the seam, so a barrier hit
            // means the pointer really travelled back to the edge.
            self.switch.clear_reentry_guard();
            match self.local_step_at(Some(at)).await {
                LocalEdge::Entered => {}
                LocalEdge::Missed { edge, pct } => {
                    // Peer offline / no size: never release onto the barrier itself.
                    self.release_capture_inside(Some((edge, pct)));
                    self.refresh_capture_edges().await;
                }
                LocalEdge::Idle => {
                    // Barrier fired but switching is blocked; free the pointer inward.
                    self.release_capture_from_hit(at.0, at.1);
                    self.refresh_capture_edges().await;
                }
            }
        } else {
            let _ = self.local_step_at(None).await;
        }
    }

    /// `hit` is where a compositor barrier stopped the pointer (Wayland hosts).
    async fn local_step_at(&mut self, hit: Option<(i32, i32)>) -> LocalEdge {
        let raw = csc_platform::take_raw_motion();
        let Some((ax, ay)) = hit.or_else(csc_platform::cursor_pos) else {
            return LocalEdge::Idle;
        };
        let (ox, oy) = csc_platform::virtual_origin();
        let x = ax - ox;
        let y = ay - oy;
        csc_platform::vdrag::note_pointer(x, y);
        let dragging = csc_platform::primary_button_down();
        if self.block_fullscreen && !dragging && csc_platform::foreground_fullscreen() {
            return LocalEdge::Idle;
        }
        let Some(mut dec) = self.switch.on_local_cursor(x, y, raw, Instant::now()) else {
            return LocalEdge::Idle;
        };
        dec.neighbor = self.session_name(&dec.neighbor).await;
        if self
            .paused_peers
            .iter()
            .any(|n| n.eq_ignore_ascii_case(&dec.neighbor))
        {
            if hit.is_some() {
                return LocalEdge::Missed {
                    edge: dec.edge,
                    pct: dec.position_pct,
                };
            }
            return LocalEdge::Idle;
        }
        let known_screen = self.peer_screens.iter().any(|(k, size)| {
            k.eq_ignore_ascii_case(&dec.neighbor) && size.width > 1 && size.height > 1
        });
        let has_peer = self.net.has_peer(&dec.neighbor).await;
        if has_peer && known_screen {
            self.enter_remote(dec, x, y).await;
            return LocalEdge::Entered;
        }
        if hit.is_some() {
            if self.last_miss.elapsed() > Duration::from_secs(2) {
                self.last_miss = Instant::now();
                if !has_peer {
                    warn!(
                        "pointer hit {:?} but {} has no session; clearing that barrier",
                        dec.edge, dec.neighbor
                    );
                } else {
                    warn!(
                        "pointer hit {:?} but {} has not reported its screen size; clearing that barrier",
                        dec.edge, dec.neighbor
                    );
                }
            }
            return LocalEdge::Missed {
                edge: dec.edge,
                pct: dec.position_pct,
            };
        }
        LocalEdge::Idle
    }

    /// Session for a layout name. A layout saved while a computer was known only
    /// by its address still reaches it once it connects under its name.
    async fn session_name(&self, name: &str) -> String {
        if self.net.has_peer(name).await || name.parse::<std::net::IpAddr>().is_err() {
            return name.to_string();
        }
        let by_address = self
            .status
            .read()
            .await
            .discovered
            .iter()
            .find(|p| p.address == name)
            .map(|p| p.name.clone());
        match by_address {
            Some(peer) if self.net.has_peer(&peer).await => peer,
            _ => name.to_string(),
        }
    }

    /// Move the pointer from this computer onto `dec.neighbor`.
    async fn enter_remote(&mut self, dec: EnterDecision, x: i32, y: i32) {
        let (ox, oy) = csc_platform::virtual_origin();
        let entry = dec.edge.opposite();
        let payload = self.enter_payload(entry, dec.position_pct);
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
        self.switch
            .inset_remote_estimate(entry, &mut self.remote_x, &mut self.remote_y, remote);
        self.switch
            .arm_arrival_guard(entry, (self.remote_x, self.remote_y));
        let reports = self.net.peer_caps(&dec.neighbor).await & CAP_EDGE_REPORT != 0;
        self.switch.set_remote_reports(reports);
        self.enter_point = (x, y);
        self.home_point = self.switch.local_reentry(entry, dec.position_pct);
        self.home_edge = Some(dec.edge);
        self.held = HeldInput::default();
        // Leave the cursor on the edge it just crossed. Warping it to the
        // monitor center is the stutter, and clipping that center traps the
        // hand there while the other computer already shows it is controlled.
        // Later motion is relative (raw input), not a reposition.
        self.park_x = x;
        self.park_y = y;
        // Barriers can activate the instant the portal enables, before the
        // periodic poll sets `hooked`. Always re-check and arm grab here.
        if csc_platform::wayland_session() {
            self.hooked = csc_platform::capture_start() || self.hooked;
        }
        if self.hooked {
            csc_platform::capture_set_grab(true, Some((ox + x, oy + y)));
        } else {
            csc_platform::warp_cursor(ox + x, oy + y);
        }
        self.grab = true;
        self.virtual_locks = Some(csc_platform::lock_leds());
        if self.config.read().await.pointer.hide_host_cursor {
            csc_platform::set_host_cursor_hidden(true);
        }
        self.grab_armed = false;
        self.set_active_screen(Some(&dec.neighbor)).await;
        self.flush_pending_clip(&dec.neighbor).await;
        self.last_local_activity = Instant::now();
        info!(
            "switch enter -> {} at {:?} {:.2} from ({x},{y}), land at ({}, {}), return at {:?}{}",
            dec.neighbor,
            dec.edge,
            dec.position_pct,
            self.remote_x,
            self.remote_y,
            self.home_point,
            if reports {
                "; remote reports its edges"
            } else {
                ""
            }
        );
    }

    // ------------------------------------------------------------ clipboard

    async fn share_peers(&self) -> Vec<String> {
        let names = self.net.peer_names().await;
        let layout = ScreenLayout::load(&self.config.read().await.layout);
        let scoped: Vec<_> = names
            .iter()
            .filter(|n| layout.in_share_scope(n))
            .cloned()
            .collect();
        // Until the user places anyone, keep clipping among connected peers.
        if scoped.is_empty() {
            names
        } else {
            scoped
        }
    }

    async fn on_clip(&mut self, ev: ClipEvent) {
        // Something copied here: files still on their way must not replace it.
        self.staging = None;
        if !self.status.read().await.sharing_enabled {
            return;
        }
        let peers = self.share_peers().await;
        let c = self.config.read().await.clipboard.clone();
        let host = self.is_host().await;
        let on_switch = c.push_mode == "on_switch";
        self.clip_seq += 1;
        let seq = self.clip_seq;
        match ev {
            ClipEvent::Text(text) => {
                info!("copied text here ({} chars)", text.chars().count());
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
                if on_switch {
                    self.pending_clip = Some(offer);
                } else {
                    self.send_clip_offer(&offer, &peers, None).await;
                }
            }
            ClipEvent::Files(paths) => {
                info!(
                    "copied {} file(s) here: {}",
                    paths.len(),
                    paths
                        .first()
                        .map(|p| p.display().to_string())
                        .unwrap_or_default()
                );
                self.my_files = Some((seq, paths.clone()));
                if host {
                    self.shared.copied("", seq, true);
                    csc_platform::paste_watch(false);
                    let files = file_entries(&paths);
                    let offer = ClipOfferPayload {
                        seq,
                        formats: vec!["files".into()],
                        text: None,
                        files,
                    };
                    if on_switch {
                        self.pending_clip = Some(offer);
                    } else {
                        self.send_clip_offer(&offer, &peers, None).await;
                    }
                    return;
                }
                if peers.is_empty() || !c.files {
                    info!("copied files are not offered: no keyboard host, or file copying is off");
                    return;
                }
                // Full entries so the paste side can build correct placeholders.
                let files = file_entries(&paths);
                let offer = ClipOfferPayload {
                    seq,
                    formats: vec!["files".into()],
                    text: None,
                    files,
                };
                if on_switch {
                    self.pending_clip = Some(offer);
                } else {
                    self.send_clip_offer(&offer, &peers, None).await;
                }
            }
            ClipEvent::Image(img) => {
                info!(
                    "copied an image here ({}x{}, {} KB PNG)",
                    img.width,
                    img.height,
                    img.png.len() / 1024
                );
                if host {
                    self.shared.copied("", seq, false);
                    csc_platform::paste_watch(false);
                }
                if peers.is_empty() || !c.enabled || !c.image {
                    return;
                }
                let limit = c.max_inline_bytes().min(csc_proto::MAX_PAYLOAD - 8);
                if img.png.len() > limit {
                    popup(
                        &format!(
                            "复制的图片有 {:.1} MB，超过同步上限 {} MB，没有同步到其他电脑。",
                            img.png.len() as f64 / (1024.0 * 1024.0),
                            c.max_inline_mb
                        ),
                        true,
                    );
                    return;
                }
                if on_switch {
                    // Images still push immediately; no pending image buffer.
                }
                let frame = csc_proto::encode_clip_image_bin(seq, &img.png);
                self.send_clip_frame(frame, &peers, None).await;
            }
        }
    }

    async fn flush_pending_clip(&mut self, peer: &str) {
        let Some(offer) = self.pending_clip.clone() else {
            return;
        };
        self.send_clip_offer(&offer, &[peer.to_string()], None)
            .await;
    }

    async fn send_clip_frame(&self, frame: Vec<u8>, peers: &[String], except: Option<&str>) {
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

    /// An image copied on `peer`. The keyboard host passes it on like text.
    async fn on_clip_image(&mut self, peer: &str, payload: Vec<u8>) {
        self.staging = None;
        let Some((seq, png)) = csc_proto::decode_clip_image_bin(&payload) else {
            warn!("bad clipboard image from {peer}");
            return;
        };
        info!("{peer} copied an image ({} KB PNG)", png.len() / 1024);
        let c = self.config.read().await.clipboard.clone();
        if self.is_host().await {
            self.shared.copied(peer, seq, false);
            csc_platform::paste_watch(false);
            let peers = self.net.peer_names().await;
            let frame = csc_proto::encode_clip_image_bin(seq, png);
            self.send_clip_frame(frame, &peers, Some(peer)).await;
        }
        if c.enabled && c.image && seq != self.last_remote_clip {
            self.last_remote_clip = seq;
            self.clip.set_image(png.to_vec());
        }
    }

    async fn send_clip_offer(
        &self,
        offer: &ClipOfferPayload,
        peers: &[String],
        except: Option<&str>,
    ) {
        let Ok(frame) = encode_json_frame(MsgType::ClipOffer, offer) else {
            return;
        };
        self.send_clip_frame(frame, peers, except).await;
    }

    async fn on_clip_offer(&mut self, peer: &str, offer: ClipOfferPayload) {
        self.staging = None;
        if !self.status.read().await.sharing_enabled {
            return;
        }
        let host = self.is_host().await;
        let c = self.config.read().await.clipboard.clone();
        if offer.formats.iter().any(|f| f == "files") {
            self.remote_files = Some((peer.to_string(), offer.seq));
            if !c.files {
                return;
            }
            if host {
                info!(
                    "{peer} copied {} file(s); Ctrl+V on another computer fetches them",
                    offer.files.len()
                );
                self.shared.copied(peer, offer.seq, true);
                // Linux uses clipboard placeholders (no XGrabKey). Windows host
                // still intercepts Ctrl+V via paste_watch.
                if cfg!(windows) {
                    csc_platform::paste_watch(true);
                }
                #[cfg(target_os = "linux")]
                self.arm_remote_paste(peer, &offer).await;
            } else if cfg!(target_os = "linux") {
                self.arm_remote_paste(peer, &offer).await;
            } else if cfg!(windows) {
                // Windows client cannot build X11 placeholders; catch Ctrl+V.
                csc_platform::paste_watch(true);
            }
            return;
        }
        let Some(text) = offer.text.clone() else {
            return;
        };
        self.remote_files = None;
        if host {
            self.shared.copied(peer, offer.seq, false);
            csc_platform::paste_watch(false);
            // Pass it on, so every controlled computer can paste it too.
            let peers = self.share_peers().await;
            self.send_clip_offer(&offer, &peers, Some(peer)).await;
        }
        if c.enabled && c.text && offer.seq != self.last_remote_clip {
            self.last_remote_clip = offer.seq;
            info!(
                "clipboard text from {peer} ({} chars)",
                text.chars().count()
            );
            self.clip.set_text(text);
        }
    }

    /// Make another computer's copied files pasteable here: fetch small
    /// batches right away, offer placeholders for big ones.
    async fn arm_remote_paste(&mut self, peer: &str, offer: &ClipOfferPayload) {
        if prefetch_copied(&offer.files) {
            self.prefetch_remote_files(peer, offer.seq).await;
        } else {
            self.arm_paste_placeholders(peer, offer);
        }
    }

    /// Ask `peer` for its copied files now. Once every file arrived and passed
    /// its hash check, they go on the clipboard, so any program pastes real data.
    async fn prefetch_remote_files(&mut self, peer: &str, seq: u64) {
        csc_platform::cancel_paste();
        self.paste_gen = 0;
        let root = clip_stage_root();
        let _ = std::fs::remove_dir_all(&root);
        let dir = root.join(seq.to_string());
        if let Err(e) = std::fs::create_dir_all(&dir) {
            warn!("cannot create {}: {e}; using placeholders", dir.display());
            return;
        }
        info!("fetching the files {peer} copied so they can be pasted here");
        let ready = self
            .transfer
            .expect_paste_ready(peer.to_string(), PasteSink::Stage(dir))
            .await;
        self.watch_paste_ready(peer, seq, ready, false);
        let located = PasteLocatedPayload {
            paste_id: seq,
            ok: true,
        };
        if let Ok(frame) = encode_json_frame(MsgType::PasteLocated, &located) {
            self.net.send_control(peer, frame).await;
        }
    }

    fn watch_paste_ready(
        &mut self,
        peer: &str,
        seq: u64,
        ready: crate::transfer::PasteReady,
        in_place: bool,
    ) {
        self.staging = Some((peer.to_string(), seq));
        let tx = self.staged_tx.clone();
        let peer = peer.to_string();
        tokio::spawn(async move {
            let paths = ready.await.ok();
            let _ = tx
                .send(StagedPaste {
                    peer,
                    seq,
                    paths,
                    in_place,
                })
                .await;
        });
    }

    async fn on_staged_paste(&mut self, staged: StagedPaste) {
        let current = self
            .staging
            .as_ref()
            .is_some_and(|(p, s)| p.eq_ignore_ascii_case(&staged.peer) && *s == staged.seq);
        if !current {
            debug!(
                "files from {} copy {} arrived after a newer copy; clipboard kept",
                staged.peer, staged.seq
            );
            return;
        }
        self.staging = None;
        match staged.paths {
            Some(paths) if !paths.is_empty() => {
                info!(
                    "files {} copied are here and verified; clipboard now holds {} real item(s)",
                    staged.peer,
                    paths.len()
                );
                // Later pastes copy the real files, never the placeholders again.
                self.clip.set_files(paths);
            }
            _ if staged.in_place => {}
            _ => {
                warn!("fetching the files {} copied failed", staged.peer);
                popup(
                    &format!(
                        "没能从 {} 取到复制的文件，剪贴板里没有放入文件。请在那台电脑上重新复制。",
                        staged.peer
                    ),
                    true,
                );
            }
        }
    }

    fn arm_paste_placeholders(&mut self, peer: &str, offer: &ClipOfferPayload) {
        let items: Vec<csc_platform::vdrag::DragItem> = offer
            .files
            .iter()
            .map(|e| csc_platform::vdrag::DragItem {
                relative_path: e.relative_path.clone(),
                size: e.size,
                mtime_unix: e.mtime_unix,
                is_directory: e.is_directory,
            })
            .collect();
        let Some((gen, paths)) = csc_platform::prepare_paste(offer.seq, &items) else {
            return;
        };
        info!(
            "offering {} placeholder(s) so a paste here can name the folder",
            paths.len()
        );
        self.clip.set_files(paths);
        self.paste_gen = gen;
        let tx = self.paste_tx.clone();
        let peer = peer.to_string();
        let seq = offer.seq;
        tokio::task::spawn_blocking(move || {
            let found = csc_platform::wait_paste(gen, Duration::from_secs(10 * 60));
            if let Some(paths) = found {
                let _ = tx.blocking_send(LocatedPaste {
                    peer,
                    seq,
                    gen,
                    paths,
                });
            }
        });
    }

    async fn on_located_paste(&mut self, found: LocatedPaste) {
        if found.gen != self.paste_gen {
            return;
        }
        info!(
            "paste landed in {} item path(s); asking {} for the files",
            found.paths.len(),
            found.peer
        );
        let ready = self
            .transfer
            .expect_paste_ready(found.peer.clone(), PasteSink::InPlace(found.paths))
            .await;
        self.watch_paste_ready(&found.peer, found.seq, ready, true);
        // The computer that was pasted on tells the keyboard host. The host sends the files.
        let located = PasteLocatedPayload {
            paste_id: found.seq,
            ok: true,
        };
        if let Ok(frame) = encode_json_frame(MsgType::PasteLocated, &located) {
            self.net.send_control(&found.peer, frame).await;
        }
    }

    /// Tray: save another computer's copied files into the fallback folder.
    async fn paste_remote_here(&mut self) {
        if self.is_host().await {
            if !self.shared.remote_files() {
                notice::tell(
                    notice::Kind::Transfer,
                    "Cross-SystemConnect 文件传输",
                    "没有其他电脑复制的文件。",
                    false,
                );
                return;
            }
            self.transfer
                .expect_paste(String::new(), PasteSink::ReceiveDir)
                .await;
            let _ = self.paste(String::new()).await;
            return;
        }
        let Some((peer, seq)) = self.remote_files.clone() else {
            notice::tell(
                notice::Kind::Transfer,
                "Cross-SystemConnect 文件传输",
                "没有其他电脑复制的文件。",
                false,
            );
            return;
        };
        self.transfer
            .expect_paste(peer.clone(), PasteSink::ReceiveDir)
            .await;
        info!("tray paste: telling {peer} the files were pasted here");
        let located = PasteLocatedPayload {
            paste_id: seq,
            ok: true,
        };
        if let Ok(frame) = encode_json_frame(MsgType::PasteLocated, &located) {
            self.net.send_control(&peer, frame).await;
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
        let shown = if target.is_empty() {
            local.clone()
        } else {
            target.clone()
        };
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

    // ------------------------------------------------------------ controlled side

    /// Input from the keyboard host moved this computer's pointer.
    async fn controlled_move(&mut self, dx: i32, dy: i32) {
        let w = self.screen.width.max(1);
        let h = self.screen.height.max(1);
        let prev = (self.remote_x, self.remote_y);
        self.remote_x = self.remote_x.saturating_add(dx).clamp(0, w - 1);
        self.remote_y = self.remote_y.saturating_add(dy).clamp(0, h - 1);
        let applied = (self.remote_x - prev.0, self.remote_y - prev.1);
        csc_platform::vdrag::note_pointer(self.remote_x, self.remote_y);
        // Pass the host's relative step. Do not re-pin when the pointer is
        // already on an edge: that jump pulls it off the edge and feels stuck.
        let (sx, sy) = if applied == (0, 0) && (dx != 0 || dy != 0) {
            (dx, dy)
        } else {
            applied
        };
        inject_controlled_delta(self.injector.as_mut(), self.remote_x, self.remote_y, sx, sy);
        let (ox, oy) = csc_platform::virtual_origin();
        let (at, moved, real) = if csc_platform::wayland_session() {
            self.commanded = Some((self.remote_x, self.remote_y));
            (
                (self.remote_x, self.remote_y),
                // Prefer host delta for edge probing when clamp ate applied.
                if applied == (0, 0) && (dx != 0 || dy != 0) {
                    (dx, dy)
                } else {
                    applied
                },
                (self.remote_x, self.remote_y),
            )
        } else {
            let Some((ax, ay)) = csc_platform::cursor_pos() else {
                return;
            };
            let real = (ax - ox, ay - oy);
            let real_delta = self
                .last_real
                .map(|l| (real.0 - l.0, real.1 - l.1))
                .unwrap_or((0, 0));
            // A rejected warp leaves the pointer where it was. Copying that
            // position back pinned every later move on Wayland.
            let real_moved = self.last_real.is_some() && real_delta != (0, 0);
            self.last_real = Some(real);
            let at = follow_real_pointer((self.remote_x, self.remote_y), real, real_moved);
            self.remote_x = at.0;
            self.remote_y = at.1;
            let moved = if real_moved { real_delta } else { applied };
            (at, moved, real)
        };
        self.switch.note_arrival_progress(at.0, at.1);
        let Some(report) = self
            .switch
            .probe_edge(&mut self.probe, at.0, at.1, (dx, dy), moved)
        else {
            return;
        };
        if self.switch.arrival_blocks(report.edge) {
            return;
        }
        if self.last_edge_sent.elapsed() < Duration::from_millis(16) {
            return;
        }
        let Some(host) = self.net.peer_names().await.into_iter().next() else {
            return;
        };
        info!(
            "pointer at {:?} pushed {:?} {:.2} (corner {:?})",
            real, report.edge, report.position_pct, report.corner
        );
        let hit = EdgeHitPayload {
            edge: report.edge,
            position_pct: report.position_pct,
            dragging: false,
            corner: report.corner,
            transfer_id: 0,
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
        if self.switch.arrival_blocks(hit.edge) {
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
        self.leave_remote(leave).await;
    }

    async fn on_paste_request(&mut self, peer: &str, req: PasteRequestPayload) {
        match self.my_files.clone() {
            Some((seq, paths)) if seq == req.seq => {
                info!(
                    "sending copied files for a paste on {}",
                    if req.to.is_empty() { peer } else { &req.to }
                );
                self.transfer
                    .send_via(
                        peer.to_string(),
                        paths,
                        csc_proto::TransferPurpose::Paste,
                        req.to,
                    )
                    .await;
            }
            _ => info!("paste request {} from {peer} is for an older copy", req.seq),
        }
    }

    async fn on_power(&mut self, ev: csc_platform::PowerEvent) {
        if matches!(
            ev,
            csc_platform::PowerEvent::Locked
                | csc_platform::PowerEvent::Unlocked
                | csc_platform::PowerEvent::SecureDesktop
        ) {
            let state = match ev {
                csc_platform::PowerEvent::Locked => "locked",
                csc_platform::PowerEvent::SecureDesktop => "secure_desktop",
                _ => "normal",
            };
            // Local lock while controlling a peer: release the parked cursor.
            // Windows reports Win+L, Ctrl+Alt+Del and UAC only as the secure
            // desktop; the input hook stops there, so the pointer would stay
            // pinned to the park point after unlocking.
            if local_lock_releases(state) {
                // Keys injected here for a host (Win of Win+L) never come up.
                self.injected.release_injected(self.injector.as_mut());
                if self.grab {
                    let why = if state == "locked" {
                        "local locked"
                    } else {
                        "local secure desktop"
                    };
                    self.return_home(why).await;
                }
            }
            self.send_peer_state(state).await;
            return;
        }
        if ev == csc_platform::PowerEvent::Resumed {
            info!("woke from sleep; refreshing discovery");
            self.net_reload.send_modify(|n| *n = n.wrapping_add(1));
            return;
        }
        let state = if ev == csc_platform::PowerEvent::ShuttingDown {
            "shutting_down"
        } else {
            "suspending"
        };
        info!("power: {state}");
        self.injected.release_injected(self.injector.as_mut());
        if self.grab {
            self.switch.unlock_to_local();
            release_pointer(&mut self.grab, Some(self.home_point));
            self.grab_armed = false;
        }
        let peers = self.status.read().await.peer_name.clone();
        let notice = PeerStatePayload {
            state: state.into(),
        };
        let reset = encode_frame(MsgType::InputReset, &[], 0);
        for peer in peers.split(',').map(str::trim).filter(|n| !n.is_empty()) {
            if let Ok(frame) = encode_json_frame(MsgType::PeerState, &notice) {
                self.net.send_control(peer, frame).await;
            }
            self.net.send_control(peer, reset.clone()).await;
        }
    }

    async fn send_peer_state(&mut self, state: &str) {
        let peers = self.status.read().await.peer_name.clone();
        let Ok(frame) = encode_json_frame(
            MsgType::PeerState,
            &PeerStatePayload {
                state: state.into(),
            },
        ) else {
            return;
        };
        for peer in peers.split(',').map(str::trim).filter(|n| !n.is_empty()) {
            self.net.send_control(peer, frame.clone()).await;
        }
    }

    async fn on_peer_state(&mut self, peer: &str, state: &str) {
        info!("peer {peer} state {state}");
        if state == "locked" || state == "secure_desktop" || state == "normal" {
            let note = match state {
                "locked" => format!("{peer} 已锁屏"),
                "secure_desktop" => format!("{peer} 在安全桌面"),
                _ => String::new(),
            };
            self.status.write().await.peer_note = note;
            if state == "secure_desktop" {
                self.paused_peers.insert(peer.to_string());
            }
            if (state == "locked" || state == "secure_desktop")
                && self.grab
                && self.switch.active_remote().eq_ignore_ascii_case(peer)
            {
                let why = if state == "locked" {
                    "peer locked"
                } else {
                    "secure desktop"
                };
                self.return_home(why).await;
            } else if state == "normal" {
                self.paused_peers.retain(|n| !n.eq_ignore_ascii_case(peer));
            }
            return;
        }
        if state == "suspending" || state == "shutting_down" {
            self.injected.release_injected(self.injector.as_mut());
            if self.grab && self.switch.active_remote().eq_ignore_ascii_case(peer) {
                self.switch.unlock_to_local();
                release_pointer(&mut self.grab, Some(self.home_point));
                self.grab_armed = false;
            }
            if self.status.read().await.role != "server" {
                self.status.write().await.host_state = "host_sleeping".into();
            }
            notice::tell(
                notice::Kind::Disconnect,
                "Cross-SystemConnect",
                "对方电脑即将休眠",
                false,
            );
        }
    }

    async fn on_from(&mut self, peer: String, msg: MsgType, payload: Vec<u8>) {
        match msg {
            MsgType::ClipOffer => match serde_json::from_slice::<ClipOfferPayload>(&payload) {
                Ok(offer) => self.on_clip_offer(&peer, offer).await,
                Err(e) => warn!("bad clip offer: {e}"),
            },
            MsgType::ClipData => self.on_clip_image(&peer, payload).await,
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
            MsgType::PasteLocated => {
                if let Ok(located) = serde_json::from_slice::<PasteLocatedPayload>(&payload) {
                    if located.ok {
                        self.on_paste_request(
                            &peer,
                            PasteRequestPayload {
                                seq: located.paste_id,
                                to: String::new(),
                            },
                        )
                        .await;
                    }
                }
            }
            MsgType::LockScreen => self.lock_all(Some(&peer)).await,
            MsgType::Activity => {
                csc_platform::note_user_activity();
                self.last_local_activity = Instant::now();
            }
            MsgType::InputReset => {
                self.injected.release_injected(self.injector.as_mut());
            }
            MsgType::ControlPause => {
                let Ok(pause) = serde_json::from_slice::<ControlPausePayload>(&payload) else {
                    return;
                };
                if pause.paused {
                    self.paused_peers.insert(peer.clone());
                    notice::tell(
                        notice::Kind::Disconnect,
                        "Cross-SystemConnect",
                        &notice::text(
                            &format!("{peer} 暂停了共享"),
                            &format!("{peer} paused sharing"),
                        ),
                        false,
                    );
                    if self.grab && self.switch.active_remote().eq_ignore_ascii_case(&peer) {
                        self.return_home("peer paused sharing").await;
                    }
                } else {
                    self.paused_peers.retain(|n| !n.eq_ignore_ascii_case(&peer));
                }
            }
            MsgType::PeerState => {
                if let Ok(state) = serde_json::from_slice::<PeerStatePayload>(&payload) {
                    self.on_peer_state(&peer, &state.state).await;
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
                // Prefer live sessions over the historical set: a superseded IP
                // key must not clear a hostname that is still connected.
                let up = self.net.greeted_names().await;
                let still = up.iter().any(|n| !n.eq_ignore_ascii_case(&peer));
                let was_up = self.greeted.remove(&peer.to_lowercase()) && !still;
                self.greeted = up.iter().map(|n| n.to_lowercase()).collect();
                let mut s = self.status.write().await;
                // Drop RTT samples for peers that are no longer greeted.
                s.peer_latency_ms
                    .retain(|k, _| up.iter().any(|n| n.eq_ignore_ascii_case(k)));
                if was_up {
                    info!("session {peer} disconnected");
                } else if still {
                    debug!(
                        "session {peer} closed; other link(s) still up: {}",
                        up.join(", ")
                    );
                    s.connected = true;
                    s.peer_name = up.join(", ");
                } else {
                    debug!("connection to {peer} closed before the handshake finished");
                }
                if still {
                    // Keep grab/status; this was a superseded IP or duplicate dial.
                    drop(s);
                } else {
                    let active = self.grab
                        && (self.switch.active_remote().eq_ignore_ascii_case(&peer)
                            || s.active_screen.eq_ignore_ascii_case(&peer));
                    if active {
                        self.switch.unlock_to_local();
                        self.held = HeldInput::default();
                        release_pointer(&mut self.grab, Some(self.home_point));
                        self.grab_armed = false;
                        // Field writes only: `settle_local` would reborrow all of `self`
                        // while `s` still holds `self.status`.
                        if let Some(edge) = self.home_edge {
                            self.switch.arm_reentry_guard(edge, self.home_point);
                        }
                        s.active_screen = s.local_name.clone();
                    }
                    if !self.grab {
                        // The host that was driving this computer is gone.
                        self.injected.release_injected(self.injector.as_mut());
                        s.being_controlled = false;
                        if self.switch.state() == SwitchState::Remote {
                            self.switch.unlock_to_local();
                            s.active_screen = s.local_name.clone();
                        }
                    }
                    s.peer_name = up.join(", ");
                    s.connected = !s.peer_name.is_empty();
                    if !s.connected {
                        s.being_controlled = false;
                    }
                    if s.role != "server" && s.host_state != "host_sleeping" {
                        s.host_state = if s.connected {
                            "connected".into()
                        } else {
                            "host_lost".into()
                        };
                    }
                    let sleeping = s.host_state == "host_sleeping";
                    let was_host = s.role == "server";
                    drop(s);
                    if was_up && !sleeping {
                        notice::tell(
                            notice::Kind::Disconnect,
                            "Cross-SystemConnect",
                            &format!("与 {peer} 断开"),
                            false,
                        );
                    }
                    self.shared.peer_gone(&peer);
                    csc_platform::paste_watch(self.shared.remote_files());
                    self.transfer.peer_gone(peer).await;
                    // Drop barriers that pointed at this peer so the compositor
                    // cannot keep swallowing the pointer on a dead edge.
                    if was_host {
                        self.refresh_capture_edges().await;
                    }
                }
            }
            Some(InboundEvent::Hello(h)) => {
                info!(
                    "HELLO from {} ({}x{}, capabilities {:#x})",
                    h.host_name, h.screen_width, h.screen_height, h.capabilities
                );
                // Refresh link status immediately so the UI shows 已连接 without
                // waiting for the next 1ms tick (and without stale IP keys).
                let up = self.net.greeted_names().await;
                let all = self.net.peer_names().await;
                self.greeted = up.iter().map(|n| n.to_lowercase()).collect();
                {
                    let mut s = self.status.write().await;
                    s.connected = !all.is_empty();
                    s.peer_name = if !up.is_empty() {
                        up.join(", ")
                    } else {
                        all.join(", ")
                    };
                    if s.role != "server" {
                        s.host_state = "connected".into();
                    }
                    if s.peer_note.contains("还没有信任本机")
                        || s.peer_note.contains("正在切换为从机")
                        || s.peer_note.contains("正在连接")
                    {
                        s.peer_note.clear();
                    }
                }
                if !h.host_name.is_empty() {
                    notice::tell(
                        notice::Kind::Connect,
                        "Cross-SystemConnect",
                        &format!("已连接 {}", h.host_name),
                        false,
                    );
                }
                if h.screen_width > 1 && h.screen_height > 1 {
                    self.peer_screens.insert(
                        h.host_name.clone(),
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
                // Land on the real monitor at that position, not in a gap of the bounding box.
                let m = self.switch.monitor_at(self.remote_x, self.remote_y);
                self.remote_x = self.remote_x.clamp(m.left, m.right - 1);
                self.remote_y = self.remote_y.clamp(m.top, m.bottom - 1);
                self.switch
                    .inset_from_edge(p.edge, &mut self.remote_x, &mut self.remote_y);
                self.probe.reset();
                // Do not report the arrival edge until the pointer is seen
                // moving off the landing point. No timer, so turning straight
                // back is never refused.
                self.switch
                    .arm_arrival_guard(p.edge, (self.remote_x, self.remote_y));
                // Seed with the pointer before the jump. If the jump is ignored,
                // the pointer has not moved and later events must not snap back.
                let (ox, oy) = csc_platform::virtual_origin();
                self.last_real = csc_platform::cursor_pos().map(|(ax, ay)| (ax - ox, ay - oy));
                // Wayland: open uinput before the pin jump so the first move is not dropped.
                if csc_platform::wayland_session() {
                    let _ = csc_platform::warm_uinput();
                }
                inject_controlled_jump(
                    self.injector.as_mut(),
                    &mut self.commanded,
                    self.remote_x,
                    self.remote_y,
                    screen,
                );
                csc_platform::vdrag::note_pointer(self.remote_x, self.remote_y);
                if let Some(locks) = p.locks {
                    self.match_locks((locks.caps, locks.num, locks.scroll));
                }
                if !self.grab {
                    self.status.write().await.being_controlled = true;
                }
                info!(
                    "pointer entered this screen from {:?} at ({}, {})",
                    p.edge, self.remote_x, self.remote_y
                );
            }
            Some(InboundEvent::SwitchLeave(p)) => {
                self.status.write().await.being_controlled = false;
                self.commanded = None;
                debug!(edge = ?p.edge, "switch leave");
                if !self.grab {
                    self.injected.release_injected(self.injector.as_mut());
                }
                let current = self.switch.active_remote().to_string();
                let local = self.local_name().await;
                let next = self.switch.neighbor_from_active(p.edge);
                if let Some(next) = next {
                    if !next.eq_ignore_ascii_case(&local) && !next.eq_ignore_ascii_case(&current) {
                        let enter = self.enter_payload(p.edge.opposite(), p.position_pct);
                        self.net
                            .send_to(&next, csc_proto::encode_switch_bin(&enter))
                            .await;
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
                    release_pointer(
                        &mut self.grab,
                        Some(self.switch.local_reentry(p.edge, p.position_pct)),
                    );
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
                    if button == 0 && !down {
                        csc_platform::vdrag::note_local_release();
                    }
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
            Some(InboundEvent::From { peer, msg, payload }) => {
                self.on_from(peer, msg, payload).await
            }
            Some(InboundEvent::Raw(ty, payload)) => {
                debug!(?ty, len = payload.len(), "unhandled frame");
            }
        }
        true
    }
}

fn uninstall(purge: bool) -> anyhow::Result<()> {
    if let Some(dir) = dirs::config_dir() {
        let desk = dir.join("autostart").join("cross-systemconnect.desktop");
        let _ = std::fs::remove_file(desk);
    }
    if let Some(runtime) = std::env::var_os("XDG_RUNTIME_DIR") {
        let rt = PathBuf::from(runtime);
        let _ = std::fs::remove_file(rt.join("cross-systemconnect-core.sock"));
        let _ = std::fs::remove_file(rt.join("cross-systemconnect-core.lock"));
    }
    if purge {
        if let Some(parent) = default_config_path().parent() {
            for name in ["config.json", "log.txt", "log.txt.1"] {
                let _ = std::fs::remove_file(parent.join(name));
            }
            let _ = std::fs::remove_dir_all(parent.join("crash"));
        }
    }
    eprintln!(
        "{}",
        if purge {
            "removed login entry, config, and logs"
        } else {
            "removed login entry; config kept"
        }
    );
    Ok(())
}

fn install_panic_hook(log_path: Option<PathBuf>) {
    let dir = log_path
        .as_ref()
        .and_then(|p| p.parent().map(|d| d.to_path_buf()))
        .or_else(|| default_config_path().parent().map(|p| p.to_path_buf()));
    let Some(dir) = dir else {
        return;
    };
    let prev = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let crash = dir.join("crash");
        let _ = std::fs::create_dir_all(&crash);
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let body = format!("{info}\n\n{:?}", std::backtrace::Backtrace::force_capture());
        let _ = std::fs::write(crash.join(format!("core-{stamp}.txt")), body);
        if let Ok(list) = std::fs::read_dir(&crash) {
            let mut files: Vec<_> = list
                .filter_map(|e| e.ok())
                .filter(|e| e.file_type().map(|t| t.is_file()).unwrap_or(false))
                .collect();
            files.sort_by_key(|e| e.file_name());
            while files.len() > 5 {
                let _ = std::fs::remove_file(files.remove(0).path());
            }
        }
        prev(info);
    }));
}

fn arg_config_path() -> PathBuf {
    std::env::args()
        .position(|a| a == "--config")
        .and_then(|i| std::env::args().nth(i + 1))
        .map(PathBuf::from)
        .unwrap_or_else(default_config_path)
}

/// `--service`, `--install-service`, `--uninstall-service` (Windows elevated
/// mode). Returns the exit code when one of them was given.
pub fn service_cli() -> Option<i32> {
    use csc_platform::elevated::{INSTALL_FLAG, SERVICE_FLAG, UNINSTALL_FLAG};
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mode = args
        .iter()
        .find(|a| [SERVICE_FLAG, INSTALL_FLAG, UNINSTALL_FLAG].contains(&a.as_str()))?
        .clone();
    #[cfg(windows)]
    {
        use csc_platform::win::service;
        let config = arg_config_path();
        let log_dir = config
            .parent()
            .map(std::path::Path::to_path_buf)
            .unwrap_or_else(std::env::temp_dir);
        init_service_log(&log_dir.join("service.log"));
        let result = match mode.as_str() {
            SERVICE_FLAG => service::run(config),
            INSTALL_FLAG => std::env::current_exe().and_then(|exe| service::install(&exe, &config)),
            _ => service::uninstall(),
        };
        Some(match result {
            Ok(()) => {
                info!("{mode}: ok");
                0
            }
            Err(e) => {
                warn!("{mode}: {e}");
                eprintln!("{mode}: {e}");
                1
            }
        })
    }
    #[cfg(not(windows))]
    {
        eprintln!("{mode}: only available on Windows");
        Some(2)
    }
}

#[cfg(windows)]
fn init_service_log(path: &std::path::Path) {
    use csc_config::logfile::{CappedLog, SharedLog};
    let file = SharedLog::default();
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    file.set(CappedLog::open(path).ok());
    let _ = tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::new("info"))
        .with_ansi(false)
        .with_writer(move || file.clone())
        .try_init();
}

/// Tell the user once when an administrator window ignored our input.
async fn watch_blocked_input() {
    let mut every = tokio::time::interval(Duration::from_secs(1));
    loop {
        every.tick().await;
        if let Some(app) = csc_platform::take_input_blocked() {
            let body = notice::text(
                &format!(
                    "前台窗口「{app}」以管理员身份运行，Windows 拦截了远程的鼠标和键盘操作。\
                     如需操作管理员窗口，请在设置里打开「以管理员服务运行（可操作管理员窗口）」。"
                ),
                &format!(
                    "The foreground window \"{app}\" runs as administrator, so Windows blocks \
                     remote mouse and keyboard input. Turn on \"Run as admin service\" in \
                     settings to control administrator windows."
                ),
            );
            notice::tell(notice::Kind::Error, "Cross-SystemConnect", &body, true);
        }
    }
}

pub async fn run() -> anyhow::Result<()> {
    if std::env::args().any(|a| a == "--uninstall") {
        let purge = std::env::args().any(|a| a == "--purge");
        uninstall(purge)?;
        return Ok(());
    }
    let config_path = arg_config_path();
    let service_child = std::env::args().any(|a| a == csc_platform::elevated::CHILD_FLAG);

    let cfg = Config::load_or_create(&config_path).unwrap_or_else(|e| {
        eprintln!("config load failed, using defaults: {e:#}");
        Config::default()
    });
    notice::apply(&cfg.notify);
    notice::set_language(&cfg.gui.language);
    let log = LogControl::init(&cfg.log);
    match log.path() {
        Some(path) => info!("logging to {}", path.display()),
        None => eprintln!("not writing a log file"),
    }
    install_panic_hook(log.path().map(|p| p.to_path_buf()));
    let _lock = match single_instance_lock() {
        Ok(file) => Some(file),
        Err(e) => {
            eprintln!("single instance lock skipped: {e:#}");
            None
        }
    };

    info!("Cross-SystemConnect-core starting name={}", cfg.name);
    if service_child {
        info!(
            "elevated service mode: running as SYSTEM in the user's session, following the input desktop"
        );
        csc_platform::set_follow_input_desktop(true);
    }

    csc_platform::init_process();
    let screen_info = create_screen_info();
    let screen = screen_info.virtual_size();
    let monitors = screen_info.monitors();
    let work_areas = screen_info.work_areas();
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
    switch.set_work_areas(work_areas.clone());
    let hooked = csc_platform::capture_start();

    let status = AppStatus {
        connected: false,
        peer_name: String::new(),
        active_screen: cfg.name.clone(),
        being_controlled: false,
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
        pairing_code: String::new(),
        home_code: String::new(),
        home_code_prev: String::new(),
        home_code_until: 0,
        offer_code: String::new(),
        offer_until: 0,
        offer_gen: 0,
        home_code_inflight: String::new(),
        pair_attempts: 0,
        pairing_open_until: 0,
        peer_note: String::new(),
        host_state: "no_host".into(),
        discovered: Vec::new(),
        search_subnet: crate::discover::subnet_label(&cfg.search_address).unwrap_or_default(),
        transfers: Vec::new(),
        peer_latency_ms: Default::default(),
    };

    let (return_hotkey, lock_hotkey) = hotkeys_of(&cfg);
    let (stay_hotkey, pause_hotkey) = watch_local(&cfg);
    let config = Arc::new(RwLock::new(cfg));
    let status = Arc::new(RwLock::new(status));
    let (quit_tx, mut quit_rx) = watch::channel(false);
    let (net_reload, net_reload_rx) = watch::channel(0u64);
    let (layout_reload, layout_rx) = watch::channel(0u64);
    let prompt = Arc::new(PeerPrompt::default());
    let (transfer, transfer_rx) = crate::transfer::channel();
    let board = crate::progress::TransferBoard::default();
    let (paste_here_tx, mut paste_here_rx) = mpsc::channel::<()>(2);

    let state = Arc::new(AppState {
        transfer: transfer.clone(),
        board: board.clone(),
        paste_here: paste_here_tx,
        config: config.clone(),
        config_path: config_path.clone(),
        status: status.clone(),
        quit_tx,
        prompt: prompt.clone(),
        net_reload: net_reload.clone(),
        layout_reload: layout_reload.clone(),
        log,
        net: RwLock::new(None),
    });

    let (inbound_tx, mut inbound_rx) = mpsc::channel::<InboundEvent>(256);
    // Bind IPC before discovery. A second core must exit so it does not also take the ports.
    let ipc_bound = match claim_ipc() {
        Ok(bound) => bound,
        Err(err) if err.downcast_ref::<IpcBusy>().is_some() => {
            info!("core already running");
            // Inside the window process, exiting would close the window too.
            if std::thread::current().name() == Some("csc-core") {
                return Ok(());
            }
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
    let (host_name, ips, nics) = nics_of(host);
    {
        let mut cfg = config.write().await;
        // Prefer ethernet. A saved Clash fake-ip is not a real NIC.
        if ensure_search_address(&mut cfg, &nics) {
            if let Err(e) = cfg.save(&config_path) {
                warn!("save default search address: {e:#}");
            }
        }
        info!("host={} ips={:?}", host_name, ips);
    }
    let search = config.read().await.search_address.clone();
    let mut supervisor = Supervisor::default();
    {
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
        net_reload.clone(),
        layout_reload.clone(),
        inbound_tx,
        transfer.clone(),
    )
    .await?;
    *state.net.write().await = Some(net.clone());

    let (paste_tx, mut paste_rx) = mpsc::channel::<LocatedPaste>(4);
    let (staged_tx, mut staged_rx) = mpsc::channel::<StagedPaste>(4);
    let (clip_tx, mut clip_rx) = mpsc::channel::<ClipEvent>(16);
    let clip = crate::clipboard::spawn(clip_tx);
    tokio::spawn(watch_memory(state.clone(), board.clone()));
    tokio::spawn(watch_blocked_input());
    {
        let search = config.read().await.search_address.clone();
        let mut s = state.status.write().await;
        // Stable until the user clicks「更换」; pairing stays open while the code exists.
        // Last three digits = this computer’s IPv4 host octet (e.g. .243 → …243).
        let ip = home_code_ip(&search, &s.local_ipv4);
        s.home_code = fresh_home_code(&ip);
        s.home_code_until = 0;
        s.pairing_open_until = 0;
    }
    crate::transfer::spawn(transfer_rx, net.clone(), config.clone(), board);
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
        clip,
        net_reload,
        layout_rx,
        injector: create_input_injector(),
        screen_info,
        screen,
        monitors,
        work_areas,
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
        home_edge: None,
        held: HeldInput::default(),
        injected: HeldInput::default(),
        return_hotkey,
        lock_hotkey,
        stay_hotkey,
        pause_hotkey,
        stay_on: false,
        last_grab_hotkey: Instant::now(),
        paused_peers: HashSet::new(),
        greeted: HashSet::new(),
        sharing_announced: true,
        remap: Vec::new(),
        sync_locks: true,
        block_fullscreen: true,
        pointer_speed: 1.0,
        pointer_peers: HashMap::new(),
        carry_x: 0.0,
        carry_y: 0.0,
        virtual_locks: None,
        last_local_activity: Instant::now(),
        last_activity_sent: Instant::now() - Duration::from_secs(30),
        pending_clip: None,
        last_lock: Instant::now() - LOCK_REPEAT,
        last_miss: Instant::now() - Duration::from_secs(5),
        wayland_host_noted: false,
        sent_moves: csc_platform::RateStats::new("pointer moves sent"),
        next_nic: Instant::now(),
        supervisor,
        paste_gen: 0,
        paste_tx: paste_tx.clone(),
        staged_tx,
        staging: None,
        clip_seq,
        last_remote_clip: 0,
        shared: SharedClip::default(),
        my_files: None,
        remote_files: None,
        probe: EdgePush,
        last_real: None,
        commanded: None,
        last_capture_edges: None,
        last_edge_sent: Instant::now() - Duration::from_secs(5),
        power_rx: csc_platform::spawn_power_watch(),
    };
    let mut tick = tokio::time::interval(Duration::from_millis(1));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

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
            Some(found) = paste_rx.recv() => core.on_located_paste(found).await,
            Some(staged) = staged_rx.recv() => core.on_staged_paste(staged).await,
            Some(()) = paste_here_rx.recv() => core.paste_remote_here().await,
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

/// Home-page pairing code: `XXXYYY` where `XXX` is random and `YYY` is the last
/// octet of this computer’s LAN IPv4 (e.g. `192.168.1.243` → `…243`). Stable
/// until the user clicks「更换」; the previous code stays valid once.
fn fresh_home_code(local_ip: &str) -> String {
    let mut buf = [0u8; 2];
    let _ = getrandom::getrandom(&mut buf);
    let prefix = u16::from_le_bytes(buf) % 1000;
    let suffix = crate::discover::ipv4_octets(local_ip)
        .map(|o| u16::from(o[3]))
        .unwrap_or(0);
    format!("{prefix:03}{suffix:03}")
}

/// Keep the random prefix; refresh the IP suffix when the NIC changes.
pub(crate) fn sync_home_code_ip(s: &mut csc_proto::AppStatus, local_ip: &str) {
    let suffix = crate::discover::ipv4_octets(local_ip)
        .map(|o| u16::from(o[3]))
        .unwrap_or(0);
    let prefix: String = s
        .home_code
        .chars()
        .filter(|c| c.is_ascii_digit())
        .take(3)
        .collect();
    let prefix = if prefix.len() == 3 {
        prefix
    } else {
        let mut buf = [0u8; 2];
        let _ = getrandom::getrandom(&mut buf);
        format!("{:03}", u16::from_le_bytes(buf) % 1000)
    };
    let next = format!("{prefix}{suffix:03}");
    if next != s.home_code {
        s.home_code_prev = std::mem::take(&mut s.home_code);
        s.home_code = next;
        s.home_code_until = 0;
        s.pairing_open_until = 0;
    }
}

pub(crate) fn rotate_home(s: &mut csc_proto::AppStatus, local_ip: &str) {
    s.home_code_prev = std::mem::take(&mut s.home_code);
    s.home_code = fresh_home_code(local_ip);
    // 0 = no auto-expiry; previous code still accepted once via home_code_prev.
    s.home_code_until = 0;
    s.pairing_open_until = 0;
}

/// A home code pairs once. Keep the IP suffix, draw a new prefix, and drop the
/// grace slot so the used code stops matching immediately.
pub(crate) fn burn_home_code(s: &mut csc_proto::AppStatus) {
    if s.home_code.len() != 6 {
        return;
    }
    let suffix: String = s.home_code.chars().skip(3).collect();
    let mut next = s.home_code.clone();
    while next == s.home_code {
        let mut buf = [0u8; 2];
        let _ = getrandom::getrandom(&mut buf);
        next = format!("{:03}{suffix}", u16::from_le_bytes(buf) % 1000);
    }
    s.home_code = next;
    s.home_code_prev.clear();
    s.home_code_until = 0;
    s.pairing_open_until = 0;
}

/// Remember the current home code so an in-flight confirm can still match it.
pub(crate) fn begin_pair_attempt(s: &mut csc_proto::AppStatus) {
    if s.pair_attempts == 0 && s.home_code.len() == 6 {
        s.home_code_inflight = s.home_code.clone();
    }
    s.pair_attempts = s.pair_attempts.saturating_add(1);
}

/// After the attempt ends (ok, reject, cancel, mismatch, timeout, handshake
/// error), issue a new home code and drop the previous one so the next try
/// must use the new digits.
pub(crate) fn finish_pair_attempt(s: &mut csc_proto::AppStatus) {
    s.pair_attempts = s.pair_attempts.saturating_sub(1);
    if s.pair_attempts > 0 {
        return;
    }
    let start = std::mem::take(&mut s.home_code_inflight);
    if start.len() == 6 && s.home_code == start {
        burn_home_code(s);
    } else {
        s.home_code_prev.clear();
    }
}

/// Prefer `search_address`, else the first local IPv4 on status.
pub(crate) fn home_code_ip(search: &str, local_ipv4: &[String]) -> String {
    if crate::discover::ipv4_octets(search).is_some() {
        return search.to_string();
    }
    local_ipv4
        .iter()
        .find(|ip| crate::discover::ipv4_octets(ip).is_some())
        .cloned()
        .unwrap_or_default()
}

/// Resident-memory self-check every minute (`limits.rss_*`). Over the restart
/// limit with no transfer running, a standalone core quits and the UI starts it
/// again on its next request. The core inside the Windows window only warns.
async fn watch_memory(state: Arc<AppState>, board: crate::progress::TransferBoard) {
    let mut every = tokio::time::interval(Duration::from_secs(60));
    every.tick().await;
    loop {
        every.tick().await;
        let Some(mb) = csc_platform::resident_mb() else {
            return;
        };
        let limits = state.config.read().await.limits.clone();
        let warn_mb = u64::from(limits.rss_warn_mb);
        let restart_mb = u64::from(limits.rss_restart_mb);
        if restart_mb > 0 && mb > restart_mb {
            if cfg!(windows) || board.busy() {
                warn!("resident memory {mb} MiB is over {restart_mb} MiB; restart postponed");
                continue;
            }
            warn!("resident memory {mb} MiB is over {restart_mb} MiB; restarting the core");
            let _ = state.quit_tx.send(true);
            return;
        }
        if warn_mb > 0 && mb > warn_mb {
            warn!("resident memory {mb} MiB is over {warn_mb} MiB");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Hotkey;

    #[test]
    fn small_copies_are_fetched_ahead_big_ones_use_placeholders() {
        let file = |size: u64| csc_proto::FileEntry {
            relative_path: "a.bin".into(),
            size,
            mtime_unix: 0,
            is_directory: false,
            mode: 0,
        };
        assert!(super::prefetch_copied(&[file(48), file(10 << 20)]));
        assert!(super::prefetch_copied(&[file(super::PREFETCH_MAX_BYTES)]));
        assert!(!super::prefetch_copied(&[file(
            super::PREFETCH_MAX_BYTES + 1
        )]));
        assert!(!super::prefetch_copied(&[file(u64::MAX), file(1)]));
        assert!(!super::prefetch_copied(&[]));
    }
    use csc_config::Config;
    use csc_proto::LocalNic;
    use std::collections::HashSet;

    #[test]
    fn used_home_code_is_replaced_and_keeps_ip_suffix() {
        let mut s = csc_proto::AppStatus {
            home_code: "582243".into(),
            home_code_prev: "111243".into(),
            ..Default::default()
        };
        super::burn_home_code(&mut s);
        assert_ne!(s.home_code, "582243");
        assert!(s.home_code.ends_with("243"));
        assert_eq!(s.home_code.len(), 6);
        assert!(s.home_code_prev.is_empty());
    }

    #[test]
    fn pair_attempt_keeps_start_code_then_rotates() {
        let mut s = csc_proto::AppStatus {
            home_code: "582243".into(),
            home_code_prev: "111243".into(),
            ..Default::default()
        };
        super::begin_pair_attempt(&mut s);
        assert_eq!(s.home_code_inflight, "582243");
        assert_eq!(s.pair_attempts, 1);
        // A second overlapping attempt still honors the first code.
        super::begin_pair_attempt(&mut s);
        assert_eq!(s.home_code, "582243");
        assert_eq!(s.pair_attempts, 2);
        super::finish_pair_attempt(&mut s);
        assert_eq!(s.home_code, "582243");
        assert_eq!(s.pair_attempts, 1);
        super::finish_pair_attempt(&mut s);
        assert_ne!(s.home_code, "582243");
        assert!(s.home_code.ends_with("243"));
        assert!(s.home_code_prev.is_empty());
        assert!(s.home_code_inflight.is_empty());
        assert_eq!(s.pair_attempts, 0);
    }

    #[test]
    fn finish_after_success_burn_does_not_rotate_twice() {
        let mut s = csc_proto::AppStatus {
            home_code: "582243".into(),
            ..Default::default()
        };
        super::begin_pair_attempt(&mut s);
        super::burn_home_code(&mut s);
        let after_burn = s.home_code.clone();
        super::finish_pair_attempt(&mut s);
        assert_eq!(s.home_code, after_burn);
        assert!(s.home_code_prev.is_empty());
    }

    #[test]
    fn reentry_stays_on_the_edge_that_was_crossed() {
        let m = csc_platform::Rect {
            left: 0,
            top: 0,
            right: 1920,
            bottom: 1080,
        };
        // Leaving the remote through its left edge returns on this monitor's right edge.
        let (x, y) = super::reentry_point(csc_proto::Edge::Left, 0.25, m);
        assert_eq!(x, 1918);
        assert_eq!(y, 270);
        // Leaving through the remote's bottom returns on this monitor's top edge.
        let (x, y) = super::reentry_point(csc_proto::Edge::Bottom, 0.5, m);
        assert_eq!(y, 1);
        assert_eq!(x, 960);
    }

    #[test]
    fn secure_desktop_releases_the_pointer_like_a_lock() {
        assert!(super::local_lock_releases("locked"));
        assert!(super::local_lock_releases("secure_desktop"));
        assert!(!super::local_lock_releases("normal"));
    }

    #[test]
    fn search_address_prefers_ethernet_when_unset() {
        let nics = vec![
            LocalNic {
                address: "40.40.105.188".into(),
                kind: "wifi".into(),
                name: "wlp".into(),
            },
            LocalNic {
                address: "10.10.10.9".into(),
                kind: "ethernet".into(),
                name: "enp".into(),
            },
        ];
        assert_eq!(
            super::preferred_search(&nics).as_deref(),
            Some("10.10.10.9")
        );
        let mut cfg = Config {
            search_address: String::new(),
            ..Default::default()
        };
        assert!(super::ensure_search_address(&mut cfg, &nics));
        assert_eq!(cfg.search_address, "10.10.10.9");
        assert!(!super::ensure_search_address(&mut cfg, &nics));
        cfg.search_address = "40.40.105.188".into();
        // A valid wifi selection is kept — only empty / unknown are replaced.
        assert!(!super::ensure_search_address(&mut cfg, &nics));
        cfg.search_address = "1.2.3.4".into();
        assert!(super::ensure_search_address(&mut cfg, &nics));
        assert_eq!(cfg.search_address, "10.10.10.9");
    }

    #[test]
    fn return_hotkey_matches_either_hand() {
        let hk = Hotkey::parse("Ctrl+Alt+End").expect("parse");
        let end = 0x4D;
        let held: HashSet<u32> = [0xE4, 0xE2].into_iter().collect();
        assert!(hk.completes(end, &held));
        let only_ctrl: HashSet<u32> = [0xE0].into_iter().collect();
        assert!(!hk.completes(end, &only_ctrl));
        assert!(!hk.completes(0x4A, &held));
        assert!(Hotkey::parse("Ctrl+Nope").is_none());
    }

    #[test]
    fn default_hotkeys_parse() {
        let back = Hotkey::parse("Ctrl+Shift+/").expect("parse");
        let ctrl_shift: HashSet<u32> = [0xE0, 0xE1].into_iter().collect();
        assert!(back.completes(0x38, &ctrl_shift));
        let lock = Hotkey::parse("Win+L").expect("parse");
        let right_win: HashSet<u32> = [0xE7].into_iter().collect();
        assert!(lock.completes(0x0F, &right_win));
        assert!(!lock.completes(0x0F, &ctrl_shift));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn wayland_pin_covers_the_desktop() {
        let d = super::wayland_pin_delta(csc_platform::ScreenSize {
            width: 1920,
            height: 1080,
        });
        assert!(d.0 <= -3840);
        assert!(d.1 <= -2160);
        let small = super::wayland_pin_delta(csc_platform::ScreenSize {
            width: 800,
            height: 600,
        });
        assert_eq!(small, (-4096, -4096));
    }

    #[test]
    fn rejected_warp_does_not_pin_the_pointer() {
        let logical = (10, 400);
        let stale = (1800, 900);
        assert_eq!(super::follow_real_pointer(logical, stale, false), logical);
        assert_eq!(
            super::follow_real_pointer(logical, (12, 400), true),
            (12, 400)
        );
    }
}
