//! Follow the input desktop (Default / Winlogon / UAC secure desktop).
//!
//! Only a process running as SYSTEM in the console session (the elevated
//! service mode) may open the Winlogon desktop; a normal process stays on
//! `Default`, so this is switched on only for the service child.

use std::cell::Cell;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use windows::Win32::System::StationsAndDesktops::{
    CloseDesktop, OpenInputDesktop, SetThreadDesktop, DESKTOP_ACCESS_FLAGS, DESKTOP_CONTROL_FLAGS,
    HDESK,
};

static FOLLOW: AtomicBool = AtomicBool::new(false);

const RECHECK: Duration = Duration::from_millis(250);

thread_local! {
    static LAST: Cell<Option<Instant>> = const { Cell::new(None) };
    static CURRENT: Cell<usize> = const { Cell::new(0) };
}

pub fn set_follow_input_desktop(on: bool) {
    FOLLOW.store(on, Ordering::Relaxed);
}

pub fn follows_input_desktop() -> bool {
    FOLLOW.load(Ordering::Relaxed)
}

/// Attach the calling thread to whatever desktop receives input now, so the
/// next SendInput reaches it. Cheap when nothing changed.
pub fn follow_input_desktop() {
    if !follows_input_desktop() {
        return;
    }
    let now = Instant::now();
    if LAST
        .get()
        .is_some_and(|t| now.saturating_duration_since(t) < RECHECK)
    {
        return;
    }
    LAST.set(Some(now));
    const GENERIC_ALL: u32 = 0x1000_0000;
    unsafe {
        let Ok(desk) = OpenInputDesktop(
            DESKTOP_CONTROL_FLAGS(0),
            false,
            DESKTOP_ACCESS_FLAGS(GENERIC_ALL),
        ) else {
            return;
        };
        if SetThreadDesktop(desk).is_ok() {
            let old = CURRENT.replace(desk.0 as usize);
            if old != 0 {
                let _ = CloseDesktop(HDESK(old as *mut _));
            }
        } else {
            let _ = CloseDesktop(desk);
        }
    }
}
