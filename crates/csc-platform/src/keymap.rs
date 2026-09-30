//! Key codes on the wire are USB HID usages (keyboard page 0x07).
//!
//! Each row maps a usage to the PC set-1 scan code (with the E0 prefix flag)
//! that Windows reports and injects, and to the Linux evdev code. X11 keycodes
//! are evdev codes plus 8.

struct Row {
    hid: u16,
    scan: u16,
    e0: bool,
    evdev: u16,
}

const fn k(hid: u16, scan: u16, evdev: u16) -> Row {
    Row {
        hid,
        scan,
        e0: false,
        evdev,
    }
}

const fn x(hid: u16, scan: u16, evdev: u16) -> Row {
    Row {
        hid,
        scan,
        e0: true,
        evdev,
    }
}

#[rustfmt::skip]
const TABLE: &[Row] = &[
    // Letters a-z
    k(0x04, 0x1E, 30), k(0x05, 0x30, 48), k(0x06, 0x2E, 46), k(0x07, 0x20, 32),
    k(0x08, 0x12, 18), k(0x09, 0x21, 33), k(0x0A, 0x22, 34), k(0x0B, 0x23, 35),
    k(0x0C, 0x17, 23), k(0x0D, 0x24, 36), k(0x0E, 0x25, 37), k(0x0F, 0x26, 38),
    k(0x10, 0x32, 50), k(0x11, 0x31, 49), k(0x12, 0x18, 24), k(0x13, 0x19, 25),
    k(0x14, 0x10, 16), k(0x15, 0x13, 19), k(0x16, 0x1F, 31), k(0x17, 0x14, 20),
    k(0x18, 0x16, 22), k(0x19, 0x2F, 47), k(0x1A, 0x11, 17), k(0x1B, 0x2D, 45),
    k(0x1C, 0x15, 21), k(0x1D, 0x2C, 44),
    // Digits 1-9, 0
    k(0x1E, 0x02, 2), k(0x1F, 0x03, 3), k(0x20, 0x04, 4), k(0x21, 0x05, 5),
    k(0x22, 0x06, 6), k(0x23, 0x07, 7), k(0x24, 0x08, 8), k(0x25, 0x09, 9),
    k(0x26, 0x0A, 10), k(0x27, 0x0B, 11),
    // Enter, Esc, Backspace, Tab, Space, punctuation
    k(0x28, 0x1C, 28), k(0x29, 0x01, 1), k(0x2A, 0x0E, 14), k(0x2B, 0x0F, 15),
    k(0x2C, 0x39, 57), k(0x2D, 0x0C, 12), k(0x2E, 0x0D, 13), k(0x2F, 0x1A, 26),
    k(0x30, 0x1B, 27), k(0x31, 0x2B, 43), k(0x33, 0x27, 39), k(0x34, 0x28, 40),
    k(0x35, 0x29, 41), k(0x36, 0x33, 51), k(0x37, 0x34, 52), k(0x38, 0x35, 53),
    k(0x39, 0x3A, 58),
    // F1-F12
    k(0x3A, 0x3B, 59), k(0x3B, 0x3C, 60), k(0x3C, 0x3D, 61), k(0x3D, 0x3E, 62),
    k(0x3E, 0x3F, 63), k(0x3F, 0x40, 64), k(0x40, 0x41, 65), k(0x41, 0x42, 66),
    k(0x42, 0x43, 67), k(0x43, 0x44, 68), k(0x44, 0x57, 87), k(0x45, 0x58, 88),
    // PrintScreen, ScrollLock, Pause
    x(0x46, 0x37, 99), k(0x47, 0x46, 70), k(0x48, 0x45, 119),
    // Insert, Home, PageUp, Delete, End, PageDown, arrows
    x(0x49, 0x52, 110), x(0x4A, 0x47, 102), x(0x4B, 0x49, 104), x(0x4C, 0x53, 111),
    x(0x4D, 0x4F, 107), x(0x4E, 0x51, 109), x(0x4F, 0x4D, 106), x(0x50, 0x4B, 105),
    x(0x51, 0x50, 108), x(0x52, 0x48, 103),
    // Keypad
    x(0x53, 0x45, 69), x(0x54, 0x35, 98), k(0x55, 0x37, 55), k(0x56, 0x4A, 74),
    k(0x57, 0x4E, 78), x(0x58, 0x1C, 96), k(0x59, 0x4F, 79), k(0x5A, 0x50, 80),
    k(0x5B, 0x51, 81), k(0x5C, 0x4B, 75), k(0x5D, 0x4C, 76), k(0x5E, 0x4D, 77),
    k(0x5F, 0x47, 71), k(0x60, 0x48, 72), k(0x61, 0x49, 73), k(0x62, 0x52, 82),
    k(0x63, 0x53, 83),
    // ISO extra key, Menu
    k(0x64, 0x56, 86), x(0x65, 0x5D, 127),
    // Modifiers
    k(0xE0, 0x1D, 29), k(0xE1, 0x2A, 42), k(0xE2, 0x38, 56), x(0xE3, 0x5B, 125),
    x(0xE4, 0x1D, 97), k(0xE5, 0x36, 54), x(0xE6, 0x38, 100), x(0xE7, 0x5C, 126),
];

/// HID usage for a Windows scan code. `e0` is the extended-key flag.
pub fn hid_from_scan(scan: u16, e0: bool) -> Option<u32> {
    TABLE
        .iter()
        .find(|r| r.scan == scan && r.e0 == e0)
        .map(|r| r.hid as u32)
}

/// Windows scan code and extended-key flag for a HID usage.
pub fn scan_from_hid(hid: u32) -> Option<(u16, bool)> {
    TABLE
        .iter()
        .find(|r| r.hid as u32 == hid)
        .map(|r| (r.scan, r.e0))
}

/// Linux evdev key code for a HID usage.
pub fn evdev_from_hid(hid: u32) -> Option<u16> {
    TABLE.iter().find(|r| r.hid as u32 == hid).map(|r| r.evdev)
}

/// HID usage for a Linux evdev key code.
pub fn hid_from_evdev(code: u16) -> Option<u32> {
    TABLE.iter().find(|r| r.evdev == code).map(|r| r.hid as u32)
}

pub const HID_LEFT_CTRL: u32 = 0xE0;
pub const HID_LEFT_SHIFT: u32 = 0xE1;
pub const HID_LEFT_ALT: u32 = 0xE2;
pub const HID_LEFT_GUI: u32 = 0xE3;

/// Left-hand modifier for a right-hand one; other usages unchanged.
pub fn fold_modifier(hid: u32) -> u32 {
    match hid {
        0xE4..=0xE7 => hid - 4,
        other => other,
    }
}

/// One bit per modifier, either hand: Ctrl 1, Shift 2, Alt 4, Win 8. 0 for other keys.
pub fn modifier_bit(hid: u32) -> u32 {
    match fold_modifier(hid) {
        HID_LEFT_CTRL => 1,
        HID_LEFT_SHIFT => 2,
        HID_LEFT_ALT => 4,
        HID_LEFT_GUI => 8,
        _ => 0,
    }
}

/// HID usage for a key name as written in a hotkey such as `Ctrl+Alt+/`.
/// Modifiers map to their left-hand usage. Case does not matter.
pub fn hid_from_name(name: &str) -> Option<u32> {
    let n = name.trim().to_ascii_lowercase();
    let b = n.as_bytes();
    if b.len() == 1 {
        return match b[0] {
            c @ b'a'..=b'z' => Some(0x04 + (c - b'a') as u32),
            b'0' => Some(0x27),
            c @ b'1'..=b'9' => Some(0x1E + (c - b'1') as u32),
            b'-' => Some(0x2D),
            b'=' => Some(0x2E),
            b'[' => Some(0x2F),
            b']' => Some(0x30),
            b'\\' => Some(0x31),
            b';' => Some(0x33),
            b'\'' => Some(0x34),
            b'`' => Some(0x35),
            b',' => Some(0x36),
            b'.' => Some(0x37),
            b'/' => Some(0x38),
            _ => None,
        };
    }
    if let Some(num) = n.strip_prefix('f').and_then(|s| s.parse::<u32>().ok()) {
        return (1..=12).contains(&num).then(|| 0x3A + num - 1);
    }
    Some(match n.as_str() {
        "ctrl" | "control" => HID_LEFT_CTRL,
        "shift" => HID_LEFT_SHIFT,
        "alt" => HID_LEFT_ALT,
        "win" | "super" | "meta" | "cmd" => HID_LEFT_GUI,
        "enter" | "return" => 0x28,
        "esc" | "escape" => 0x29,
        "backspace" => 0x2A,
        "tab" => 0x2B,
        "space" => 0x2C,
        "capslock" => 0x39,
        "printscreen" | "print" => 0x46,
        "scrolllock" => 0x47,
        "pause" | "break" => 0x48,
        "insert" | "ins" => 0x49,
        "home" => 0x4A,
        "pageup" | "pgup" => 0x4B,
        "delete" | "del" => 0x4C,
        "end" => 0x4D,
        "pagedown" | "pgdn" => 0x4E,
        "right" => 0x4F,
        "left" => 0x50,
        "down" => 0x51,
        "up" => 0x52,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hotkey_names() {
        assert_eq!(hid_from_name("Home"), Some(0x4A));
        assert_eq!(hid_from_name("ctrl"), Some(HID_LEFT_CTRL));
        assert_eq!(hid_from_name("F12"), Some(0x45));
        assert_eq!(hid_from_name("q"), Some(0x14));
        assert_eq!(hid_from_name("/"), Some(0x38));
        assert_eq!(hid_from_name("Win"), Some(HID_LEFT_GUI));
        assert_eq!(modifier_bit(0xE7), 8);
        assert_eq!(modifier_bit(0x0F), 0);
        assert_eq!(hid_from_evdev(102), Some(0x4A));
        assert_eq!(fold_modifier(0xE6), HID_LEFT_ALT);
    }

    #[test]
    fn letters_and_modifiers_roundtrip() {
        assert_eq!(hid_from_scan(0x1E, false), Some(0x04));
        assert_eq!(scan_from_hid(0x04), Some((0x1E, false)));
        assert_eq!(evdev_from_hid(0x04), Some(30));
        assert_eq!(hid_from_scan(0x1D, true), Some(0xE4));
        assert_eq!(hid_from_scan(0x1D, false), Some(0xE0));
        assert_eq!(evdev_from_hid(0x52), Some(103));
    }

    #[test]
    fn every_usage_and_scan_is_unique() {
        for (i, a) in TABLE.iter().enumerate() {
            for b in &TABLE[i + 1..] {
                assert_ne!(a.hid, b.hid, "duplicate usage {:#x}", a.hid);
                assert!(
                    !(a.scan == b.scan && a.e0 == b.e0),
                    "duplicate scan {:#x} e0={}",
                    a.scan,
                    a.e0
                );
            }
        }
    }
}
