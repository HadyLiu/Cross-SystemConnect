use crate::HostNetInfo;
use std::ffi::OsString;
use std::os::windows::ffi::OsStringExt;
use windows::core::PWSTR;
use windows::Win32::NetworkManagement::IpHelper::{
    GetAdaptersAddresses, GAA_FLAG_SKIP_ANYCAST, GAA_FLAG_SKIP_DNS_SERVER, GAA_FLAG_SKIP_MULTICAST,
    IF_TYPE_SOFTWARE_LOOPBACK, IP_ADAPTER_ADDRESSES_LH,
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

    unsafe {
        let flags = GAA_FLAG_SKIP_ANYCAST | GAA_FLAG_SKIP_MULTICAST | GAA_FLAG_SKIP_DNS_SERVER;
        let mut size = 0u32;
        let _ = GetAdaptersAddresses(AF_INET.0 as u32, flags, None, None, &mut size);
        if size == 0 {
            return;
        }
        let mut buf = vec![0u8; size as usize];
        let addrs = buf.as_mut_ptr() as *mut IP_ADAPTER_ADDRESSES_LH;
        let rc = GetAdaptersAddresses(AF_INET.0 as u32, flags, None, Some(addrs), &mut size);
        if rc != 0 {
            return;
        }
        let mut p = addrs;
        while !p.is_null() {
            let a = &*p;
            // IfOperStatusUp == 1
            if a.OperStatus.0 == 1 && a.IfType != IF_TYPE_SOFTWARE_LOOPBACK {
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
                            if s != "127.0.0.1" && !info.ipv4_addresses.contains(&s) {
                                info.ipv4_addresses.push(s);
                            }
                        }
                    }
                    u = ua.Next;
                }
            }
            p = a.Next;
        }
    }
}
