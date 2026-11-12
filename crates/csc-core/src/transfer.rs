//! File batches between computers: pasted files and explicit file sends.
//!
//! Sender: FileOffer. Receiver checks names and limits, asks about name
//! conflicts, then FileRequest or FileError. Sender streams FileBegin /
//! FileData / FileEnd per entry and FileDone on the bulk queue, so they stay
//! in order and never hold up pointer input. The receiver acknowledges every
//! MiB (FileAck) and the sender keeps at most `WINDOW` bytes unacknowledged.
//! The keyboard host relays batches between controlled computers (see `relay`).

use crate::net::NetHub;
use crate::path_safe::{adapt_relative_path, check_relative_path, name_problem};
use crate::progress::TransferBoard;
use crate::relay::{self, Hop, Relay};
use csc_config::{default_config_path, Config};
use csc_proto::{
    decode_file_batch, decode_file_data, encode_file_batch, encode_file_data, encode_json_frame,
    DragCancelPayload, FileAckPayload, FileBeginPayload, FileDonePayload, FileEndPayload,
    FileEntry, FileErrorPayload, FileOfferPayload, FileRequestPayload, FileResume, MsgType,
    RenamedName, TransferConflict, TransferProgress, TransferPurpose, TransferState, CAP_FILE_ACK,
};
use serde::de::DeserializeOwned;
use serde::Deserialize;
use std::collections::{HashMap, HashSet};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::io::AsyncReadExt;
use tokio::sync::{mpsc, oneshot, Notify, RwLock};
use tracing::{debug, info, warn};
use xxhash_rust::xxh64::Xxh64;

const CHUNK: usize = 64 * 1024;
const MAX_ENTRIES: usize = 20_000;
/// Renamed names listed in the transfer window; the rest only count in the log.
const MAX_RENAMED_SHOWN: usize = 200;
const OFFER_TTL: Duration = Duration::from_secs(30 * 60);
/// Unacknowledged bytes a sender may have in flight.
const WINDOW: u64 = 8 * 1024 * 1024;
/// A receiver that acknowledges nothing for this long is given up.
const ACK_TIMEOUT: Duration = Duration::from_secs(60);
const TITLE: &str = "Cross-SystemConnect 文件传输";
/// Hidden temporary name used while replacing an existing file.
const PART_SUFFIX: &str = ".csc-part";
/// A paste waits this long for the other computer's offer, then is dropped.
const EXPECT_TTL: Duration = Duration::from_secs(2 * 60);
/// Placeholder marker prefix inside a dragged folder (Doc §14.6).
pub const FOLDER_MARKER_PREFIX: &str = ".csc-drag-";

fn clear_folder_markers(dir: &Path) {
    let Ok(list) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in list.flatten() {
        let name = entry.file_name();
        let Some(s) = name.to_str() else {
            continue;
        };
        if s.starts_with(FOLDER_MARKER_PREFIX) || s.starts_with(".csc-paste-") || s == ".csc-drag" {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

/// What to do with a top-level name that already exists.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Choice {
    Skip,
    KeepBoth,
    Replace,
}

/// Where an incoming batch is written.
pub enum Sink {
    /// Into a folder. Existing names are asked about in the transfer window.
    Dir(PathBuf),
    /// Over placeholder files a file manager copied, one per top-level item.
    InPlace(Vec<PathBuf>),
}

/// Where the next pasted batch from a computer goes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PasteSink {
    /// The receive folder (tray paste).
    ReceiveDir,
    /// Over placeholder copies a file manager made.
    InPlace(Vec<PathBuf>),
    /// Fetched ahead into a private folder, then put on the clipboard as real files.
    Stage(PathBuf),
}

/// Local paths of a finished paste batch, or why it failed.
pub type PasteReady = oneshot::Receiver<Vec<PathBuf>>;

/// A paste waiting for the other computer's FileOffer.
struct Expect {
    /// Only an offer from this computer fills it; empty accepts any.
    peer: String,
    sink: PasteSink,
    at: Instant,
    ready: Option<oneshot::Sender<Vec<PathBuf>>>,
}

impl Expect {
    fn fits(&self, peer: &str, now: Instant) -> bool {
        (self.peer.is_empty() || self.peer.eq_ignore_ascii_case(peer))
            && now.duration_since(self.at) < EXPECT_TTL
    }
}

impl Drop for Expect {
    /// Never used: placeholder copies that were never filled are 48-byte marker
    /// files with the real name. Remove them so they cannot be mistaken for the file.
    fn drop(&mut self) {
        if let PasteSink::InPlace(paths) = &self.sink {
            remove_unfilled_placeholders(paths);
        }
    }
}

/// Delete placeholder copies that still hold only the marker.
fn remove_unfilled_placeholders(paths: &[PathBuf]) {
    for p in paths {
        if csc_platform::is_paste_placeholder(p) {
            warn!("removing unfilled paste placeholder {}", p.display());
            let _ = std::fs::remove_file(p);
        } else if p.is_dir() {
            if let Ok(list) = std::fs::read_dir(p) {
                for e in list.flatten() {
                    if csc_platform::is_paste_placeholder(&e.path()) {
                        let _ = std::fs::remove_file(e.path());
                    }
                }
            }
        }
    }
}

pub enum TransferCmd {
    Send {
        peer: String,
        paths: Vec<PathBuf>,
        purpose: TransferPurpose,
        relay_to: String,
    },
    /// Keyboard host: `from` may send a batch for `to` through us.
    AllowRelay {
        from: String,
        to: String,
    },
    /// Where the next paste offer from `peer` goes.
    ExpectPaste {
        peer: String,
        sink: PasteSink,
        ready: Option<oneshot::Sender<Vec<PathBuf>>>,
    },
    Cancel {
        id: u64,
    },
    Resolve {
        id: u64,
        decisions: Vec<(u32, Choice)>,
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
    async fn cmd(&self, cmd: TransferCmd) {
        let _ = self.tx.send(cmd).await;
    }

    pub async fn send_files(&self, peer: String, paths: Vec<PathBuf>, purpose: TransferPurpose) {
        self.send_via(peer, paths, purpose, String::new()).await;
    }

    pub async fn send_via(
        &self,
        peer: String,
        paths: Vec<PathBuf>,
        purpose: TransferPurpose,
        relay_to: String,
    ) {
        self.cmd(TransferCmd::Send {
            peer,
            paths,
            purpose,
            relay_to,
        })
        .await;
    }

    /// The next paste offer from `peer` (empty: any) goes to `sink`.
    pub async fn expect_paste(&self, peer: String, sink: PasteSink) {
        self.cmd(TransferCmd::ExpectPaste {
            peer,
            sink,
            ready: None,
        })
        .await;
    }

    /// Like [`Self::expect_paste`], and report the local top-level paths once
    /// every file arrived and passed its hash check. A dropped sender means it failed.
    pub async fn expect_paste_ready(&self, peer: String, sink: PasteSink) -> PasteReady {
        let (tx, rx) = oneshot::channel();
        self.cmd(TransferCmd::ExpectPaste {
            peer,
            sink,
            ready: Some(tx),
        })
        .await;
        rx
    }

    pub async fn allow_relay(&self, from: String, to: String) {
        self.cmd(TransferCmd::AllowRelay { from, to }).await;
    }

    pub async fn cancel(&self, id: u64) {
        self.cmd(TransferCmd::Cancel { id }).await;
    }

    pub async fn resolve(&self, id: u64, decisions: Vec<(u32, Choice)>) {
        self.cmd(TransferCmd::Resolve { id, decisions }).await;
    }

    pub async fn frame(&self, peer: String, msg: MsgType, payload: Vec<u8>) {
        self.cmd(TransferCmd::Frame { peer, msg, payload }).await;
    }

    pub async fn peer_gone(&self, peer: String) {
        self.cmd(TransferCmd::PeerGone(peer)).await;
    }
}

pub fn channel() -> (TransferHandle, mpsc::Receiver<TransferCmd>) {
    let (tx, rx) = mpsc::channel(256);
    (TransferHandle { tx }, rx)
}

fn popup(body: impl Into<String>, error: bool) {
    let body = body.into();
    let kind = if error {
        crate::notice::Kind::Error
    } else {
        crate::notice::Kind::Transfer
    };
    crate::notice::tell(kind, TITLE, &body, error);
}

fn mib(bytes: u64) -> String {
    format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
}

/// Where received files go: the configured folder, else the desktop.
pub fn receive_root(cfg: &Config) -> PathBuf {
    let set = cfg.file_transfer.receive_dir.trim();
    if !set.is_empty() {
        return PathBuf::from(set);
    }
    csc_config::user_dirs::desktop_dir()
        .or_else(csc_config::user_dirs::download_dir)
        .or_else(|| default_config_path().parent().map(|p| p.join("received")))
        .unwrap_or_else(|| PathBuf::from("received"))
}

fn unix_mode(meta: &std::fs::Metadata) -> u32 {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        meta.permissions().mode() & 0o777
    }
    #[cfg(not(unix))]
    {
        let _ = meta;
        0
    }
}

fn apply_mode(preserve: bool, strip: bool, path: &Path, mode: u32) {
    if mode == 0 || !preserve {
        return;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut bits = mode & 0o777;
        if strip {
            if let Some(desk) = csc_config::user_dirs::desktop_dir() {
                if path.starts_with(&desk) {
                    bits &= !0o111;
                }
            }
        }
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(bits));
    }
    #[cfg(not(unix))]
    {
        let _ = (path, strip);
    }
}

fn mark_of_web(enabled: bool, path: &Path, peer: &str) {
    if !enabled {
        return;
    }
    #[cfg(windows)]
    {
        let zone = format!("{}:Zone.Identifier", path.display());
        let body = format!("[ZoneTransfer]\r\nZoneId=3\r\nHostUrl=csc://{peer}\r\n");
        let _ = std::fs::write(zone, body);
    }
    #[cfg(not(windows))]
    {
        let _ = (path, peer);
    }
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
        let root = &csc_platform::clip::tidy_path(root.clone());
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

fn mtime_of(meta: &std::fs::Metadata) -> i64 {
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn walk(
    path: &Path,
    rel: String,
    plan: &mut Plan,
    problems: &mut Vec<String>,
) -> Result<(), String> {
    if plan.entries.len() >= MAX_ENTRIES {
        return Err(format!("文件数量超过 {MAX_ENTRIES} 个，请压缩后再发送"));
    }
    let leaf = rel.rsplit('/').next().unwrap_or(&rel);
    if let Some(why) = name_problem(leaf) {
        // Control characters are invisible in a notification; show them escaped.
        let shown = if rel.chars().any(char::is_control) {
            rel.escape_debug().to_string()
        } else {
            rel.clone()
        };
        problems.push(format!("{shown}：{why}"));
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
            mtime_unix: mtime_of(&meta),
            is_directory: true,
            mode: unix_mode(&meta),
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
            mtime_unix: mtime_of(&meta),
            is_directory: false,
            mode: unix_mode(&meta),
        });
        plan.sources.push(Some(path.to_path_buf()));
    }
    Ok(())
}

/// "a.txt" or "a.txt 等 5 项".
fn batch_title(entries: &[FileEntry]) -> String {
    let tops: Vec<&FileEntry> = entries
        .iter()
        .filter(|e| !e.relative_path.contains('/'))
        .collect();
    match tops.as_slice() {
        [] => "文件".into(),
        [one] => one.relative_path.clone(),
        [first, ..] => format!("{} 等 {} 项", first.relative_path, tops.len()),
    }
}

// ---------------------------------------------------------------- layout of a batch

/// Top-level item of each entry, and the path below it.
struct Shape {
    /// Index of each entry's top-level entry.
    top_of: Vec<usize>,
    /// Path of each entry below its top-level item, with `/` separators.
    rest: Vec<Option<String>>,
    tops: Vec<usize>,
}

fn shape(entries: &[FileEntry], cfg: &Config) -> Result<Shape, String> {
    if entries.is_empty() || entries.len() > MAX_ENTRIES {
        return Err("文件列表为空或过多".into());
    }
    let per_file = cfg.file_transfer.max_size_mb.max(0) as u64 * 1024 * 1024;
    if let Some(big) = entries.iter().find(|e| e.size > per_file) {
        return Err(format!("“{}” 超过单个文件上限", big.relative_path));
    }
    let mut by_name: HashMap<&str, usize> = HashMap::new();
    let mut s = Shape {
        top_of: Vec::with_capacity(entries.len()),
        rest: Vec::with_capacity(entries.len()),
        tops: Vec::new(),
    };
    for (i, e) in entries.iter().enumerate() {
        check_relative_path(&e.relative_path)
            .map_err(|why| format!("{}：{why}", e.relative_path))?;
        match e.relative_path.split_once('/') {
            None => {
                if by_name.insert(&e.relative_path, i).is_some() {
                    return Err(format!("重复的名称 {}", e.relative_path));
                }
                s.tops.push(i);
                s.top_of.push(i);
                s.rest.push(None);
            }
            Some((top, rest)) => {
                let t = *by_name
                    .get(top)
                    .filter(|t| entries[**t].is_directory)
                    .ok_or_else(|| format!("{}：缺少上级文件夹", e.relative_path))?;
                s.top_of.push(t);
                s.rest.push(Some(rest.to_string()));
            }
        }
    }
    Ok(s)
}

fn below(base: &Path, rest: &Option<String>) -> PathBuf {
    let mut p = base.to_path_buf();
    if let Some(rest) = rest {
        for part in rest.split('/') {
            p.push(part);
        }
    }
    p
}

/// `name`, or `name (1)`, `name (2)` ... so nothing in `dir` is overwritten.
fn unique_name(dir: &Path, name: &str, taken: &HashSet<String>) -> String {
    let free = |n: &str| !taken.contains(n) && std::fs::symlink_metadata(dir.join(n)).is_err();
    if free(name) {
        return name.to_string();
    }
    let (stem, ext) = match name.rfind('.') {
        Some(i) if i > 0 => (&name[..i], &name[i..]),
        _ => (name, ""),
    };
    (1..10_000)
        .map(|n| format!("{stem} ({n}){ext}"))
        .find(|candidate| free(candidate))
        .unwrap_or_else(|| name.to_string())
}

fn part_path(target: &Path) -> PathBuf {
    let name = target
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    target.with_file_name(format!(".{name}{PART_SUFFIX}"))
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

struct Sending {
    peer: String,
    cancel: Arc<AtomicBool>,
    acked: Arc<AtomicU64>,
    wake: Arc<Notify>,
}

struct Current {
    index: u32,
    /// None for an entry being skipped (older senders ignore `skip`).
    file: Option<std::fs::File>,
    path: PathBuf,
    hasher: Xxh64,
    written: u64,
}

enum Target {
    Dir(PathBuf),
    InPlace,
}

struct Incoming {
    peer: String,
    origin_device_id: String,
    purpose: TransferPurpose,
    entries: Vec<FileEntry>,
    shape: Shape,
    target: Target,
    /// Local path of each entry.
    targets: Vec<PathBuf>,
    /// Temporary path for files that replace an existing one.
    temps: Vec<Option<PathBuf>>,
    skip: Vec<bool>,
    /// Top-level items written from scratch, removed again if the batch fails.
    fresh: Vec<PathBuf>,
    /// Local paths of the kept top-level items.
    tops: Vec<PathBuf>,
    current: Option<Current>,
    requested: bool,
    received: u64,
    acked: u64,
    /// Bytes already on disk for an entry, filled when a partial file matches.
    resume: HashMap<u32, u64>,
    /// Told the local top-level paths once the whole batch is verified.
    ready: Option<oneshot::Sender<Vec<PathBuf>>>,
    /// Fetched ahead for the clipboard, not saved for the user yet.
    staged: bool,
    /// Files whose FileEnd arrived with a matching size and hash.
    verified: HashSet<u32>,
    /// Waiting for the user to choose, by top-level entry index.
    conflicts: Vec<TransferConflict>,
    renamed: Vec<RenamedName>,
}

impl Incoming {
    /// A file the batch ended without: still open, or never verified.
    fn missing(&self) -> Option<String> {
        if let Some(cur) = &self.current {
            return Some(cur.path.display().to_string());
        }
        self.entries
            .iter()
            .enumerate()
            .find(|(i, e)| {
                !e.is_directory
                    && !self.skip.get(*i).copied().unwrap_or(false)
                    && !self.verified.contains(&(*i as u32))
            })
            .map(|(_, e)| e.relative_path.clone())
    }

    fn dest(&self) -> String {
        match &self.target {
            Target::Dir(d) => d.display().to_string(),
            Target::InPlace => self
                .tops
                .first()
                .and_then(|p| p.parent())
                .map(|p| p.display().to_string())
                .unwrap_or_default(),
        }
    }
}

struct Engine {
    net: NetHub,
    config: Arc<RwLock<Config>>,
    board: TransferBoard,
    outgoing: HashMap<u64, Outgoing>,
    sending: HashMap<u64, Sending>,
    incoming: HashMap<u64, Incoming>,
    /// Where the next pasted batch goes.
    expect_paste: Option<Expect>,
    relay: Relay,
    next_id: u64,
}

pub fn spawn(
    rx: mpsc::Receiver<TransferCmd>,
    net: NetHub,
    config: Arc<RwLock<Config>>,
    board: TransferBoard,
) {
    let seed = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(1);
    let engine = Engine {
        net,
        config,
        board,
        outgoing: HashMap::new(),
        sending: HashMap::new(),
        incoming: HashMap::new(),
        expect_paste: None,
        relay: Relay::default(),
        next_id: seed << 12,
    };
    tokio::spawn(engine.run(rx));
}

fn parse<T: DeserializeOwned>(payload: &[u8]) -> Option<T> {
    serde_json::from_slice(payload).ok()
}

/// The transfer id of any JSON file frame.
fn id_of(payload: &[u8]) -> Option<u64> {
    #[derive(Deserialize)]
    struct Id {
        transfer_id: u64,
    }
    parse::<Id>(payload).map(|i| i.transfer_id)
}

impl Engine {
    async fn run(mut self, mut rx: mpsc::Receiver<TransferCmd>) {
        let window = self.config.read().await.file_transfer.resume_window_min;
        let receive = self.config.read().await.file_transfer.receive_dir.clone();
        tokio::task::spawn_blocking(move || cleanup_expired_parts(&receive, window));
        while let Some(cmd) = rx.recv().await {
            self.outgoing.retain(|_, o| o.created.elapsed() < OFFER_TTL);
            self.sending.retain(|_, s| Arc::strong_count(&s.cancel) > 1);
            if self
                .expect_paste
                .as_ref()
                .is_some_and(|e| e.at.elapsed() >= EXPECT_TTL)
            {
                debug!("paste expectation expired without an offer");
                self.expect_paste = None;
            }
            match cmd {
                TransferCmd::Send {
                    peer,
                    paths,
                    purpose,
                    relay_to,
                } => self.offer(peer, paths, purpose, relay_to).await,
                TransferCmd::AllowRelay { from, to } => self.relay.allow(&from, &to),
                TransferCmd::ExpectPaste { peer, sink, ready } => {
                    self.expect_paste = Some(Expect {
                        peer,
                        sink,
                        at: Instant::now(),
                        ready,
                    });
                }
                TransferCmd::Cancel { id } => self.cancel_incoming(id).await,
                TransferCmd::Resolve { id, decisions } => self.resolve(id, decisions).await,
                TransferCmd::Frame { peer, msg, payload } => {
                    if let Err(e) = self.on_frame(&peer, msg, payload).await {
                        warn!("file transfer from {peer}: {e:#}");
                    }
                }
                TransferCmd::PeerGone(peer) => self.peer_gone(&peer).await,
            }
        }
    }

    async fn send_json<T: serde::Serialize>(&self, peer: &str, msg: MsgType, v: &T) {
        if let Ok(frame) = encode_json_frame(msg, v) {
            self.net.send_control(peer, frame).await;
        }
    }

    fn new_id(&mut self) -> u64 {
        self.next_id += 1;
        self.next_id
    }

    async fn planned(&self, paths: Vec<PathBuf>) -> Result<Plan, String> {
        let cfg = self.config.read().await.clone();
        let plan = tokio::task::spawn_blocking(move || plan(&paths))
            .await
            .map_err(|e| e.to_string())??;
        let per_file = cfg.file_transfer.max_size_mb.max(0) as u64 * 1024 * 1024;
        if let Some(big) = plan.entries.iter().find(|e| e.size > per_file) {
            return Err(format!(
                "“{}” 大小 {}，超过单个文件上限 {} MB，已取消发送。",
                big.relative_path,
                mib(big.size),
                cfg.file_transfer.max_size_mb
            ));
        }
        Ok(plan)
    }

    fn register(
        &mut self,
        peer: String,
        purpose: TransferPurpose,
        plan: Plan,
    ) -> (u64, Vec<FileEntry>, u64) {
        let id = self.new_id();
        let entries = plan.entries.clone();
        let total = plan.total;
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
        (id, entries, total)
    }

    async fn offer(
        &mut self,
        peer: String,
        paths: Vec<PathBuf>,
        purpose: TransferPurpose,
        relay_to: String,
    ) {
        // Cross-screen drag is retired; map leftover Drag purposes to paste.
        let purpose = match purpose {
            TransferPurpose::Drag | TransferPurpose::DragDrop => TransferPurpose::Paste,
            other => other,
        };
        let plan = match self.planned(paths).await {
            Ok(p) => p,
            Err(msg) => {
                popup(msg, true);
                return;
            }
        };
        info!(
            "offering {} entries ({}) to {peer} for {:?}{}",
            plan.entries.len(),
            mib(plan.total),
            purpose,
            if relay_to.is_empty() {
                String::new()
            } else {
                format!(" via host to {relay_to}")
            }
        );
        let (id, entries, total) = self.register(peer.clone(), purpose, plan);
        let offer = FileOfferPayload {
            transfer_id: id,
            purpose,
            entries,
            total_bytes: total,
            origin: String::new(),
            relay_to,
            origin_device_id: self.config.read().await.device_id.clone(),
        };
        self.send_json(&peer, MsgType::FileOffer, &offer).await;
    }

    async fn on_frame(&mut self, peer: &str, msg: MsgType, payload: Vec<u8>) -> anyhow::Result<()> {
        let id = if msg == MsgType::FileData {
            decode_file_data(&payload).map(|(id, _, _)| id)
        } else if msg == MsgType::FileBatch {
            decode_file_batch(&payload).map(|(id, _)| id)
        } else {
            id_of(&payload)
        }
        .ok_or_else(|| anyhow::anyhow!("bad {msg:?} frame"))?;

        if let Some(hop) = self.relay.hop(peer, id) {
            self.relay_frame(hop, msg, &payload).await;
            return Ok(());
        }
        match msg {
            MsgType::FileOffer => {
                let offer: FileOfferPayload =
                    parse(&payload).ok_or_else(|| anyhow::anyhow!("bad offer"))?;
                if offer.relay_to.is_empty() {
                    self.accept_offer(peer, offer).await;
                } else {
                    self.relay_offer(peer, offer).await;
                }
            }
            MsgType::FileRequest => {
                let req: FileRequestPayload =
                    parse(&payload).ok_or_else(|| anyhow::anyhow!("bad request"))?;
                self.start_sending(peer, req).await;
            }
            MsgType::FileAck => {
                let ack: FileAckPayload =
                    parse(&payload).ok_or_else(|| anyhow::anyhow!("bad ack"))?;
                if let Some(s) = self.sending.get(&ack.transfer_id) {
                    s.acked.fetch_max(ack.bytes, Ordering::AcqRel);
                    s.wake.notify_one();
                }
            }
            MsgType::FileCancel | MsgType::DragCancel => {
                let _reason = parse::<DragCancelPayload>(&payload)
                    .map(|c| c.reason)
                    .unwrap_or_default();
                if self.cancel_send(id) {
                    info!("{peer} cancelled transfer {id}");
                } else if let Some(inc) = self.incoming.remove(&id) {
                    discard(inc);
                    self.board
                        .finish(id, TransferState::Cancelled, "对方已取消");
                }
            }
            MsgType::FileError => {
                let err: FileErrorPayload =
                    parse(&payload).ok_or_else(|| anyhow::anyhow!("bad error"))?;
                if self.cancel_send(err.transfer_id) {
                    popup(
                        format!("{} 未能接收文件：{}", self.label(peer).await, err.message),
                        true,
                    );
                } else if let Some(inc) = self.incoming.remove(&err.transfer_id) {
                    discard(inc);
                    self.board
                        .finish(err.transfer_id, TransferState::Failed, err.message);
                }
            }
            MsgType::FileBegin => {
                let b: FileBeginPayload =
                    parse(&payload).ok_or_else(|| anyhow::anyhow!("bad begin"))?;
                if let Err(msg) = self.begin_entry(peer, &b) {
                    self.fail_incoming(b.transfer_id, &msg).await;
                }
            }
            MsgType::FileData => {
                let (id, index, data) =
                    decode_file_data(&payload).ok_or_else(|| anyhow::anyhow!("bad data"))?;
                match self.write_data(peer, id, index, data) {
                    Ok(Some(ack)) => self.send_json(peer, MsgType::FileAck, &ack).await,
                    Ok(None) => {}
                    Err(msg) => self.fail_incoming(id, &msg).await,
                }
            }
            MsgType::FileBatch => {
                let (id, items) =
                    decode_file_batch(&payload).ok_or_else(|| anyhow::anyhow!("bad batch"))?;
                for item in items {
                    match self.write_data(peer, id, item.index, item.data) {
                        Ok(Some(ack)) => self.send_json(peer, MsgType::FileAck, &ack).await,
                        Ok(None) => {}
                        Err(msg) => {
                            self.fail_incoming(id, &msg).await;
                            break;
                        }
                    }
                }
            }
            MsgType::FileEnd => {
                let end: FileEndPayload =
                    parse(&payload).ok_or_else(|| anyhow::anyhow!("bad end"))?;
                if let Err(msg) = self.end_entry(peer, &end) {
                    self.fail_incoming(end.transfer_id, &msg).await;
                }
            }
            MsgType::FileDone => {
                let done: FileDonePayload =
                    parse(&payload).ok_or_else(|| anyhow::anyhow!("bad done"))?;
                self.finish(peer, done.transfer_id).await;
            }
            _ => {}
        }
        Ok(())
    }

    async fn label(&self, peer: &str) -> String {
        self.net.peer_label(peer).await
    }

    // ------------------------------------------------------------ relaying

    async fn relay_offer(&mut self, from: &str, mut offer: FileOfferPayload) {
        let to = offer.relay_to.clone();
        let src_id = offer.transfer_id;
        if !self.relay.take_allowed(from, &to) || !self.net.has_peer(&to).await {
            let e = FileErrorPayload {
                transfer_id: src_id,
                message: format!("键鼠主机无法转发到 {to}"),
            };
            self.send_json(from, MsgType::FileError, &e).await;
            return;
        }
        let rid = self.new_id();
        self.relay.open(&self.net, from, src_id, &to, rid);
        offer.transfer_id = rid;
        offer.relay_to.clear();
        offer.origin = self.label(from).await;
        self.send_json(&to, MsgType::FileOffer, &offer).await;
    }

    async fn relay_frame(&mut self, hop: Hop, msg: MsgType, payload: &[u8]) {
        match hop {
            Hop::ToTarget { dst, rid } => {
                let Some(frame) = relay::rewrite(msg, payload, rid) else {
                    return;
                };
                match msg {
                    MsgType::FileCancel | MsgType::DragCancel => {
                        self.net.send_control(&dst, frame).await;
                        self.relay.close(rid);
                    }
                    _ => {
                        if !self.relay.push(rid, frame) {
                            warn!("relay queue to {dst} overflowed; dropping batch {rid}");
                            self.relay.close(rid);
                            let e = FileErrorPayload {
                                transfer_id: rid,
                                message: "键鼠主机转发失败".into(),
                            };
                            self.send_json(&dst, MsgType::FileError, &e).await;
                        } else if matches!(msg, MsgType::FileDone | MsgType::FileError) {
                            self.relay.close(rid);
                        }
                    }
                }
            }
            Hop::ToSource { src, src_id } => {
                // The host acknowledges the source itself as it forwards.
                if msg == MsgType::FileAck {
                    return;
                }
                if let Some(frame) = relay::rewrite(msg, payload, src_id) {
                    self.net.send_control(&src, frame).await;
                }
                if matches!(
                    msg,
                    MsgType::FileError | MsgType::FileCancel | MsgType::DragCancel
                ) {
                    if let Some(Hop::ToTarget { rid, .. }) = self.relay.hop(&src, src_id) {
                        self.relay.close(rid);
                    }
                }
            }
        }
    }

    // ------------------------------------------------------------ receiving

    async fn accept_offer(&mut self, peer: &str, offer: FileOfferPayload) {
        let id = offer.transfer_id;
        let cfg = self.config.read().await.clone();
        let purpose = offer.purpose.normalized();
        let allowed = match purpose {
            TransferPurpose::Paste | TransferPurpose::Clipboard => cfg.clipboard.files,
            TransferPurpose::Drag | TransferPurpose::DragDrop => cfg.file_transfer.enabled,
        };
        if !allowed {
            let e = FileErrorPayload {
                transfer_id: id,
                message: "对方已关闭文件接收".into(),
            };
            self.send_json(peer, MsgType::FileError, &e).await;
            return;
        }
        let from = if offer.origin.is_empty() {
            self.label(peer).await
        } else {
            offer.origin.clone()
        };
        let via = if offer.origin.is_empty() {
            String::new()
        } else {
            self.label(peer).await
        };
        let mut ready = None;
        let mut staged = false;
        let sink = if matches!(purpose, TransferPurpose::Paste | TransferPurpose::Clipboard) {
            let expect = self.expect_paste.take_if(|e| e.fits(peer, Instant::now()));
            match expect {
                Some(mut e) => {
                    ready = e.ready.take();
                    // Taken out: the placeholders now belong to this batch.
                    match std::mem::replace(&mut e.sink, PasteSink::ReceiveDir) {
                        PasteSink::ReceiveDir => Sink::Dir(receive_root(&cfg)),
                        PasteSink::InPlace(paths) => Sink::InPlace(paths),
                        PasteSink::Stage(dir) => {
                            staged = true;
                            Sink::Dir(dir)
                        }
                    }
                }
                None => {
                    Sink::Dir(csc_platform::paste_folder().unwrap_or_else(|| receive_root(&cfg)))
                }
            }
        } else {
            // Legacy Drag/DragDrop offers still land in the receive folder.
            Sink::Dir(receive_root(&cfg))
        };
        let space_dir = match &sink {
            Sink::Dir(dir) => dir.clone(),
            Sink::InPlace(paths) => paths
                .first()
                .and_then(|p| p.parent().map(|d| d.to_path_buf()))
                .unwrap_or_else(|| receive_root(&cfg)),
        };
        if let Some(free) = csc_platform::free_bytes(&space_dir) {
            let need = offer.total_bytes.saturating_add(64 * 1024 * 1024);
            if need > free {
                let e = FileErrorPayload {
                    transfer_id: id,
                    message: format!("磁盘空间不足，需要 {need} 字节，可用 {free} 字节"),
                };
                popup(e.message.clone(), true);
                self.send_json(peer, MsgType::FileError, &e).await;
                if let Sink::InPlace(paths) = &sink {
                    remove_unfilled_placeholders(paths);
                }
                return;
            }
        }
        self.receive(
            peer,
            id,
            purpose,
            offer.entries,
            offer.total_bytes,
            sink,
            from,
            via,
            offer.origin_device_id,
            ready,
            staged,
        )
        .await;
    }

    #[allow(clippy::too_many_arguments)]
    async fn receive(
        &mut self,
        peer: &str,
        id: u64,
        purpose: TransferPurpose,
        entries: Vec<FileEntry>,
        total: u64,
        sink: Sink,
        from: String,
        via: String,
        origin_device_id: String,
        ready: Option<oneshot::Sender<Vec<PathBuf>>>,
        staged: bool,
    ) {
        let cfg = self.config.read().await.clone();
        let title = batch_title(&entries);
        let files_total = entries.iter().filter(|e| !e.is_directory).count() as u32;
        let placeholders = match &sink {
            Sink::InPlace(paths) => paths.clone(),
            Sink::Dir(_) => Vec::new(),
        };
        match prepare_incoming(peer, purpose, entries, total, sink, &cfg) {
            Ok(mut inc) => {
                inc.origin_device_id = origin_device_id;
                inc.ready = ready;
                inc.staged = staged;
                info!(
                    "receiving {} entries ({}) from {from} into {}",
                    inc.entries.len(),
                    mib(total),
                    inc.dest()
                );
                self.board.upsert(TransferProgress {
                    id,
                    peer: from,
                    via,
                    title,
                    files_total,
                    bytes_total: total,
                    state: if inc.conflicts.is_empty() {
                        TransferState::Running
                    } else {
                        TransferState::Conflict
                    },
                    dest: inc.dest(),
                    conflicts: inc.conflicts.clone(),
                    renamed: inc.renamed.clone(),
                    ..Default::default()
                });
                let ready = inc.conflicts.is_empty();
                self.incoming.insert(id, inc);
                if ready {
                    self.request(id, HashMap::new()).await;
                }
            }
            Err(why) => {
                remove_unfilled_placeholders(&placeholders);
                popup(format!("拒绝接收来自 {from} 的文件：{why}"), true);
                let e = FileErrorPayload {
                    transfer_id: id,
                    message: why,
                };
                self.send_json(peer, MsgType::FileError, &e).await;
            }
        }
    }

    async fn resolve(&mut self, id: u64, decisions: Vec<(u32, Choice)>) {
        let Some(inc) = self.incoming.get(&id).filter(|i| !i.requested) else {
            return;
        };
        let wanted: HashSet<u32> = inc.conflicts.iter().map(|c| c.index).collect();
        let choices: HashMap<usize, Choice> = decisions
            .into_iter()
            .filter(|(i, _)| wanted.contains(i))
            .map(|(i, c)| (i as usize, c))
            .collect();
        if choices.len() < wanted.len() {
            return;
        }
        self.request(id, choices).await;
    }

    /// Fix the local names and ask the sender for the data.
    async fn request(&mut self, id: u64, choices: HashMap<usize, Choice>) {
        let Some(inc) = self.incoming.get_mut(&id) else {
            return;
        };
        if let Err(why) = place(inc, &choices) {
            self.fail_incoming(id, &why).await;
            return;
        }
        inc.requested = true;
        inc.conflicts.clear();
        let peer = inc.peer.clone();
        let window_min = self.config.read().await.file_transfer.resume_window_min;
        let skip: Vec<u32> = inc
            .skip
            .iter()
            .enumerate()
            .filter(|(_, s)| **s)
            .map(|(i, _)| i as u32)
            .collect();
        let resume = resume_matches(inc, window_min);
        let already: u64 = resume.iter().map(|r| r.offset).sum();
        inc.resume = resume.iter().map(|r| (r.index, r.offset)).collect();
        inc.received = already;
        let all_skipped = skip.len() == inc.entries.len();
        let wanted: u64 = inc
            .entries
            .iter()
            .zip(&inc.skip)
            .filter(|(_, s)| !**s)
            .map(|(e, _)| e.size)
            .sum();
        let files = inc
            .entries
            .iter()
            .zip(&inc.skip)
            .filter(|(e, s)| !**s && !e.is_directory)
            .count() as u32;
        let dest = inc.dest();
        if all_skipped {
            self.incoming.remove(&id);
            self.board
                .finish(id, TransferState::Cancelled, "已全部跳过");
            self.send_json(
                &peer,
                MsgType::FileCancel,
                &FileRequestPayload {
                    transfer_id: id,
                    skip: Vec::new(),
                    resume: Vec::new(),
                },
            )
            .await;
            return;
        }
        self.board.update(id, |p| {
            p.state = TransferState::Running;
            p.conflicts.clear();
            p.bytes_total = wanted;
            p.bytes_done = already;
            p.files_total = files;
            p.dest = dest;
        });
        self.send_json(
            &peer,
            MsgType::FileRequest,
            &FileRequestPayload {
                transfer_id: id,
                skip,
                resume,
            },
        )
        .await;
    }

    fn incoming_of(&mut self, peer: &str, id: u64) -> Result<&mut Incoming, String> {
        self.incoming
            .get_mut(&id)
            .filter(|i| i.peer.eq_ignore_ascii_case(peer) && i.requested)
            .ok_or_else(|| "未知的传输".to_string())
    }

    fn begin_entry(&mut self, peer: &str, b: &FileBeginPayload) -> Result<(), String> {
        let inc = self.incoming_of(peer, b.transfer_id)?;
        if inc.current.is_some() {
            return Err("上一个文件尚未结束".into());
        }
        let i = b.index as usize;
        let entry = inc.entries.get(i).ok_or("文件序号无效")?.clone();
        let path = inc.targets[i].clone();
        if inc.skip[i] {
            inc.current = (!entry.is_directory).then(|| Current {
                index: b.index,
                file: None,
                path,
                hasher: Xxh64::new(0),
                written: 0,
            });
            return Ok(());
        }
        if entry.is_directory {
            if std::fs::symlink_metadata(&path).is_ok_and(|m| !m.is_dir()) {
                std::fs::remove_file(&path).map_err(|e| format!("{}：{e}", path.display()))?;
            }
            std::fs::create_dir_all(&path).map_err(|e| format!("{}：{e}", path.display()))?;
            if matches!(inc.target, Target::InPlace) && inc.shape.rest[i].is_none() {
                clear_folder_markers(&path);
            }
            return Ok(());
        }
        let write_to = inc.temps[i].clone().unwrap_or_else(|| path.clone());
        if let Some(parent) = write_to.parent() {
            std::fs::create_dir_all(parent).map_err(|e| format!("{}：{e}", parent.display()))?;
        }
        let offset = inc.resume.get(&b.index).copied().unwrap_or(0);
        let (file, hasher, written) = open_for_resume(&write_to, offset)?;
        inc.current = Some(Current {
            index: b.index,
            file: Some(file),
            path: write_to,
            hasher,
            written,
        });
        Ok(())
    }

    /// Write a chunk. Returns an acknowledgement to send when one is due.
    fn write_data(
        &mut self,
        peer: &str,
        id: u64,
        index: u32,
        data: &[u8],
    ) -> Result<Option<FileAckPayload>, String> {
        let board = self.board.clone();
        let inc = self.incoming_of(peer, id)?;
        let size = inc.entries.get(index as usize).map(|e| e.size).unwrap_or(0);
        let cur = inc
            .current
            .as_mut()
            .filter(|c| c.index == index)
            .ok_or("数据与文件不对应")?;
        if cur.written + data.len() as u64 > size {
            return Err("收到的数据多于文件大小".into());
        }
        cur.hasher.update(data);
        cur.written += data.len() as u64;
        if let Some(file) = cur.file.as_mut() {
            file.write_all(data)
                .map_err(|e| format!("写入 {}：{e}", cur.path.display()))?;
            board.add_bytes(id, data.len() as u64);
        }
        inc.received += data.len() as u64;
        if inc.received - inc.acked >= relay::ACK_STEP {
            inc.acked = inc.received;
            return Ok(Some(FileAckPayload {
                transfer_id: id,
                bytes: inc.received,
            }));
        }
        Ok(None)
    }

    fn end_entry(&mut self, peer: &str, end: &FileEndPayload) -> Result<(), String> {
        let board = self.board.clone();
        let (preserve, strip, motw, keep_mtime) = self
            .config
            .try_read()
            .map(|c| {
                (
                    c.file_transfer.preserve_mode,
                    c.file_transfer.strip_exec_on_desktop,
                    c.file_transfer.mark_of_web,
                    c.file_transfer.preserve_mtime,
                )
            })
            .unwrap_or((true, true, true, true));
        let inc = self.incoming_of(peer, end.transfer_id)?;
        let i = end.index as usize;
        let size = inc.entries.get(i).map(|e| e.size).unwrap_or(0);
        let Some(cur) = inc.current.take().filter(|c| c.index == end.index) else {
            // Directories have no current file.
            return if inc.entries.get(i).is_some_and(|e| e.is_directory) {
                Ok(())
            } else {
                Err("结束标记与文件不对应".into())
            };
        };
        let Some(file) = cur.file else {
            inc.verified.insert(end.index);
            return Ok(());
        };
        if cur.written != size {
            return Err(format!("{} 不完整", cur.path.display()));
        }
        if cur.hasher.digest() != end.xxhash64 {
            drop(file);
            let _ = std::fs::remove_file(&cur.path);
            return Err(format!("{} 校验失败", cur.path.display()));
        }
        let _ = file.sync_all();
        let _ = std::fs::remove_file(meta_path(&cur.path));
        // Apply mtime after the file is closed (below).
        let mtime = if keep_mtime {
            inc.entries[i].mtime_unix
        } else {
            0
        };
        if mtime > 0 {
            let t = std::time::UNIX_EPOCH + Duration::from_secs(mtime as u64);
            let _ = file.set_modified(t);
        }
        drop(file);
        let finished = if inc.temps[i].is_some() {
            inc.targets[i].clone()
        } else {
            cur.path.clone()
        };
        if inc.temps[i].is_some() {
            let target = &inc.targets[i];
            if std::fs::symlink_metadata(target).is_ok_and(|m| m.is_dir()) {
                std::fs::remove_dir_all(target)
                    .map_err(|e| format!("{}：{e}", target.display()))?;
            }
            std::fs::rename(&cur.path, target)
                .map_err(|e| format!("替换 {}：{e}", target.display()))?;
        }
        let mode = inc.entries.get(i).map(|e| e.mode).unwrap_or(0);
        apply_mode(preserve, strip, &finished, mode);
        mark_of_web(motw, &finished, peer);
        inc.verified.insert(end.index);
        board.file_done(end.transfer_id);
        Ok(())
    }

    async fn finish(&mut self, peer: &str, id: u64) {
        let Some(inc) = self
            .incoming
            .remove(&id)
            .filter(|i| i.peer.eq_ignore_ascii_case(peer))
        else {
            return;
        };
        let files = inc
            .entries
            .iter()
            .zip(&inc.skip)
            .filter(|(e, s)| !**s && !e.is_directory)
            .count();
        if let Some(missing) = inc.missing() {
            let why = format!("{missing} 没有完整收到");
            self.incoming.insert(id, inc);
            self.fail_incoming(id, &why).await;
            return;
        }
        let mut inc = inc;
        if let Some(tx) = inc.ready.take() {
            let _ = tx.send(inc.tops.clone());
        }
        let message = if inc.staged {
            format!("已取到 {files} 个文件，可以粘贴了")
        } else {
            format!("已接收 {files} 个文件，保存在 {}", inc.dest())
        };
        let open_path = match inc.tops.as_slice() {
            [one] if !inc.staged => one.display().to_string(),
            _ => String::new(),
        };
        info!("received {files} files from {peer} (all hashes checked)");
        self.board.update(id, |p| p.open_path = open_path);
        self.board.finish(id, TransferState::Done, message);
    }

    async fn fail_incoming(&mut self, id: u64, why: &str) {
        let Some(inc) = self.incoming.remove(&id) else {
            return;
        };
        let peer = inc.peer.clone();
        discard(inc);
        warn!("receiving {id} from {peer} failed: {why}");
        self.board.finish(id, TransferState::Failed, why);
        let e = FileErrorPayload {
            transfer_id: id,
            message: why.to_string(),
        };
        self.send_json(&peer, MsgType::FileError, &e).await;
    }

    async fn cancel_incoming(&mut self, id: u64) {
        let Some(inc) = self.incoming.remove(&id) else {
            return;
        };
        let peer = inc.peer.clone();
        discard(inc);
        self.board.finish(id, TransferState::Cancelled, "已取消");
        self.send_json(
            &peer,
            MsgType::FileCancel,
            &FileRequestPayload {
                transfer_id: id,
                skip: Vec::new(),
                resume: Vec::new(),
            },
        )
        .await;
    }

    async fn peer_gone(&mut self, peer: &str) {
        let gone: Vec<u64> = self
            .incoming
            .iter()
            .filter(|(_, i)| i.peer.eq_ignore_ascii_case(peer))
            .map(|(id, _)| *id)
            .collect();
        let window_min = self.config.read().await.file_transfer.resume_window_min;
        for id in gone {
            if let Some(inc) = self.incoming.remove(&id) {
                if window_min == 0 {
                    discard(inc);
                    self.board
                        .finish(id, TransferState::Failed, "连接断开，文件未接收完");
                } else {
                    park_partial(inc);
                    self.board.finish(id, TransferState::Paused, "等待重连");
                }
            }
        }
        self.outgoing
            .retain(|_, o| !o.peer.eq_ignore_ascii_case(peer));
        self.sending.retain(|_, s| {
            let keep = !s.peer.eq_ignore_ascii_case(peer);
            if !keep {
                s.cancel.store(true, Ordering::Relaxed);
                s.wake.notify_one();
            }
            keep
        });
        for (other, other_id) in self.relay.drop_peer(peer) {
            let e = FileErrorPayload {
                transfer_id: other_id,
                message: format!("{peer} 的连接断开"),
            };
            self.send_json(&other, MsgType::FileError, &e).await;
        }
    }

    // ------------------------------------------------------------ sending

    /// Stop an offered or running send. True when there was one.
    fn cancel_send(&mut self, id: u64) -> bool {
        if let Some(o) = self.outgoing.remove(&id) {
            o.cancel.store(true, Ordering::Relaxed);
            return true;
        }
        if let Some(s) = self.sending.remove(&id) {
            s.cancel.store(true, Ordering::Relaxed);
            s.wake.notify_one();
            return true;
        }
        false
    }

    async fn start_sending(&mut self, peer: &str, req: FileRequestPayload) {
        let id = req.transfer_id;
        let Some(out) = self
            .outgoing
            .remove(&id)
            .filter(|o| o.peer.eq_ignore_ascii_case(peer))
        else {
            return;
        };
        let windowed = self.net.peer_caps(peer).await & CAP_FILE_ACK != 0;
        let acked = Arc::new(AtomicU64::new(0));
        let wake = Arc::new(Notify::new());
        self.sending.insert(
            id,
            Sending {
                peer: out.peer.clone(),
                cancel: out.cancel.clone(),
                acked: acked.clone(),
                wake: wake.clone(),
            },
        );
        let net = self.net.clone();
        let config = self.config.clone();
        let skip: HashSet<u32> = req.skip.into_iter().collect();
        let resume: HashMap<u32, u64> = req
            .resume
            .into_iter()
            .map(|r| (r.index, r.offset))
            .collect();
        let label = self.label(peer).await;
        tokio::spawn(async move {
            let peer = out.peer.clone();
            let purpose = out.purpose;
            let cancel = out.cancel.clone();
            let flow = Flow {
                windowed,
                acked,
                wake,
            };
            match stream(&net, &config, id, out, &skip, &resume, &flow).await {
                Ok(()) => info!("sent batch {id} ({purpose:?}) to {peer}"),
                Err(_) if cancel.load(Ordering::Relaxed) => {
                    info!("transfer {id} to {peer} cancelled");
                }
                Err(why) => {
                    popup(format!("发送文件到 {label} 失败：{why}"), true);
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
            drop(cancel);
        });
    }
}

/// Decide the local path of every entry. `choices` answers each conflict.
fn place(inc: &mut Incoming, choices: &HashMap<usize, Choice>) -> Result<(), String> {
    let n = inc.entries.len();
    inc.skip = vec![false; n];
    inc.temps = vec![None; n];
    inc.fresh.clear();
    inc.tops.clear();
    let mut top_path: HashMap<usize, PathBuf> = HashMap::new();
    let mut replacing: HashSet<usize> = HashSet::new();
    match &inc.target {
        Target::Dir(dir) => {
            let mut taken = HashSet::new();
            for &t in &inc.shape.tops {
                let name = &inc.entries[t].relative_path;
                let local = match choices.get(&t) {
                    Some(Choice::Skip) => {
                        inc.skip[t] = true;
                        continue;
                    }
                    Some(Choice::Replace) => {
                        replacing.insert(t);
                        name.clone()
                    }
                    Some(Choice::KeepBoth) | None => unique_name(dir, name, &taken),
                };
                taken.insert(local.clone());
                let p = dir.join(&local);
                if !replacing.contains(&t) {
                    inc.fresh.push(p.clone());
                }
                top_path.insert(t, p);
            }
        }
        Target::InPlace => {
            for (&t, p) in inc.shape.tops.iter().zip(inc.targets.clone()) {
                top_path.insert(t, p);
            }
            inc.fresh = top_path.values().cloned().collect();
        }
    }
    let mut targets = Vec::with_capacity(n);
    for i in 0..n {
        let t = inc.shape.top_of[i];
        if !top_path.contains_key(&t) {
            inc.skip[i] = true;
            targets.push(PathBuf::new());
            continue;
        }
        let path = below(&top_path[&t], &inc.shape.rest[i]);
        if replacing.contains(&t)
            && !inc.entries[i].is_directory
            && std::fs::symlink_metadata(&path).is_ok()
        {
            inc.temps[i] = Some(part_path(&path));
        }
        targets.push(path);
    }
    inc.tops = inc
        .shape
        .tops
        .iter()
        .filter_map(|t| top_path.get(t).cloned())
        .collect();
    inc.targets = targets;
    Ok(())
}

fn prepare_incoming(
    peer: &str,
    purpose: TransferPurpose,
    entries: Vec<FileEntry>,
    total: u64,
    sink: Sink,
    cfg: &Config,
) -> Result<Incoming, String> {
    let sum: u64 = entries.iter().map(|e| e.size).sum();
    if sum != total {
        return Err("文件大小不一致".into());
    }
    let mut renamed = Vec::new();
    let mut renamed_count = 0usize;
    let entries: Vec<FileEntry> = entries
        .into_iter()
        .map(|mut e| {
            if let Ok(adapted) = adapt_relative_path(&e.relative_path) {
                if adapted != e.relative_path {
                    renamed_count += 1;
                    if renamed.len() < MAX_RENAMED_SHOWN {
                        renamed.push(RenamedName {
                            from: e.relative_path.clone(),
                            to: adapted.clone(),
                        });
                    }
                    e.relative_path = adapted;
                }
            }
            e
        })
        .collect();
    if renamed_count > 0 {
        info!("renamed {renamed_count} received names so this computer can store them");
    }
    let shape = shape(&entries, cfg)?;
    let n = entries.len();
    let (target, targets, conflicts) = match sink {
        Sink::Dir(dir) => {
            std::fs::create_dir_all(&dir)
                .map_err(|e| format!("无法创建 {}：{e}", dir.display()))?;
            let conflicts = shape
                .tops
                .iter()
                .filter_map(|&t| {
                    let e = &entries[t];
                    let meta = std::fs::symlink_metadata(dir.join(&e.relative_path)).ok()?;
                    let new_size = entries
                        .iter()
                        .zip(&shape.top_of)
                        .filter(|(_, top)| **top == t)
                        .map(|(e, _)| e.size)
                        .sum();
                    Some(TransferConflict {
                        index: t as u32,
                        name: e.relative_path.clone(),
                        is_directory: e.is_directory,
                        new_size,
                        old_size: if meta.is_dir() { 0 } else { meta.len() },
                        old_mtime_unix: mtime_of(&meta),
                    })
                })
                .collect();
            (Target::Dir(dir), Vec::new(), conflicts)
        }
        Sink::InPlace(paths) => {
            if paths.len() != shape.tops.len() {
                return Err("放下的位置与文件不对应".into());
            }
            (Target::InPlace, paths, Vec::new())
        }
    };
    Ok(Incoming {
        peer: peer.to_string(),
        origin_device_id: String::new(),
        purpose,
        entries,
        shape,
        target,
        targets,
        temps: vec![None; n],
        skip: vec![false; n],
        fresh: Vec::new(),
        tops: Vec::new(),
        current: None,
        requested: false,
        received: 0,
        acked: 0,
        resume: HashMap::new(),
        ready: None,
        staged: false,
        verified: HashSet::new(),
        conflicts,
        renamed,
    })
}

#[derive(serde::Serialize, serde::Deserialize)]
struct PartMeta {
    origin_device_id: String,
    relative_path: String,
    size: u64,
    mtime_unix: i64,
    /// xxh64(origin | path | size | mtime); identity for resume.
    #[serde(default)]
    transfer_key: u64,
    written: u64,
    saved_unix: u64,
}

fn transfer_key(origin: &str, relative_path: &str, size: u64, mtime_unix: i64) -> u64 {
    let mut h = Xxh64::new(0);
    h.update(origin.as_bytes());
    h.update(&[0]);
    h.update(relative_path.as_bytes());
    h.update(&[0]);
    h.update(&size.to_le_bytes());
    h.update(&mtime_unix.to_le_bytes());
    h.digest()
}

fn meta_path(part: &Path) -> PathBuf {
    PathBuf::from(format!("{}.json", part.display()))
}

fn read_meta(part: &Path) -> Option<PartMeta> {
    let text = std::fs::read_to_string(meta_path(part)).ok()?;
    serde_json::from_str(&text).ok()
}

fn write_meta(part: &Path, meta: &PartMeta) {
    if let Ok(text) = serde_json::to_string(meta) {
        let _ = std::fs::write(meta_path(part), text);
    }
}

/// Delete `.csc-part` files older than the resume window under the receive dir.
fn cleanup_expired_parts(receive_dir: &str, window_min: u32) {
    let root = if receive_dir.trim().is_empty() {
        csc_config::user_dirs::desktop_dir().or_else(csc_config::user_dirs::home_dir)
    } else {
        Some(PathBuf::from(receive_dir))
    };
    let Some(root) = root else {
        return;
    };
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let window = window_min as u64 * 60;
    let Ok(entries) = std::fs::read_dir(&root) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if !name.ends_with(".csc-part.json") {
            continue;
        }
        let part = path.with_file_name(name.trim_end_matches(".json"));
        let Some(meta) = read_meta(&part) else {
            let _ = std::fs::remove_file(&path);
            continue;
        };
        if window_min == 0 || now.saturating_sub(meta.saved_unix) > window {
            let _ = std::fs::remove_file(&part);
            let _ = std::fs::remove_file(&path);
        }
    }
}

/// Open a new file, or the partial file at `offset` with its hash caught up.
fn open_for_resume(path: &Path, offset: u64) -> Result<(std::fs::File, Xxh64, u64), String> {
    if offset == 0 {
        let file = std::fs::File::create(path).map_err(|e| format!("{}：{e}", path.display()))?;
        return Ok((file, Xxh64::new(0), 0));
    }
    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .map_err(|e| format!("{}：{e}", path.display()))?;
    let mut hasher = Xxh64::new(0);
    let mut written = 0u64;
    let mut buf = [0u8; 64 * 1024];
    while written < offset {
        let want = ((offset - written) as usize).min(buf.len());
        let n = file
            .read(&mut buf[..want])
            .map_err(|e| format!("{}：{e}", path.display()))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        written += n as u64;
    }
    if written != offset {
        file.set_len(0)
            .map_err(|e| format!("{}：{e}", path.display()))?;
        file.seek(SeekFrom::Start(0))
            .map_err(|e| format!("{}：{e}", path.display()))?;
        return Ok((file, Xxh64::new(0), 0));
    }
    Ok((file, hasher, written))
}

/// Partial files from the same source, still inside the resume window.
fn resume_matches(inc: &mut Incoming, window_min: u32) -> Vec<FileResume> {
    if window_min == 0 || inc.origin_device_id.is_empty() {
        return Vec::new();
    }
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let window = window_min as u64 * 60;
    let mut out = Vec::new();
    for i in 0..inc.entries.len() {
        if inc.skip[i] || inc.entries[i].is_directory {
            continue;
        }
        let dest = &inc.targets[i];
        if dest.as_os_str().is_empty() {
            continue;
        }
        let part = part_path(dest);
        let Some(meta) = read_meta(&part) else {
            continue;
        };
        let fresh = now.saturating_sub(meta.saved_unix) <= window;
        let key = transfer_key(
            &inc.origin_device_id,
            &inc.entries[i].relative_path,
            inc.entries[i].size,
            inc.entries[i].mtime_unix,
        );
        let identity = if meta.transfer_key != 0 {
            meta.transfer_key == key
        } else {
            meta.origin_device_id == inc.origin_device_id
                && meta.relative_path == inc.entries[i].relative_path
                && meta.size == inc.entries[i].size
                && meta.mtime_unix == inc.entries[i].mtime_unix
        };
        let same = identity && meta.written > 0 && meta.written < meta.size;
        let len_ok = std::fs::metadata(&part).is_ok_and(|m| m.len() == meta.written);
        if fresh && same && len_ok {
            inc.temps[i] = Some(part);
            out.push(FileResume {
                index: i as u32,
                offset: meta.written,
            });
        } else {
            let _ = std::fs::remove_file(&part);
            let _ = std::fs::remove_file(meta_path(&part));
        }
    }
    out
}

/// Keep partial files so a later offer from the same computer can continue.
fn park_partial(mut inc: Incoming) {
    if let Some(cur) = inc.current.take() {
        drop(cur.file);
    }
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    for i in 0..inc.entries.len() {
        if inc.skip.get(i).copied().unwrap_or(true) || inc.entries[i].is_directory {
            continue;
        }
        let dest = &inc.targets[i];
        if dest.as_os_str().is_empty() {
            continue;
        }
        let writing = inc.temps[i].clone().unwrap_or_else(|| dest.clone());
        let Ok(meta_len) = std::fs::metadata(&writing).map(|m| m.len()) else {
            continue;
        };
        let size = inc.entries[i].size;
        if meta_len == 0 || meta_len >= size {
            continue;
        }
        let part = part_path(dest);
        if writing != part && std::fs::rename(&writing, &part).is_err() {
            continue;
        }
        write_meta(
            &part,
            &PartMeta {
                origin_device_id: inc.origin_device_id.clone(),
                relative_path: inc.entries[i].relative_path.clone(),
                size,
                mtime_unix: inc.entries[i].mtime_unix,
                transfer_key: transfer_key(
                    &inc.origin_device_id,
                    &inc.entries[i].relative_path,
                    size,
                    inc.entries[i].mtime_unix,
                ),
                written: meta_len,
                saved_unix: now,
            },
        );
    }
}

/// Drop a failed or cancelled batch: the partial file, and every top-level
/// item this batch created. Files it was replacing stay as they were.
fn discard(inc: Incoming) {
    if let Some(cur) = inc.current {
        if let Some(file) = cur.file {
            drop(file);
            let _ = std::fs::remove_file(&cur.path);
        }
    }
    for p in inc.temps.iter().flatten() {
        let _ = std::fs::remove_file(p);
    }
    if !inc.requested {
        return;
    }
    for p in &inc.fresh {
        if std::fs::symlink_metadata(p).is_ok_and(|m| m.is_dir()) {
            let _ = std::fs::remove_dir_all(p);
        } else {
            let _ = std::fs::remove_file(p);
        }
    }
    debug!("discarded incoming batch ({:?})", inc.purpose);
}

// ---------------------------------------------------------------- streaming

struct Flow {
    windowed: bool,
    acked: Arc<AtomicU64>,
    wake: Arc<Notify>,
}

impl Flow {
    /// Wait until fewer than `WINDOW` bytes are unacknowledged.
    async fn room(&self, sent: u64, cancel: &AtomicBool) -> Result<(), String> {
        if !self.windowed {
            return Ok(());
        }
        let mut since = Instant::now();
        let mut last = self.acked.load(Ordering::Acquire);
        while sent.saturating_sub(self.acked.load(Ordering::Acquire)) > WINDOW {
            if cancel.load(Ordering::Relaxed) {
                return Err("已取消".into());
            }
            let _ = tokio::time::timeout(Duration::from_millis(500), self.wake.notified()).await;
            let now = self.acked.load(Ordering::Acquire);
            if now != last {
                last = now;
                since = Instant::now();
            } else if since.elapsed() > ACK_TIMEOUT {
                return Err("对方长时间没有响应".into());
            }
        }
        Ok(())
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
    skip: &HashSet<u32>,
    resume: &HashMap<u32, u64>,
    flow: &Flow,
) -> Result<(), String> {
    let (limit, rate) = {
        let c = config.read().await;
        (
            c.file_transfer.limit_speed,
            c.file_transfer.max_speed_mibs.max(1) as f64,
        )
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
        if skip.contains(&index) {
            continue;
        }
        bulk_json(
            net,
            &out.peer,
            MsgType::FileBegin,
            &FileBeginPayload {
                transfer_id: id,
                index,
            },
        )
        .await?;
        let Some(src) = &out.sources[i] else {
            continue;
        };
        let mut file = tokio::fs::File::open(src)
            .await
            .map_err(|e| format!("{}：{e}", src.display()))?;
        let mut hasher = Xxh64::new(0);
        let offset = resume.get(&index).copied().unwrap_or(0).min(entry.size);
        let mut hashed = 0u64;
        while hashed < offset {
            let want = ((offset - hashed) as usize).min(CHUNK);
            let n = file
                .read(&mut buf[..want])
                .await
                .map_err(|e| format!("{}：{e}", src.display()))?;
            if n == 0 {
                return Err(format!("{} 在续传时变小了", src.display()));
            }
            hasher.update(&buf[..n]);
            hashed += n as u64;
        }
        let mut left = entry.size - offset;
        // Whole small files ride in one FILE_BATCH frame (Doc §12.8).
        if offset == 0 && entry.size > 0 && entry.size <= 64 * 1024 && left == entry.size {
            let mut whole = vec![0u8; entry.size as usize];
            file.read_exact(&mut whole)
                .await
                .map_err(|e| format!("{}：{e}", src.display()))?;
            hasher.update(&whole);
            let digest = hasher.digest();
            let Some(frame) = encode_file_batch(id, &[(index, digest, whole.as_slice())]) else {
                return Err("小文件打包失败".into());
            };
            flow.room(sent, &out.cancel).await?;
            if !net.send_bulk(&out.peer, frame).await {
                return Err("连接已断开".into());
            }
            sent += entry.size;
            left = 0;
            if limit {
                let due = Duration::from_secs_f64(sent as f64 / bytes_per_sec);
                if let Some(wait) = due.checked_sub(started.elapsed()) {
                    tokio::time::sleep(wait).await;
                }
            }
        }
        while left > 0 {
            flow.room(sent, &out.cancel).await?;
            let want = (left as usize).min(CHUNK);
            let n = file
                .read(&mut buf[..want])
                .await
                .map_err(|e| format!("{}：{e}", src.display()))?;
            if n == 0 {
                return Err(format!("{} 在发送时变小了", src.display()));
            }
            hasher.update(&buf[..n]);
            if !net
                .send_bulk(&out.peer, encode_file_data(id, index, &buf[..n]))
                .await
            {
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
    info!("sent {} to {}", mib(out.total), out.peer);
    bulk_json(
        net,
        &out.peer,
        MsgType::FileDone,
        &FileDonePayload { transfer_id: id },
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("csc-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn entry(rel: &str, size: u64, dir: bool) -> FileEntry {
        FileEntry {
            relative_path: rel.into(),
            size,
            mtime_unix: 0,
            is_directory: dir,
            mode: 0,
        }
    }

    fn incoming_in(dir: &Path, entries: Vec<FileEntry>) -> Incoming {
        let total = entries.iter().map(|e| e.size).sum();
        prepare_incoming(
            "peer",
            TransferPurpose::Paste,
            entries,
            total,
            Sink::Dir(dir.to_path_buf()),
            &Config::default(),
        )
        .expect("prepare")
    }

    #[test]
    fn plan_rejects_windows_names() {
        let dir = temp("plan");
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
        let dir = temp("uniq");
        std::fs::write(dir.join("a.txt"), b"").unwrap();
        let none = HashSet::new();
        assert_eq!(unique_name(&dir, "a.txt", &none), "a (1).txt");
        assert_eq!(unique_name(&dir, "b.txt", &none), "b.txt");
        let taken: HashSet<String> = ["a (1).txt".to_string()].into();
        assert_eq!(unique_name(&dir, "a.txt", &taken), "a (2).txt");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn existing_top_names_are_conflicts() {
        let dir = temp("conf");
        std::fs::write(dir.join("a.txt"), b"old").unwrap();
        std::fs::create_dir_all(dir.join("pics")).unwrap();
        let inc = incoming_in(
            &dir,
            vec![
                entry("a.txt", 3, false),
                entry("pics", 0, true),
                entry("pics/x.png", 4, false),
                entry("new.txt", 1, false),
            ],
        );
        let names: Vec<_> = inc
            .conflicts
            .iter()
            .map(|c| (c.index, c.name.as_str()))
            .collect();
        assert_eq!(names, vec![(0, "a.txt"), (1, "pics")]);
        assert_eq!(inc.conflicts[1].new_size, 4);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn choices_decide_paths_and_skips() {
        let dir = temp("place");
        std::fs::write(dir.join("a.txt"), b"old").unwrap();
        std::fs::create_dir_all(dir.join("pics")).unwrap();
        std::fs::write(dir.join("pics").join("x.png"), b"old").unwrap();
        std::fs::write(dir.join("b.txt"), b"old").unwrap();
        let mut inc = incoming_in(
            &dir,
            vec![
                entry("a.txt", 3, false),
                entry("pics", 0, true),
                entry("pics/x.png", 4, false),
                entry("pics/y.png", 4, false),
                entry("b.txt", 1, false),
            ],
        );
        let choices: HashMap<usize, Choice> = [
            (0, Choice::KeepBoth),
            (1, Choice::Replace),
            (4, Choice::Skip),
        ]
        .into();
        place(&mut inc, &choices).unwrap();
        assert_eq!(inc.targets[0], dir.join("a (1).txt"));
        assert_eq!(inc.temps[0], None);
        // Replacing a folder merges: existing files are written beside and renamed over.
        assert_eq!(inc.targets[2], dir.join("pics").join("x.png"));
        assert_eq!(inc.temps[2], Some(dir.join("pics").join(".x.png.csc-part")));
        assert_eq!(inc.temps[3], None);
        assert_eq!(inc.skip, vec![false, false, false, false, true]);
        assert_eq!(inc.fresh, vec![dir.join("a (1).txt")]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn replaced_file_survives_a_failed_batch() {
        let dir = temp("replace");
        std::fs::write(dir.join("a.txt"), b"old").unwrap();
        let mut inc = incoming_in(&dir, vec![entry("a.txt", 3, false)]);
        place(&mut inc, &[(0, Choice::Replace)].into()).unwrap();
        inc.requested = true;
        let part = inc.temps[0].clone().unwrap();
        std::fs::write(&part, b"ne").unwrap();
        discard(inc);
        assert!(!part.exists());
        assert_eq!(std::fs::read(dir.join("a.txt")).unwrap(), b"old");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn matching_partial_file_resumes() {
        let dir = temp("resume");
        let mut inc = incoming_in(&dir, vec![entry("a.txt", 10, false)]);
        place(&mut inc, &HashMap::new()).unwrap();
        inc.origin_device_id = "dev-1".into();
        let part = part_path(&dir.join("a.txt"));
        std::fs::write(&part, b"hello").unwrap();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        write_meta(
            &part,
            &PartMeta {
                origin_device_id: "dev-1".into(),
                relative_path: "a.txt".into(),
                size: 10,
                mtime_unix: 0,
                transfer_key: transfer_key("dev-1", "a.txt", 10, 0),
                written: 5,
                saved_unix: now,
            },
        );
        let resume = resume_matches(&mut inc, 30);
        assert_eq!(resume.len(), 1);
        assert_eq!(resume[0].offset, 5);
        assert_eq!(inc.temps[0].as_deref(), Some(part.as_path()));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn in_place_uses_the_found_placeholders() {
        let dir = temp("inplace");
        let found = dir.join("报告 (1).docx");
        let entries = vec![
            entry("报告.docx", 2, false),
            entry("照片", 0, true),
            entry("照片/a.jpg", 1, false),
        ];
        let mut inc = prepare_incoming(
            "peer",
            TransferPurpose::Paste,
            entries,
            3,
            Sink::InPlace(vec![found.clone(), dir.join("照片")]),
            &Config::default(),
        )
        .unwrap();
        place(&mut inc, &HashMap::new()).unwrap();
        assert_eq!(inc.targets[0], found);
        assert_eq!(inc.targets[2], dir.join("照片").join("a.jpg"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn engine(dir: &Path) -> Engine {
        let mut cfg = Config::default();
        cfg.clipboard.files = true;
        cfg.file_transfer.receive_dir = dir.display().to_string();
        cfg.file_transfer.resume_window_min = 0;
        Engine {
            net: NetHub::offline(),
            config: Arc::new(RwLock::new(cfg)),
            board: TransferBoard::default(),
            outgoing: HashMap::new(),
            sending: HashMap::new(),
            incoming: HashMap::new(),
            expect_paste: None,
            relay: Relay::default(),
            next_id: 1,
        }
    }

    fn json<T: serde::Serialize>(v: &T) -> Vec<u8> {
        serde_json::to_vec(v).unwrap()
    }

    fn data(id: u64, index: u32, bytes: &[u8]) -> Vec<u8> {
        let mut p = Vec::new();
        p.extend_from_slice(&id.to_le_bytes());
        p.extend_from_slice(&index.to_le_bytes());
        p.extend_from_slice(bytes);
        p
    }

    fn digest(bytes: &[u8]) -> u64 {
        let mut h = Xxh64::new(0);
        h.update(bytes);
        h.digest()
    }

    /// Offer one file, then send `sent` bytes and a FileEnd hashed over `real`.
    async fn deliver(e: &mut Engine, id: u64, name: &str, real: &[u8], sent: &[u8], end: bool) {
        let offer = FileOfferPayload {
            transfer_id: id,
            purpose: TransferPurpose::Paste,
            entries: vec![entry(name, real.len() as u64, false)],
            total_bytes: real.len() as u64,
            origin: String::new(),
            relay_to: String::new(),
            origin_device_id: String::new(),
        };
        e.on_frame("win", MsgType::FileOffer, json(&offer))
            .await
            .unwrap();
        let begin = FileBeginPayload {
            transfer_id: id,
            index: 0,
        };
        e.on_frame("win", MsgType::FileBegin, json(&begin))
            .await
            .unwrap();
        for chunk in sent.chunks(7) {
            e.on_frame("win", MsgType::FileData, data(id, 0, chunk))
                .await
                .unwrap();
        }
        if end {
            let fin = FileEndPayload {
                transfer_id: id,
                index: 0,
                xxhash64: digest(real),
            };
            e.on_frame("win", MsgType::FileEnd, json(&fin))
                .await
                .unwrap();
        }
        e.on_frame(
            "win",
            MsgType::FileDone,
            json(&FileDonePayload { transfer_id: id }),
        )
        .await
        .unwrap();
    }

    fn expect(e: &mut Engine, peer: &str, sink: PasteSink) -> PasteReady {
        let (tx, rx) = oneshot::channel();
        e.expect_paste = Some(Expect {
            peer: peer.into(),
            sink,
            at: Instant::now(),
            ready: Some(tx),
        });
        rx
    }

    #[tokio::test]
    async fn fetched_ahead_files_arrive_whole_and_verified() {
        let dir = temp("stage-ok");
        let stage = dir.join("stage");
        std::fs::create_dir_all(&stage).unwrap();
        let mut e = engine(&dir);
        let ready = expect(&mut e, "win", PasteSink::Stage(stage.clone()));
        let body: Vec<u8> = (0..1000u32).flat_map(|i| i.to_le_bytes()).collect();
        deliver(&mut e, 11, "固件.bin", &body, &body, true).await;
        let paths = ready.await.expect("batch reported ready");
        assert_eq!(paths, vec![stage.join("固件.bin")]);
        assert_eq!(std::fs::read(&paths[0]).unwrap(), body);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn corrupted_data_fails_and_leaves_no_file() {
        let dir = temp("stage-bad");
        let stage = dir.join("stage");
        std::fs::create_dir_all(&stage).unwrap();
        let mut e = engine(&dir);
        let ready = expect(&mut e, "win", PasteSink::Stage(stage.clone()));
        let body = b"0123456789abcdefghij".to_vec();
        let mut bad = body.clone();
        bad[5] ^= 0xff;
        deliver(&mut e, 12, "a.hex", &body, &bad, true).await;
        assert!(
            ready.await.is_err(),
            "a hash mismatch must not be reported ready"
        );
        assert!(!stage.join("a.hex").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn batch_without_file_end_is_not_done() {
        let dir = temp("stage-short");
        let stage = dir.join("stage");
        std::fs::create_dir_all(&stage).unwrap();
        let mut e = engine(&dir);
        let ready = expect(&mut e, "win", PasteSink::Stage(stage.clone()));
        let body = b"0123456789".to_vec();
        deliver(&mut e, 13, "a.md", &body, &body[..4], false).await;
        assert!(ready.await.is_err());
        assert!(!stage.join("a.md").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn placeholder_copy_is_filled_in_place() {
        let dir = temp("inplace-fill");
        let copy = dir.join("报告.docx");
        let mut marker = b"CSC-PLACEHOLDER\0".to_vec();
        marker.extend_from_slice(&[7u8; 32]);
        std::fs::write(&copy, &marker).unwrap();
        let mut e = engine(&dir);
        let ready = expect(&mut e, "win", PasteSink::InPlace(vec![copy.clone()]));
        let body = b"real document body".to_vec();
        deliver(&mut e, 14, "报告.docx", &body, &body, true).await;
        assert_eq!(ready.await.unwrap(), vec![copy.clone()]);
        assert_eq!(std::fs::read(&copy).unwrap(), body);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn unfilled_placeholders_are_removed_and_never_reused() {
        let dir = temp("inplace-stale");
        let copy = dir.join("Cat.hex");
        let mut marker = b"CSC-PLACEHOLDER\0".to_vec();
        marker.extend_from_slice(&[9u8; 32]);
        std::fs::write(&copy, &marker).unwrap();
        let mut e = engine(&dir);
        // Placeholders for a paste whose offer never came (the link dropped).
        let ready = expect(&mut e, "win", PasteSink::InPlace(vec![copy.clone()]));
        // A later offer from another computer must not be written over them.
        assert!(!e.expect_paste.as_ref().unwrap().fits("mac", Instant::now()));
        let stale = Instant::now() + EXPECT_TTL;
        assert!(!e.expect_paste.as_ref().unwrap().fits("win", stale));
        e.expect_paste = None;
        assert!(ready.await.is_err());
        if cfg!(all(unix, not(target_os = "macos"))) {
            assert!(
                !copy.exists(),
                "a 48-byte marker must not stay behind as the file"
            );
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn older_purposes_map_to_paste() {
        assert_eq!(
            TransferPurpose::Clipboard.normalized(),
            TransferPurpose::Paste
        );
    }
}
