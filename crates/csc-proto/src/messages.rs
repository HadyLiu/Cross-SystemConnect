use crate::frame::{encode_frame, MsgType, FLAG_JSON_PAYLOAD};
use serde::{Deserialize, Serialize};
use std::hash::Hash;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Edge {
    Left,
    Right,
    Top,
    Bottom,
    TopLeft,
    TopRight,
    BottomLeft,
    BottomRight,
}

impl Edge {
    pub fn opposite(self) -> Self {
        match self {
            Self::Left => Self::Right,
            Self::Right => Self::Left,
            Self::Top => Self::Bottom,
            Self::Bottom => Self::Top,
            Self::TopLeft => Self::BottomRight,
            Self::TopRight => Self::BottomLeft,
            Self::BottomLeft => Self::TopRight,
            Self::BottomRight => Self::TopLeft,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Left => "left",
            Self::Right => "right",
            Self::Top => "top",
            Self::Bottom => "bottom",
            Self::TopLeft => "top_left",
            Self::TopRight => "top_right",
            Self::BottomLeft => "bottom_left",
            Self::BottomRight => "bottom_right",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "left" => Some(Self::Left),
            "right" => Some(Self::Right),
            "top" => Some(Self::Top),
            "bottom" => Some(Self::Bottom),
            "top_left" => Some(Self::TopLeft),
            "top_right" => Some(Self::TopRight),
            "bottom_left" => Some(Self::BottomLeft),
            "bottom_right" => Some(Self::BottomRight),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HelloPayload {
    pub protocol_version: u16,
    pub host_name: String,
    pub screen_width: i32,
    pub screen_height: i32,
    pub capabilities: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SwitchPayload {
    pub edge: Edge,
    pub position_pct: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MouseMovePayload {
    pub dx: i32,
    pub dy: i32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MouseButtonPayload {
    pub button: u32,
    pub down: u8,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MouseWheelPayload {
    pub wheel_v: i32,
    pub wheel_h: i32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KeyPayload {
    pub hid_usage: u32,
    pub down: u8,
    pub modifiers: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileEntry {
    pub relative_path: String,
    pub size: u64,
    pub mtime_unix: i64,
    pub is_directory: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClipOfferPayload {
    pub seq: u64,
    pub formats: Vec<String>,
    #[serde(default)]
    pub text: Option<String>,
    #[serde(default)]
    pub files: Vec<FileEntry>,
}

/// Why a batch of files is being sent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TransferPurpose {
    /// Sent by older versions for copied files; handled like `Paste`.
    Clipboard,
    /// Files dropped on the edge window: saved to the receive folder.
    Drag,
    /// Copied files pasted with Ctrl+V on the receiver: saved to the receive folder.
    Paste,
    /// Files dragged across and released over a folder on the receiver.
    DragDrop,
}

impl TransferPurpose {
    /// Clipboard batches from older versions are pastes now.
    pub fn normalized(self) -> Self {
        match self {
            Self::Clipboard => Self::Paste,
            other => other,
        }
    }
}

/// Sender to receiver: files are ready. The receiver answers with FileRequest or FileError.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileOfferPayload {
    pub transfer_id: u64,
    pub purpose: TransferPurpose,
    /// Every file and directory, relative to the batch, with `/` separators.
    pub entries: Vec<FileEntry>,
    pub total_bytes: u64,
    /// Computer the files come from when the keyboard host relays them.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub origin: String,
    /// Sender to keyboard host: pass this batch on to that computer.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub relay_to: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileRequestPayload {
    pub transfer_id: u64,
    /// Entry indexes the receiver does not want (name conflicts it skips).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub skip: Vec<u32>,
}

/// Receiver to sender: this many bytes of the batch are written.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileAckPayload {
    pub transfer_id: u64,
    pub bytes: u64,
}

/// Controlled computer to keyboard host: the real pointer pushed through an outer edge.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EdgeHitPayload {
    pub edge: Edge,
    pub position_pct: f32,
    /// Mouse button 1 was held, so a file drag may follow with `DragBegin`.
    #[serde(default)]
    pub dragging: bool,
    /// Corner zone the pointer is in; the host prefers it when the layout has one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub corner: Option<Edge>,
}

/// Keyboard host to the computer that copied files: send them to `to` for pasting.
/// An empty `to` means the keyboard host itself.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PasteRequestPayload {
    pub seq: u64,
    #[serde(default)]
    pub to: String,
}

/// Source to target: a file drag crossed onto the target. The target starts a
/// system drag of virtual files and answers `FileRequest` once they are dropped.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DragBeginPayload {
    pub transfer_id: u64,
    pub entries: Vec<FileEntry>,
    pub total_bytes: u64,
    /// Target computer when the source is a controlled computer (relayed by the host).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub to: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub origin: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DragCancelPayload {
    pub transfer_id: u64,
    /// `user`, `no_target`, `pointer_left` or `unsupported`.
    #[serde(default)]
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileBeginPayload {
    pub transfer_id: u64,
    pub index: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileEndPayload {
    pub transfer_id: u64,
    pub index: u32,
    pub xxhash64: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileDonePayload {
    pub transfer_id: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileErrorPayload {
    pub transfer_id: u64,
    pub message: String,
}

/// Binary file chunk: transfer id, entry index, then the bytes.
pub fn encode_file_data(transfer_id: u64, index: u32, data: &[u8]) -> Vec<u8> {
    let mut body = Vec::with_capacity(12 + data.len());
    body.extend_from_slice(&transfer_id.to_le_bytes());
    body.extend_from_slice(&index.to_le_bytes());
    body.extend_from_slice(data);
    encode_frame(MsgType::FileData, &body, 0)
}

/// Rewrite the transfer id at the front of a FileData payload, for relaying.
pub fn set_file_data_id(payload: &mut [u8], transfer_id: u64) -> bool {
    if payload.len() < 12 {
        return false;
    }
    payload[0..8].copy_from_slice(&transfer_id.to_le_bytes());
    true
}

pub fn decode_file_data(payload: &[u8]) -> Option<(u64, u32, &[u8])> {
    if payload.len() < 12 {
        return None;
    }
    let id = u64::from_le_bytes(payload[0..8].try_into().ok()?);
    let index = u32::from_le_bytes(payload[8..12].try_into().ok()?);
    Some((id, index, &payload[12..]))
}

pub fn encode_json_frame<T: Serialize>(
    msg_type: MsgType,
    value: &T,
) -> Result<Vec<u8>, serde_json::Error> {
    let payload = serde_json::to_vec(value)?;
    Ok(encode_frame(msg_type, &payload, FLAG_JSON_PAYLOAD))
}

fn write_str(out: &mut Vec<u8>, s: &str) {
    let b = s.as_bytes();
    out.extend_from_slice(&(b.len() as u16).to_le_bytes());
    out.extend_from_slice(b);
}

fn read_str(data: &[u8], off: &mut usize) -> Option<String> {
    if *off + 2 > data.len() {
        return None;
    }
    let n = u16::from_le_bytes([data[*off], data[*off + 1]]) as usize;
    *off += 2;
    if *off + n > data.len() {
        return None;
    }
    let s = String::from_utf8_lossy(&data[*off..*off + n]).into_owned();
    *off += n;
    Some(s)
}

/// Compact binary HELLO: version, host name, screen size, capabilities.
pub fn encode_hello_bin(h: &HelloPayload) -> Vec<u8> {
    let mut body = Vec::new();
    body.extend_from_slice(&h.protocol_version.to_le_bytes());
    write_str(&mut body, &h.host_name);
    body.extend_from_slice(&h.screen_width.to_le_bytes());
    body.extend_from_slice(&h.screen_height.to_le_bytes());
    body.extend_from_slice(&h.capabilities.to_le_bytes());
    encode_frame(MsgType::Hello, &body, 0)
}

pub fn decode_hello_bin(payload: &[u8]) -> Option<HelloPayload> {
    if payload.len() < 2 {
        return None;
    }
    let mut off = 2;
    let protocol_version = u16::from_le_bytes([payload[0], payload[1]]);
    let host_name = read_str(payload, &mut off)?;
    if off + 12 > payload.len() {
        return None;
    }
    let screen_width = i32::from_le_bytes(payload[off..off + 4].try_into().ok()?);
    off += 4;
    let screen_height = i32::from_le_bytes(payload[off..off + 4].try_into().ok()?);
    off += 4;
    let capabilities = u32::from_le_bytes(payload[off..off + 4].try_into().ok()?);
    Some(HelloPayload {
        protocol_version,
        host_name,
        screen_width,
        screen_height,
        capabilities,
    })
}

pub fn encode_switch_bin(s: &SwitchPayload) -> Vec<u8> {
    encode_switch_msg(MsgType::SwitchEnter, s)
}

pub fn encode_switch_leave_bin(s: &SwitchPayload) -> Vec<u8> {
    encode_switch_msg(MsgType::SwitchLeave, s)
}

fn encode_switch_msg(msg_type: MsgType, s: &SwitchPayload) -> Vec<u8> {
    let edge = match s.edge {
        Edge::Left => 0u8,
        Edge::Right => 1,
        Edge::Top => 2,
        Edge::Bottom => 3,
        Edge::TopLeft => 4,
        Edge::TopRight => 5,
        Edge::BottomLeft => 6,
        Edge::BottomRight => 7,
    };
    let mut body = vec![edge];
    body.extend_from_slice(&s.position_pct.to_le_bytes());
    encode_frame(msg_type, &body, 0)
}

pub fn encode_mouse_move_bin(m: &MouseMovePayload) -> Vec<u8> {
    let mut body = Vec::with_capacity(8);
    body.extend_from_slice(&m.dx.to_le_bytes());
    body.extend_from_slice(&m.dy.to_le_bytes());
    encode_frame(MsgType::MouseMove, &body, 0)
}

pub fn encode_mouse_button_bin(m: &MouseButtonPayload) -> Vec<u8> {
    let mut body = Vec::with_capacity(5);
    body.extend_from_slice(&m.button.to_le_bytes());
    body.push(m.down);
    encode_frame(MsgType::MouseButton, &body, 0)
}

pub fn decode_mouse_button_bin(payload: &[u8]) -> Option<MouseButtonPayload> {
    if payload.len() < 5 {
        return None;
    }
    Some(MouseButtonPayload {
        button: u32::from_le_bytes(payload[0..4].try_into().ok()?),
        down: payload[4],
    })
}

pub fn encode_mouse_wheel_bin(m: &MouseWheelPayload) -> Vec<u8> {
    let mut body = Vec::with_capacity(8);
    body.extend_from_slice(&m.wheel_v.to_le_bytes());
    body.extend_from_slice(&m.wheel_h.to_le_bytes());
    encode_frame(MsgType::MouseWheel, &body, 0)
}

pub fn decode_mouse_wheel_bin(payload: &[u8]) -> Option<MouseWheelPayload> {
    if payload.len() < 8 {
        return None;
    }
    Some(MouseWheelPayload {
        wheel_v: i32::from_le_bytes(payload[0..4].try_into().ok()?),
        wheel_h: i32::from_le_bytes(payload[4..8].try_into().ok()?),
    })
}

pub fn encode_key_bin(k: &KeyPayload) -> Vec<u8> {
    let mut body = Vec::with_capacity(9);
    body.extend_from_slice(&k.hid_usage.to_le_bytes());
    body.push(k.down);
    body.extend_from_slice(&k.modifiers.to_le_bytes());
    encode_frame(MsgType::Key, &body, 0)
}
