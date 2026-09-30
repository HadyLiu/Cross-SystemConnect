//! Lock this computer's screen, and notice the lock key pressed on this computer.

use crate::keymap::{fold_modifier, modifier_bit};
use std::sync::atomic::{AtomicU32, Ordering};

static LOCK_KEY: AtomicU32 = AtomicU32::new(0);
static LOCK_MODS: AtomicU32 = AtomicU32::new(0);
static LOCK_HITS: AtomicU32 = AtomicU32::new(0);

/// Count presses of `mods` + `key` (HID usages) while input belongs to this
/// computer. The keys still reach the system. False when a part of `mods`
/// is not a modifier; nothing is watched then.
pub fn lock_watch(mods: &[u32], key: u32) -> bool {
    let mut bits = 0;
    for m in mods {
        let bit = modifier_bit(*m);
        if bit == 0 {
            LOCK_KEY.store(0, Ordering::Release);
            return false;
        }
        bits |= bit;
    }
    LOCK_MODS.store(bits, Ordering::Release);
    LOCK_KEY.store(fold_modifier(key), Ordering::Release);
    true
}

/// Lock key presses since the last call.
pub fn lock_hits() -> u32 {
    LOCK_HITS.swap(0, Ordering::AcqRel)
}

/// A key went down on this computer; `held` is the `modifier_bit` set held with it.
pub(crate) fn lock_key_down(hid: u32, held: u32) {
    let key = LOCK_KEY.load(Ordering::Acquire);
    let mods = LOCK_MODS.load(Ordering::Acquire);
    if key != 0 && fold_modifier(hid) == key && held & mods == mods {
        LOCK_HITS.fetch_add(1, Ordering::AcqRel);
    }
}

/// Show the lock screen without blocking the caller.
pub fn lock_screen() {
    let _ = std::thread::Builder::new()
        .name("lock-screen".into())
        .spawn(lock);
}

#[cfg(windows)]
fn lock() {
    use windows::Win32::System::Shutdown::LockWorkStation;
    match unsafe { LockWorkStation() } {
        Ok(()) => tracing::info!("screen locked"),
        Err(e) => tracing::warn!("LockWorkStation: {e}"),
    }
}

/// Desktop-specific lock calls first: `loginctl lock-session` reports success
/// even when no desktop component listens for it.
#[cfg(all(unix, not(target_os = "macos")))]
#[rustfmt::skip]
const LOCKERS: &[&[&str]] = &[
    &[
        "dbus-send", "--session", "--print-reply", "--reply-timeout=3000",
        "--dest=org.deepin.dde.LockFront1", "/org/deepin/dde/LockFront1",
        "org.deepin.dde.LockFront1.Show",
    ],
    &[
        "dbus-send", "--session", "--print-reply", "--reply-timeout=3000",
        "--dest=com.deepin.dde.lockFront", "/com/deepin/dde/lockFront",
        "com.deepin.dde.lockFront.Show",
    ],
    &[
        "dbus-send", "--session", "--print-reply", "--reply-timeout=3000",
        "--dest=org.gnome.ScreenSaver", "/org/gnome/ScreenSaver",
        "org.gnome.ScreenSaver.Lock",
    ],
    &[
        "dbus-send", "--session", "--print-reply", "--reply-timeout=3000",
        "--dest=org.freedesktop.ScreenSaver", "/org/freedesktop/ScreenSaver",
        "org.freedesktop.ScreenSaver.Lock",
    ],
    &["loginctl", "lock-session"],
    &["xdg-screensaver", "lock"],
];

#[cfg(all(unix, not(target_os = "macos")))]
fn lock() {
    use std::process::{Command, Stdio};
    for cmd in LOCKERS {
        let ok = Command::new(cmd[0])
            .args(&cmd[1..])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|s| s.success());
        if ok {
            tracing::info!("screen locked with {}", cmd.join(" "));
            return;
        }
    }
    tracing::warn!("no way to lock the screen was found");
}

#[cfg(not(any(windows, all(unix, not(target_os = "macos")))))]
fn lock() {
    tracing::warn!("locking the screen is not supported here");
}
