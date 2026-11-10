//! Cross-SystemConnect configuration load/save.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use tracing::{info, warn};

pub mod logfile;

/// Product data directory name (user-facing).
pub const APP_DIR_NAME: &str = "Cross-SystemConnect";
/// Legacy directory used by older builds.
pub const LEGACY_APP_DIR_NAME: &str = "CSC";

/// Default is client. A saved config may override this to server (keyboard host).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    Server,
    #[default]
    Client,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SwitchConfig {
    #[serde(default)]
    pub edge_delay_ms: i32,
    #[serde(default)]
    pub double_tap_edge: bool,
    #[serde(default = "default_double_tap_window")]
    pub double_tap_window_ms: i32,
    #[serde(default = "default_corner")]
    pub corner_dead_zone_px: i32,
    #[serde(default)]
    pub relative_mouse_moves: bool,
    /// Length along an outer side, from a corner, that switches to a diagonal
    /// neighbor. A single corner pixel is too hard to hit.
    #[serde(default = "default_diagonal_zone")]
    pub diagonal_zone_px: i32,
    /// After the pointer reaches an edge, how far it must keep pushing outward
    /// before it switches. 0 switches on contact.
    #[serde(default = "default_edge_push")]
    pub edge_push_px: i32,
    /// `none`, `ctrl`, `shift`, or `alt`. Holding it stops edge switching.
    #[serde(default = "default_block_modifier")]
    pub block_modifier: String,
    /// Do not switch while the foreground window is fullscreen.
    #[serde(default = "default_true")]
    pub block_when_fullscreen: bool,
}

fn default_edge_push() -> i32 {
    8
}
fn default_block_modifier() -> String {
    "none".into()
}
fn default_double_tap_window() -> i32 {
    250
}
fn default_corner() -> i32 {
    10
}
fn default_diagonal_zone() -> i32 {
    80
}

impl Default for SwitchConfig {
    fn default() -> Self {
        Self {
            edge_delay_ms: 0,
            double_tap_edge: false,
            double_tap_window_ms: 250,
            corner_dead_zone_px: 10,
            relative_mouse_moves: false,
            diagonal_zone_px: default_diagonal_zone(),
            edge_push_px: default_edge_push(),
            block_modifier: default_block_modifier(),
            block_when_fullscreen: true,
        }
    }
}

/// Largest copied text sent to other computers. One frame holds at most 16 MiB,
/// and JSON escaping makes text longer, so 15 MB is the ceiling.
pub const MAX_INLINE_MB: i32 = 15;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClipboardConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default = "default_true")]
    pub text: bool,
    #[serde(default = "default_true")]
    pub image: bool,
    /// Copied files can be pasted on another computer. Nothing is sent until
    /// Ctrl+V there; the files then go to its receive folder.
    #[serde(default = "default_true")]
    pub files: bool,
    #[serde(default = "default_inline")]
    pub max_inline_mb: i32,
    /// `immediate` = offer on every change; `on_switch` = only when entering/leaving a peer.
    #[serde(default = "default_push_mode")]
    pub push_mode: String,
}

fn default_true() -> bool {
    true
}
fn default_inline() -> i32 {
    MAX_INLINE_MB
}
fn default_push_mode() -> String {
    "immediate".into()
}

impl Default for ClipboardConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            text: true,
            image: true,
            files: true,
            max_inline_mb: MAX_INLINE_MB,
            push_mode: default_push_mode(),
        }
    }
}

impl ClipboardConfig {
    /// Text limit in bytes, within 1..=MAX_INLINE_MB.
    pub fn max_inline_bytes(&self) -> usize {
        self.max_inline_mb.clamp(1, MAX_INLINE_MB) as usize * 1024 * 1024
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileTransferConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub receive_dir: String,
    #[serde(default = "default_ft_max")]
    pub max_size_mb: i32,
    #[serde(default)]
    pub limit_speed: bool,
    #[serde(default = "default_speed")]
    pub max_speed_mibs: i32,
    /// Minutes to keep a partial file after the link drops. 0 disables resume.
    #[serde(default = "default_resume_min")]
    pub resume_window_min: u32,
    /// Linux to Linux: keep permission bits in 0o777.
    #[serde(default = "default_true")]
    pub preserve_mode: bool,
    /// Keep the source modification time when receiving.
    #[serde(default = "default_true")]
    pub preserve_mtime: bool,
    /// Drop executable bits when the file lands on the desktop.
    #[serde(default = "default_true")]
    pub strip_exec_on_desktop: bool,
    /// Windows: write a Zone.Identifier stream so the file is “from another computer”.
    #[serde(default = "default_true")]
    pub mark_of_web: bool,
}

fn default_resume_min() -> u32 {
    30
}

fn default_ft_max() -> i32 {
    10240
}
fn default_speed() -> i32 {
    4
}

impl Default for FileTransferConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            receive_dir: String::new(),
            max_size_mb: 10240,
            limit_speed: false,
            max_speed_mibs: 4,
            resume_window_min: default_resume_min(),
            preserve_mode: true,
            preserve_mtime: true,
            strip_exec_on_desktop: true,
            mark_of_web: true,
        }
    }
}

/// One physical key forwarded as another. Usages are HID keyboard usages.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KeyRemap {
    pub from: u32,
    pub to: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KeyboardConfig {
    #[serde(default = "default_true")]
    pub sync_locks: bool,
    #[serde(default)]
    pub remap: Vec<KeyRemap>,
}

impl Default for KeyboardConfig {
    fn default() -> Self {
        Self {
            sync_locks: true,
            remap: Vec::new(),
        }
    }
}

/// Inclusive range for pointer speed (default 1.0, which matches this computer).
pub const POINTER_SPEED_MIN: f32 = 0.4;
pub const POINTER_SPEED_MAX: f32 = 4.0;

fn default_pointer_speed() -> f32 {
    1.0
}

/// How fast the pointer moves on another computer. 1.0 matches this computer.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PointerConfig {
    #[serde(default = "default_pointer_speed")]
    pub speed: f32,
    /// Per-computer override, keyed by the other computer's name.
    #[serde(default)]
    pub peers: BTreeMap<String, f32>,
    /// Scale moves by the ratio of display scales so physical speed matches.
    #[serde(default = "default_true")]
    pub match_physical_speed: bool,
    /// Hide this computer's cursor while controlling another one.
    #[serde(default = "default_true")]
    pub hide_host_cursor: bool,
}

impl Default for PointerConfig {
    fn default() -> Self {
        Self {
            speed: 1.0,
            peers: BTreeMap::new(),
            match_physical_speed: true,
            hide_host_cursor: true,
        }
    }
}

fn default_rss_warn_mb() -> u32 {
    200
}
fn default_rss_restart_mb() -> u32 {
    500
}

/// Resident-memory self-check of the core process. 0 turns a check off.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LimitsConfig {
    #[serde(default = "default_rss_warn_mb")]
    pub rss_warn_mb: u32,
    /// Above this, the core exits when no transfer runs and the UI restarts it.
    #[serde(default = "default_rss_restart_mb")]
    pub rss_restart_mb: u32,
}

impl Default for LimitsConfig {
    fn default() -> Self {
        Self {
            rss_warn_mb: default_rss_warn_mb(),
            rss_restart_mb: default_rss_restart_mb(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HotkeyConfig {
    /// Locks this computer and every connected one.
    #[serde(default = "default_lock")]
    pub lock_screen: String,
    #[serde(default = "default_return")]
    pub return_to_server: String,
    /// Hold the pointer on the screen it is already on.
    #[serde(default = "default_lock_to_screen")]
    pub lock_to_screen: String,
    /// Local keyboard on a controlled computer pauses sharing.
    #[serde(default = "default_emergency_pause")]
    pub emergency_pause: String,
}

fn default_lock() -> String {
    "Ctrl+Shift+L".into()
}
fn default_lock_to_screen() -> String {
    "Ctrl+Shift+K".into()
}
fn default_return() -> String {
    "Ctrl+Shift+/".into()
}
fn default_emergency_pause() -> String {
    "Ctrl+Shift+P".into()
}
/// Keys of earlier versions, replaced wherever they are still found.
const RETIRED_RETURN_KEYS: &[&str] = &["Ctrl+Alt+Home", "Ctrl+Alt+/"];

/// `Win+L`, `Ctrl+Alt+L`, `ScrollLock`, or empty — replaced once when moving to v5.
fn retired_lock_key(key: &str) -> bool {
    let k = key.replace(' ', "");
    k.is_empty()
        || k.eq_ignore_ascii_case("Win+L")
        || k.eq_ignore_ascii_case("Ctrl+Alt+L")
        || k.eq_ignore_ascii_case("ScrollLock")
}

fn new_device_id() -> String {
    let mut b = [0u8; 16];
    let _ = getrandom::getrandom(&mut b);
    b[6] = (b[6] & 0x0f) | 0x40;
    b[8] = (b[8] & 0x3f) | 0x80;
    format!(
        "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7], b[8], b[9], b[10], b[11], b[12], b[13],
        b[14], b[15]
    )
}

impl Default for HotkeyConfig {
    fn default() -> Self {
        Self {
            lock_screen: default_lock(),
            return_to_server: default_return(),
            lock_to_screen: default_lock_to_screen(),
            emergency_pause: default_emergency_pause(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GuiConfig {
    #[serde(default = "default_theme")]
    pub theme: String,
    #[serde(default)]
    pub launch_at_login: bool,
    #[serde(default)]
    pub prevent_sleep: bool,
    #[serde(default = "default_true")]
    pub auto_hide: bool,
    #[serde(default = "default_true")]
    pub close_to_tray: bool,
    /// `zh-CN` (default), `en-US`, or `auto`.
    #[serde(default = "default_language")]
    pub language: String,
    /// False only on a config this program just created.
    #[serde(default = "default_true")]
    pub wizard_done: bool,
    /// Ask GitHub Releases for a newer version when the settings window starts.
    #[serde(default)]
    pub check_updates: bool,
}

fn default_language() -> String {
    "zh-CN".into()
}

fn default_theme() -> String {
    "dark".into()
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DiagnosticsConfig {
    /// Accept BENCH_SYNC / BENCH_ECHO only when this is on.
    #[serde(default)]
    pub bench_enabled: bool,
}

/// Which desktop notices the core may show. Pairing always opens its own window.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NotifyConfig {
    /// A computer connected. Off unless the user asks.
    #[serde(default)]
    pub connect: bool,
    /// A computer disconnected, or the keyboard host is about to sleep.
    #[serde(default = "default_true")]
    pub disconnect: bool,
    /// A transfer finished, failed, or had nowhere to land.
    #[serde(default = "default_true")]
    pub transfer: bool,
    /// Clipboard and file errors.
    #[serde(default = "default_true")]
    pub errors: bool,
}

impl Default for NotifyConfig {
    fn default() -> Self {
        Self {
            connect: false,
            disconnect: true,
            transfer: true,
            errors: true,
        }
    }
}

impl Default for GuiConfig {
    fn default() -> Self {
        Self {
            theme: default_theme(),
            launch_at_login: false,
            prevent_sleep: false,
            auto_hide: true,
            close_to_tray: true,
            language: default_language(),
            wizard_done: false,
            check_updates: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LogConfig {
    #[serde(default = "default_log_level")]
    pub level: String,
    #[serde(default = "default_true")]
    pub to_file: bool,
    /// Empty: `log.txt` in the program folder (see `default_log_path`).
    #[serde(default)]
    pub file: String,
    /// Rotate by rewriting the file after this many kilobytes. 0 means the default.
    #[serde(default = "default_max_kb")]
    pub max_kb: u32,
}

fn default_max_kb() -> u32 {
    1024
}

fn default_log_level() -> String {
    "info".into()
}

impl Default for LogConfig {
    fn default() -> Self {
        Self {
            level: default_log_level(),
            to_file: true,
            file: String::new(),
            max_kb: default_max_kb(),
        }
    }
}

impl LogConfig {
    /// File to write: the chosen one, or `log.txt` in the program folder.
    pub fn path(&self) -> PathBuf {
        let chosen = self.file.trim();
        if chosen.is_empty() {
            default_log_path()
        } else {
            PathBuf::from(chosen)
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NetworkExtras {
    #[serde(default = "default_true")]
    pub heartbeat_enabled: bool,
    #[serde(default = "default_hb")]
    pub heartbeat_ms: i32,
    /// UDP discovery port. 0 means “not in the file yet”; migration copies `control_port`.
    #[serde(default)]
    pub discovery_port: u16,
    /// Addresses dialed in addition to discovery. Empty port uses the control port.
    #[serde(default)]
    pub manual_peers: Vec<ManualPeer>,
}

/// A computer to dial even when it is outside the discovery subnet.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ManualPeer {
    pub address: String,
    #[serde(default)]
    pub control_port: u16,
}

fn default_hb() -> i32 {
    5000
}

impl Default for NetworkExtras {
    fn default() -> Self {
        Self {
            heartbeat_enabled: true,
            heartbeat_ms: 5000,
            // 0 means the file never had this field. Migration copies control_port.
            discovery_port: 0,
            manual_peers: Vec::new(),
        }
    }
}

/// Keep other computers awake while someone is using this set of machines.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PowerConfig {
    #[serde(default = "default_true")]
    pub keep_peers_awake: bool,
}

impl Default for PowerConfig {
    fn default() -> Self {
        Self {
            keep_peers_awake: true,
        }
    }
}

/// layout[host][edge] = neighbor
pub type LayoutMap = BTreeMap<String, BTreeMap<String, String>>;

/// One screen block. `x`/`y` are offsets from the keyboard host at (0,0); up is negative `y`.
/// Values are usually ring slots (−1…1); farther peers may stack as multiples of a slot.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ScreenPlace {
    pub name: String,
    #[serde(default)]
    pub x: i32,
    #[serde(default)]
    pub y: i32,
}

/// Ring-1 empty slots around the host (right, left, bottom, top, then corners).
pub const SCREEN_SLOTS: [(i32, i32); 8] = [
    (1, 0),
    (-1, 0),
    (0, 1),
    (0, -1),
    (1, 1),
    (-1, 1),
    (1, -1),
    (-1, -1),
];

/// Which edge of the host a place sits on (same rules as the old desk UI).
pub fn sector_of(x: i32, y: i32) -> &'static str {
    let ax = x.unsigned_abs();
    let ay = y.unsigned_abs();
    let horiz = ax as f32 >= ay as f32 * 0.55;
    let vert = ay as f32 >= ax as f32 * 0.55;
    if horiz && vert {
        if y < 0 {
            if x >= 0 {
                "top_right"
            } else {
                "top_left"
            }
        } else if x >= 0 {
            "bottom_right"
        } else {
            "bottom_left"
        }
    } else if ax >= ay {
        if x >= 0 {
            "right"
        } else {
            "left"
        }
    } else if y >= 0 {
        "bottom"
    } else {
        "top"
    }
}

fn opposite_edge(edge: &str) -> &'static str {
    match edge {
        "right" => "left",
        "left" => "right",
        "top" => "bottom",
        "bottom" => "top",
        "top_right" => "bottom_left",
        "top_left" => "bottom_right",
        "bottom_right" => "top_left",
        "bottom_left" => "top_right",
        _ => "left",
    }
}

/// Nearest free ring slot for `name`. Occupied slots (other names) are skipped.
pub fn nearest_screen_slot(x: i32, y: i32, name: &str, places: &[ScreenPlace]) -> (i32, i32) {
    let taken: std::collections::HashSet<(i32, i32)> = places
        .iter()
        .filter(|p| !p.name.eq_ignore_ascii_case(name))
        .map(|p| (p.x, p.y))
        .collect();
    let mut best = None;
    let mut best_d = i64::MAX;
    for &(sx, sy) in &SCREEN_SLOTS {
        if taken.contains(&(sx, sy)) {
            continue;
        }
        let d = i64::from(sx - x).pow(2) + i64::from(sy - y).pow(2);
        if d < best_d {
            best_d = d;
            best = Some((sx, sy));
        }
    }
    if let Some(slot) = best {
        return slot;
    }
    let mut near = SCREEN_SLOTS[0];
    let mut nd = i64::MAX;
    for &(sx, sy) in &SCREEN_SLOTS {
        let d = i64::from(sx - x).pow(2) + i64::from(sy - y).pow(2);
        if d < nd {
            nd = d;
            near = (sx, sy);
        }
    }
    let side = sector_of(near.0, near.1);
    let n = places
        .iter()
        .filter(|p| !p.name.eq_ignore_ascii_case(name) && sector_of(p.x, p.y) == side)
        .count() as i32;
    (near.0 * (n + 1), near.1 * (n + 1))
}

/// Version written by this build. Files without the field are version 1.
pub const CONFIG_VERSION: u32 = 5;

fn default_config_version() -> u32 {
    1
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    #[serde(default = "default_config_version")]
    pub config_version: u32,
    #[serde(default)]
    pub role: Role,
    #[serde(default)]
    pub name: String,
    /// Stable identity. Generated once; not edited from the settings page.
    #[serde(default)]
    pub device_id: String,
    #[serde(default = "default_bind")]
    pub bind_address: String,
    /// Local IPv4 chosen for discovery. Search stays on that /24 (for example 10.10.10.10 → 10.10.10.0/24).
    #[serde(default)]
    pub search_address: String,
    #[serde(default)]
    pub server_address: String,
    #[serde(default = "default_control_port")]
    pub control_port: u16,
    #[serde(default = "default_data_port")]
    pub data_port: u16,
    /// Kept so older config files still load. The control channel is always TLS.
    #[serde(default)]
    pub tls: bool,
    #[serde(default)]
    pub layout: LayoutMap,
    /// Drag-arranged screen blocks. Empty means “derive from layout”.
    #[serde(default)]
    pub screens: Vec<ScreenPlace>,
    #[serde(default, rename = "switch")]
    pub switch_cfg: SwitchConfig,
    #[serde(default)]
    pub clipboard: ClipboardConfig,
    #[serde(default)]
    pub file_transfer: FileTransferConfig,
    #[serde(default)]
    pub hotkeys: HotkeyConfig,
    #[serde(default)]
    pub gui: GuiConfig,
    #[serde(default)]
    pub diagnostics: DiagnosticsConfig,
    #[serde(default)]
    pub notify: NotifyConfig,
    #[serde(default)]
    pub power: PowerConfig,
    #[serde(default)]
    pub keyboard: KeyboardConfig,
    #[serde(default)]
    pub pointer: PointerConfig,
    #[serde(default)]
    pub log: LogConfig,
    #[serde(default)]
    pub network: NetworkExtras,
    #[serde(default)]
    pub limits: LimitsConfig,
    #[serde(default)]
    pub enable_enter_command: bool,
    #[serde(default)]
    pub enable_exit_command: bool,
    #[serde(default)]
    pub enter_command: String,
    #[serde(default)]
    pub exit_command: String,
    #[serde(default = "default_log_level")]
    pub log_level: String,
    /// Computer names that may connect again without a prompt.
    #[serde(default)]
    pub trusted_peers: Vec<String>,
    /// SHA-256 of a peer certificate, keyed by that computer's name.
    #[serde(default)]
    pub peer_fingerprints: BTreeMap<String, String>,
}

fn default_bind() -> String {
    "0.0.0.0".into()
}
fn default_control_port() -> u16 {
    45277
}
fn default_data_port() -> u16 {
    45278
}
fn default_discovery_port() -> u16 {
    45433
}

impl Default for Config {
    fn default() -> Self {
        Self {
            config_version: CONFIG_VERSION,
            role: Role::Client,
            name: local_computer_name(),
            device_id: new_device_id(),
            bind_address: default_bind(),
            search_address: String::new(),
            server_address: String::new(),
            control_port: default_control_port(),
            data_port: default_data_port(),
            tls: false,
            layout: LayoutMap::new(),
            screens: Vec::new(),
            switch_cfg: SwitchConfig::default(),
            clipboard: ClipboardConfig::default(),
            file_transfer: FileTransferConfig::default(),
            hotkeys: HotkeyConfig::default(),
            gui: GuiConfig::default(),
            diagnostics: DiagnosticsConfig::default(),
            notify: NotifyConfig::default(),
            power: PowerConfig::default(),
            keyboard: KeyboardConfig::default(),
            pointer: PointerConfig::default(),
            log: LogConfig::default(),
            network: NetworkExtras {
                discovery_port: default_discovery_port(),
                ..NetworkExtras::default()
            },
            limits: LimitsConfig::default(),
            enable_enter_command: false,
            enable_exit_command: false,
            enter_command: String::new(),
            exit_command: String::new(),
            log_level: default_log_level(),
            trusted_peers: Vec::new(),
            peer_fingerprints: BTreeMap::new(),
        }
    }
}

pub fn local_computer_name() -> String {
    read_os_hostname().unwrap_or_else(|| "PC".into())
}

/// OS hostname. Linux desktop sessions often have no `HOSTNAME` variable, so
/// read the kernel name before falling back to the `hostname` crate or the environment.
fn read_os_hostname() -> Option<String> {
    #[cfg(unix)]
    {
        if let Some(name) = hostname_from_file("/proc/sys/kernel/hostname") {
            return Some(name);
        }
        if let Some(name) = hostname_from_file("/etc/hostname") {
            return Some(name);
        }
    }
    if let Some(name) = hostname::get()
        .ok()
        .and_then(|h| h.into_string().ok())
        .and_then(|text| normalize_hostname(&text))
    {
        return Some(name);
    }
    for key in ["COMPUTERNAME", "HOSTNAME", "HOST"] {
        if let Ok(value) = std::env::var(key) {
            if let Some(name) = normalize_hostname(&value) {
                return Some(name);
            }
        }
    }
    None
}

#[cfg(unix)]
fn hostname_from_file(path: &str) -> Option<String> {
    let text = std::fs::read_to_string(path).ok()?;
    normalize_hostname(&text)
}

fn normalize_hostname(raw: &str) -> Option<String> {
    let name = raw.split(['\n', '\r']).next()?.trim();
    if name.is_empty() || name == "(none)" {
        None
    } else {
        Some(name.to_string())
    }
}

/// `PC` and `LOCAL-PC` are unread fallbacks, not names the user chose.
fn name_needs_os_hostname(name: &str) -> bool {
    name.is_empty() || name == "PC" || name == "LOCAL-PC"
}

fn app_config_dir() -> PathBuf {
    #[cfg(windows)]
    {
        dirs::config_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join(APP_DIR_NAME)
    }
    #[cfg(not(windows))]
    {
        dirs::config_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("cross-systemconnect")
    }
}

fn legacy_config_dir() -> PathBuf {
    #[cfg(windows)]
    {
        dirs::config_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join(LEGACY_APP_DIR_NAME)
    }
    #[cfg(not(windows))]
    {
        dirs::config_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("csc")
    }
}

/// Prefer new app dir; if missing but legacy CSC config exists, migrate once.
pub fn default_config_path() -> PathBuf {
    let modern = app_config_dir().join("config.json");
    if modern.exists() {
        return modern;
    }
    let legacy = legacy_config_dir().join("config.json");
    if legacy.exists() {
        if let Err(e) = fs::create_dir_all(app_config_dir()) {
            warn!("create config dir: {e}");
            return legacy;
        }
        match fs::copy(&legacy, &modern) {
            Ok(_) => {
                info!(
                    "Migrated config {} → {}",
                    legacy.display(),
                    modern.display()
                );
                return modern;
            }
            Err(e) => {
                warn!("config migrate failed: {e}; using legacy path");
                return legacy;
            }
        }
    }
    modern
}

/// PID file the tray uses to stop the kernel when the quit message is missed.
pub fn core_lock_path() -> PathBuf {
    app_config_dir().join("Cross-SystemConnect-core.lock")
}

/// `log.txt` next to the running program.
pub fn default_log_path() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|dir| dir.join("log.txt")))
        .unwrap_or_else(fallback_log_path)
}

/// Used when the program folder cannot be written, for example an installed
/// program under Program Files or /usr.
pub fn fallback_log_path() -> PathBuf {
    #[cfg(windows)]
    {
        app_config_dir().join("Cross-SystemConnect.log")
    }
    #[cfg(not(windows))]
    {
        dirs::state_dir()
            .or_else(dirs::data_local_dir)
            .unwrap_or_else(|| PathBuf::from("."))
            .join("cross-systemconnect")
            .join("Cross-SystemConnect.log")
    }
}

fn rename_layout_host(layout: &mut LayoutMap, from: &str, to: &str) {
    if from == to || from.is_empty() {
        return;
    }
    let mut next = LayoutMap::new();
    for (host, edges) in layout.iter() {
        let hk = if host == from { to } else { host.as_str() };
        let mut e2 = BTreeMap::new();
        for (edge, neighbor) in edges {
            let n = if neighbor == from {
                to
            } else {
                neighbor.as_str()
            };
            e2.insert(edge.clone(), n.to_string());
        }
        next.insert(hk.to_string(), e2);
    }
    *layout = next;
}

impl Config {
    pub fn load_or_create(path: &Path) -> Result<Self> {
        if !path.exists() {
            let cfg = Config::default();
            cfg.save(path)?;
            info!("Created default config at {}", path.display());
            return Ok(cfg);
        }
        let text =
            fs::read_to_string(path).with_context(|| format!("read config {}", path.display()))?;
        let mut cfg: Config = serde_json::from_str(&text)
            .with_context(|| format!("parse config {}", path.display()))?;
        if name_needs_os_hostname(&cfg.name) {
            let host = local_computer_name();
            if !host.is_empty() && host != cfg.name {
                let previous = cfg.name.clone();
                rename_layout_host(&mut cfg.layout, &previous, &host);
                for screen in &mut cfg.screens {
                    if screen.name == previous {
                        screen.name = host.clone();
                    }
                }
                cfg.name = host.clone();
                info!("Migrated config name {previous} → {host}");
                if let Err(e) = cfg.save(path) {
                    warn!("Failed to persist migrated config: {e}");
                }
            }
        }
        if cfg.log.level.is_empty() {
            cfg.log.level = cfg.log_level.clone();
        }
        let upgraded = cfg.upgrade();
        if upgraded {
            info!("Upgraded config to version {CONFIG_VERSION}");
        }
        let retired = cfg.retire_old_hotkeys();
        if retired {
            info!(
                "Hotkeys set to return {} and lock {}",
                cfg.hotkeys.return_to_server, cfg.hotkeys.lock_screen
            );
        }
        let fingerprints_trusted = cfg.ensure_fingerprints_trusted();
        if upgraded || retired || fingerprints_trusted {
            if let Err(e) = cfg.save(path) {
                warn!("Failed to persist upgraded config: {e}");
            }
        }
        cfg.clamp_limits();
        Ok(cfg)
    }

    /// One-time changes for files written by older builds. True when something changed.
    fn upgrade(&mut self) -> bool {
        if self.config_version >= CONFIG_VERSION {
            return false;
        }
        if self.config_version < 2 {
            // Version 2 raised the copied-text limit to the maximum for everyone.
            self.clipboard.max_inline_mb = MAX_INLINE_MB;
        }
        if self.config_version < 4 {
            // Version 4: the log is always written to a file, at level info, unless changed later.
            self.log.to_file = true;
            self.log.level = default_log_level();
            self.log_level = default_log_level();
        }
        if self.config_version < 5 {
            // Version 5: name-keyed fingerprints are not trusted. Pair again.
            self.peer_fingerprints.clear();
            if self.device_id.is_empty() {
                self.device_id = new_device_id();
            }
            if self.log.max_kb == 0 {
                self.log.max_kb = default_max_kb();
            }
            if self.network.discovery_port == 0 {
                self.network.discovery_port = self.control_port;
            }
            if retired_lock_key(&self.hotkeys.lock_screen) {
                self.hotkeys.lock_screen = default_lock();
            }
            if self
                .hotkeys
                .lock_to_screen
                .replace(' ', "")
                .eq_ignore_ascii_case("Ctrl+Alt+K")
            {
                self.hotkeys.lock_to_screen = default_lock_to_screen();
            }
        }
        self.config_version = CONFIG_VERSION;
        true
    }

    /// The old keys are gone for good, not just as defaults, and an empty key
    /// would leave the action without one. True when a key was replaced.
    fn retire_old_hotkeys(&mut self) -> bool {
        let retire = |key: &mut String, old: &[&str], new: fn() -> String| {
            let k = key.replace(' ', "");
            let stale = k.is_empty() || old.iter().any(|o| k.eq_ignore_ascii_case(o));
            if stale {
                *key = new();
            }
            stale
        };
        let back = retire(
            &mut self.hotkeys.return_to_server,
            RETIRED_RETURN_KEYS,
            default_return,
        );
        let lock_empty = self.hotkeys.lock_screen.trim().is_empty();
        if lock_empty {
            self.hotkeys.lock_screen = default_lock();
        }
        let hold_empty = self.hotkeys.lock_to_screen.trim().is_empty();
        if hold_empty {
            self.hotkeys.lock_to_screen = default_lock_to_screen();
        }
        if self.hotkeys.emergency_pause.trim().is_empty() {
            self.hotkeys.emergency_pause = default_emergency_pause();
        }
        let block = self.switch_cfg.block_modifier.trim().to_ascii_lowercase();
        self.switch_cfg.block_modifier = match block.as_str() {
            "ctrl" | "shift" | "alt" => block,
            _ => "none".into(),
        };
        back || lock_empty || hold_empty
    }

    fn clamp_limits(&mut self) {
        self.clipboard.max_inline_mb = self.clipboard.max_inline_mb.clamp(1, MAX_INLINE_MB);
        if self.clipboard.push_mode != "on_switch" {
            self.clipboard.push_mode = "immediate".into();
        }
        self.switch_cfg.edge_push_px = self.switch_cfg.edge_push_px.clamp(0, 200);
        self.pointer.speed = self
            .pointer
            .speed
            .clamp(POINTER_SPEED_MIN, POINTER_SPEED_MAX);
        for speed in self.pointer.peers.values_mut() {
            *speed = speed.clamp(POINTER_SPEED_MIN, POINTER_SPEED_MAX);
        }
        self.retire_old_hotkeys();
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let text = serde_json::to_string_pretty(self)?;
        fs::write(path, text + "\n")?;
        Ok(())
    }

    /// Replace only the top-level keys present in `v`.
    /// `trusted_peers` is never taken from the UI patch.
    pub fn merge_from_value(&mut self, v: serde_json::Value) -> Result<()> {
        let mut current = serde_json::to_value(&*self)?;
        if let (Some(base), Some(patch)) = (current.as_object_mut(), v.as_object()) {
            for (k, val) in patch {
                if k == "trusted_peers"
                    || k == "peer_fingerprints"
                    || k == "config_version"
                    || k == "device_id"
                {
                    continue;
                }
                base.insert(k.clone(), val.clone());
            }
        }
        *self = serde_json::from_value(current)?;
        if self.name.is_empty() {
            self.name = local_computer_name();
        }
        self.clamp_limits();
        Ok(())
    }

    /// Add or update a manually entered peer address. False when `address` is empty.
    pub fn add_manual_peer(&mut self, address: &str, control_port: u16) -> bool {
        let address = address.trim();
        if address.is_empty() {
            return false;
        }
        let peers = &mut self.network.manual_peers;
        match peers
            .iter_mut()
            .find(|p| p.address.eq_ignore_ascii_case(address))
        {
            Some(p) => p.control_port = control_port,
            None => peers.push(ManualPeer {
                address: address.to_string(),
                control_port,
            }),
        }
        true
    }

    /// Remove a manually entered peer address. True when one was removed.
    pub fn remove_manual_peer(&mut self, address: &str) -> bool {
        let address = address.trim();
        let peers = &mut self.network.manual_peers;
        let before = peers.len();
        peers.retain(|p| !p.address.eq_ignore_ascii_case(address));
        peers.len() != before
    }

    pub fn trust_peer(&mut self, host_name: &str) -> bool {
        let host = host_name.trim();
        if host.is_empty() {
            return false;
        }
        if self
            .trusted_peers
            .iter()
            .any(|h| h.eq_ignore_ascii_case(host))
        {
            return false;
        }
        self.trusted_peers.push(host.to_string());
        true
    }

    pub fn is_trusted_peer(&self, host_name: &str) -> bool {
        let host = host_name.trim();
        !host.is_empty()
            && self
                .trusted_peers
                .iter()
                .any(|h| h.eq_ignore_ascii_case(host))
    }

    pub fn peer_fingerprint<'a>(&'a self, host_name: &str) -> Option<&'a str> {
        let host = host_name.trim();
        self.peer_fingerprints
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case(host))
            .map(|(_, fp)| fp.as_str())
    }

    /// Name is trusted and the certificate fingerprint is the one we stored.
    pub fn certificate_trusted(&self, host_name: &str, fingerprint: &str) -> bool {
        self.is_trusted_peer(host_name)
            && self
                .peer_fingerprint(host_name)
                .is_some_and(|fp| fp.eq_ignore_ascii_case(fingerprint.trim()))
    }

    /// Remember a keyboard host after the user chooses trust.
    pub fn remember_peer(&mut self, host_name: &str, fingerprint: &str) {
        let host = host_name.trim();
        let fp = fingerprint.trim().to_ascii_lowercase();
        if host.is_empty() || fp.is_empty() {
            return;
        }
        self.trust_peer(host);
        let key = self
            .trusted_peers
            .iter()
            .find(|h| h.eq_ignore_ascii_case(host))
            .cloned()
            .unwrap_or_else(|| host.to_string());
        self.peer_fingerprints.insert(key, fp);
    }

    /// Keyboard host: keep the first certificate for a controlled computer.
    /// A later different certificate is refused.
    pub fn pin_peer(&mut self, host_name: &str, fingerprint: &str) -> Result<(), String> {
        let host = host_name.trim();
        let fp = fingerprint.trim().to_ascii_lowercase();
        if host.is_empty() || fp.is_empty() {
            return Err("peer has no certificate".into());
        }
        if let Some(existing) = self.peer_fingerprint(host) {
            if existing.eq_ignore_ascii_case(&fp) {
                // Already pinned — still ensure it appears in trusted_peers / topology.
                self.trust_peer(host);
                return Ok(());
            }
            return Err(format!(
                "certificate for {host} changed; refusing the connection"
            ));
        }
        self.remember_peer(host, &fp);
        Ok(())
    }

    /// Older builds could pin a fingerprint without listing the name as trusted.
    pub fn ensure_fingerprints_trusted(&mut self) -> bool {
        let mut changed = false;
        let names: Vec<String> = self.peer_fingerprints.keys().cloned().collect();
        for name in names {
            if self.trust_peer(&name) {
                changed = true;
            }
        }
        changed
    }

    /// Build the switch adjacency map from desk placements (host at 0,0).
    pub fn layout_from_screens(&self) -> LayoutMap {
        let host = if self.name.trim().is_empty() {
            "本机".to_string()
        } else {
            self.name.trim().to_string()
        };
        let peers: Vec<&ScreenPlace> = self
            .screens
            .iter()
            .filter(|s| !s.name.is_empty() && !s.name.eq_ignore_ascii_case(&host))
            .filter(|s| !(s.x == 0 && s.y == 0))
            .collect();
        let mut groups: BTreeMap<&str, Vec<&ScreenPlace>> = BTreeMap::new();
        for p in peers {
            groups.entry(sector_of(p.x, p.y)).or_default().push(p);
        }
        let mut out = LayoutMap::new();
        let mut link = |a: &str, edge: &str, b: &str| {
            out.entry(a.to_string())
                .or_default()
                .insert(edge.to_string(), b.to_string());
        };
        for (side, mut list) in groups {
            list.sort_by_key(|p| i64::from(p.x).pow(2) + i64::from(p.y).pow(2));
            let back = opposite_edge(side);
            if list.is_empty() {
                continue;
            }
            link(&host, side, &list[0].name);
            link(&list[0].name, back, &host);
            for i in 0..list.len().saturating_sub(1) {
                link(&list[i].name, side, &list[i + 1].name);
                link(&list[i + 1].name, back, &list[i].name);
            }
        }
        out
    }

    /// Replace `layout` from current `screens` placements.
    pub fn sync_layout_from_screens(&mut self) {
        self.layout = self.layout_from_screens();
    }

    /// Ensure every trusted peer (except self) has a desk slot; drop orphans
    /// that are no longer trusted. Returns true if anything changed.
    pub fn ensure_trusted_screen_places(&mut self) -> bool {
        let host = self.name.trim().to_string();
        let peers: Vec<String> = self
            .trusted_peers
            .iter()
            .filter(|n| !n.eq_ignore_ascii_case(&host))
            .cloned()
            .collect();
        let mut changed = false;

        // Drop desk tiles for computers that are no longer trusted (forget /
        // reset left screens behind, which showed up as ghost topology nodes).
        let before = self.screens.len();
        self.screens.retain(|s| {
            s.name.eq_ignore_ascii_case(&host)
                || peers.iter().any(|p| p.eq_ignore_ascii_case(&s.name))
        });
        if self.screens.len() != before {
            changed = true;
        }

        // The desk only draws ring slots. A place saved farther out (an older
        // build stored pixel offsets) would hide the peer; keep its direction.
        for i in 0..self.screens.len() {
            let s = &self.screens[i];
            if s.name.eq_ignore_ascii_case(&host) || (s.x.abs() <= 1 && s.y.abs() <= 1) {
                continue;
            }
            let want = (s.x.signum(), s.y.signum());
            let name = s.name.clone();
            let free = |slot: (i32, i32), places: &[ScreenPlace]| {
                !places
                    .iter()
                    .any(|p| !p.name.eq_ignore_ascii_case(&name) && (p.x, p.y) == slot)
            };
            let slot = if free(want, &self.screens) {
                want
            } else {
                nearest_screen_slot(want.0, want.1, &name, &self.screens)
            };
            if slot.0.abs() <= 1 && slot.1.abs() <= 1 {
                self.screens[i].x = slot.0;
                self.screens[i].y = slot.1;
                changed = true;
            }
        }

        for name in &peers {
            if self
                .screens
                .iter()
                .any(|s| s.name.eq_ignore_ascii_case(name))
            {
                continue;
            }
            let (x, y) = nearest_screen_slot(1, 0, name, &self.screens);
            self.screens.push(ScreenPlace {
                name: name.clone(),
                x,
                y,
            });
            changed = true;
        }

        // Keep host marker at center when any peer is placed; clear host-only
        // leftover when the desk is empty.
        let host_name = if host.is_empty() {
            "本机".to_string()
        } else {
            host
        };
        let has_peer_tile = self
            .screens
            .iter()
            .any(|s| !s.name.eq_ignore_ascii_case(&host_name));
        if has_peer_tile {
            if !self
                .screens
                .iter()
                .any(|s| s.name.eq_ignore_ascii_case(&host_name) && s.x == 0 && s.y == 0)
            {
                self.screens
                    .retain(|s| !s.name.eq_ignore_ascii_case(&host_name));
                self.screens.insert(
                    0,
                    ScreenPlace {
                        name: host_name,
                        x: 0,
                        y: 0,
                    },
                );
                changed = true;
            }
        } else if !self.screens.is_empty() {
            // No trusted peers — don't keep a lone host marker on the desk.
            self.screens.clear();
            changed = true;
        }

        if changed {
            self.sync_layout_from_screens();
        }
        changed
    }

    /// Drop a remembered computer. The next connection has to pair again.
    pub fn forget_peer(&mut self, host_name: &str) -> bool {
        let host = host_name.trim();
        if host.is_empty() {
            return false;
        }
        let peers = self.trusted_peers.len();
        let prints = self.peer_fingerprints.len();
        let screens = self.screens.len();
        self.trusted_peers.retain(|h| !h.eq_ignore_ascii_case(host));
        self.peer_fingerprints
            .retain(|name, _| !name.eq_ignore_ascii_case(host));
        self.screens.retain(|s| !s.name.eq_ignore_ascii_case(host));
        let dropped = peers != self.trusted_peers.len()
            || prints != self.peer_fingerprints.len()
            || screens != self.screens.len();
        if dropped {
            self.sync_layout_from_screens();
        }
        dropped
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ui_patch_keeps_trust_and_port() {
        let mut cfg = Config::default();
        cfg.trusted_peers.push("Other-PC".into());
        cfg.control_port = 24900;
        cfg.server_address = "192.168.1.9".into();
        cfg.merge_from_value(serde_json::json!({
            "name": "Desk",
            "role": "server",
            "trusted_peers": ["should-not-apply"]
        }))
        .unwrap();
        assert_eq!(cfg.name, "Desk");
        assert_eq!(cfg.role, Role::Server);
        assert_eq!(cfg.control_port, 24900);
        assert_eq!(cfg.server_address, "192.168.1.9");
        assert_eq!(cfg.trusted_peers, vec!["Other-PC".to_string()]);
    }

    #[test]
    fn manual_peers_add_update_remove() {
        let mut cfg = Config::default();
        assert!(!cfg.add_manual_peer("  ", 0));
        assert!(cfg.add_manual_peer(" host.lan ", 0));
        assert!(cfg.add_manual_peer("HOST.lan", 24910));
        assert_eq!(cfg.network.manual_peers.len(), 1);
        assert_eq!(cfg.network.manual_peers[0].control_port, 24910);
        assert!(cfg.remove_manual_peer("host.LAN"));
        assert!(!cfg.remove_manual_peer("host.lan"));
        assert_eq!(cfg.limits.rss_warn_mb, 200);
        assert_eq!(cfg.limits.rss_restart_mb, 500);
    }

    #[test]
    fn trust_matches_name_without_case() {
        let mut cfg = Config::default();
        assert!(cfg.trust_peer(" Desk "));
        assert!(!cfg.trust_peer("desk"));
        assert!(cfg.is_trusted_peer("DESK"));
        assert!(!cfg.is_trusted_peer("  "));
        assert!(!cfg.trust_peer(""));
    }

    #[test]
    fn certificate_trust_requires_matching_fingerprint() {
        let mut cfg = Config::default();
        cfg.trust_peer("Desk");
        assert!(!cfg.certificate_trusted("Desk", "aa"));
        cfg.remember_peer("desk", "AA");
        assert!(cfg.certificate_trusted("DESK", "aa"));
        assert!(!cfg.certificate_trusted("DESK", "bb"));
        assert!(cfg.pin_peer("Other", "cc").is_ok());
        assert!(cfg.pin_peer("other", "cc").is_ok());
        assert!(cfg.pin_peer("Other", "dd").is_err());
    }

    #[test]
    fn missing_notify_block_uses_defaults() {
        let cfg: Config = serde_json::from_str(r#"{"name":"Desk"}"#).unwrap();
        assert!(!cfg.notify.connect);
        assert!(cfg.notify.disconnect);
        assert!(cfg.notify.transfer);
        assert!(cfg.notify.errors);
    }

    #[test]
    fn forget_peer_drops_name_and_fingerprint() {
        let mut cfg = Config {
            name: "Host".into(),
            ..Default::default()
        };
        cfg.remember_peer("Desk", "aa");
        cfg.pin_peer("Other", "bb").unwrap();
        cfg.screens = vec![
            ScreenPlace {
                name: "Host".into(),
                x: 0,
                y: 0,
            },
            ScreenPlace {
                name: "Desk".into(),
                x: 1,
                y: 0,
            },
            ScreenPlace {
                name: "Other".into(),
                x: 0,
                y: 1,
            },
        ];
        assert!(!cfg.forget_peer("  "));
        assert!(cfg.forget_peer("desk"));
        assert!(!cfg.is_trusted_peer("Desk"));
        assert!(cfg.peer_fingerprint("Desk").is_none());
        assert!(cfg.peer_fingerprint("Other").is_some());
        assert!(!cfg
            .screens
            .iter()
            .any(|s| s.name.eq_ignore_ascii_case("Desk")));
        assert!(cfg
            .screens
            .iter()
            .any(|s| s.name.eq_ignore_ascii_case("Other")));
        assert!(!cfg.forget_peer("desk"));
    }

    #[test]
    fn ensure_trusted_screen_places_prunes_orphans() {
        let mut cfg = Config {
            name: "Host".into(),
            screens: vec![
                ScreenPlace {
                    name: "Host".into(),
                    x: 0,
                    y: 0,
                },
                ScreenPlace {
                    name: "Ghost".into(),
                    x: 1,
                    y: 0,
                },
            ],
            ..Default::default()
        };
        assert!(cfg.ensure_trusted_screen_places());
        assert!(cfg.screens.is_empty());
        assert!(cfg.layout.is_empty());
    }

    #[test]
    fn far_places_come_back_to_the_ring() {
        let mut cfg = Config {
            name: "hady-one".into(),
            trusted_peers: vec!["hady-TM1703".into()],
            screens: vec![
                ScreenPlace {
                    name: "hady-one".into(),
                    x: 0,
                    y: 0,
                },
                ScreenPlace {
                    name: "hady-TM1703".into(),
                    x: 137,
                    y: 0,
                },
            ],
            ..Default::default()
        };
        assert!(cfg.ensure_trusted_screen_places());
        let peer = cfg
            .screens
            .iter()
            .find(|s| s.name == "hady-TM1703")
            .unwrap();
        assert_eq!((peer.x, peer.y), (1, 0));
        assert_eq!(cfg.layout["hady-one"]["right"], "hady-TM1703");
        assert!(!cfg.ensure_trusted_screen_places());
    }

    #[test]
    fn fingerprint_only_peers_are_listed_as_trusted() {
        let mut cfg = Config::default();
        cfg.peer_fingerprints
            .insert("Legacy-PC".into(), "abcd".into());
        assert!(!cfg.is_trusted_peer("Legacy-PC"));
        assert!(cfg.ensure_fingerprints_trusted());
        assert!(cfg.is_trusted_peer("Legacy-PC"));
        assert!(!cfg.ensure_fingerprints_trusted());
    }

    #[test]
    fn layout_from_screens_links_host_and_peer() {
        let cfg = Config {
            name: "Host".into(),
            screens: vec![
                ScreenPlace {
                    name: "Host".into(),
                    x: 0,
                    y: 0,
                },
                ScreenPlace {
                    name: "Peer".into(),
                    x: 1,
                    y: 0,
                },
            ],
            ..Default::default()
        };
        let layout = cfg.layout_from_screens();
        assert_eq!(
            layout
                .get("Host")
                .and_then(|e| e.get("right"))
                .map(|s| s.as_str()),
            Some("Peer")
        );
        assert_eq!(
            layout
                .get("Peer")
                .and_then(|e| e.get("left"))
                .map(|s| s.as_str()),
            Some("Host")
        );
    }

    #[test]
    fn upgrade_raises_text_limit_once() {
        let dir = std::env::temp_dir().join(format!("csc-cfg-up-{}", std::process::id()));
        let path = dir.join("config.json");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            &path,
            r#"{"name":"Desk","clipboard":{"max_inline_mb":1,"max_total_mb":64}}"#,
        )
        .unwrap();
        let mut cfg = Config::load_or_create(&path).unwrap();
        assert_eq!(cfg.config_version, CONFIG_VERSION);
        assert_eq!(cfg.clipboard.max_inline_mb, MAX_INLINE_MB);

        cfg.clipboard.max_inline_mb = 3;
        cfg.save(&path).unwrap();
        let again = Config::load_or_create(&path).unwrap();
        assert_eq!(again.clipboard.max_inline_mb, 3);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn upgrade_turns_log_file_on_once() {
        let dir = std::env::temp_dir().join(format!("csc-cfg-log-{}", std::process::id()));
        let path = dir.join("config.json");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            &path,
            r#"{"config_version":3,"name":"Desk","log_level":"debug","log":{"level":"debug","to_file":false,"file":""}}"#,
        )
        .unwrap();
        let mut cfg = Config::load_or_create(&path).unwrap();
        assert!(cfg.log.to_file);
        assert_eq!(cfg.log.level, "info");
        assert_eq!(cfg.log_level, "info");
        assert_eq!(cfg.log.path(), default_log_path());

        cfg.log.to_file = false;
        cfg.log.level = "warn".into();
        cfg.save(&path).unwrap();
        let again = Config::load_or_create(&path).unwrap();
        assert!(!again.log.to_file);
        assert_eq!(again.log.level, "warn");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn old_hotkeys_are_replaced() {
        let dir = std::env::temp_dir().join(format!("csc-cfg-hk-{}", std::process::id()));
        let path = dir.join("config.json");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            &path,
            r#"{"config_version":2,"name":"Desk","hotkeys":{"lock_screen":"ScrollLock","return_to_server":"Ctrl+Alt+Home","toggle_enable":"Ctrl+Alt+Pause"}}"#,
        )
        .unwrap();
        let cfg = Config::load_or_create(&path).unwrap();
        assert_eq!(cfg.hotkeys.return_to_server, "Ctrl+Shift+/");
        assert_eq!(cfg.hotkeys.lock_screen, "Ctrl+Shift+L");
        assert!(!std::fs::read_to_string(&path)
            .unwrap()
            .contains("toggle_enable"));

        // v5 replaces the old lock keys once. After that the user can set one back.
        std::fs::write(
            &path,
            r#"{"config_version":3,"name":"Desk","control_port":25000,"peer_fingerprints":{"Desk":"ab"},"hotkeys":{"lock_screen":"Ctrl+Alt+L","return_to_server":"ctrl+alt+home"}}"#,
        )
        .unwrap();
        let mut cfg = Config::load_or_create(&path).unwrap();
        assert_eq!(cfg.config_version, 5);
        assert_eq!(cfg.hotkeys.return_to_server, "Ctrl+Shift+/");
        assert_eq!(cfg.hotkeys.lock_screen, "Ctrl+Shift+L");
        assert!(cfg.peer_fingerprints.is_empty());
        assert!(!cfg.device_id.is_empty());
        assert_eq!(cfg.network.discovery_port, 25000);
        assert_eq!(cfg.log.max_kb, 1024);
        assert!(std::fs::read_to_string(&path)
            .unwrap()
            .contains("Ctrl+Shift+/"));

        cfg.merge_from_value(serde_json::json!({
            "hotkeys": {"lock_screen": "scroll lock", "return_to_server": "Ctrl+Alt+Home"}
        }))
        .unwrap();
        assert_eq!(cfg.hotkeys.return_to_server, "Ctrl+Shift+/");
        assert_eq!(cfg.hotkeys.lock_screen, "scroll lock");
        cfg.merge_from_value(serde_json::json!({
            "hotkeys": {"lock_screen": "", "return_to_server": ""}
        }))
        .unwrap();
        assert_eq!(cfg.hotkeys.return_to_server, "Ctrl+Shift+/");
        assert_eq!(cfg.hotkeys.lock_screen, "Ctrl+Shift+L");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn patch_clamps_text_limit_and_keeps_version() {
        let mut cfg = Config::default();
        cfg.merge_from_value(serde_json::json!({
            "clipboard": {"max_inline_mb": 999},
            "config_version": 1
        }))
        .unwrap();
        assert_eq!(cfg.clipboard.max_inline_mb, MAX_INLINE_MB);
        assert_eq!(cfg.config_version, CONFIG_VERSION);
        assert_eq!(cfg.clipboard.max_inline_bytes(), 15 * 1024 * 1024);
    }

    #[test]
    fn hostname_line_ignores_blank_and_placeholder() {
        assert_eq!(normalize_hostname("desk\n").as_deref(), Some("desk"));
        assert_eq!(normalize_hostname("  \n"), None);
        assert_eq!(normalize_hostname("(none)\n"), None);
        assert!(name_needs_os_hostname(""));
        assert!(name_needs_os_hostname("PC"));
        assert!(!name_needs_os_hostname("Desk"));
        assert!(!local_computer_name().is_empty());
    }
}
