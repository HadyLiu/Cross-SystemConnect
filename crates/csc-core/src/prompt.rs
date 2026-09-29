//! One in-flight client connection prompt (trust / this time / reject).

use csc_proto::{AppStatus, PendingPeer};
use std::sync::Arc;
use tokio::sync::{oneshot, Mutex, RwLock};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Choice {
    Trust,
    Once,
    Reject,
}

struct Pending {
    host_name: String,
    tx: oneshot::Sender<Choice>,
}

#[derive(Default)]
pub struct PeerPrompt {
    slot: Mutex<Option<Pending>>,
}

impl PeerPrompt {
    pub async fn ask(
        &self,
        status: &Arc<RwLock<AppStatus>>,
        name: &str,
        host_name: &str,
        address: &str,
    ) -> Choice {
        let (tx, rx) = oneshot::channel();
        {
            let mut slot = self.slot.lock().await;
            *slot = Some(Pending {
                host_name: host_name.to_string(),
                tx,
            });
        }
        {
            let mut s = status.write().await;
            s.pending_peer = Some(PendingPeer {
                name: name.to_string(),
                host_name: host_name.to_string(),
                address: address.to_string(),
            });
        }
        let choice = rx.await.unwrap_or(Choice::Reject);
        {
            let mut s = status.write().await;
            s.pending_peer = None;
        }
        {
            let mut slot = self.slot.lock().await;
            *slot = None;
        }
        choice
    }

    pub async fn resolve(&self, host_name: &str, choice: Choice) -> Result<(), String> {
        let mut slot = self.slot.lock().await;
        match slot.take() {
            Some(pending) if pending.host_name.eq_ignore_ascii_case(host_name) => {
                pending
                    .tx
                    .send(choice)
                    .map_err(|_| "prompt already closed".to_string())
            }
            Some(pending) => {
                let got = pending.host_name.clone();
                *slot = Some(pending);
                Err(format!("pending host is {got}"))
            }
            None => Err("no pending connection request".into()),
        }
    }
}
