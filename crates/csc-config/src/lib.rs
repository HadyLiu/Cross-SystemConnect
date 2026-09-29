//! Cross-SystemConnect configuration load/save.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use tracing::{info, warn};

/// Product data directory name (user-facing).
pub const APP_DIR_NAME: &str = "Cross-SystemConnect";
/// Legacy directory used by older builds.
pub const LEGACY_APP_DIR_NAME: &str = "CSC";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    Server,
    Client,
}

impl Default for Role {
    fn default() -> Self {
        // Default: client; saved config may override to server (KM host).
        Self::Client
    }
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
}

fn default_double_tap_window() -> i32 {
    250
}
fn default_corner() -> i32 {
    10
}

impl Default for SwitchConfig {
    fn default() -> Self {
        Self {
            edge_delay_ms: 0,
            double_tap_edge: false,
            double_tap_window_ms: 250,
            corner_dead_zone_px: 10,
            relative_mouse_moves: false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClipboardConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default = "default_true")]
    pub text: bool,
    #[serde(default = "default_true")]
    pub image: bool,
    #[serde(default = "default_true")]
    pub files: bool,
    #[serde(default = "default_inline")]
    pub max_inline_mb: i32,
    #[serde(default = "default_total")]
    pub max_total_mb: i32,
}

fn default_true() -> bool {
    true
}
fn default_inline() -> i32 {
    1
}
fn default_total() -> i32 {
    64
}

impl Default for ClipboardConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            text: true,
            image: true,
            files: true,
            max_inline_mb: 1,
            max_total_mb: 64,
        }
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
    #[serde(default = "default_true")]
    pub limit_speed: bool,
    #[serde(default = "default_speed")]
    pub max_speed_mibs: i32,
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
            limit_speed: true,
            max_speed_mibs: 4,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HotkeyConfig {
    #[serde(default = "default_lock")]
    pub lock_screen: String,
    #[serde(default = "default_return")]
    pub return_to_server: String,
    #[serde(default = "default_toggle")]
    pub toggle_enable: String,
}

fn default_lock() -> String {
    "ScrollLock".into()
}
fn default_return() -> String {
    "Ctrl+Alt+Home".into()
}
fn default_toggle() -> String {
    "Ctrl+Alt+Pause".into()
}

impl Default for HotkeyConfig {
    fn default() -> Self {
        Self {
            lock_screen: default_lock(),
            return_to_server: default_return(),
            toggle_enable: default_toggle(),
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
}

fn default_theme() -> String {
    "dark".into()
}

impl Default for GuiConfig {
    fn default() -> Self {
        Self {
            theme: default_theme(),
            launch_at_login: false,
            prevent_sleep: false,
            auto_hide: true,
            close_to_tray: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogConfig {
    #[serde(default = "default_log_level")]
    pub level: String,
    #[serde(default)]
    pub to_file: bool,
    #[serde(default)]
    pub file: String,
}

fn default_log_level() -> String {
    "info".into()
}

impl Default for LogConfig {
    fn default() -> Self {
        Self {
            level: default_log_level(),
            to_file: false,
            file: String::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NetworkExtras {
    #[serde(default = "default_true")]
    pub heartbeat_enabled: bool,
    #[serde(default = "default_hb")]
    pub heartbeat_ms: i32,
}

fn default_hb() -> i32 {
    5000
}

impl Default for NetworkExtras {
    fn default() -> Self {
        Self {
            heartbeat_enabled: true,
            heartbeat_ms: 5000,
        }
    }
}

/// layout[host][edge] = neighbor
pub type LayoutMap = BTreeMap<String, BTreeMap<String, String>>;

/// One screen block on the arrangement desk. `x`/`y` are grid cells; up is negative `y`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ScreenPlace {
    pub name: String,
    #[serde(default)]
    pub x: i32,
    #[serde(default)]
    pub y: i32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub role: Role,
    #[serde(default)]
    pub name: String,
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
    pub log: LogConfig,
    #[serde(default)]
    pub network: NetworkExtras,
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
}

fn default_bind() -> String {
    "0.0.0.0".into()
}
fn default_control_port() -> u16 {
    24900
}
fn default_data_port() -> u16 {
    24901
}

impl Default for Config {
    fn default() -> Self {
        Self {
            role: Role::Client,
            name: local_computer_name(),
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
            log: LogConfig::default(),
            network: NetworkExtras::default(),
            enable_enter_command: false,
            enable_exit_command: false,
            enter_command: String::new(),
            exit_command: String::new(),
            log_level: default_log_level(),
            trusted_peers: Vec::new(),
        }
    }
}

pub fn local_computer_name() -> String {
    hostname::get()
        .ok()
        .and_then(|h| h.into_string().ok())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "PC".into())
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

pub fn default_log_path() -> PathBuf {
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
            let n = if neighbor == from { to } else { neighbor.as_str() };
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
        let text = fs::read_to_string(path)
            .with_context(|| format!("read config {}", path.display()))?;
        let mut cfg: Config = serde_json::from_str(&text)
            .with_context(|| format!("parse config {}", path.display()))?;
        if cfg.name.is_empty() {
            cfg.name = local_computer_name();
        }
        if cfg.name == "LOCAL-PC" {
            let host = local_computer_name();
            rename_layout_host(&mut cfg.layout, "LOCAL-PC", &host);
            cfg.name = host.clone();
            info!("Migrated config name LOCAL-PC → {host}");
            if let Err(e) = cfg.save(path) {
                warn!("Failed to persist migrated config: {e}");
            }
        }
        if cfg.log.level.is_empty() {
            cfg.log.level = cfg.log_level.clone();
        }
        Ok(cfg)
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
                if k == "trusted_peers" {
                    continue;
                }
                base.insert(k.clone(), val.clone());
            }
        }
        *self = serde_json::from_value(current)?;
        if self.name.is_empty() {
            self.name = local_computer_name();
        }
        Ok(())
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
}
