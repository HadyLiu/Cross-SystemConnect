use bytes::{Buf, BufMut, BytesMut};
use thiserror::Error;

pub const MAGIC: u16 = 0x5343; // 'C''S' LE
/// Protocol 2. Version 1 frames are rejected.
pub const VERSION: u8 = 2;
pub const HEADER_SIZE: usize = 12;
/// Control-connection payload limit (architecture §10.2).
pub const CONTROL_MAX: usize = 64 * 1024 + 256;
/// `FILE_DATA` / `CLIP_DATA` / `FILE_BATCH` payload limit.
pub const DATA_BLOCK_MAX: usize = 64 * 1024 + 64;
/// Data-connection JSON payload limit.
pub const DATA_JSON_MAX: usize = 4 * 1024 * 1024;

#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MsgType {
    Hello = 0x01,
    Heartbeat = 0x02,
    PairRequest = 0x03,
    PairConfirm = 0x04,
    PairResult = 0x05,
    DataAttach = 0x06,
    PeerState = 0x07,
    PairCommit = 0x08,
    PairNonce = 0x09,
    SwitchEnter = 0x10,
    SwitchLeave = 0x11,
    /// Controlled computer to keyboard host: its real pointer is pressing an outer edge.
    EdgeHit = 0x12,
    FocusInfo = 0x13,
    ScreenUpdate = 0x14,
    Activity = 0x15,
    ControlPause = 0x16,
    MouseMove = 0x20,
    MouseButton = 0x21,
    MouseWheel = 0x22,
    Key = 0x23,
    InputReset = 0x24,
    ClipOffer = 0x30,
    ClipRequest = 0x31,
    /// A copied image (PNG), see `encode_clip_image_bin`.
    ClipData = 0x32,
    /// Keyboard host to the computer that copied files: send them for pasting.
    PasteRequest = 0x33,
    PasteLocated = 0x34,
    ClipEnd = 0x37,
    /// Reserved (cross-screen drag removed); ignored if received.
    DragBegin = 0x40,
    /// Reserved (cross-screen drag removed); ignored if received.
    DragCancel = 0x41,
    /// Reserved (cross-screen drag removed); ignored if received.
    DragRetarget = 0x42,
    FileRequest = 0x50,
    FileData = 0x51,
    FileEnd = 0x52,
    FileCancel = 0x53,
    FileError = 0x54,
    FileOffer = 0x55,
    FileBegin = 0x56,
    FileDone = 0x57,
    FileAck = 0x58,
    FileBatch = 0x59,
    /// Lock the screen. The keyboard host passes it on to its other computers.
    LockScreen = 0x60,
    BenchSync = 0x70,
    BenchEcho = 0x71,
}

/// Reserved: cross-screen virtual drag was removed. Never advertise.
pub const CAP_VIRTUAL_DRAG: u32 = 1 << 0;
/// Peer reports edge contact itself (`EdgeHit`) instead of the host guessing.
pub const CAP_EDGE_REPORT: u32 = 1 << 1;
/// Peer acknowledges received file bytes (`FileAck`).
pub const CAP_FILE_ACK: u32 = 1 << 2;
/// Separate TLS data connection (`DATA_ATTACH`).
pub const CAP_DATA_CHANNEL: u32 = 1 << 3;
/// Clipboard bodies are fetched with `CLIP_REQUEST`, not pushed inline.
pub const CAP_LAZY_CLIP: u32 = 1 << 4;
/// Controlled computer reports whether focus is a file target (`FOCUS_INFO`).
pub const CAP_FOCUS_INFO: u32 = 1 << 5;
/// `SCREEN_UPDATE` lists each monitor.
pub const CAP_MULTI_MONITOR: u32 = 1 << 6;
/// Linux file paste uses clipboard placeholders, so Ctrl+V must reach that computer.
pub const CAP_CLIP_PLACEHOLDER: u32 = 1 << 7;
/// Capabilities common to every platform. Linux adds `CAP_CLIP_PLACEHOLDER`
/// in `local_capabilities`; `CAP_VIRTUAL_DRAG` is never advertised.
pub const CAPABILITIES: u32 = CAP_EDGE_REPORT
    | CAP_FILE_ACK
    | CAP_DATA_CHANNEL
    | CAP_LAZY_CLIP
    | CAP_FOCUS_INFO
    | CAP_MULTI_MONITOR;

impl MsgType {
    pub fn from_u8(v: u8) -> Option<Self> {
        Some(match v {
            0x01 => Self::Hello,
            0x02 => Self::Heartbeat,
            0x03 => Self::PairRequest,
            0x04 => Self::PairConfirm,
            0x05 => Self::PairResult,
            0x06 => Self::DataAttach,
            0x07 => Self::PeerState,
            0x08 => Self::PairCommit,
            0x09 => Self::PairNonce,
            0x10 => Self::SwitchEnter,
            0x11 => Self::SwitchLeave,
            0x12 => Self::EdgeHit,
            0x13 => Self::FocusInfo,
            0x14 => Self::ScreenUpdate,
            0x15 => Self::Activity,
            0x16 => Self::ControlPause,
            0x20 => Self::MouseMove,
            0x21 => Self::MouseButton,
            0x22 => Self::MouseWheel,
            0x23 => Self::Key,
            0x24 => Self::InputReset,
            0x30 => Self::ClipOffer,
            0x31 => Self::ClipRequest,
            0x32 => Self::ClipData,
            0x33 => Self::PasteRequest,
            0x34 => Self::PasteLocated,
            0x37 => Self::ClipEnd,
            0x40 => Self::DragBegin,
            0x41 => Self::DragCancel,
            0x42 => Self::DragRetarget,
            0x50 => Self::FileRequest,
            0x51 => Self::FileData,
            0x52 => Self::FileEnd,
            0x53 => Self::FileCancel,
            0x54 => Self::FileError,
            0x55 => Self::FileOffer,
            0x56 => Self::FileBegin,
            0x57 => Self::FileDone,
            0x58 => Self::FileAck,
            0x59 => Self::FileBatch,
            0x60 => Self::LockScreen,
            0x70 => Self::BenchSync,
            0x71 => Self::BenchEcho,
            _ => return None,
        })
    }
}

/// Which end of the session is checking a frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionRole {
    Host,
    Controlled,
}

/// True when `msg` is not allowed from the other computer (architecture §10.3).
pub fn bad_direction(role: SessionRole, msg: MsgType) -> bool {
    match role {
        SessionRole::Host => matches!(
            msg,
            MsgType::MouseMove
                | MsgType::MouseButton
                | MsgType::MouseWheel
                | MsgType::Key
                | MsgType::SwitchEnter
                | MsgType::SwitchLeave
                | MsgType::PasteRequest
                | MsgType::PairCommit
        ),
        SessionRole::Controlled => matches!(
            msg,
            MsgType::EdgeHit | MsgType::ControlPause | MsgType::PasteLocated | MsgType::DragRetarget
        ),
    }
}

/// Bit 0 set: payload is binary. Clear: payload is UTF-8 JSON (or empty).
pub const FLAG_BINARY: u32 = 1;
/// JSON frames leave bit 0 clear.
pub const FLAG_JSON_PAYLOAD: u32 = 0;

/// Largest payload this decoder will buffer for `ty` (the raw type byte).
pub fn max_payload(ty: u8) -> usize {
    match ty {
        // Clipboard still rides the control connection until the data port carries it.
        0x30 | 0x32 => 16 * 1024 * 1024,
        0x51 | 0x59 => DATA_BLOCK_MAX,
        0x06 | 0x31 | 0x37 | 0x40 | 0x41 | 0x42 | 0x50 | 0x52 | 0x53 | 0x54 | 0x55 | 0x56
        | 0x57 | 0x58 => DATA_JSON_MAX,
        known if MsgType::from_u8(known).is_some() => CONTROL_MAX,
        _ => DATA_JSON_MAX,
    }
}

#[derive(Debug, Clone)]
pub struct Frame {
    pub version: u8,
    pub msg_type: MsgType,
    pub flags: u32,
    pub payload: Vec<u8>,
}

#[derive(Debug, Error)]
pub enum FrameError {
    #[error("incomplete frame")]
    Incomplete,
    #[error("bad magic")]
    BadMagic,
    #[error("unsupported version {0}")]
    BadVersion(u8),
    #[error("unknown message type {0:#x}")]
    BadType(u8),
    #[error("frame of {0} bytes is too large")]
    TooLarge(usize),
}

pub fn encode_frame(msg_type: MsgType, payload: &[u8], flags: u32) -> Vec<u8> {
    let mut out = Vec::with_capacity(HEADER_SIZE + payload.len());
    out.put_u16_le(MAGIC);
    out.put_u8(VERSION);
    out.put_u8(msg_type as u8);
    out.put_u32_le(flags);
    out.put_u32_le(payload.len() as u32);
    out.extend_from_slice(payload);
    out
}

/// Largest payload the decoder will buffer. Per-type limits are `max_payload`.
pub const MAX_PAYLOAD: usize = 16 * 1024 * 1024;

/// Try decode one frame from the front of `buf`. On success advances buf.
/// Frames of a type this build does not know are skipped, so a newer peer
/// can add messages without breaking the session.
pub fn try_decode_frame(buf: &mut BytesMut) -> Result<Option<Frame>, FrameError> {
    loop {
        if buf.len() < HEADER_SIZE {
            return Ok(None);
        }
        let magic = u16::from_le_bytes([buf[0], buf[1]]);
        if magic != MAGIC {
            return Err(FrameError::BadMagic);
        }
        let version = buf[2];
        if version != VERSION {
            return Err(FrameError::BadVersion(version));
        }
        let flags = u32::from_le_bytes([buf[4], buf[5], buf[6], buf[7]]);
        let len = u32::from_le_bytes([buf[8], buf[9], buf[10], buf[11]]) as usize;
        if len > max_payload(buf[3]) {
            return Err(FrameError::TooLarge(len));
        }
        if buf.len() < HEADER_SIZE + len {
            return Ok(None);
        }
        let ty = MsgType::from_u8(buf[3]);
        buf.advance(HEADER_SIZE);
        let payload = buf.split_to(len).to_vec();
        if let Some(msg_type) = ty {
            return Ok(Some(Frame {
                version,
                msg_type,
                flags,
                payload,
            }));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_heartbeat() {
        let raw = encode_frame(MsgType::Heartbeat, b"hi", 0);
        let mut buf = BytesMut::from(&raw[..]);
        let f = try_decode_frame(&mut buf).unwrap().unwrap();
        assert_eq!(f.msg_type, MsgType::Heartbeat);
        assert_eq!(f.payload, b"hi");
        assert_eq!(f.version, VERSION);
        assert!(buf.is_empty());
    }

    #[test]
    fn version_1_is_rejected() {
        let mut raw = encode_frame(MsgType::Heartbeat, b"", 0);
        raw[2] = 1;
        let mut buf = BytesMut::from(&raw[..]);
        assert!(matches!(
            try_decode_frame(&mut buf),
            Err(FrameError::BadVersion(1))
        ));
    }

    #[test]
    fn oversized_control_frame_is_rejected() {
        let mut raw = encode_frame(MsgType::Key, b"", 0);
        let huge = (CONTROL_MAX as u32) + 1;
        raw[8..12].copy_from_slice(&huge.to_le_bytes());
        let mut buf = BytesMut::from(&raw[..]);
        assert!(matches!(
            try_decode_frame(&mut buf),
            Err(FrameError::TooLarge(_))
        ));
    }

    #[test]
    fn unknown_type_is_skipped() {
        let mut raw = encode_frame(MsgType::Heartbeat, b"x", 0);
        raw[3] = 0xEE;
        raw.extend(encode_frame(MsgType::Key, b"k", 0));
        let mut buf = BytesMut::from(&raw[..]);
        let f = try_decode_frame(&mut buf).unwrap().unwrap();
        assert_eq!(f.msg_type, MsgType::Key);
        assert!(buf.is_empty());
    }

    #[test]
    fn host_rejects_pointer_frames() {
        assert!(bad_direction(SessionRole::Host, MsgType::Key));
        assert!(!bad_direction(SessionRole::Host, MsgType::EdgeHit));
        assert!(bad_direction(SessionRole::Controlled, MsgType::EdgeHit));
        assert!(!bad_direction(SessionRole::Controlled, MsgType::Key));
    }
}
