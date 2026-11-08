use crate::frame::{encode_frame, MsgType, FLAG_BINARY, FLAG_JSON_PAYLOAD};
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

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Monitor {
    pub id: u32,
    pub rect: Rect,
    pub scale: f32,
    pub primary: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct HelloPorts {
    pub control: u16,
    pub data: u16,
}

impl Default for HelloPorts {
    fn default() -> Self {
        Self {
            control: 24900,
            data: 24901,
        }
    }
}

/// `HELLO` on the control connection. Field names match architecture §10.3.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HelloPayload {
    #[serde(rename = "proto")]
    pub protocol_version: u16,
    #[serde(default = "proto_floor")]
    pub proto_min: u16,
    #[serde(default = "proto_floor")]
    pub proto_max: u16,
    #[serde(default)]
    pub app_version: String,
    #[serde(default)]
    pub device_id: String,
    #[serde(rename = "name")]
    pub host_name: String,
    #[serde(default)]
    pub role: String,
    #[serde(default)]
    pub screen_width: i32,
    #[serde(default)]
    pub screen_height: i32,
    #[serde(default)]
    pub monitors: Vec<Monitor>,
    #[serde(rename = "caps")]
    pub capabilities: u32,
    #[serde(default)]
    pub ports: HelloPorts,
}

fn proto_floor() -> u16 {
    2
}

impl Default for HelloPayload {
    fn default() -> Self {
        Self {
            protocol_version: 2,
            proto_min: 2,
            proto_max: 2,
            app_version: String::new(),
            device_id: String::new(),
            host_name: String::new(),
            role: String::new(),
            screen_width: 0,
            screen_height: 0,
            monitors: Vec::new(),
            capabilities: 0,
            ports: HelloPorts::default(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SwitchPayload {
    pub edge: Edge,
    pub position_pct: f32,
    /// Host lock lights to copy onto the computer being entered.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub locks: Option<LockState>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub struct LockState {
    pub caps: bool,
    pub num: bool,
    pub scroll: bool,
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

/// Wheel movement in 1/120 notch units (120 per detent).
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
    /// Permission bits in 0o777. 0 means the sender did not provide them.
    #[serde(default)]
    pub mode: u32,
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
    /// Reserved (cross-screen drag removed); treated like `Paste` if received.
    Drag,
    /// Copied files pasted with Ctrl+V on the receiver: saved to the receive folder.
    Paste,
    /// Reserved (cross-screen drag removed); treated like `Paste` if received.
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
    /// Stable id of the computer the bytes come from. Resume checks this.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub origin_device_id: String,
}

/// One file the receiver already has some bytes of.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileResume {
    pub index: u32,
    pub offset: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileRequestPayload {
    pub transfer_id: u64,
    /// Entry indexes the receiver does not want (name conflicts it skips).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub skip: Vec<u32>,
    /// Bytes already stored for these entries. The sender continues from each offset.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub resume: Vec<FileResume>,
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
    /// Drag being carried across this edge. 0 when the pointer is not dragging.
    #[serde(default)]
    pub transfer_id: u64,
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
    /// `user`, `no_target`, `pointer_left`, `retarget`, `esc`, or `unsupported`.
    #[serde(default)]
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PairRequestPayload {
    pub device_id: String,
    pub name: String,
    pub role: String,
    #[serde(rename = "proto", default = "proto_floor")]
    pub protocol_version: u16,
    /// Six digits this computer typed from the other screen. Empty when it did not.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub code: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PairConfirmPayload {
    pub accepted: bool,
    /// Six digits typed from the other computer's home page.
    /// `cancel` when this computer clicked cancel. Empty when it is only confirming.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub code: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PairResultPayload {
    pub status: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PairCommitPayload {
    /// Hex SHA-256 of the commitment.
    pub commit: String,
    /// Six digits the keyboard host typed. Empty when it did not type one.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub code: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PairNoncePayload {
    /// Hex of the 32-byte nonce.
    pub nonce: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DataAttachPayload {
    pub device_id: String,
    /// Base64 of the 32-byte exporter token.
    pub session_token: String,
    pub data_port: u16,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PeerStatePayload {
    pub state: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FocusInfoPayload {
    pub file_target: bool,
    pub fullscreen: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScreenUpdatePayload {
    pub monitors: Vec<Monitor>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ControlPausePayload {
    pub paused: bool,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PasteLocatedPayload {
    pub paste_id: u64,
    pub ok: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DragRetargetPayload {
    pub transfer_id: u64,
    pub to: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClipRequestPayload {
    pub clip_id: u64,
    pub fmt: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClipEndPayload {
    pub clip_id: u64,
    pub xxh64: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LockScreenPayload {
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BenchSyncPayload {
    pub seq: u64,
    pub t1: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub t2: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub t3: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BenchEchoPayload {
    pub seq: u64,
    pub t1: u64,
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
    encode_frame(MsgType::FileData, &body, FLAG_BINARY)
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

/// One complete small file inside a `FILE_BATCH` frame.
#[derive(Debug, Clone)]
pub struct FileBatchItem<'a> {
    pub index: u32,
    pub xxhash64: u64,
    pub data: &'a [u8],
}

/// Binary: `transfer_id u64 | count u16 | [index u32 | len u32 | xxh64 u64 | bytes]×count`.
pub fn encode_file_batch(transfer_id: u64, items: &[(u32, u64, &[u8])]) -> Option<Vec<u8>> {
    if items.is_empty() || items.len() > u16::MAX as usize {
        return None;
    }
    let mut body =
        Vec::with_capacity(10 + items.iter().map(|(_, _, d)| 16 + d.len()).sum::<usize>());
    body.extend_from_slice(&transfer_id.to_le_bytes());
    body.extend_from_slice(&(items.len() as u16).to_le_bytes());
    for &(index, xxh, data) in items {
        if data.len() > u32::MAX as usize {
            return None;
        }
        body.extend_from_slice(&index.to_le_bytes());
        body.extend_from_slice(&(data.len() as u32).to_le_bytes());
        body.extend_from_slice(&xxh.to_le_bytes());
        body.extend_from_slice(data);
    }
    // Same ceiling as FileData (64 KiB + framing).
    if body.len() > crate::DATA_BLOCK_MAX {
        return None;
    }
    Some(encode_frame(MsgType::FileBatch, &body, FLAG_BINARY))
}

pub fn decode_file_batch(payload: &[u8]) -> Option<(u64, Vec<FileBatchItem<'_>>)> {
    if payload.len() < 10 {
        return None;
    }
    let id = u64::from_le_bytes(payload[0..8].try_into().ok()?);
    let count = u16::from_le_bytes(payload[8..10].try_into().ok()?) as usize;
    let mut out = Vec::with_capacity(count);
    let mut off = 10usize;
    for _ in 0..count {
        if payload.len() < off + 16 {
            return None;
        }
        let index = u32::from_le_bytes(payload[off..off + 4].try_into().ok()?);
        let len = u32::from_le_bytes(payload[off + 4..off + 8].try_into().ok()?) as usize;
        let xxhash64 = u64::from_le_bytes(payload[off + 8..off + 16].try_into().ok()?);
        off += 16;
        if payload.len() < off + len {
            return None;
        }
        out.push(FileBatchItem {
            index,
            xxhash64,
            data: &payload[off..off + len],
        });
        off += len;
    }
    if off != payload.len() {
        return None;
    }
    Some((id, out))
}

/// Rewrite the transfer id at the front of a FileBatch payload, for relaying.
pub fn set_file_batch_id(payload: &mut [u8], transfer_id: u64) -> bool {
    if payload.len() < 10 {
        return false;
    }
    payload[0..8].copy_from_slice(&transfer_id.to_le_bytes());
    true
}

pub fn encode_activity() -> Vec<u8> {
    encode_frame(MsgType::Activity, &[], 0)
}

pub fn encode_json_frame<T: Serialize>(
    msg_type: MsgType,
    value: &T,
) -> Result<Vec<u8>, serde_json::Error> {
    let payload = serde_json::to_vec(value)?;
    Ok(encode_frame(msg_type, &payload, FLAG_JSON_PAYLOAD))
}

/// `HELLO` as JSON (protocol 2).
pub fn encode_hello_bin(h: &HelloPayload) -> Vec<u8> {
    encode_json_frame(MsgType::Hello, h).unwrap_or_default()
}

pub fn decode_hello_bin(payload: &[u8]) -> Option<HelloPayload> {
    let hello: HelloPayload = serde_json::from_slice(payload).ok()?;
    if hello.protocol_version != 2 || hello.ports.control == hello.ports.data {
        return None;
    }
    Some(hello)
}

pub fn encode_switch_bin(s: &SwitchPayload) -> Vec<u8> {
    encode_switch_msg(MsgType::SwitchEnter, s)
}

pub fn encode_switch_leave_bin(s: &SwitchPayload) -> Vec<u8> {
    encode_switch_msg(MsgType::SwitchLeave, s)
}

pub fn encode_lock_screen_bin() -> Vec<u8> {
    encode_frame(MsgType::LockScreen, &[], 0)
}

/// A copied image: `ClipData` with the copy's sequence number, then the PNG.
pub fn encode_clip_image_bin(seq: u64, png: &[u8]) -> Vec<u8> {
    let mut body = Vec::with_capacity(8 + png.len());
    body.extend_from_slice(&seq.to_le_bytes());
    body.extend_from_slice(png);
    encode_frame(MsgType::ClipData, &body, FLAG_BINARY)
}

pub fn decode_clip_image_bin(payload: &[u8]) -> Option<(u64, &[u8])> {
    let seq = u64::from_le_bytes(payload.get(..8)?.try_into().ok()?);
    let png = &payload[8..];
    (!png.is_empty()).then_some((seq, png))
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
    if let Some(locks) = s.locks {
        let mut flags = 0x80u8;
        if locks.caps {
            flags |= 1;
        }
        if locks.num {
            flags |= 2;
        }
        if locks.scroll {
            flags |= 4;
        }
        body.push(flags);
    }
    encode_frame(msg_type, &body, FLAG_BINARY)
}

pub fn encode_mouse_move_bin(m: &MouseMovePayload) -> Vec<u8> {
    let mut body = Vec::with_capacity(8);
    body.extend_from_slice(&m.dx.to_le_bytes());
    body.extend_from_slice(&m.dy.to_le_bytes());
    encode_frame(MsgType::MouseMove, &body, FLAG_BINARY)
}

pub fn encode_mouse_button_bin(m: &MouseButtonPayload) -> Vec<u8> {
    let mut body = Vec::with_capacity(5);
    body.extend_from_slice(&m.button.to_le_bytes());
    body.push(m.down);
    encode_frame(MsgType::MouseButton, &body, FLAG_BINARY)
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
    encode_frame(MsgType::MouseWheel, &body, FLAG_BINARY)
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
    encode_frame(MsgType::Key, &body, FLAG_BINARY)
}
