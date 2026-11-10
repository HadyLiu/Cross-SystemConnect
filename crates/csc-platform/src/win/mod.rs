mod capture;
mod dark_mode;
pub mod hook;
mod host_net;
mod injector;
mod paste_dir;
pub mod power;
pub mod screen;

pub use capture::WinInputCapture;
pub use dark_mode::set as set_popup_menu_dark;
pub use host_net::fill_host_net;
pub use host_net::ipv4_if_index;
pub use injector::WinInputInjector;
pub use paste_dir::paste_folder;
pub use screen::cursor_pos;
pub use screen::init_dpi_awareness;
pub use screen::primary_button_down;
pub use screen::set_cursor_visible;
pub use screen::virtual_origin;
pub use screen::warp_cursor;
pub use screen::WinScreenInfo;

/// Send IPv4 unicast out `if_index` instead of the default route.
/// Clash's Wintun adapter installs that default route while its window is
/// open, even when the system proxy switch is off.
pub fn pin_ipv4_interface(raw: std::os::windows::io::RawSocket, if_index: u32) {
    use windows::Win32::Networking::WinSock::SOCKET;
    pin_socket(SOCKET(raw as usize), if_index);
}

fn pin_socket(sock: windows::Win32::Networking::WinSock::SOCKET, if_index: u32) {
    if if_index == 0 {
        return;
    }
    use windows::Win32::Networking::WinSock::{setsockopt, WSAGetLastError};
    const IPPROTO_IP: i32 = 0;
    const IP_UNICAST_IF: i32 = 31;
    let value = if_index.to_be();
    let bytes = value.to_ne_bytes();
    unsafe {
        if setsockopt(sock, IPPROTO_IP, IP_UNICAST_IF, Some(&bytes)) != 0 {
            tracing::warn!(
                "pin egress to interface {if_index}: {}",
                WSAGetLastError().0
            );
        }
    }
}

/// Connect `remote` from `local`, forcing egress onto the NIC that owns `local`.
pub fn connect_from(
    local: std::net::Ipv4Addr,
    remote: std::net::SocketAddr,
    timeout: std::time::Duration,
) -> std::io::Result<std::net::TcpStream> {
    use std::os::windows::io::FromRawSocket;
    use windows::Win32::Networking::WinSock::{
        bind, closesocket, connect, setsockopt, socket, WSAGetLastError, AF_INET, IPPROTO_TCP,
        SOCKADDR, SOCKADDR_IN, SOCK_STREAM, SOL_SOCKET, SO_SNDTIMEO,
    };
    let std::net::IpAddr::V4(dest_ip) = remote.ip() else {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "peer address is not IPv4",
        ));
    };
    let wsa = || std::io::Error::from_raw_os_error(unsafe { WSAGetLastError().0 });
    unsafe {
        let sock = socket(AF_INET.0 as i32, SOCK_STREAM, IPPROTO_TCP.0).map_err(io_err)?;
        let local_sa = sockaddr_v4(local, 0);
        if bind(
            sock,
            &local_sa as *const SOCKADDR_IN as *const SOCKADDR,
            std::mem::size_of::<SOCKADDR_IN>() as i32,
        ) != 0
        {
            let err = wsa();
            let _ = closesocket(sock);
            return Err(err);
        }
        // Bind picks the LAN source. Do not also set IP_UNICAST_IF: on a
        // connected socket that option makes later sends fail, and the session
        // drops as soon as a heartbeat cannot be written.
        let ms = timeout.as_millis().min(u32::MAX as u128) as u32;
        let ms_bytes = ms.to_ne_bytes();
        let _ = setsockopt(sock, SOL_SOCKET, SO_SNDTIMEO, Some(&ms_bytes));
        let remote_sa = sockaddr_v4(dest_ip, remote.port());
        if connect(
            sock,
            &remote_sa as *const SOCKADDR_IN as *const SOCKADDR,
            std::mem::size_of::<SOCKADDR_IN>() as i32,
        ) != 0
        {
            let err = wsa();
            let _ = closesocket(sock);
            return Err(err);
        }
        let zero = 0u32.to_ne_bytes();
        let _ = setsockopt(sock, SOL_SOCKET, SO_SNDTIMEO, Some(&zero));
        Ok(std::net::TcpStream::from_raw_socket(sock.0 as _))
    }
}

fn sockaddr_v4(
    ip: std::net::Ipv4Addr,
    port: u16,
) -> windows::Win32::Networking::WinSock::SOCKADDR_IN {
    use windows::Win32::Networking::WinSock::{AF_INET, SOCKADDR_IN};
    let mut addr = SOCKADDR_IN {
        sin_family: AF_INET,
        sin_port: port.to_be(),
        ..Default::default()
    };
    // In-memory bytes are network order, matching how addresses are read back.
    addr.sin_addr.S_un.S_addr = u32::from_ne_bytes(ip.octets());
    addr
}

fn io_err(err: windows::core::Error) -> std::io::Error {
    std::io::Error::other(err)
}

pub fn disable_udp_connreset(raw: std::os::windows::io::RawSocket) {
    const SIO_UDP_CONNRESET: u32 = 0x9800_000C;
    #[link(name = "ws2_32")]
    extern "system" {
        fn WSAIoctl(
            s: usize,
            control: u32,
            in_buf: *const u8,
            in_len: u32,
            out_buf: *mut u8,
            out_len: u32,
            returned: *mut u32,
            overlapped: *mut u8,
            completion: *mut u8,
        ) -> i32;
    }
    unsafe {
        let mut returned = 0u32;
        let disabled: u32 = 0;
        let _ = WSAIoctl(
            raw as usize,
            SIO_UDP_CONNRESET,
            &disabled as *const u32 as *const u8,
            std::mem::size_of::<u32>() as u32,
            std::ptr::null_mut(),
            0,
            &mut returned,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        );
    }
}
