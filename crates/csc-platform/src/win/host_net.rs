use crate::{HostNetInfo, LocalNic};
use std::ffi::OsString;
use std::os::windows::ffi::OsStringExt;
use windows::core::PWSTR;
use windows::Win32::NetworkManagement::IpHelper::{
    GetAdaptersAddresses, GAA_FLAG_INCLUDE_ALL_INTERFACES, GAA_FLAG_SKIP_ANYCAST,
    GAA_FLAG_SKIP_DNS_SERVER, GAA_FLAG_SKIP_MULTICAST, IF_TYPE_ETHERNET_CSMACD, IF_TYPE_IEEE80211,
    IP_ADAPTER_ADDRESSES_LH,
};
use windows::Win32::Networking::WinSock::{AF_INET, SOCKADDR_IN};
use windows::Win32::System::SystemInformation::{ComputerNameDnsHostname, GetComputerNameExW};

pub fn fill_host_net(info: &mut HostNetInfo) {
    unsafe {
        let mut buf = [0u16; 256];
        let mut n = buf.len() as u32;
        if GetComputerNameExW(ComputerNameDnsHostname, PWSTR(buf.as_mut_ptr()), &mut n).is_ok()
            && n > 0
        {
            let len = (n as usize).min(buf.len());
            let name = OsString::from_wide(&buf[..len]);
            if let Some(s) = name.to_str() {
                if !s.is_empty() {
                    info.host_name = s.to_string();
                }
            }
        }
    }

    let mut nics = Vec::new();
    unsafe {
        // Include adapters that are enabled but currently unplugged, so a configured
        // ethernet address such as 10.10.10.10 still appears.
        let flags = GAA_FLAG_SKIP_ANYCAST
            | GAA_FLAG_SKIP_MULTICAST
            | GAA_FLAG_SKIP_DNS_SERVER
            | GAA_FLAG_INCLUDE_ALL_INTERFACES;
        let mut size = 16 * 1024u32;
        for _ in 0..4 {
            let mut buf = vec![0u8; size as usize];
            let addrs = buf.as_mut_ptr() as *mut IP_ADAPTER_ADDRESSES_LH;
            let rc = GetAdaptersAddresses(AF_INET.0 as u32, flags, None, Some(addrs), &mut size);
            if rc == 111 {
                // ERROR_BUFFER_OVERFLOW: `size` now holds the required length.
                continue;
            }
            if rc != 0 {
                break;
            }
            let mut p = addrs;
            while !p.is_null() {
                let a = &*p;
                let kind = if a.IfType == IF_TYPE_ETHERNET_CSMACD {
                    Some("ethernet")
                } else if a.IfType == IF_TYPE_IEEE80211 {
                    Some("wifi")
                } else {
                    None
                };
                if let Some(kind) = kind {
                    let friendly = pwstr_to_string(a.FriendlyName);
                    let description = pwstr_to_string(a.Description);
                    if !crate::looks_virtual(&friendly, &description) {
                        let mut u = a.FirstUnicastAddress;
                        while !u.is_null() {
                            let ua = &*u;
                            if !ua.Address.lpSockaddr.is_null() {
                                let sa = ua.Address.lpSockaddr;
                                if (*sa).sa_family == AF_INET {
                                    let sin = &*(sa as *const SOCKADDR_IN);
                                    let ip = u32::from_be(sin.sin_addr.S_un.S_addr);
                                    let s = format!(
                                        "{}.{}.{}.{}",
                                        (ip >> 24) & 0xff,
                                        (ip >> 16) & 0xff,
                                        (ip >> 8) & 0xff,
                                        ip & 0xff
                                    );
                                    if crate::usable_unicast_v4(&s)
                                        && !nics.iter().any(|n: &LocalNic| n.address == s)
                                    {
                                        nics.push(LocalNic {
                                            address: s,
                                            kind: kind.to_string(),
                                            name: if friendly.is_empty() {
                                                description.clone()
                                            } else {
                                                friendly.clone()
                                            },
                                        });
                                    }
                                }
                            }
                            u = ua.Next;
                        }
                    }
                }
                p = a.Next;
            }
            break;
        }
    }
    info.set_nics(nics);
}

fn pwstr_to_string(p: PWSTR) -> String {
    if p.0.is_null() {
        return String::new();
    }
    unsafe {
        let mut len = 0usize;
        while len < 512 && *p.0.add(len) != 0 {
            len += 1;
        }
        String::from_utf16_lossy(std::slice::from_raw_parts(p.0, len))
    }
}
