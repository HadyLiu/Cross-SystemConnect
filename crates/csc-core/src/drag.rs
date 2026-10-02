//! Target side of a file drag from another computer: a system drag of
//! virtual files on this screen. Where the user drops decides where the
//! transfer engine writes (see `transfer::Sink`).

use crate::transfer::{receive_root, Sink, TransferHandle};
use csc_config::Config;
use csc_platform::vdrag::{self, DragDesc, DragEvent, DragItem, StagedFeed};
use csc_proto::DragBeginPayload;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::sync::{mpsc, RwLock};
use tracing::{info, warn};

enum DragCmd {
    Begin {
        peer: String,
        begin: DragBeginPayload,
    },
    PointerLeft,
}

#[derive(Clone)]
pub struct DragHandle {
    tx: mpsc::Sender<DragCmd>,
}

impl DragHandle {
    pub async fn begin(&self, peer: String, begin: DragBeginPayload) {
        let _ = self.tx.send(DragCmd::Begin { peer, begin }).await;
    }

    /// The pointer left this screen; a drag still running here is cancelled.
    pub async fn pointer_left(&self) {
        let _ = self.tx.send(DragCmd::PointerLeft).await;
    }
}

struct Running {
    feed: Option<Arc<StagedFeed>>,
    bound: bool,
    /// Set when the pointer leaves so a later button release is not a drop.
    cancel: Option<Arc<AtomicBool>>,
}

pub fn spawn(transfer: TransferHandle, config: Arc<RwLock<Config>>) -> DragHandle {
    let (tx, rx) = mpsc::channel(8);
    tokio::spawn(run(rx, transfer, config));
    DragHandle { tx }
}

fn badge_of(items: &[DragItem]) -> (String, bool, bool) {
    let tops: Vec<&DragItem> = items.iter().filter(|it| it.is_top()).collect();
    if tops.len() == 1 {
        (tops[0].name().to_string(), tops[0].is_directory, false)
    } else {
        (
            format!("{} items", tops.len()),
            tops.iter().all(|it| it.is_directory),
            true,
        )
    }
}

fn items_of(begin: &DragBeginPayload) -> Vec<DragItem> {
    begin
        .entries
        .iter()
        .map(|e| DragItem {
            relative_path: e.relative_path.clone(),
            size: e.size,
            mtime_unix: e.mtime_unix,
            is_directory: e.is_directory,
        })
        .collect()
}

async fn run(
    mut rx: mpsc::Receiver<DragCmd>,
    transfer: TransferHandle,
    config: Arc<RwLock<Config>>,
) {
    let (ev_tx, mut ev_rx) = mpsc::channel::<DragEvent>(16);
    let (std_tx, std_rx) = std::sync::mpsc::channel::<DragEvent>();
    // Platform drags report on a std channel from their own threads.
    std::thread::Builder::new()
        .name("drag-events".into())
        .spawn(move || {
            while let Ok(ev) = std_rx.recv() {
                if ev_tx.blocking_send(ev).is_err() {
                    return;
                }
            }
        })
        .ok();
    let mut running: HashMap<u64, Running> = HashMap::new();
    let mut active: Option<u64> = None;
    loop {
        tokio::select! {
            cmd = rx.recv() => {
                let Some(cmd) = cmd else { return };
                match cmd {
                    DragCmd::Begin { peer, begin } => {
                        let id = begin.transfer_id;
                        if let Some(old) = active.take() {
                            vdrag::cancel(old);
                        }
                        let items = items_of(&begin);
                        let feed = cfg!(windows).then(|| {
                            Arc::new(StagedFeed::new(vdrag::staging_root().join(id.to_string()), &items))
                        });
                        info!("drag {id} from {peer}: {} items", items.len());
                        transfer.park_drag(peer, begin).await;
                        if csc_platform::wayland_session() {
                            let (label, is_dir, stacked) = badge_of(&items);
                            let cancel = Arc::new(AtomicBool::new(false));
                            vdrag::arm_follow();
                            let tx = std_tx.clone();
                            let flag = cancel.clone();
                            info!("drag {id} follows the pointer until the left button is released");
                            let spawned = std::thread::Builder::new()
                                .name("csc-drag-badge".into())
                                .spawn(move || {
                                    let ev = match vdrag::watch_drop(&label, is_dir, stacked, &flag)
                                    {
                                        Some((x, y)) => match vdrag::folder_at(x, y) {
                                            Some(dir) => DragEvent::Folder { id, dir },
                                            None => DragEvent::Cancelled {
                                                id,
                                                reason: "no-folder",
                                            },
                                        },
                                        None => DragEvent::Cancelled {
                                            id,
                                            reason: "cancelled",
                                        },
                                    };
                                    let _ = tx.send(ev);
                                })
                                .is_ok();
                            if spawned {
                                running.insert(
                                    id,
                                    Running {
                                        feed,
                                        bound: false,
                                        cancel: Some(cancel),
                                    },
                                );
                                active = Some(id);
                            } else {
                                transfer.cancel_drag(id, "unsupported").await;
                            }
                            continue;
                        }
                        let desc = DragDesc { id, items, staged: feed.clone() };
                        if vdrag::start(desc, std_tx.clone()) {
                            running.insert(
                                id,
                                Running {
                                    feed,
                                    bound: false,
                                    cancel: None,
                                },
                            );
                            active = Some(id);
                        } else {
                            warn!("could not start a drag here");
                            transfer.cancel_drag(id, "unsupported").await;
                        }
                    }
                    DragCmd::PointerLeft => {
                        if let Some(id) = active.take() {
                            if let Some(run) = running.get(&id) {
                                if let Some(flag) = &run.cancel {
                                    flag.store(true, Ordering::Release);
                                }
                            }
                            vdrag::cancel(id);
                        }
                    }
                }
            }
            Some(ev) = ev_rx.recv() => {
                let id = ev.id();
                if active == Some(id) && !matches!(ev, DragEvent::Staged { .. }) {
                    active = None;
                }
                let Some(run) = running.get_mut(&id) else { continue };
                if run.bound {
                    continue;
                }
                let sink = match ev {
                    DragEvent::Staged { .. } => match run.feed.clone() {
                        Some(feed) => Sink::Staged(feed),
                        None => continue,
                    },
                    DragEvent::Folder { dir, .. } => {
                        info!("drag {id} dropped into {}", dir.display());
                        Sink::Dir(dir)
                    }
                    DragEvent::InPlace { tops, .. } => {
                        info!("drag {id} dropped as {} placeholder(s)", tops.len());
                        Sink::InPlace(tops)
                    }
                    DragEvent::Lost { .. } => {
                        let dir = receive_root(&*config.read().await);
                        csc_platform::notify(
                            "Cross-SystemConnect 文件传输",
                            &format!("没有找到放下的位置，文件改存到：{}", dir.display()),
                            false,
                        );
                        Sink::Dir(dir)
                    }
                    DragEvent::Cancelled { reason, .. } => {
                        info!("drag {id} ended without a drop ({reason})");
                        if reason == "no-folder" {
                            csc_platform::notify(
                                "Cross-SystemConnect 文件传输",
                                "没有放到文件夹上，已取消。请拖到文件夹后松开左键。",
                                false,
                            );
                        }
                        running.remove(&id);
                        transfer.cancel_drag(id, reason).await;
                        continue;
                    }
                };
                run.bound = true;
                transfer.bind_drop(id, sink).await;
                running.retain(|_, r| !r.bound || r.feed.as_ref().is_some_and(|f| !f.all_done() && !f.is_failed()));
            }
        }
    }
}
