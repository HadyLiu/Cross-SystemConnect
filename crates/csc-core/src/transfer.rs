//! File batches between computers, for copied files (clipboard) and files
//! dropped on a screen edge.
//!
//! Sender: FileOffer. Receiver checks names and limits, then FileRequest or FileError.
//! Sender streams FileBegin / FileData / FileEnd per entry and FileDone, all on
//! the bulk queue so they stay in order and never hold up pointer input.

use crate::clipboard::ClipHandle;
use crate::net::NetHub;
use crate::path_safe::{check_relative_path, name_problem};
use csc_config::{default_config_path, Config};
use csc_proto::{
    decode_file_data, encode_file_data, encode_json_frame, FileBeginPayload, FileDonePayload,
    FileEndPayload, FileEntry, FileErrorPayload, FileOfferPayload, FileRequestPayload, MsgType,
    TransferPurpose,
};
use serde::de::DeserializeOwned;
use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::io::AsyncReadExt;
use tokio::sync::{mpsc, RwLock};
use tracing::{info, warn};
use xxhash_rust::xxh64::Xxh64;

const CHUNK: usize = 64 * 1024;
const MAX_ENTRIES: usize = 20_000;
const OFFER_TTL: Duration = Duration::from_secs(30 * 60);
const TITLE: &str = "Cross-SystemConnect 文件传输";

pub enum TransferCmd {
    Send {
        peer: String,
        paths: Vec<PathBuf>,
        purpose: TransferPurpose,
    },
    Frame {
        peer: String,
        msg: MsgType,
        payload: Vec<u8>,
    },
    PeerGone(String),
}

#[derive(Clone)]
pub struct TransferHandle {
    tx: mpsc::Sender<TransferCmd>,
}

impl TransferHandle {
    pub async fn send_files(&self, peer: String, paths: Vec<PathBuf>, purpose: TransferPurpose) {
        let _ = self
            .tx
            .send(TransferCmd::Send {
                peer,
                paths,
                purpose,
            })
            .await;
    }

    pub async fn frame(&self, peer: String, msg: MsgType, payload: Vec<u8>) {
        let _ = self.tx.send(TransferCmd::Frame { peer, msg, payload }).await;
    }

    pub async fn peer_gone(&self, peer: String) {
        let _ = self.tx.send(TransferCmd::PeerGone(peer)).await;
    }
}

pub fn channel() -> (TransferHandle, mpsc::Receiver<TransferCmd>) {
    let (tx, rx) = mpsc::channel(256);
    (TransferHandle { tx }, rx)
}

fn popup(body: impl Into<String>, error: bool) {
    let body = body.into();
    if error {
        warn!("{body}");
    } else {
        info!("{body}");
    }
    csc_platform::notify(TITLE, &body, error);
}

fn mib(bytes: u64) -> String {
    format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
}

/// Where received files go: the configured folder, else Downloads/Cross-SystemConnect.
pub fn receive_root(cfg: &Config) -> PathBuf {
    if !cfg.file_transfer.receive_dir.trim().is_empty() {
        return PathBuf::from(cfg.file_transfer.receive_dir.trim());
    }
    dirs::download_dir()
        .map(|d| d.join("Cross-SystemConnect"))
        .or_else(|| default_config_path().parent().map(|p| p.join("received")))
        .unwrap_or_else(|| PathBuf::from("received"))
}

fn clipboard_root(cfg: &Config) -> PathBuf {
    receive_root(cfg).join(".clipboard")
}

// ---------------------------------------------------------------- planning

struct Plan {
    entries: Vec<FileEntry>,
    /// Local source of each entry; None for directories.
    sources: Vec<Option<PathBuf>>,
    total: u64,
}

/// Walk the selection. Every name must be valid on Windows too; the first
/// problems are reported together so the user can fix them in one go.
fn plan(paths: &[PathBuf]) -> Result<Plan, String> {
    let mut plan = Plan {
        entries: Vec::new(),
        sources: Vec::new(),
        total: 0,
    };
    let mut problems = Vec::new();
    for root in paths {
        let Some(name) = root.file_name().and_then(|n| n.to_str()) else {
            problems.push(format!("{}：名称编码无法识别", root.display()));
            continue;
        };
        walk(root, name.to_string(), &mut plan, &mut problems)?;
    }
    if !problems.is_empty() {
        let more = problems.len().saturating_sub(8);
        let mut msg = String::from("以下文件名无法在另一台电脑上使用，已取消发送：\n");
        for p in problems.iter().take(8) {
            msg.push_str("• ");
            msg.push_str(p);
            msg.push('\n');
        }
        if more > 0 {
            msg.push_str(&format!("……另有 {more} 项"));
        }
        return Err(msg);
    }
    if plan.entries.is_empty() {
        return Err("没有可发送的文件".into());
    }
    Ok(plan)
}

fn walk(path: &Path, rel: String, plan: &mut Plan, problems: &mut Vec<String>) -> Result<(), String> {
    if plan.entries.len() >= MAX_ENTRIES {
        return Err(format!("文件数量超过 {MAX_ENTRIES} 个，请压缩后再发送"));
    }
    let leaf = rel.rsplit('/').next().unwrap_or(&rel);
    if let Some(why) = name_problem(leaf) {
        problems.push(format!("{rel}：{why}"));
        return Ok(());
    }
    if check_relative_path(&rel).is_err() {
        problems.push(format!("{rel}：路径过长"));
        return Ok(());
    }
    let meta = std::fs::metadata(path).map_err(|e| format!("{}：{e}", path.display()))?;
    if meta.is_dir() {
        plan.entries.push(FileEntry {
            relative_path: rel.clone(),
            size: 0,
            mtime_unix: 0,
            is_directory: true,
        });
        plan.sources.push(None);
        let mut children: Vec<_> = std::fs::read_dir(path)
            .map_err(|e| format!("{}：{e}", path.display()))?
            .filter_map(|e| e.ok())
            .collect();
        children.sort_by_key(|e| e.file_name());
        for child in children {
            // Links inside a folder are skipped: they can point outside it or loop.
            if child.file_type().map(|t| t.is_symlink()).unwrap_or(true) {
                continue;
            }
            let Some(name) = child.file_name().to_str().map(str::to_string) else {
                problems.push(format!("{rel}/?：名称编码无法识别"));
                continue;
            };
            walk(&child.path(), format!("{rel}/{name}"), plan, problems)?;
        }
    } else if meta.is_file() {
        plan.total += meta.len();
        plan.entries.push(FileEntry {
            relative_path: rel,
            size: meta.len(),
            mtime_unix: 0,
            is_directory: false,
        });
        plan.sources.push(Some(path.to_path_buf()));
    }
    Ok(())
}

// ---------------------------------------------------------------- state

struct Outgoing {
    peer: String,
    purpose: TransferPurpose,
    entries: Vec<FileEntry>,
    sources: Vec<Option<PathBuf>>,
    total: u64,
    created: Instant,
    cancel: Arc<AtomicBool>,
}

struct Current {
    index: u32,
    file: std::fs::File,
    path: PathBuf,
    hasher: Xxh64,
    written: u64,
}

struct Incoming {
    peer: String,
    purpose: TransferPurpose,
    entries: Vec<FileEntry>,
    /// Local path of each entry.
    targets: Vec<PathBuf>,
    /// Local paths of the top-level items, for the clipboard or the report.
    tops: Vec<PathBuf>,
    dir: PathBuf,
    total: u64,
    current: Option<Current>,
}

struct Engine {
    net: NetHub,
    config: Arc<RwLock<Config>>,
    clip: ClipHandle,
    outgoing: HashMap<u64, Outgoing>,
    /// Cancel flags of batches being streamed, by transfer id.
    sending: HashMap<u64, (String, Arc<AtomicBool>)>,
    incoming: HashMap<u64, Incoming>,
    next_id: u64,
}

pub fn spawn(
    rx: mpsc::Receiver<TransferCmd>,
    net: NetHub,
    config: Arc<RwLock<Config>>,
    clip: ClipHandle,
) {
    let seed = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(1);
    let engine = Engine {
        net,
        config,
        clip,
        outgoing: HashMap::new(),
        sending: HashMap::new(),
        incoming: HashMap::new(),
        next_id: seed << 12,
    };
    tokio::spawn(engine.run(rx));
}

fn parse<T: DeserializeOwned>(payload: &[u8]) -> Option<T> {
    serde_json::from_slice(payload).ok()
}

impl Engine {
    async fn run(mut self, mut rx: mpsc::Receiver<TransferCmd>) {
        {
            let root = clipboard_root(&*self.config.read().await);
            tokio::task::spawn_blocking(move || prune_clipboard_dirs(&root));
        }
        while let Some(cmd) = rx.recv().await {
            self.outgoing.retain(|_, o| o.created.elapsed() < OFFER_TTL);
            self.sending
                .retain(|_, (_, cancel)| Arc::strong_count(cancel) > 1);
            match cmd {
                TransferCmd::Send {
                    peer,
                    paths,
                    purpose,
                } => self.offer(peer, paths, purpose).await,
                TransferCmd::Frame { peer, msg, payload } => {
                    if let Err(e) = self.on_frame(&peer, msg, &payload).await {
                        warn!("file transfer from {peer}: {e:#}");
                    }
                }
                TransferCmd::PeerGone(peer) => self.peer_gone(&peer),
            }
        }
    }

    async fn send_json<T: serde::Serialize>(&self, peer: &str, msg: MsgType, v: &T) {
        if let Ok(frame) = encode_json_frame(msg, v) {
            self.net.send_control(peer, frame).await;
        }
    }

    async fn offer(&mut self, peer: String, paths: Vec<PathBuf>, purpose: TransferPurpose) {
        let cfg = self.config.read().await.clone();
        let planned = tokio::task::spawn_blocking(move || plan(&paths)).await;
        let plan = match planned {
            Ok(Ok(p)) => p,
            Ok(Err(msg)) => {
                popup(msg, true);
                return;
            }
            Err(e) => {
                warn!("plan transfer: {e}");
                return;
            }
        };
        let limit = match purpose {
            TransferPurpose::Clipboard => cfg.clipboard.max_total_mb.max(0) as u64 * 1024 * 1024,
            TransferPurpose::Drag => u64::MAX,
        };
        if plan.total > limit {
            popup(
                format!(
                    "复制的文件共 {}，超过剪贴板文件上限 {} MB，未同步到 {peer}。\n可以把文件拖到屏幕边缘发送，或在设置中调大上限。",
                    mib(plan.total),
                    cfg.clipboard.max_total_mb
                ),
                true,
            );
            return;
        }
        let per_file = cfg.file_transfer.max_size_mb.max(0) as u64 * 1024 * 1024;
        if let Some(big) = plan.entries.iter().find(|e| e.size > per_file) {
            popup(
                format!(
                    "“{}” 大小 {}，超过单个文件上限 {} MB，已取消发送。",
                    big.relative_path,
                    mib(big.size),
                    cfg.file_transfer.max_size_mb
                ),
                true,
            );
            return;
        }
        self.next_id += 1;
        let id = self.next_id;
        let offer = FileOfferPayload {
            transfer_id: id,
            purpose,
            entries: plan.entries.clone(),
            total_bytes: plan.total,
        };
        info!(
            "offering {} entries ({}) to {peer} for {:?}",
            plan.entries.len(),
            mib(plan.total),
            purpose
        );
        self.send_json(&peer, MsgType::FileOffer, &offer).await;
        self.outgoing.insert(
            id,
            Outgoing {
                peer,
                purpose,
                entries: plan.entries,
                sources: plan.sources,
                total: plan.total,
                created: Instant::now(),
                cancel: Arc::new(AtomicBool::new(false)),
            },
        );
    }

    async fn on_frame(&mut self, peer: &str, msg: MsgType, payload: &[u8]) -> anyhow::Result<()> {
        match msg {
            MsgType::FileOffer => {
                let offer: FileOfferPayload =
                    parse(payload).ok_or_else(|| anyhow::anyhow!("bad offer"))?;
                self.accept_offer(peer, offer).await;
            }
            MsgType::FileRequest => {
                let req: FileRequestPayload =
                    parse(payload).ok_or_else(|| anyhow::anyhow!("bad request"))?;
                self.start_sending(peer, req.transfer_id);
            }
            MsgType::FileCancel => {
                let req: FileRequestPayload =
                    parse(payload).ok_or_else(|| anyhow::anyhow!("bad cancel"))?;
                self.cancel_send(req.transfer_id);
            }
            MsgType::FileError => {
                let err: FileErrorPayload =
                    parse(payload).ok_or_else(|| anyhow::anyhow!("bad error"))?;
                if self.cancel_send(err.transfer_id) {
                    popup(format!("{peer} 未能接收文件：{}", err.message), true);
                } else if let Some(inc) = self.incoming.remove(&err.transfer_id) {
                    discard(inc);
                    popup(format!("从 {peer} 接收文件失败：{}", err.message), true);
                }
            }
            MsgType::FileBegin => {
                let b: FileBeginPayload =
                    parse(payload).ok_or_else(|| anyhow::anyhow!("bad begin"))?;
                if let Err(msg) = self.begin_entry(peer, &b) {
                    self.fail_incoming(b.transfer_id, &msg).await;
                }
            }
            MsgType::FileData => {
                let (id, index, data) =
                    decode_file_data(payload).ok_or_else(|| anyhow::anyhow!("bad data"))?;
                if let Err(msg) = self.write_data(peer, id, index, data) {
                    self.fail_incoming(id, &msg).await;
                }
            }
            MsgType::FileEnd => {
                let end: FileEndPayload =
                    parse(payload).ok_or_else(|| anyhow::anyhow!("bad end"))?;
                if let Err(msg) = self.end_entry(peer, &end) {
                    self.fail_incoming(end.transfer_id, &msg).await;
                }
            }
            MsgType::FileDone => {
                let done: FileDonePayload =
                    parse(payload).ok_or_else(|| anyhow::anyhow!("bad done"))?;
                self.finish(peer, done.transfer_id);
            }
            _ => {}
        }
        Ok(())
    }

    // ------------------------------------------------------------ receiving

    async fn accept_offer(&mut self, peer: &str, offer: FileOfferPayload) {
        let id = offer.transfer_id;
        let cfg = self.config.read().await.clone();
        let refuse = |why: String| FileErrorPayload {
            transfer_id: id,
            message: why,
        };
        let allowed = match offer.purpose {
            TransferPurpose::Clipboard => cfg.clipboard.enabled && cfg.clipboard.files,
            TransferPurpose::Drag => cfg.file_transfer.enabled,
        };
        if !allowed {
            let e = refuse("对方已关闭文件共享".into());
            self.send_json(peer, MsgType::FileError, &e).await;
            return;
        }
        match self.prepare_incoming(peer, &offer, &cfg) {
            Ok(inc) => {
                info!(
                    "receiving {} entries ({}) from {peer} into {}",
                    inc.entries.len(),
                    mib(inc.total),
                    inc.dir.display()
                );
                self.incoming.insert(id, inc);
                self.send_json(peer, MsgType::FileRequest, &FileRequestPayload { transfer_id: id })
                    .await;
            }
            Err(why) => {
                popup(format!("拒绝接收来自 {peer} 的文件：{why}"), true);
                self.send_json(peer, MsgType::FileError, &refuse(why)).await;
            }
        }
    }

    fn prepare_incoming(
        &self,
        peer: &str,
        offer: &FileOfferPayload,
        cfg: &Config,
    ) -> Result<Incoming, String> {
        if offer.entries.is_empty() || offer.entries.len() > MAX_ENTRIES {
            return Err("文件列表为空或过多".into());
        }
        let total: u64 = offer.entries.iter().map(|e| e.size).sum();
        if total != offer.total_bytes {
            return Err("文件大小不一致".into());
        }
        let limit = match offer.purpose {
            TransferPurpose::Clipboard => cfg.clipboard.max_total_mb.max(0) as u64 * 1024 * 1024,
            TransferPurpose::Drag => u64::MAX,
        };
        if total > limit {
            return Err(format!("共 {}，超过剪贴板文件上限", mib(total)));
        }
        let per_file = cfg.file_transfer.max_size_mb.max(0) as u64 * 1024 * 1024;
        if let Some(big) = offer.entries.iter().find(|e| e.size > per_file) {
            return Err(format!("“{}” 超过单个文件上限", big.relative_path));
        }
        let dir = match offer.purpose {
            TransferPurpose::Clipboard => clipboard_root(cfg).join(offer.transfer_id.to_string()),
            TransferPurpose::Drag => receive_root(cfg),
        };
        std::fs::create_dir_all(&dir).map_err(|e| format!("无法创建 {}：{e}", dir.display()))?;

        // Every path is checked before anything is written. Top-level names that
        // already exist in the folder get a " (n)" suffix instead of overwriting.
        let mut renamed: HashMap<String, String> = HashMap::new();
        let mut targets = Vec::with_capacity(offer.entries.len());
        let mut tops = Vec::new();
        for e in &offer.entries {
            let rel = check_relative_path(&e.relative_path)
                .map_err(|why| format!("{}：{why}", e.relative_path))?;
            let (top, rest) = match e.relative_path.split_once('/') {
                Some((t, r)) => (t.to_string(), Some(r)),
                None => (e.relative_path.clone(), None),
            };
            if rest.is_none() {
                if renamed.contains_key(&top) {
                    return Err(format!("重复的名称 {top}"));
                }
                let local = unique_name(&dir, &top);
                tops.push(dir.join(&local));
                renamed.insert(top.clone(), local);
            }
            let Some(local_top) = renamed.get(&top) else {
                return Err(format!("{}：缺少上级文件夹", e.relative_path));
            };
            let mut path = dir.join(local_top);
            if let Some(rest) = rest {
                path.push(rest.replace('/', std::path::MAIN_SEPARATOR_STR));
            }
            debug_assert!(path.starts_with(&dir) && rel.components().count() > 0);
            targets.push(path);
        }
        Ok(Incoming {
            peer: peer.to_string(),
            purpose: offer.purpose,
            entries: offer.entries.clone(),
            targets,
            tops,
            dir,
            total,
            current: None,
        })
    }

    fn begin_entry(&mut self, peer: &str, b: &FileBeginPayload) -> Result<(), String> {
        let inc = self
            .incoming
            .get_mut(&b.transfer_id)
            .filter(|i| i.peer.eq_ignore_ascii_case(peer))
            .ok_or("未知的传输")?;
        if inc.current.is_some() {
            return Err("上一个文件尚未结束".into());
        }
        let i = b.index as usize;
        let (entry, path) = inc
            .entries
            .get(i)
            .zip(inc.targets.get(i))
            .ok_or("文件序号无效")?;
        if entry.is_directory {
            std::fs::create_dir_all(path).map_err(|e| format!("{}：{e}", path.display()))?;
            return Ok(());
        }
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| format!("{}：{e}", parent.display()))?;
        }
        let file = std::fs::File::create(path).map_err(|e| format!("{}：{e}", path.display()))?;
        inc.current = Some(Current {
            index: b.index,
            file,
            path: path.clone(),
            hasher: Xxh64::new(0),
            written: 0,
        });
        Ok(())
    }

    fn write_data(&mut self, peer: &str, id: u64, index: u32, data: &[u8]) -> Result<(), String> {
        let inc = self
            .incoming
            .get_mut(&id)
            .filter(|i| i.peer.eq_ignore_ascii_case(peer))
            .ok_or("未知的传输")?;
        let size = inc.entries.get(index as usize).map(|e| e.size).unwrap_or(0);
        let cur = inc
            .current
            .as_mut()
            .filter(|c| c.index == index)
            .ok_or("数据与文件不对应")?;
        if cur.written + data.len() as u64 > size {
            return Err("收到的数据多于文件大小".into());
        }
        cur.file
            .write_all(data)
            .map_err(|e| format!("写入 {}：{e}", cur.path.display()))?;
        cur.hasher.update(data);
        cur.written += data.len() as u64;
        Ok(())
    }

    fn end_entry(&mut self, peer: &str, end: &FileEndPayload) -> Result<(), String> {
        let inc = self
            .incoming
            .get_mut(&end.transfer_id)
            .filter(|i| i.peer.eq_ignore_ascii_case(peer))
            .ok_or("未知的传输")?;
        let size = inc.entries.get(end.index as usize).map(|e| e.size).unwrap_or(0);
        let cur = inc
            .current
            .take()
            .filter(|c| c.index == end.index)
            .ok_or("结束标记与文件不对应")?;
        if cur.written != size {
            return Err(format!("{} 不完整", cur.path.display()));
        }
        if cur.hasher.digest() != end.xxhash64 {
            let _ = std::fs::remove_file(&cur.path);
            return Err(format!("{} 校验失败", cur.path.display()));
        }
        Ok(())
    }

    fn finish(&mut self, peer: &str, id: u64) {
        let Some(inc) = self.incoming.remove(&id).filter(|i| i.peer.eq_ignore_ascii_case(peer)) else {
            return;
        };
        let files = inc.entries.iter().filter(|e| !e.is_directory).count();
        match inc.purpose {
            TransferPurpose::Clipboard => {
                info!("copied files from {peer} are on the clipboard ({files} files)");
                self.clip.set_files(inc.tops);
            }
            TransferPurpose::Drag => popup(
                format!(
                    "已从 {peer} 接收 {files} 个文件（{}）\n保存在：{}",
                    mib(inc.total),
                    inc.dir.display()
                ),
                false,
            ),
        }
    }

    async fn fail_incoming(&mut self, id: u64, why: &str) {
        let Some(inc) = self.incoming.remove(&id) else {
            return;
        };
        let peer = inc.peer.clone();
        discard(inc);
        popup(format!("从 {peer} 接收文件失败：{why}"), true);
        let e = FileErrorPayload {
            transfer_id: id,
            message: why.to_string(),
        };
        self.send_json(&peer, MsgType::FileError, &e).await;
    }

    fn peer_gone(&mut self, peer: &str) {
        let gone: Vec<u64> = self
            .incoming
            .iter()
            .filter(|(_, i)| i.peer.eq_ignore_ascii_case(peer))
            .map(|(id, _)| *id)
            .collect();
        for id in gone {
            if let Some(inc) = self.incoming.remove(&id) {
                discard(inc);
                popup(format!("与 {peer} 的连接断开，文件未接收完"), true);
            }
        }
        self.outgoing
            .retain(|_, o| !o.peer.eq_ignore_ascii_case(peer));
        self.sending.retain(|_, (p, cancel)| {
            let keep = !p.eq_ignore_ascii_case(peer);
            if !keep {
                cancel.store(true, Ordering::Relaxed);
            }
            keep
        });
    }

    // ------------------------------------------------------------ sending

    /// Stop an offered or running send. True when there was one.
    fn cancel_send(&mut self, id: u64) -> bool {
        if let Some(o) = self.outgoing.remove(&id) {
            o.cancel.store(true, Ordering::Relaxed);
            return true;
        }
        if let Some((_, cancel)) = self.sending.remove(&id) {
            cancel.store(true, Ordering::Relaxed);
            return true;
        }
        false
    }

    fn start_sending(&mut self, peer: &str, id: u64) {
        let Some(out) = self.outgoing.remove(&id).filter(|o| o.peer.eq_ignore_ascii_case(peer)) else {
            return;
        };
        self.sending
            .insert(id, (out.peer.clone(), out.cancel.clone()));
        let net = self.net.clone();
        let config = self.config.clone();
        tokio::spawn(async move {
            let peer = out.peer.clone();
            let purpose = out.purpose;
            let files = out.entries.iter().filter(|e| !e.is_directory).count();
            let total = out.total;
            let cancel = out.cancel.clone();
            match stream(&net, &config, id, out).await {
                Ok(()) => {
                    if purpose == TransferPurpose::Drag {
                        popup(format!("已发送 {files} 个文件（{}）到 {peer}", mib(total)), false);
                    } else {
                        info!("sent copied files to {peer}");
                    }
                }
                Err(_) if cancel.load(Ordering::Relaxed) => {
                    info!("transfer {id} to {peer} cancelled");
                }
                Err(why) => {
                    popup(format!("发送文件到 {peer} 失败：{why}"), true);
                    if let Ok(frame) = encode_json_frame(
                        MsgType::FileError,
                        &FileErrorPayload {
                            transfer_id: id,
                            message: why,
                        },
                    ) {
                        net.send_bulk(&peer, frame).await;
                    }
                }
            }
        });
    }
}

async fn bulk_json<T: serde::Serialize>(
    net: &NetHub,
    peer: &str,
    msg: MsgType,
    v: &T,
) -> Result<(), String> {
    let frame = encode_json_frame(msg, v).map_err(|e| e.to_string())?;
    if net.send_bulk(peer, frame).await {
        Ok(())
    } else {
        Err("连接已断开".into())
    }
}

async fn stream(
    net: &NetHub,
    config: &Arc<RwLock<Config>>,
    id: u64,
    out: Outgoing,
) -> Result<(), String> {
    let (limit, rate) = {
        let c = config.read().await;
        (c.file_transfer.limit_speed, c.file_transfer.max_speed_mibs.max(1) as f64)
    };
    let bytes_per_sec = rate * 1024.0 * 1024.0;
    let started = Instant::now();
    let mut sent: u64 = 0;
    let mut buf = vec![0u8; CHUNK];
    for (i, entry) in out.entries.iter().enumerate() {
        if out.cancel.load(Ordering::Relaxed) {
            return Err("已取消".into());
        }
        let index = i as u32;
        bulk_json(net, &out.peer, MsgType::FileBegin, &FileBeginPayload { transfer_id: id, index })
            .await?;
        let Some(src) = &out.sources[i] else {
            continue;
        };
        let mut file = tokio::fs::File::open(src)
            .await
            .map_err(|e| format!("{}：{e}", src.display()))?;
        let mut hasher = Xxh64::new(0);
        let mut left = entry.size;
        while left > 0 {
            let want = (left as usize).min(CHUNK);
            let n = file
                .read(&mut buf[..want])
                .await
                .map_err(|e| format!("{}：{e}", src.display()))?;
            if n == 0 {
                return Err(format!("{} 在发送时变小了", src.display()));
            }
            hasher.update(&buf[..n]);
            if !net.send_bulk(&out.peer, encode_file_data(id, index, &buf[..n])).await {
                return Err("连接已断开".into());
            }
            left -= n as u64;
            sent += n as u64;
            if limit {
                let due = Duration::from_secs_f64(sent as f64 / bytes_per_sec);
                if let Some(wait) = due.checked_sub(started.elapsed()) {
                    tokio::time::sleep(wait).await;
                }
            }
            if out.cancel.load(Ordering::Relaxed) {
                return Err("已取消".into());
            }
        }
        bulk_json(
            net,
            &out.peer,
            MsgType::FileEnd,
            &FileEndPayload {
                transfer_id: id,
                index,
                xxhash64: hasher.digest(),
            },
        )
        .await?;
    }
    bulk_json(net, &out.peer, MsgType::FileDone, &FileDonePayload { transfer_id: id }).await
}

/// `name`, or `name (1)`, `name (2)` ... so nothing in `dir` is overwritten.
fn unique_name(dir: &Path, name: &str) -> String {
    if !dir.join(name).exists() {
        return name.to_string();
    }
    let (stem, ext) = match name.rfind('.') {
        Some(i) if i > 0 => (&name[..i], &name[i..]),
        _ => (name, ""),
    };
    (1..)
        .map(|n| format!("{stem} ({n}){ext}"))
        .find(|candidate| !dir.join(candidate).exists())
        .unwrap_or_else(|| name.to_string())
}

/// Drop a failed batch. Clipboard batches are private and removed whole;
/// for dropped files only the partly written file goes.
fn discard(inc: Incoming) {
    if let Some(cur) = inc.current {
        drop(cur.file);
        let _ = std::fs::remove_file(&cur.path);
    }
    if inc.purpose == TransferPurpose::Clipboard {
        let _ = std::fs::remove_dir_all(&inc.dir);
    }
}

/// Clipboard batches older than a day are no longer on anyone's clipboard.
fn prune_clipboard_dirs(root: &Path) {
    let Ok(list) = std::fs::read_dir(root) else {
        return;
    };
    for entry in list.flatten() {
        let old = entry
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.elapsed().ok())
            .is_some_and(|age| age > Duration::from_secs(24 * 3600));
        if old {
            let _ = std::fs::remove_dir_all(entry.path());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plan_rejects_windows_names() {
        let dir = std::env::temp_dir().join(format!("csc-plan-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("ok")).unwrap();
        std::fs::write(dir.join("ok").join("a.txt"), b"hi").unwrap();
        std::fs::write(dir.join("ok").join("bad:name.txt"), b"x").unwrap();
        let err = plan(&[dir.join("ok")]).err().expect("rejected");
        assert!(err.contains("bad:name.txt"), "{err}");
        std::fs::remove_file(dir.join("ok").join("bad:name.txt")).unwrap();
        let p = plan(&[dir.join("ok")]).expect("plan");
        assert_eq!(p.entries.len(), 2);
        assert_eq!(p.entries[1].relative_path, "ok/a.txt");
        assert_eq!(p.total, 2);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn unique_name_adds_suffix() {
        let dir = std::env::temp_dir().join(format!("csc-uniq-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.txt"), b"").unwrap();
        assert_eq!(unique_name(&dir, "a.txt"), "a (1).txt");
        assert_eq!(unique_name(&dir, "b.txt"), "b.txt");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
