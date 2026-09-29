//! Host name and local IPv4 discovery.

#[derive(Debug, Clone, Default)]
pub struct HostNetInfo {
    pub host_name: String,
    pub ipv4_addresses: Vec<String>,
}

pub fn query_host_net_info() -> HostNetInfo {
    let mut info = HostNetInfo {
        host_name: hostname_fallback(),
        ipv4_addresses: Vec::new(),
    };

    #[cfg(windows)]
    {
        crate::win::fill_host_net(&mut info);
    }
    #[cfg(unix)]
    {
        crate::unix_host::fill_host_net(&mut info);
    }

    info
}

fn hostname_fallback() -> String {
    std::env::var("COMPUTERNAME")
        .or_else(|_| std::env::var("HOSTNAME"))
        .unwrap_or_else(|_| "PC".into())
}

#[cfg(unix)]
mod unix_host {
    use super::HostNetInfo;
    use std::ffi::CStr;

    pub fn fill_host_net(info: &mut HostNetInfo) {
        let mut buf = [0u8; 256];
        // SAFETY: gethostname writes into buf
        let rc = unsafe { libc::gethostname(buf.as_mut_ptr() as *mut _, buf.len()) };
        if rc == 0 {
            if let Ok(s) = CStr::from_bytes_until_nul(&buf) {
                if let Ok(name) = s.to_str() {
                    if !name.is_empty() {
                        info.host_name = name.to_string();
                    }
                }
            }
        }
        // Enumerate IPv4 via getifaddrs
        unsafe {
            let mut ifap: *mut libc::ifaddrs = std::ptr::null_mut();
            if libc::getifaddrs(&mut ifap) != 0 {
                return;
            }
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
                    if s != "127.0.0.1" && !info.ipv4_addresses.contains(&s) {
                        info.ipv4_addresses.push(s);
                    }
                }
                p = ifa.ifa_next;
            }
            libc::freeifaddrs(ifap);
        }
    }
}
