//! Host-side auto-repeat filter. The controlled computer repeats a held key
//! itself, so a repeated press from the local keyboard is dropped instead of
//! being sent as another key down.

use std::sync::atomic::{AtomicU64, Ordering};

static HELD: [AtomicU64; 4] = [
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
];

/// Record a key edge. False for a press of a key that is already down.
pub fn forward(hid: u32, down: bool) -> bool {
    let Some(word) = HELD.get((hid / 64) as usize) else {
        return true;
    };
    let bit = 1u64 << (hid % 64);
    if down {
        word.fetch_or(bit, Ordering::AcqRel) & bit == 0
    } else {
        word.fetch_and(!bit, Ordering::AcqRel);
        true
    }
}

/// Forget held keys, when input switches between computers.
pub fn reset() {
    for word in &HELD {
        word.store(0, Ordering::Release);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repeated_press_is_dropped() {
        reset();
        assert!(forward(0x04, true));
        assert!(!forward(0x04, true));
        assert!(forward(0x04, false));
        assert!(forward(0x04, true));
        assert!(forward(0x1_0000, true));
        assert!(forward(0x1_0000, true));
        reset();
    }
}
