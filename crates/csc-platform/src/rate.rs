//! Debug statistic for input streams: events per second and the longest gap.

use std::time::{Duration, Instant};
use tracing::debug;

/// Logs once a second while events keep coming.
pub struct RateStats {
    label: &'static str,
    since: Option<Instant>,
    last: Option<Instant>,
    count: u32,
    max_gap: Duration,
}

impl RateStats {
    pub const fn new(label: &'static str) -> Self {
        Self {
            label,
            since: None,
            last: None,
            count: 0,
            max_gap: Duration::ZERO,
        }
    }

    pub fn note(&mut self) {
        let now = Instant::now();
        if let Some(last) = self.last {
            let gap = now - last;
            // A pause in moving is not a stall.
            if gap < Duration::from_millis(500) {
                self.max_gap = self.max_gap.max(gap);
            } else {
                self.since = None;
                self.count = 0;
            }
        }
        self.last = Some(now);
        self.count += 1;
        let since = *self.since.get_or_insert(now);
        if now - since >= Duration::from_secs(1) {
            debug!(
                events = self.count,
                max_gap_ms = self.max_gap.as_millis() as u64,
                "{}",
                self.label
            );
            self.since = Some(now);
            self.count = 0;
            self.max_gap = Duration::ZERO;
        }
    }
}
