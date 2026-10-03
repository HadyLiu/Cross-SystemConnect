//! Notice when this session is on the secure desktop (lock, UAC, Ctrl+Alt+Del).

use crate::PowerEvent;
use std::sync::mpsc::{self, Receiver};
use std::time::Duration;
use windows::Win32::Foundation::HANDLE;
use windows::Win32::System::StationsAndDesktops::{
    CloseDesktop, GetUserObjectInformationW, OpenInputDesktop, DESKTOP_CONTROL_FLAGS,
    DESKTOP_READOBJECTS, UOI_NAME,
};

pub fn spawn() -> Receiver<PowerEvent> {
    let (tx, rx) = mpsc::channel();
    let _ = std::thread::Builder::new()
        .name("csc-desktop".into())
        .spawn(move || {
            let mut secure = false;
            loop {
                let now = desktop_is_secure();
                if now != secure {
                    secure = now;
                    let ev = if now {
                        PowerEvent::SecureDesktop
                    } else {
                        PowerEvent::Unlocked
                    };
                    if tx.send(ev).is_err() {
                        break;
                    }
                }
                std::thread::sleep(Duration::from_millis(500));
            }
        });
    rx
}

fn desktop_is_secure() -> bool {
    unsafe {
        let Ok(desk) = OpenInputDesktop(DESKTOP_CONTROL_FLAGS(0), false, DESKTOP_READOBJECTS)
        else {
            return true;
        };
        let mut buf = [0u16; 64];
        let mut needed = 0u32;
        let ok = GetUserObjectInformationW(
            HANDLE(desk.0),
            UOI_NAME,
            Some(buf.as_mut_ptr().cast()),
            (buf.len() * 2) as u32,
            Some(&mut needed),
        );
        let _ = CloseDesktop(desk);
        if ok.is_err() {
            return false;
        }
        let name = String::from_utf16_lossy(&buf);
        let name = name.trim_end_matches('\0');
        !name.eq_ignore_ascii_case("Default")
    }
}
