//! One thread reading the core status for the transfer window and tray green dot.

use crate::ipc_client;
use crate::UserEvent;
use serde_json::Value;
use std::time::{Duration, Instant};
use tao::event_loop::EventLoopProxy;

const TICK: Duration = Duration::from_millis(80);
/// Resend an unchanged, non-empty transfer list this often, so a window that
/// finished loading after the last change still shows it.
const REFRESH: Duration = Duration::from_secs(1);
const BLINK: Duration = Duration::from_millis(500);

pub fn spawn(proxy: EventLoopProxy<UserEvent>) {
    std::thread::spawn(move || {
        let mut transfers: Vec<Value> = Vec::new();
        let mut sent_at = Instant::now();
        let mut tray = TraySnap {
            connected: false,
            being_controlled: false,
            blink_on: true,
        };
        let mut blink_at = Instant::now();
        loop {
            std::thread::sleep(TICK);
            let status = ipc_client::get_status().ok();
            let t = status
                .as_ref()
                .and_then(|s| s.get("transfers"))
                .and_then(|v| v.as_array())
                .cloned()
                .unwrap_or_default();
            let changed = t != transfers;
            if changed || (!t.is_empty() && sent_at.elapsed() >= REFRESH) {
                transfers = t.clone();
                sent_at = Instant::now();
                if proxy.send_event(UserEvent::Transfers(t)).is_err() {
                    return;
                }
            }

            let connected = status
                .as_ref()
                .and_then(|s| s.get("connected").and_then(|v| v.as_bool()))
                .unwrap_or(false);
            let being_controlled = connected
                && status
                    .as_ref()
                    .and_then(|s| s.get("beingControlled").and_then(|v| v.as_bool()))
                    .unwrap_or(false);
            let mut blink_on = tray.blink_on;
            if being_controlled {
                if blink_at.elapsed() >= BLINK {
                    blink_on = !blink_on;
                    blink_at = Instant::now();
                }
            } else {
                blink_on = true;
                blink_at = Instant::now();
            }
            let next = TraySnap {
                connected,
                being_controlled,
                blink_on,
            };
            if next != tray {
                tray = next;
                if proxy
                    .send_event(UserEvent::TrayLink {
                        connected,
                        being_controlled,
                        blink_on,
                    })
                    .is_err()
                {
                    return;
                }
            }
        }
    });
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct TraySnap {
    connected: bool,
    being_controlled: bool,
    blink_on: bool,
}
