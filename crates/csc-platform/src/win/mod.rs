mod capture;
pub mod power;
mod paste_dir;
pub mod dragsrc;
pub mod hook;
mod host_net;
mod injector;
pub mod screen;

pub use capture::WinInputCapture;
pub use host_net::fill_host_net;
pub use injector::WinInputInjector;
pub use screen::cursor_pos;
pub use screen::init_dpi_awareness;
pub use screen::primary_button_down;
pub use screen::set_cursor_visible;
pub use screen::virtual_origin;
pub use screen::warp_cursor;
pub use paste_dir::paste_folder;
pub use screen::WinScreenInfo;

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
