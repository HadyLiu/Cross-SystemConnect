//! UDP discovery on the control port.
//!
//! The client sends an announce to that port. The server searches the same port,
//! then connects to the client over TCP after the user allows it.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::time::{Duration, Instant};

pub const REPROMPT_GAP: Duration = Duration::from_secs(4);

const MAGIC: &[u8] = b"CSCD";
const VERSION: u8 = 1;
const KIND_ANNOUNCE: u8 = 1;
const KIND_SEARCH: u8 = 2;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Announce {
    pub name: String,
    pub host_name: String,
    pub tcp_port: u16,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Datagram {
    Announce(Announce),
    Search,
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

pub fn encode_search() -> Vec<u8> {
    let mut out = Vec::with_capacity(6);
    out.extend_from_slice(MAGIC);
    out.push(VERSION);
    out.push(KIND_SEARCH);
    out
}

pub fn encode_announce(a: &Announce) -> Vec<u8> {
    let mut out = Vec::with_capacity(6 + a.name.len() + a.host_name.len() + 6);
    out.extend_from_slice(MAGIC);
    out.push(VERSION);
    out.push(KIND_ANNOUNCE);
    write_str(&mut out, &a.name);
    write_str(&mut out, &a.host_name);
    out.extend_from_slice(&a.tcp_port.to_le_bytes());
    out
}

pub fn decode_datagram(buf: &[u8]) -> Option<Datagram> {
    if buf.len() < 6 || &buf[0..4] != MAGIC || buf[4] != VERSION {
        return None;
    }
    match buf[5] {
        KIND_SEARCH => Some(Datagram::Search),
        KIND_ANNOUNCE => {
            let mut off = 6;
            let name = read_str(buf, &mut off)?;
            let host_name = read_str(buf, &mut off)?;
            if off + 2 > buf.len() {
                return None;
            }
            let tcp_port = u16::from_le_bytes([buf[off], buf[off + 1]]);
            if name.is_empty() && host_name.is_empty() {
                return None;
            }
            Some(Datagram::Announce(Announce {
                name,
                host_name,
                tcp_port,
            }))
        }
        _ => None,
    }
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

pub fn is_own_announce(host_name: &str, local_host: &str, src_ip: &str, local_ips: &[String]) -> bool {
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

fn write_str(out: &mut Vec<u8>, s: &str) {
    let b = s.as_bytes();
    let n = u16::try_from(b.len()).unwrap_or(u16::MAX) as usize;
    let n = n.min(b.len());
    out.extend_from_slice(&(n as u16).to_le_bytes());
    out.extend_from_slice(&b[..n]);
}

fn read_str(buf: &[u8], off: &mut usize) -> Option<String> {
    if *off + 2 > buf.len() {
        return None;
    }
    let n = u16::from_le_bytes([buf[*off], buf[*off + 1]]) as usize;
    *off += 2;
    if n > 512 || *off + n > buf.len() {
        return None;
    }
    let s = std::str::from_utf8(&buf[*off..*off + n]).ok()?.to_string();
    *off += n;
    Some(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn announce_roundtrip() {
        let a = Announce {
            name: "Desk".into(),
            host_name: "OFFICE-PC".into(),
            tcp_port: 24900,
        };
        match decode_datagram(&encode_announce(&a)) {
            Some(Datagram::Announce(got)) => assert_eq!(got, a),
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn search_packet() {
        assert_eq!(decode_datagram(&encode_search()), Some(Datagram::Search));
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
        assert!(targets.iter().any(|a| a.ip().is_multicast() || a.ip().to_string() == "255.255.255.255"));
        assert!(targets.iter().any(|a| a.ip().to_string() == "192.168.1.255"));
        assert!(!targets.iter().any(|a| a.ip().to_string().starts_with("127.")));
    }

    #[test]
    fn subnet_scan_stays_on_selected_lan() {
        let targets = subnet_scan_targets("10.10.10.10", 24900);
        assert!(targets.iter().any(|a| a.ip().to_string() == "10.10.10.255"));
        assert!(targets.iter().any(|a| a.ip().to_string() == "10.10.10.1"));
        assert!(targets.iter().any(|a| a.ip().to_string() == "10.10.10.254"));
        assert!(!targets.iter().any(|a| a.ip().to_string() == "10.10.10.10"));
        assert!(!targets.iter().any(|a| !a.ip().to_string().starts_with("10.10.10.")));
        assert!(same_subnet24("10.10.10.10", "10.10.10.20"));
        assert!(!same_subnet24("10.10.10.10", "192.168.1.8"));
        assert_eq!(subnet_label("10.10.10.10").as_deref(), Some("10.10.10.0/24"));
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
