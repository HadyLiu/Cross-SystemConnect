//! UDP discovery on the control port.
//!
//! The client sends an announce to that port. The server searches the same port,
//! then connects to the client over TCP after the user allows it.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::time::{Duration, Instant};

pub const REPROMPT_GAP: Duration = Duration::from_secs(4);

const MAGIC: &[u8] = b"CSCD";
const VERSION: u8 = 2;
const KIND_PROBE: u8 = 1;
const KIND_REPLY: u8 = 2;
const KIND_ANNOUNCE: u8 = 3;
const KIND_PING: u8 = 4;
const KIND_PONG: u8 = 5;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Announce {
    pub name: String,
    /// Same as `name`. Kept so callers can match the computer name.
    pub host_name: String,
    pub control_port: u16,
    pub data_port: u16,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Echo {
    pub name: String,
    pub control_port: u16,
    pub data_port: u16,
    pub nonce: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Datagram {
    Announce(Announce),
    /// Host scan. Carries the host's control and data ports.
    Search(Announce),
    /// Unicast RTT probe on the discovery socket.
    Ping(Echo),
    Pong(Echo),
}

#[derive(Debug, Clone)]
pub struct RejectMark {
    pub last: Instant,
}

pub fn reject_mark(now: Instant) -> RejectMark {
    RejectMark { last: now }
}

/// After reject, further announces are ignored until the sender goes quiet.
/// Returns true when this packet should be ignored.
pub fn suppress_repeat(mark: &mut RejectMark, now: Instant) -> bool {
    let gap = now.saturating_duration_since(mark.last);
    mark.last = now;
    gap < REPROMPT_GAP
}

pub fn encode_search(control_port: u16, data_port: u16, name: &str) -> Vec<u8> {
    encode_packet(KIND_PROBE, control_port, data_port, name)
}

/// A probe. The controlled computer answers with a reply.
pub fn encode_search_unlinked(control_port: u16, data_port: u16, name: &str) -> Vec<u8> {
    encode_search(control_port, data_port, name)
}

pub fn encode_announce(a: &Announce) -> Vec<u8> {
    let name = if a.name.is_empty() {
        &a.host_name
    } else {
        &a.name
    };
    encode_packet(KIND_REPLY, a.control_port, a.data_port, name)
}

#[cfg(test)]
pub fn encode_port_announce(a: &Announce) -> Vec<u8> {
    let name = if a.name.is_empty() {
        &a.host_name
    } else {
        &a.name
    };
    encode_packet(KIND_ANNOUNCE, a.control_port, a.data_port, name)
}

fn encode_packet(kind: u8, control_port: u16, data_port: u16, name: &str) -> Vec<u8> {
    let name = clip_name(name);
    let mut out = Vec::with_capacity(11 + name.len());
    out.extend_from_slice(MAGIC);
    out.push(VERSION);
    out.push(kind);
    out.extend_from_slice(&control_port.to_le_bytes());
    out.extend_from_slice(&data_port.to_le_bytes());
    out.push(name.len() as u8);
    out.extend_from_slice(name);
    out
}

/// Discovery-channel RTT probe. `nonce` is echoed by the peer in a pong.
pub fn encode_ping(control_port: u16, data_port: u16, name: &str, nonce: u64) -> Vec<u8> {
    encode_echo(KIND_PING, control_port, data_port, name, nonce)
}

pub fn encode_pong(control_port: u16, data_port: u16, name: &str, nonce: u64) -> Vec<u8> {
    encode_echo(KIND_PONG, control_port, data_port, name, nonce)
}

fn encode_echo(kind: u8, control_port: u16, data_port: u16, name: &str, nonce: u64) -> Vec<u8> {
    let mut out = encode_packet(kind, control_port, data_port, name);
    out.extend_from_slice(&nonce.to_le_bytes());
    out
}

pub fn decode_datagram(buf: &[u8]) -> Option<Datagram> {
    if buf.len() < 11 || &buf[0..4] != MAGIC || buf[4] != VERSION {
        return None;
    }
    let control_port = u16::from_le_bytes([buf[6], buf[7]]);
    let data_port = u16::from_le_bytes([buf[8], buf[9]]);
    if control_port == 0 || data_port == 0 || control_port == data_port {
        return None;
    }
    let name_len = buf[10] as usize;
    if name_len > 63 || buf.len() < 11 + name_len {
        return None;
    }
    let name = std::str::from_utf8(&buf[11..11 + name_len])
        .ok()?
        .to_string();
    let rest = &buf[11 + name_len..];
    match buf[5] {
        KIND_PROBE => Some(Datagram::Search(Announce {
            host_name: name.clone(),
            name,
            control_port,
            data_port,
        })),
        KIND_REPLY | KIND_ANNOUNCE => Some(Datagram::Announce(Announce {
            host_name: name.clone(),
            name,
            control_port,
            data_port,
        })),
        KIND_PING | KIND_PONG if rest.len() >= 8 => {
            let nonce = u64::from_le_bytes(rest[..8].try_into().ok()?);
            let echo = Echo {
                name,
                control_port,
                data_port,
                nonce,
            };
            if buf[5] == KIND_PING {
                Some(Datagram::Ping(echo))
            } else {
                Some(Datagram::Pong(echo))
            }
        }
        _ => None,
    }
}

fn clip_name(name: &str) -> &[u8] {
    let bytes = name.as_bytes();
    if bytes.len() <= 63 {
        return bytes;
    }
    let mut n = 63;
    while n > 0 && !name.is_char_boundary(n) {
        n -= 1;
    }
    &bytes[..n]
}

pub fn ipv4_octets(ip: &str) -> Option<[u8; 4]> {
    let mut p = [0u8; 4];
    let parts: Vec<&str> = ip.split('.').collect();
    if parts.len() != 4 {
        return None;
    }
    for (i, part) in parts.iter().enumerate() {
        p[i] = part.parse().ok()?;
    }
    if p[0] == 127 {
        return None;
    }
    Some(p)
}

pub fn same_subnet24(a: &str, b: &str) -> bool {
    match (ipv4_octets(a), ipv4_octets(b)) {
        (Some(x), Some(y)) => x[0] == y[0] && x[1] == y[1] && x[2] == y[2],
        _ => false,
    }
}

pub fn subnet_label(ip: &str) -> Option<String> {
    let p = ipv4_octets(ip)?;
    Some(format!("{}.{}.{}.0/24", p[0], p[1], p[2]))
}

/// Directed broadcast plus every other host on the /24 of `selected`.
pub fn subnet_scan_targets(selected: &str, port: u16) -> Vec<SocketAddr> {
    let Some(p) = ipv4_octets(selected) else {
        return Vec::new();
    };
    let mut out = Vec::with_capacity(255);
    out.push(SocketAddr::new(
        IpAddr::V4(Ipv4Addr::new(p[0], p[1], p[2], 255)),
        port,
    ));
    for host in 1..255 {
        if host == p[3] {
            continue;
        }
        out.push(SocketAddr::new(
            IpAddr::V4(Ipv4Addr::new(p[0], p[1], p[2], host)),
            port,
        ));
    }
    out
}

pub fn broadcast_targets(port: u16, local_ips: &[String]) -> Vec<SocketAddr> {
    let mut out = vec![SocketAddr::new(IpAddr::V4(Ipv4Addr::BROADCAST), port)];
    for ip in local_ips {
        if let Some(b) = slash24_broadcast(ip) {
            let sa = SocketAddr::new(IpAddr::V4(b), port);
            if !out.contains(&sa) {
                out.push(sa);
            }
        }
    }
    out
}

pub fn is_own_announce(
    host_name: &str,
    local_host: &str,
    src_ip: &str,
    local_ips: &[String],
) -> bool {
    if !local_host.is_empty() && host_name.eq_ignore_ascii_case(local_host) {
        return true;
    }
    local_ips.iter().any(|ip| ip == src_ip)
}

fn slash24_broadcast(ip: &str) -> Option<Ipv4Addr> {
    let mut p = [0u8; 4];
    let parts: Vec<&str> = ip.split('.').collect();
    if parts.len() != 4 {
        return None;
    }
    for (i, part) in parts.iter().enumerate() {
        p[i] = part.parse().ok()?;
    }
    if p[0] == 127 {
        return None;
    }
    Some(Ipv4Addr::new(p[0], p[1], p[2], 255))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn announce_roundtrip() {
        let a = Announce {
            name: "Desk".into(),
            host_name: "OFFICE-PC".into(),
            control_port: 24900,
            data_port: 24901,
        };
        match decode_datagram(&encode_announce(&a)) {
            Some(Datagram::Announce(got)) => {
                assert_eq!(got.control_port, a.control_port);
                assert_eq!(got.data_port, a.data_port);
                assert_eq!(got.name, "Desk");
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn search_packet() {
        let raw = encode_search(24900, 24901, "host");
        match decode_datagram(&raw) {
            Some(Datagram::Search(a)) => {
                assert_eq!(a.control_port, 24900);
                assert_eq!(a.data_port, 24901);
                assert_eq!(a.name, "host");
            }
            other => panic!("unexpected {other:?}"),
        }
        assert!(decode_datagram(&encode_search(24900, 24900, "host")).is_none());
    }

    #[test]
    fn port_change_announce_is_accepted() {
        let a = Announce {
            name: "Desk".into(),
            host_name: String::new(),
            control_port: 25000,
            data_port: 25001,
        };
        match decode_datagram(&encode_port_announce(&a)) {
            Some(Datagram::Announce(got)) => assert_eq!(got.control_port, 25000),
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn ping_pong_roundtrip() {
        let raw = encode_ping(24900, 24901, "host", 0xAABB_CCDD_1122_3344);
        match decode_datagram(&raw) {
            Some(Datagram::Ping(e)) => {
                assert_eq!(e.nonce, 0xAABB_CCDD_1122_3344);
                assert_eq!(e.name, "host");
                let pong = encode_pong(e.control_port, e.data_port, &e.name, e.nonce);
                match decode_datagram(&pong) {
                    Some(Datagram::Pong(p)) => assert_eq!(p.nonce, e.nonce),
                    other => panic!("unexpected {other:?}"),
                }
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn reject_suppresses_until_gap() {
        let t0 = Instant::now();
        let mut mark = reject_mark(t0);
        assert!(suppress_repeat(&mut mark, t0 + Duration::from_secs(1)));
        assert!(!suppress_repeat(
            &mut mark,
            t0 + Duration::from_secs(1) + REPROMPT_GAP
        ));
    }

    #[test]
    fn broadcast_includes_global_and_subnet() {
        let targets = broadcast_targets(24900, &["192.168.1.20".into(), "127.0.0.1".into()]);
        assert!(targets
            .iter()
            .any(|a| a.ip().is_multicast() || a.ip().to_string() == "255.255.255.255"));
        assert!(targets
            .iter()
            .any(|a| a.ip().to_string() == "192.168.1.255"));
        assert!(!targets
            .iter()
            .any(|a| a.ip().to_string().starts_with("127.")));
    }

    #[test]
    fn subnet_scan_stays_on_selected_lan() {
        let targets = subnet_scan_targets("10.10.10.10", 24900);
        assert!(targets.iter().any(|a| a.ip().to_string() == "10.10.10.255"));
        assert!(targets.iter().any(|a| a.ip().to_string() == "10.10.10.1"));
        assert!(targets.iter().any(|a| a.ip().to_string() == "10.10.10.254"));
        assert!(!targets.iter().any(|a| a.ip().to_string() == "10.10.10.10"));
        assert!(!targets
            .iter()
            .any(|a| !a.ip().to_string().starts_with("10.10.10.")));
        assert!(same_subnet24("10.10.10.10", "10.10.10.20"));
        assert!(!same_subnet24("10.10.10.10", "192.168.1.8"));
        assert_eq!(
            subnet_label("10.10.10.10").as_deref(),
            Some("10.10.10.0/24")
        );
    }

    #[test]
    fn own_host_is_filtered() {
        assert!(is_own_announce(
            "Office",
            "office",
            "10.0.0.2",
            &["10.0.0.1".into()]
        ));
        assert!(is_own_announce(
            "Other",
            "Office",
            "10.0.0.1",
            &["10.0.0.1".into()]
        ));
        assert!(!is_own_announce(
            "Other",
            "Office",
            "10.0.0.8",
            &["10.0.0.1".into()]
        ));
    }
}
