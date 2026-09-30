//! Progress of incoming batches, read by the UI through `get_status`.
//! Only the receiving side registers anything; senders show nothing.

use csc_proto::{TransferProgress, TransferState};
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Finished batches stay visible this long unless the user closes them first.
const KEEP_FINISHED: Duration = Duration::from_secs(30);
const RATE_WINDOW: Duration = Duration::from_secs(1);

struct Item {
    p: TransferProgress,
    finished_at: Option<Instant>,
    samples: VecDeque<(Instant, u64)>,
}

impl Item {
    fn sample(&mut self, now: Instant) {
        self.samples.push_back((now, self.p.bytes_done));
        while self
            .samples
            .front()
            .is_some_and(|(t, _)| now.duration_since(*t) > RATE_WINDOW)
            && self.samples.len() > 2
        {
            self.samples.pop_front();
        }
        self.p.bytes_per_sec = match (self.samples.front(), self.samples.back()) {
            (Some((t0, b0)), Some((t1, b1))) if t1 > t0 => {
                let secs = t1.duration_since(*t0).as_secs_f64();
                ((b1 - b0) as f64 / secs) as u64
            }
            _ => 0,
        };
    }
}

#[derive(Clone, Default)]
pub struct TransferBoard {
    items: Arc<Mutex<Vec<Item>>>,
}

impl TransferBoard {
    fn with<R>(&self, f: impl FnOnce(&mut Vec<Item>) -> R) -> R {
        let mut items = self.items.lock().unwrap_or_else(|e| e.into_inner());
        f(&mut items)
    }

    /// Add a batch, or replace the one with the same id.
    pub fn upsert(&self, p: TransferProgress) {
        self.with(|items| {
            let finished_at = p.state.finished().then(Instant::now);
            if let Some(item) = items.iter_mut().find(|i| i.p.id == p.id) {
                item.p = p;
                item.finished_at = finished_at;
            } else {
                items.push(Item {
                    p,
                    finished_at,
                    samples: VecDeque::new(),
                });
            }
        });
    }

    pub fn update(&self, id: u64, f: impl FnOnce(&mut TransferProgress)) {
        self.with(|items| {
            if let Some(item) = items.iter_mut().find(|i| i.p.id == id) {
                f(&mut item.p);
                if item.p.state.finished() && item.finished_at.is_none() {
                    item.finished_at = Some(Instant::now());
                }
            }
        });
    }

    pub fn add_bytes(&self, id: u64, n: u64) {
        self.with(|items| {
            if let Some(item) = items.iter_mut().find(|i| i.p.id == id) {
                item.p.bytes_done = (item.p.bytes_done + n).min(item.p.bytes_total);
                item.sample(Instant::now());
            }
        });
    }

    pub fn file_done(&self, id: u64) {
        self.update(id, |p| p.files_done = (p.files_done + 1).min(p.files_total));
    }

    pub fn finish(&self, id: u64, state: TransferState, message: impl Into<String>) {
        let message = message.into();
        self.update(id, |p| {
            p.state = state;
            p.bytes_per_sec = 0;
            if !message.is_empty() {
                p.message = message;
            }
            if state == TransferState::Done {
                p.bytes_done = p.bytes_total;
                p.files_done = p.files_total;
            }
            p.conflicts.clear();
        });
    }

    /// Close a finished batch in the window.
    pub fn dismiss(&self, id: u64) {
        self.with(|items| items.retain(|i| i.p.id != id || !i.p.state.finished()));
    }

    pub fn snapshot(&self) -> Vec<TransferProgress> {
        self.snapshot_at(Instant::now())
    }

    fn snapshot_at(&self, now: Instant) -> Vec<TransferProgress> {
        self.with(|items| {
            items.retain(|i| {
                i.finished_at
                    .is_none_or(|t| now.duration_since(t) < KEEP_FINISHED)
            });
            for item in items.iter_mut() {
                // A stalled batch shows 0 B/s instead of its last rate.
                if item
                    .samples
                    .back()
                    .is_some_and(|(t, _)| now.duration_since(*t) > RATE_WINDOW * 2)
                {
                    item.p.bytes_per_sec = 0;
                }
            }
            items.iter().map(|i| i.p.clone()).collect()
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn batch(id: u64, total: u64) -> TransferProgress {
        TransferProgress {
            id,
            bytes_total: total,
            files_total: 2,
            state: TransferState::Running,
            ..Default::default()
        }
    }

    #[test]
    fn bytes_are_capped_and_finish_fills_up() {
        let board = TransferBoard::default();
        board.upsert(batch(1, 100));
        board.add_bytes(1, 70);
        board.add_bytes(1, 70);
        assert_eq!(board.snapshot()[0].bytes_done, 100);
        board.finish(1, TransferState::Done, "saved");
        let s = &board.snapshot()[0];
        assert_eq!((s.files_done, s.state), (2, TransferState::Done));
        assert_eq!(s.message, "saved");
    }

    #[test]
    fn finished_batches_expire_and_can_be_dismissed() {
        let board = TransferBoard::default();
        board.upsert(batch(1, 10));
        board.upsert(batch(2, 10));
        board.finish(1, TransferState::Failed, "x");
        board.dismiss(2);
        assert_eq!(board.snapshot().len(), 2, "running batch cannot be dismissed");
        board.dismiss(1);
        assert_eq!(board.snapshot().len(), 1);
        board.finish(2, TransferState::Cancelled, "");
        let later = Instant::now() + KEEP_FINISHED + Duration::from_secs(1);
        assert!(board.snapshot_at(later).is_empty());
    }

    #[test]
    fn rate_comes_from_recent_samples() {
        let board = TransferBoard::default();
        board.upsert(batch(1, u64::MAX));
        board.with(|items| {
            let t0 = Instant::now();
            let item = &mut items[0];
            item.p.bytes_done = 0;
            item.sample(t0);
            item.p.bytes_done = 500;
            item.sample(t0 + Duration::from_millis(500));
            assert_eq!(item.p.bytes_per_sec, 1000);
        });
    }
}
