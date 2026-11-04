//! Keyboard lock lights, from sysfs. Works on X11 and Wayland.

use std::fs;

pub(crate) fn lock_leds() -> (bool, bool, bool) {
    (led("capslock"), led("numlock"), led("scrolllock"))
}

fn led(suffix: &str) -> bool {
    let Ok(dir) = fs::read_dir("/sys/class/leds") else {
        return false;
    };
    for entry in dir.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if !name.ends_with(suffix) {
            continue;
        }
        if let Ok(text) = fs::read_to_string(entry.path().join("brightness")) {
            return text.trim() != "0";
        }
    }
    false
}
