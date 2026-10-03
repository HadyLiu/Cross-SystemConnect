use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IpcRequest {
    pub id: u64,
    pub method: String,
    #[serde(default)]
    pub params: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IpcResponse {
    pub id: u64,
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl IpcResponse {
    pub fn ok(id: u64, result: Value) -> Self {
        Self {
            id,
            ok: true,
            result: Some(result),
            error: None,
        }
    }

    pub fn err(id: u64, error: impl Into<String>) -> Self {
        Self {
            id,
            ok: false,
            result: None,
            error: Some(error.into()),
        }
    }
}

fn default_host_state() -> String {
    "no_host".into()
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct AppStatus {
    pub connected: bool,
    pub peer_name: String,
    pub active_screen: String,
    pub sharing_enabled: bool,
    pub role: String,
    pub local_name: String,
    pub launch_at_login: bool,
    pub host_name: String,
    pub local_ipv4: Vec<String>,
    /// Ethernet first, then Wi-Fi. Virtual adapters are omitted.
    #[serde(default)]
    pub local_nics: Vec<LocalNic>,
    /// UDP/TCP port the server searches and the client announces on.
    #[serde(default)]
    pub discovery_port: u16,
    /// `searching` while the server looks for clients, `announcing` while the client sends.
    #[serde(default)]
    pub link_state: String,
    /// Set while the server is waiting for trust / this-time / reject.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending_peer: Option<PendingPeer>,
    /// Six digits the keyboard host shows while a new computer is pairing.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub pairing_code: String,
    /// Unix seconds until which a new computer may start pairing. 0 means closed.
    #[serde(default)]
    pub pairing_open_until: u64,
    /// Short note about a peer, such as a lock screen. Empty when there is nothing to show.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub peer_note: String,
    /// Controlled computer: `no_host`, `connected`, `host_sleeping`, `host_lost`, `reconnecting`.
    #[serde(default = "default_host_state")]
    pub host_state: String,
    /// Peers seen on the selected LAN, whether or not they are connected yet.
    #[serde(default)]
    pub discovered: Vec<DiscoveredPeer>,
    /// For example `10.10.10.0/24` when search is bound to 10.10.10.10.
    #[serde(default)]
    pub search_subnet: String,
    /// Shown while files are dragged against an edge that leads to a peer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub drop_zone: Option<DropZone>,
    /// Batches this computer is receiving or has just received.
    #[serde(default)]
    pub transfers: Vec<TransferProgress>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum TransferState {
    #[default]
    Waiting,
    /// Names already exist in the target folder; waiting for the user to choose.
    Conflict,
    Running,
    Done,
    Failed,
    Cancelled,
    /// The link dropped; the partial file is kept until the resume window ends.
    #[serde(rename = "paused_link_lost")]
    Paused,
}

impl TransferState {
    pub fn finished(self) -> bool {
        matches!(
            self,
            Self::Done | Self::Failed | Self::Cancelled | Self::Paused
        )
    }
}

/// One top-level name that already exists where a batch is going.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct TransferConflict {
    /// Entry index of the top-level item in the offer.
    pub index: u32,
    pub name: String,
    pub is_directory: bool,
    pub new_size: u64,
    pub old_size: u64,
    pub old_mtime_unix: i64,
}

/// Progress of one incoming batch, shown in the receiver's transfer window.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct TransferProgress {
    pub id: u64,
    /// Computer the files come from.
    pub peer: String,
    /// Keyboard host that relays them, when not direct.
    #[serde(default)]
    pub via: String,
    pub title: String,
    pub files_done: u32,
    pub files_total: u32,
    pub bytes_done: u64,
    pub bytes_total: u64,
    pub bytes_per_sec: u64,
    pub state: TransferState,
    /// Failure reason, or where the files were saved.
    #[serde(default)]
    pub message: String,
    /// Folder the batch goes into.
    #[serde(default)]
    pub dest: String,
    /// The received item to open when there is exactly one.
    #[serde(default)]
    pub open_path: String,
    #[serde(default)]
    pub conflicts: Vec<TransferConflict>,
    /// Names changed so this computer can store them.
    #[serde(default)]
    pub renamed: Vec<RenamedName>,
}

/// A received name that had to change, as plain text for the transfer window.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct RenamedName {
    pub from: String,
    pub to: String,
}

/// Screen area (absolute desktop pixels) where a drop sends files to `peer`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct DropZone {
    pub peer: String,
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
    /// Files entering the window start a drag onto the other screen (`drag_enter`).
    /// Otherwise they are sent when dropped on the window (`send_files`).
    #[serde(default)]
    pub virtual_drag: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct LocalNic {
    pub address: String,
    /// `ethernet` or `wifi`.
    pub kind: String,
    pub name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct DiscoveredPeer {
    pub name: String,
    pub host_name: String,
    pub address: String,
    #[serde(default)]
    pub seen_unix: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct PendingPeer {
    pub name: String,
    pub host_name: String,
    pub address: String,
}

/// Filesystem path of the core's Unix socket.
///
/// Uses `XDG_RUNTIME_DIR` only when that directory already exists. An empty or
/// missing runtime dir falls back to `/tmp`, and both processes must agree.
pub fn core_socket_path() -> std::path::PathBuf {
    socket_path_from(
        std::env::var("XDG_RUNTIME_DIR").ok().as_deref(),
        std::env::var("USER")
            .or_else(|_| std::env::var("LOGNAME"))
            .ok()
            .as_deref(),
    )
}

pub fn socket_path_from(xdg: Option<&str>, user: Option<&str>) -> std::path::PathBuf {
    if let Some(dir) = xdg.map(str::trim).filter(|s| !s.is_empty()) {
        let dir = std::path::PathBuf::from(dir);
        if dir.is_dir() {
            return dir.join("cross-systemconnect-core.sock");
        }
    }
    let user = user
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("user");
    std::path::PathBuf::from(format!("/tmp/cross-systemconnect-core-{user}.sock"))
}

/// Encode length-prefixed JSON for IPC.
pub fn encode_ipc_message(value: &impl Serialize) -> Result<Vec<u8>, serde_json::Error> {
    let body = serde_json::to_vec(value)?;
    let mut out = Vec::with_capacity(4 + body.len());
    out.extend_from_slice(&(body.len() as u32).to_le_bytes());
    out.extend_from_slice(&body);
    Ok(out)
}

/// Try parse one length-prefixed JSON message; returns (consumed_bytes, value) or None if incomplete.
pub fn try_decode_ipc_message(buf: &[u8]) -> Result<Option<(usize, Value)>, serde_json::Error> {
    if buf.len() < 4 {
        return Ok(None);
    }
    let len = u32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]) as usize;
    if buf.len() < 4 + len {
        return Ok(None);
    }
    let value: Value = serde_json::from_slice(&buf[4..4 + len])?;
    Ok(Some((4 + len, value)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ipc_roundtrip_and_short_buffer() {
        let raw = encode_ipc_message(&serde_json::json!({"method": "get_status"})).unwrap();
        assert!(try_decode_ipc_message(&raw[..3]).unwrap().is_none());
        assert!(try_decode_ipc_message(&raw[..raw.len() - 1])
            .unwrap()
            .is_none());
        let (n, value) = try_decode_ipc_message(&raw).unwrap().unwrap();
        assert_eq!(n, raw.len());
        assert_eq!(value["method"], "get_status");
    }

    #[test]
    fn socket_path_prefers_an_existing_runtime_dir() {
        let dir = std::env::temp_dir();
        let path = socket_path_from(Some(dir.to_str().unwrap()), Some("ada"));
        assert_eq!(path, dir.join("cross-systemconnect-core.sock"));
    }

    #[test]
    fn socket_path_falls_back_when_runtime_dir_is_missing() {
        let missing = socket_path_from(Some("/this/does/not/exist/csc-runtime"), Some("ada"));
        assert_eq!(
            missing,
            std::path::PathBuf::from("/tmp/cross-systemconnect-core-ada.sock")
        );
        let blank = socket_path_from(Some("  "), None);
        assert_eq!(
            blank,
            std::path::PathBuf::from("/tmp/cross-systemconnect-core-user.sock")
        );
    }
}
