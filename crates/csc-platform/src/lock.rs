//! Lock this computer's screen, and notice the lock key pressed on this computer.

use crate::keymap::{fold_modifier, modifier_bit};
use std::sync::atomic::{AtomicU32, Ordering};

static LOCK_KEY: AtomicU32 = AtomicU32::new(0);
static LOCK_MODS: AtomicU32 = AtomicU32::new(0);
static LOCK_HITS: AtomicU32 = AtomicU32::new(0);
static STAY_KEY: AtomicU32 = AtomicU32::new(0);
static STAY_MODS: AtomicU32 = AtomicU32::new(0);
static STAY_HITS: AtomicU32 = AtomicU32::new(0);
static PAUSE_KEY: AtomicU32 = AtomicU32::new(0);
static PAUSE_MODS: AtomicU32 = AtomicU32::new(0);
static PAUSE_HITS: AtomicU32 = AtomicU32::new(0);
static HELD_MODS: AtomicU32 = AtomicU32::new(0);

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

/// Watch the “stay on this screen” combination while input belongs here.
pub fn stay_watch(mods: &[u32], key: u32) {
    watch_into(mods, key, &STAY_MODS, &STAY_KEY);
}

pub fn stay_hits() -> u32 {
    STAY_HITS.swap(0, Ordering::AcqRel)
}

/// Watch the local emergency-pause combination.
pub fn pause_watch(mods: &[u32], key: u32) {
    watch_into(mods, key, &PAUSE_MODS, &PAUSE_KEY);
}

pub fn pause_hits() -> u32 {
    PAUSE_HITS.swap(0, Ordering::AcqRel)
}

/// Modifier bits currently held on this computer (`keymap::modifier_bit`).
pub fn mods_held() -> u32 {
    HELD_MODS.load(Ordering::Acquire)
}

fn watch_into(mods: &[u32], key: u32, mod_slot: &AtomicU32, key_slot: &AtomicU32) {
    let mut bits = 0;
    for m in mods {
        let bit = modifier_bit(*m);
        if bit == 0 {
            key_slot.store(0, Ordering::Release);
            return;
        }
        bits |= bit;
    }
    mod_slot.store(bits, Ordering::Release);
    key_slot.store(fold_modifier(key), Ordering::Release);
}

fn count_hit(hid: u32, held: u32, key_slot: &AtomicU32, mod_slot: &AtomicU32, hits: &AtomicU32) {
    let key = key_slot.load(Ordering::Acquire);
    let mods = mod_slot.load(Ordering::Acquire);
    if key != 0 && fold_modifier(hid) == key && held & mods == mods {
        hits.fetch_add(1, Ordering::AcqRel);
    }
}

/// A key went down on this computer; `held` is the `modifier_bit` set held with it.
pub(crate) fn lock_key_down(hid: u32, held: u32) {
    HELD_MODS.store(held, Ordering::Release);
    count_hit(hid, held, &LOCK_KEY, &LOCK_MODS, &LOCK_HITS);
    count_hit(hid, held, &STAY_KEY, &STAY_MODS, &STAY_HITS);
    count_hit(hid, held, &PAUSE_KEY, &PAUSE_MODS, &PAUSE_HITS);
}

/// Shortcuts the desktop delivers itself (Wayland GlobalShortcuts portal).
#[cfg(all(unix, not(target_os = "macos")))]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Watch {
    Lock,
    Stay,
    Pause,
}

#[cfg(all(unix, not(target_os = "macos")))]
impl Watch {
    pub(crate) const ALL: [Watch; 3] = [Watch::Lock, Watch::Stay, Watch::Pause];

    fn slots(self) -> (&'static AtomicU32, &'static AtomicU32, &'static AtomicU32) {
        match self {
            Watch::Lock => (&LOCK_MODS, &LOCK_KEY, &LOCK_HITS),
            Watch::Stay => (&STAY_MODS, &STAY_KEY, &STAY_HITS),
            Watch::Pause => (&PAUSE_MODS, &PAUSE_KEY, &PAUSE_HITS),
        }
    }

    /// Modifier bits and key (HID) being watched; `None` when nothing is set.
    pub(crate) fn combo(self) -> Option<(u32, u32)> {
        let (mods, key, _) = self.slots();
        let key = key.load(Ordering::Acquire);
        (key != 0).then(|| (mods.load(Ordering::Acquire), key))
    }

    pub(crate) fn hit(self) {
        if self.combo().is_some() {
            self.slots().2.fetch_add(1, Ordering::AcqRel);
        }
    }
}

#[cfg(all(unix, not(target_os = "macos")))]
pub(crate) fn note_mods(held: u32) {
    HELD_MODS.store(held, Ordering::Release);
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
