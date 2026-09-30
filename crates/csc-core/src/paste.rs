//! The keyboard host's view of the shared clipboard: which computer copied
//! last, and whether that was files. Every key press passes through the
//! keyboard host, so it alone decides what Ctrl+V on a computer means.

use std::time::{Duration, Instant};

/// A second Ctrl+V for the same files and computer within this time is a repeat.
const REPEAT: Duration = Duration::from_secs(3);

#[derive(Debug, Clone, PartialEq)]
struct Owner {
    /// Computer that copied; empty for the keyboard host itself.
    peer: String,
    seq: u64,
    files: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub enum PasteAction {
    /// The key host copied the files: send them to `to`.
    SendOwn { to: String },
    /// Ask `from`, which copied them, to send its files to `to` (empty: the key host).
    Request { from: String, seq: u64, to: String },
    /// A repeat of a paste already started: swallow the key, do nothing.
    Repeat,
}

#[derive(Debug, Default)]
pub struct SharedClip {
    owner: Option<Owner>,
    last: Option<(u64, String, String, Instant)>,
}

fn same(a: &str, b: &str) -> bool {
    a.eq_ignore_ascii_case(b)
}

impl SharedClip {
    /// `peer` (empty: this computer) copied something; the last copy wins.
    pub fn copied(&mut self, peer: &str, seq: u64, files: bool) {
        self.owner = Some(Owner {
            peer: peer.to_string(),
            seq,
            files,
        });
    }

    /// Files copied on another computer: Ctrl+V here must be caught.
    pub fn remote_files(&self) -> bool {
        self.owner
            .as_ref()
            .is_some_and(|o| o.files && !o.peer.is_empty())
    }

    /// Ctrl+V pressed on `target` (empty: this computer). None means an
    /// ordinary paste the key goes through for.
    pub fn paste(&mut self, target: &str, now: Instant) -> Option<PasteAction> {
        let owner = self.owner.clone().filter(|o| o.files)?;
        if same(&owner.peer, target) {
            return None;
        }
        if let Some((seq, from, to, at)) = &self.last {
            if *seq == owner.seq && same(from, &owner.peer) && same(to, target) && now.duration_since(*at) < REPEAT {
                return Some(PasteAction::Repeat);
            }
        }
        self.last = Some((owner.seq, owner.peer.clone(), target.to_string(), now));
        Some(if owner.peer.is_empty() {
            PasteAction::SendOwn {
                to: target.to_string(),
            }
        } else {
            PasteAction::Request {
                from: owner.peer,
                seq: owner.seq,
                to: target.to_string(),
            }
        })
    }

    /// The copying computer went away; its files cannot be pasted any more.
    pub fn peer_gone(&mut self, peer: &str) {
        if self
            .owner
            .as_ref()
            .is_some_and(|o| !o.peer.is_empty() && same(&o.peer, peer))
        {
            self.owner = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_four_cases() {
        let t = Instant::now();
        let mut c = SharedClip::default();
        c.copied("", 1, true);
        assert!(!c.remote_files());
        assert_eq!(c.paste("", t), None, "local paste of local files");
        assert_eq!(c.paste("B", t), Some(PasteAction::SendOwn { to: "B".into() }));

        c.copied("B", 7, true);
        assert!(c.remote_files());
        assert_eq!(
            c.paste("", t),
            Some(PasteAction::Request { from: "B".into(), seq: 7, to: String::new() })
        );
        assert_eq!(
            c.paste("C", t),
            Some(PasteAction::Request { from: "B".into(), seq: 7, to: "C".into() })
        );
        assert_eq!(c.paste("b", t), None, "B pastes its own files");
    }

    #[test]
    fn a_new_copy_replaces_the_files() {
        let t = Instant::now();
        let mut c = SharedClip::default();
        c.copied("B", 1, true);
        c.copied("C", 2, false);
        assert!(!c.remote_files());
        assert_eq!(c.paste("", t), None, "text is pasted normally");
        c.copied("B", 3, true);
        c.peer_gone("B");
        assert_eq!(c.paste("", t), None);
    }

    #[test]
    fn repeats_are_swallowed_for_a_while() {
        let t = Instant::now();
        let mut c = SharedClip::default();
        c.copied("B", 1, true);
        assert!(matches!(c.paste("", t), Some(PasteAction::Request { .. })));
        assert_eq!(c.paste("", t + Duration::from_secs(1)), Some(PasteAction::Repeat));
        assert!(matches!(c.paste("C", t + Duration::from_secs(1)), Some(PasteAction::Request { .. })));
        assert!(matches!(c.paste("", t + Duration::from_secs(5)), Some(PasteAction::Request { .. })));
    }
}
