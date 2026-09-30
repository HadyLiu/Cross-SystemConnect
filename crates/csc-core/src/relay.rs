//! Keyboard host relaying a batch between two controlled computers.
//!
//! The host never stores the files. Each batch gets a route
//! `(source, source id) <-> (target, relay id)`; frames are rewritten to the
//! other side's id and passed on. Data toward the target goes through one
//! bounded queue per route, and the host acknowledges the source as it hands
//! bytes to the target's link, so at most one ack window sits on the host.

use crate::net::NetHub;
use csc_proto::{
    encode_frame, encode_json_frame, set_file_data_id, FileAckPayload, MsgType, FLAG_JSON_PAYLOAD,
};
use std::time::{Duration, Instant};
use tokio::sync::mpsc;
use tracing::{debug, info};

/// Acknowledge the source after this many bytes reach the target's link.
pub const ACK_STEP: u64 = 1024 * 1024;
/// A route whose batch never started is dropped after this long.
const IDLE_ROUTE: Duration = Duration::from_secs(30 * 60);
/// Frames queued toward the target. The source's window keeps it well below this.
const ROUTE_QUEUE: usize = 512;

struct Route {
    src: String,
    src_id: u64,
    dst: String,
    rid: u64,
    tx: mpsc::Sender<Vec<u8>>,
    created: Instant,
}

/// Which side of a route a frame came from.
pub enum Hop {
    /// From the source; pass on to the target with the relay id.
    ToTarget { dst: String, rid: u64 },
    /// From the target; pass back to the source with its own id.
    ToSource { src: String, src_id: u64 },
}

#[derive(Default)]
pub struct Relay {
    routes: Vec<Route>,
    /// Pastes and drags the host started: (source, target), lower-case.
    allowed: Vec<(String, String, Instant)>,
}

fn same(a: &str, b: &str) -> bool {
    a.eq_ignore_ascii_case(b)
}

/// Replace `transfer_id` in a JSON payload.
pub fn rewrite_json(msg: MsgType, payload: &[u8], id: u64) -> Option<Vec<u8>> {
    let mut v: serde_json::Value = serde_json::from_slice(payload).ok()?;
    v.as_object_mut()?
        .insert("transfer_id".into(), serde_json::Value::from(id));
    let body = serde_json::to_vec(&v).ok()?;
    Some(encode_frame(msg, &body, FLAG_JSON_PAYLOAD))
}

/// A frame for the other side of the route, with that side's id.
pub fn rewrite(msg: MsgType, payload: &[u8], id: u64) -> Option<Vec<u8>> {
    if msg == MsgType::FileData {
        let mut body = payload.to_vec();
        set_file_data_id(&mut body, id).then(|| encode_frame(msg, &body, 0))
    } else {
        rewrite_json(msg, payload, id)
    }
}

impl Relay {
    pub fn allow(&mut self, src: &str, dst: &str) {
        self.allowed
            .retain(|(_, _, at)| at.elapsed() < Duration::from_secs(120));
        self.allowed
            .push((src.to_ascii_lowercase(), dst.to_ascii_lowercase(), Instant::now()));
    }

    /// Take a permission granted by `allow`.
    pub fn take_allowed(&mut self, src: &str, dst: &str) -> bool {
        let Some(i) = self.allowed.iter().position(|(s, d, at)| {
            same(s, src) && same(d, dst) && at.elapsed() < Duration::from_secs(120)
        }) else {
            return false;
        };
        self.allowed.remove(i);
        true
    }

    /// New route from `src`'s batch `src_id` to `dst`, identified there by `rid`.
    pub fn open(&mut self, net: &NetHub, src: &str, src_id: u64, dst: &str, rid: u64) {
        self.routes.retain(|r| r.created.elapsed() < IDLE_ROUTE);
        let (tx, rx) = mpsc::channel(ROUTE_QUEUE);
        info!("relaying batch {src_id} from {src} to {dst} as {rid}");
        tokio::spawn(forward(
            net.clone(),
            src.to_string(),
            src_id,
            dst.to_string(),
            rx,
        ));
        self.routes.push(Route {
            src: src.to_string(),
            src_id,
            dst: dst.to_string(),
            rid,
            tx,
            created: Instant::now(),
        });
    }

    /// Where a frame from `peer` about `id` goes, if it belongs to a route.
    pub fn hop(&self, peer: &str, id: u64) -> Option<Hop> {
        self.routes.iter().find_map(|r| {
            if same(&r.src, peer) && r.src_id == id {
                Some(Hop::ToTarget {
                    dst: r.dst.clone(),
                    rid: r.rid,
                })
            } else if same(&r.dst, peer) && r.rid == id {
                Some(Hop::ToSource {
                    src: r.src.clone(),
                    src_id: r.src_id,
                })
            } else {
                None
            }
        })
    }

    /// Queue a frame for the target, in order with the data. False when the queue is full.
    pub fn push(&self, rid: u64, frame: Vec<u8>) -> bool {
        self.routes
            .iter()
            .find(|r| r.rid == rid)
            .is_some_and(|r| r.tx.try_send(frame).is_ok())
    }

    pub fn close(&mut self, rid: u64) {
        self.routes.retain(|r| r.rid != rid);
    }

    /// Routes that go through `peer`, removed. Each is (other side, id there).
    pub fn drop_peer(&mut self, peer: &str) -> Vec<(String, u64)> {
        let mut other = Vec::new();
        self.routes.retain(|r| {
            if same(&r.src, peer) {
                other.push((r.dst.clone(), r.rid));
                false
            } else if same(&r.dst, peer) {
                other.push((r.src.clone(), r.src_id));
                false
            } else {
                true
            }
        });
        other
    }
}

async fn forward(
    net: NetHub,
    src: String,
    src_id: u64,
    dst: String,
    mut rx: mpsc::Receiver<Vec<u8>>,
) {
    let mut forwarded = 0u64;
    let mut acked = 0u64;
    while let Some(frame) = rx.recv().await {
        let data = if frame.get(3) == Some(&(MsgType::FileData as u8)) {
            frame.len().saturating_sub(csc_proto::HEADER_SIZE + 12) as u64
        } else {
            0
        };
        let last = matches!(
            frame.get(3).copied(),
            Some(t) if t == MsgType::FileDone as u8 || t == MsgType::FileError as u8
        );
        if !net.send_bulk(&dst, frame).await {
            debug!("relay target {dst} gone");
            break;
        }
        forwarded += data;
        if forwarded - acked >= ACK_STEP {
            acked = forwarded;
            let ack = FileAckPayload {
                transfer_id: src_id,
                bytes: forwarded,
            };
            if let Ok(f) = encode_json_frame(MsgType::FileAck, &ack) {
                net.send_control(&src, f).await;
            }
        }
        if last {
            break;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use csc_proto::{decode_file_data, encode_file_data, try_decode_frame, FileRequestPayload};

    #[test]
    fn json_and_data_frames_get_the_other_id() {
        let req = serde_json::to_vec(&FileRequestPayload {
            transfer_id: 5,
            skip: vec![2],
        })
        .unwrap();
        let out = rewrite(MsgType::FileRequest, &req, 99).unwrap();
        let mut buf = bytes::BytesMut::from(&out[..]);
        let f = try_decode_frame(&mut buf).unwrap().unwrap();
        let back: FileRequestPayload = serde_json::from_slice(&f.payload).unwrap();
        assert_eq!((back.transfer_id, back.skip), (99, vec![2]));

        let data = encode_file_data(5, 3, b"abc");
        let payload = &data[csc_proto::HEADER_SIZE..];
        let out = rewrite(MsgType::FileData, payload, 77).unwrap();
        let (id, index, bytes) = decode_file_data(&out[csc_proto::HEADER_SIZE..]).unwrap();
        assert_eq!((id, index, bytes), (77, 3, &b"abc"[..]));
    }

    #[test]
    fn permissions_are_single_use() {
        let mut relay = Relay::default();
        relay.allow("B", "C");
        assert!(!relay.take_allowed("C", "B"));
        assert!(relay.take_allowed("b", "c"));
        assert!(!relay.take_allowed("B", "C"));
    }
}
