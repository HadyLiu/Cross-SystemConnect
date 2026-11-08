//! One thread reading the core status for the transfer window and tray green dot.

use crate::events::{EventBus, UiEvent};
use serde_json::Value;
use std::time::{Duration, Instant};

const TICK: Duration = Duration::from_millis(80);
const REFRESH: Duration = Duration::from_secs(1);
const BLINK: Duration = Duration::from_millis(500);

pub fn spawn(bus: EventBus, ctx: eframe::egui::Context) {
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
            let status = crate::ipc_client::get_status().ok();
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
                bus.send(UiEvent::Transfers(t));
                ctx.request_repaint();
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
                bus.send(UiEvent::TrayLink {
                    connected,
                    being_controlled,
                    blink_on,
                });
                ctx.request_repaint();
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
