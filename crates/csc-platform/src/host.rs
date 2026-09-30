//! Host name and local IPv4 discovery.

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalNic {
    pub address: String,
    /// `ethernet` or `wifi`.
    pub kind: String,
    /// Adapter friendly name. Not the computer name.
    pub name: String,
}

#[derive(Debug, Clone, Default)]
pub struct HostNetInfo {
    pub host_name: String,
    pub ipv4_addresses: Vec<String>,
    pub nics: Vec<LocalNic>,
}

impl HostNetInfo {
    pub fn set_nics(&mut self, mut nics: Vec<LocalNic>) {
        nics.retain(|n| usable_unicast_v4(&n.address));
        nics.sort_by_key(|nic| nic_rank(&nic.kind));
        self.ipv4_addresses = nics.iter().map(|n| n.address.clone()).collect();
        self.nics = nics;
    }
}

fn nic_rank(kind: &str) -> u8 {
    if kind == "ethernet" {
        0
    } else if kind == "wifi" {
        1
    } else {
        2
    }
}

/// Skip loopback and APIPA. A configured address counts even when the link is down.
pub fn usable_unicast_v4(ip: &str) -> bool {
    let Some(p) = parse_v4(ip) else {
        return false;
    };
    p[0] != 0 && p[0] != 127 && !(p[0] == 169 && p[1] == 254)
}

pub fn looks_virtual(name: &str, description: &str) -> bool {
    let s = format!("{name} {description}").to_ascii_lowercase();
    [
        "virtual",
        "hyper-v",
        "vethernet",
        "vmware",
        "virtualbox",
        "vbox",
        "wsl",
        "docker",
        "loopback",
        "bluetooth",
        "tap-windows",
        "vpn",
        "zerotier",
        "tailscale",
        "wireguard",
        "pseudo",
        "npcap",
        "wan miniport",
    ]
    .iter()
    .any(|k| s.contains(k))
}

fn parse_v4(ip: &str) -> Option<[u8; 4]> {
    let mut out = [0u8; 4];
    let mut parts = ip.split('.');
    for slot in &mut out {
        let p = parts.next()?;
        if p.is_empty() || (p.len() > 1 && p.starts_with('0')) {
            return None;
        }
        *slot = p.parse().ok()?;
    }
    if parts.next().is_some() {
        return None;
    }
    Some(out)
}

pub fn query_host_net_info() -> HostNetInfo {
    let mut info = HostNetInfo {
        host_name: csc_config::local_computer_name(),
        ipv4_addresses: Vec::new(),
        nics: Vec::new(),
    };

    #[cfg(windows)]
    {
        crate::win::fill_host_net(&mut info);
    }
    #[cfg(unix)]
    {
        unix_host::fill_host_net(&mut info);
    }

    info
}

#[cfg(unix)]
mod unix_host {
    use super::{looks_virtual, usable_unicast_v4, HostNetInfo, LocalNic};
    use std::ffi::CStr;

    pub fn fill_host_net(info: &mut HostNetInfo) {
        // Enumerate IPv4 via getifaddrs. The computer name is already set.
        unsafe {
            let mut ifap: *mut libc::ifaddrs = std::ptr::null_mut();
            if libc::getifaddrs(&mut ifap) != 0 {
                return;
            }
            let mut nics = Vec::new();
            let mut p = ifap;
            while !p.is_null() {
                let ifa = &*p;
                if !ifa.ifa_addr.is_null() && (*ifa.ifa_addr).sa_family as i32 == libc::AF_INET {
                    let sin = &*(ifa.ifa_addr as *const libc::sockaddr_in);
                    let ip = u32::from_be(sin.sin_addr.s_addr);
                    let a = ((ip >> 24) & 0xff) as u8;
                    let b = ((ip >> 16) & 0xff) as u8;
                    let c = ((ip >> 8) & 0xff) as u8;
                    let d = (ip & 0xff) as u8;
                    let s = format!("{a}.{b}.{c}.{d}");
                    if !usable_unicast_v4(&s) {
                        p = ifa.ifa_next;
                        continue;
                    }
                    let name = if ifa.ifa_name.is_null() {
                        String::new()
                    } else {
                        CStr::from_ptr(ifa.ifa_name).to_string_lossy().into_owned()
                    };
                    let lname = name.to_ascii_lowercase();
                    if lname == "lo"
                        || lname.starts_with("docker")
                        || lname.starts_with("veth")
                        || lname.starts_with("br-")
                        || lname.starts_with("virbr")
                        || lname.starts_with("tun")
                        || lname.starts_with("tap")
                        || looks_virtual(&name, "")
                    {
                        p = ifa.ifa_next;
                        continue;
                    }
                    let kind = if lname.starts_with("wl") || lname.starts_with("wifi") {
                        "wifi"
                    } else {
                        "ethernet"
                    };
                    if !nics.iter().any(|n: &LocalNic| n.address == s) {
                        nics.push(LocalNic {
                            address: s,
                            kind: kind.to_string(),
                            name,
                        });
                    }
                }
                p = ifa.ifa_next;
            }
            libc::freeifaddrs(ifap);
            info.set_nics(nics);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ethernet_is_listed_before_wifi_and_virtual_is_rejected() {
        assert!(usable_unicast_v4("10.10.10.10"));
        assert!(usable_unicast_v4("192.168.0.88"));
        assert!(!usable_unicast_v4("127.0.0.1"));
        assert!(!usable_unicast_v4("169.254.1.2"));
        assert!(looks_virtual(
            "vEthernet (WSL)",
            "Hyper-V Virtual Ethernet Adapter"
        ));
        assert!(looks_virtual(
            "以太网 4",
            "VirtualBox Host-Only Ethernet Adapter"
        ));
        assert!(!looks_virtual("以太网", "Intel(R) Ethernet Connection"));
        assert!(!looks_virtual("WLAN", "Intel(R) Wi-Fi 6 AX201"));

        let mut info = HostNetInfo::default();
        info.set_nics(vec![
            LocalNic {
                address: "192.168.0.88".into(),
                kind: "wifi".into(),
                name: "WLAN".into(),
            },
            LocalNic {
                address: "169.254.1.2".into(),
                kind: "ethernet".into(),
                name: "以太网".into(),
            },
            LocalNic {
                address: "10.10.10.10".into(),
                kind: "ethernet".into(),
                name: "以太网".into(),
            },
        ]);
        assert_eq!(
            info.ipv4_addresses,
            vec!["10.10.10.10".to_string(), "192.168.0.88".to_string()]
        );
        assert_eq!(info.nics[0].kind, "ethernet");
        assert_eq!(info.nics[1].kind, "wifi");
    }
}
