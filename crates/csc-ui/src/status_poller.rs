//! One thread reading the core status for the edge window and the transfer window.
//! Polls fast: the edge window only matters while a drag is held against the edge.

use crate::drop_zone::{zone_from_status, Zone};
use crate::UserEvent;
use crate::ipc_client;
use serde_json::Value;
use std::time::{Duration, Instant};
use tao::event_loop::EventLoopProxy;

const TICK: Duration = Duration::from_millis(80);
/// Resend an unchanged, non-empty transfer list this often, so a window that
/// finished loading after the last change still shows it.
const REFRESH: Duration = Duration::from_secs(1);

pub fn spawn(proxy: EventLoopProxy<UserEvent>) {
    std::thread::spawn(move || {
        let mut zone: Option<Zone> = None;
        let mut transfers: Vec<Value> = Vec::new();
        let mut sent_at = Instant::now();
        loop {
            std::thread::sleep(TICK);
            let status = ipc_client::get_status().ok();
            let z = status.as_ref().and_then(zone_from_status);
            if z != zone {
                zone = z.clone();
                if proxy.send_event(UserEvent::DropZone(z)).is_err() {
                    return;
                }
            }
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
        }
    });
}
