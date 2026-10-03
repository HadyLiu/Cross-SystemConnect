//! Queued control prompts shown on the controlled computer (trust / cancel).
//!
//! A later request does not cancel an earlier one. The UI shows the front of the queue.

use csc_proto::{AppStatus, PendingPeer};
use std::collections::VecDeque;
use std::sync::Arc;
use tokio::sync::{oneshot, Mutex, RwLock};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Choice {
    /// `code` is what the user typed. Empty when this computer is not asking for one.
    Trust { code: String },
    Reject,
}

struct Pending {
    name: String,
    host_name: String,
    address: String,
    tx: oneshot::Sender<Choice>,
}

#[derive(Default)]
pub struct PeerPrompt {
    queue: Mutex<VecDeque<Pending>>,
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
            let mut q = self.queue.lock().await;
            let show = q.is_empty();
            q.push_back(Pending {
                name: name.to_string(),
                host_name: host_name.to_string(),
                address: address.to_string(),
                tx,
            });
            if show {
                let peer = head_peer(&q);
                drop(q);
                status.write().await.pending_peer = peer;
            }
        }
        let choice = rx.await.unwrap_or(Choice::Reject);
        {
            let mut q = self.queue.lock().await;
            if let Some(i) = q
                .iter()
                .position(|p| p.host_name.eq_ignore_ascii_case(host_name))
            {
                q.remove(i);
            }
            let peer = head_peer(&q);
            drop(q);
            status.write().await.pending_peer = peer;
        }
        choice
    }

    pub async fn resolve(
        &self,
        status: &Arc<RwLock<AppStatus>>,
        host_name: &str,
        choice: Choice,
    ) -> Result<(), String> {
        let next = {
            let mut q = self.queue.lock().await;
            let Some(i) = q
                .iter()
                .position(|p| p.host_name.eq_ignore_ascii_case(host_name))
            else {
                return Err("no pending connection request".into());
            };
            let pending = q.remove(i).expect("index just found");
            pending
                .tx
                .send(choice)
                .map_err(|_| "prompt already closed".to_string())?;
            head_peer(&q)
        };
        status.write().await.pending_peer = next;
        Ok(())
    }
}

fn head_peer(q: &VecDeque<Pending>) -> Option<PendingPeer> {
    q.front().map(|p| PendingPeer {
        name: p.name.clone(),
        host_name: p.host_name.clone(),
        address: p.address.clone(),
    })
}
