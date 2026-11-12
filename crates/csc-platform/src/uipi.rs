//! Windows refuses injected input into a window whose process has a higher
//! integrity level than the injector (UIPI): an app run as administrator,
//! such as Clash, ignores the mouse and keyboard without any error. The check
//! itself lives in `win::elevation`; this part is plain logic so it can be tested.

use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Integrity levels (mandatory label RIDs).
pub const LOW: u32 = 0x1000;
pub const MEDIUM: u32 = 0x2000;
pub const HIGH: u32 = 0x3000;
pub const SYSTEM: u32 = 0x4000;

/// A window owned by a process at `theirs` drops input injected from `ours`.
pub fn blocks_input(ours: u32, theirs: u32) -> bool {
    theirs > ours
}

/// Same app again within this time: say nothing.
pub const REPEAT_AFTER: Duration = Duration::from_secs(10 * 60);

/// Warn once per foreground app, again only after [`REPEAT_AFTER`].
#[derive(Default)]
pub struct WarnOnce {
    pub last: Option<(String, Instant)>,
}

impl WarnOnce {
    pub fn should_warn(&mut self, app: &str, now: Instant) -> bool {
        let fresh = match &self.last {
            Some((name, at)) => {
                !name.eq_ignore_ascii_case(app)
                    || now.saturating_duration_since(*at) >= REPEAT_AFTER
            }
            None => true,
        };
        if fresh {
            self.last = Some((app.to_string(), now));
        }
        fresh
    }
}

static PENDING: Mutex<Option<String>> = Mutex::new(None);

/// Platform side: an elevated foreground app is ignoring injected input.
pub fn report(app: &str) {
    *PENDING.lock().unwrap_or_else(|e| e.into_inner()) = Some(app.to_string());
}

/// Core side: the app to warn about, once.
pub fn take() -> Option<String> {
    PENDING.lock().unwrap_or_else(|e| e.into_inner()).take()
}

/// Short program name from a full image path.
pub fn app_name(image: &str) -> String {
    image
        .rsplit(['\\', '/'])
        .next()
        .unwrap_or(image)
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_higher_integrity_blocks() {
        assert!(blocks_input(MEDIUM, HIGH));
        assert!(blocks_input(HIGH, SYSTEM));
        assert!(!blocks_input(HIGH, HIGH));
        assert!(!blocks_input(SYSTEM, HIGH));
        assert!(!blocks_input(MEDIUM, LOW));
    }

    #[test]
    fn warns_once_per_app_until_it_is_stale() {
        let mut w = WarnOnce::default();
        let t0 = Instant::now();
        assert!(w.should_warn("clash-verge.exe", t0));
        assert!(!w.should_warn("Clash-Verge.exe", t0 + Duration::from_secs(5)));
        assert!(w.should_warn("taskmgr.exe", t0 + Duration::from_secs(6)));
        assert!(w.should_warn("taskmgr.exe", t0 + Duration::from_secs(6) + REPEAT_AFTER));
    }

    #[test]
    fn pending_warning_is_taken_once() {
        report("a.exe");
        assert_eq!(take().as_deref(), Some("a.exe"));
        assert_eq!(take(), None);
        assert_eq!(app_name(r"C:\Program Files\Clash\clash.exe"), "clash.exe");
        assert_eq!(app_name("plain"), "plain");
    }
}
