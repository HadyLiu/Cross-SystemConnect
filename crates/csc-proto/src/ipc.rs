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
    /// UDP/TCP port the server searches and the client announces on.
    #[serde(default)]
    pub discovery_port: u16,
    /// `searching` while the server looks for clients, `announcing` while the client sends.
    #[serde(default)]
    pub link_state: String,
    /// Set while the server is waiting for trust / this-time / reject.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending_peer: Option<PendingPeer>,
    /// Peers seen on the selected LAN, whether or not they are connected yet.
    #[serde(default)]
    pub discovered: Vec<DiscoveredPeer>,
    /// For example `10.10.10.0/24` when search is bound to 10.10.10.10.
    #[serde(default)]
    pub search_subnet: String,
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
