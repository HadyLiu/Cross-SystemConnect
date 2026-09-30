use bytes::{Buf, BufMut, BytesMut};
use thiserror::Error;

pub const MAGIC: u16 = 0x5343; // 'C''S' LE
pub const VERSION: u8 = 1;
pub const HEADER_SIZE: usize = 12;

#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MsgType {
    Hello = 0x01,
    Heartbeat = 0x02,
    SwitchEnter = 0x10,
    SwitchLeave = 0x11,
    /// Controlled computer to keyboard host: its real pointer is pressing an outer edge.
    EdgeHit = 0x12,
    MouseMove = 0x20,
    MouseButton = 0x21,
    MouseWheel = 0x22,
    Key = 0x23,
    ClipOffer = 0x30,
    ClipRequest = 0x31,
    ClipData = 0x32,
    /// Keyboard host to the computer that copied files: send them for pasting.
    PasteRequest = 0x33,
    DragBegin = 0x40,
    DragCancel = 0x41,
    FileRequest = 0x50,
    FileData = 0x51,
    FileEnd = 0x52,
    FileCancel = 0x53,
    FileError = 0x54,
    FileOffer = 0x55,
    FileBegin = 0x56,
    FileDone = 0x57,
    FileAck = 0x58,
    /// Lock the screen. The keyboard host passes it on to its other computers.
    LockScreen = 0x60,
}

/// Peer supports dragging a virtual file onto its desktop (`DragBegin`).
pub const CAP_VIRTUAL_DRAG: u32 = 1 << 0;
/// Peer reports edge contact itself (`EdgeHit`) instead of the host guessing.
pub const CAP_EDGE_REPORT: u32 = 1 << 1;
/// Peer acknowledges received file bytes (`FileAck`).
pub const CAP_FILE_ACK: u32 = 1 << 2;
/// Everything this build supports.
pub const CAPABILITIES: u32 = CAP_VIRTUAL_DRAG | CAP_EDGE_REPORT | CAP_FILE_ACK;

impl MsgType {
    pub fn from_u8(v: u8) -> Option<Self> {
        Some(match v {
            0x01 => Self::Hello,
            0x02 => Self::Heartbeat,
            0x10 => Self::SwitchEnter,
            0x11 => Self::SwitchLeave,
            0x12 => Self::EdgeHit,
            0x20 => Self::MouseMove,
            0x21 => Self::MouseButton,
            0x22 => Self::MouseWheel,
            0x23 => Self::Key,
            0x30 => Self::ClipOffer,
            0x31 => Self::ClipRequest,
            0x32 => Self::ClipData,
            0x33 => Self::PasteRequest,
            0x40 => Self::DragBegin,
            0x41 => Self::DragCancel,
            0x50 => Self::FileRequest,
            0x51 => Self::FileData,
            0x52 => Self::FileEnd,
            0x53 => Self::FileCancel,
            0x54 => Self::FileError,
            0x55 => Self::FileOffer,
            0x56 => Self::FileBegin,
            0x57 => Self::FileDone,
            0x58 => Self::FileAck,
            0x60 => Self::LockScreen,
            _ => return None,
        })
    }
}

pub const FLAG_JSON_PAYLOAD: u32 = 1;

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

/// Largest payload accepted; a bigger length means a corrupt stream.
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
        if len > MAX_PAYLOAD {
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
        assert!(buf.is_empty());
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
}
