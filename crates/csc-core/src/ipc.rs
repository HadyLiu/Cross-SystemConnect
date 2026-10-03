//! Local IPC server (Named Pipe / Unix socket).

use crate::prompt::{Choice, PeerPrompt};
use csc_config::Config;
use csc_proto::{encode_ipc_message, try_decode_ipc_message, AppStatus, IpcRequest, IpcResponse};
use serde_json::json;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::{watch, RwLock};
use tracing::{info, warn};

pub struct AppState {
    pub config: Arc<RwLock<Config>>,
    pub config_path: PathBuf,
    pub status: Arc<RwLock<AppStatus>>,
    /// UI "quit" IPC: delayed notify so the response can flush first.
    pub quit_tx: watch::Sender<bool>,
    pub prompt: Arc<PeerPrompt>,
    /// Bumped when role / name / port / bind address change so discovery restarts.
    pub net_reload: watch::Sender<u64>,
    /// Bumped when the screen arrangement changes. Does not drop connections.
    pub layout_reload: watch::Sender<u64>,
    /// Core logger; follows the log settings when they are saved.
    pub log: crate::logging::LogControl,
    pub transfer: crate::transfer::TransferHandle,
    /// Progress of incoming batches, merged into `get_status`.
    pub board: crate::progress::TransferBoard,
    /// Files dragged into the edge window: (peer, paths).
    pub drag_enter: tokio::sync::mpsc::Sender<(String, Vec<PathBuf>)>,
    /// Tray asked to paste another computer's files into the fallback folder.
    pub paste_here: tokio::sync::mpsc::Sender<()>,
}

fn param_u64(params: &serde_json::Value, keys: &[&str]) -> Option<u64> {
    keys.iter().find_map(|k| {
        let v = params.get(*k)?;
        v.as_u64().or_else(|| v.as_str()?.parse().ok())
    })
}

fn param_paths(params: &serde_json::Value) -> Vec<PathBuf> {
    params
        .get("paths")
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|p| p.as_str())
                .map(PathBuf::from)
                .collect()
        })
        .unwrap_or_default()
}

/// Bind the local IPC endpoint, then serve it in the background.
///
/// Returns once the endpoint exists. Callers can talk to the core immediately;
/// the accept loop keeps running on a spawned task.
/// The IPC endpoint is already owned by a running core.
#[derive(Debug)]
pub struct IpcBusy;

impl std::fmt::Display for IpcBusy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("IPC endpoint already served")
    }
}

impl std::error::Error for IpcBusy {}

pub enum IpcBound {
    #[cfg(windows)]
    Pipe(tokio::net::windows::named_pipe::NamedPipeServer),
    #[cfg(unix)]
    Unix(tokio::net::UnixListener),
}

/// Bind the pipe or socket before discovery starts. A second process gets [`IpcBusy`]
/// and must exit instead of binding the control port as well.
pub fn claim_ipc() -> anyhow::Result<IpcBound> {
    #[cfg(windows)]
    {
        claim_pipe()
    }
    #[cfg(unix)]
    {
        bind_unix_listener().map(IpcBound::Unix)
    }
}

pub async fn serve_ipc(bound: IpcBound, state: Arc<AppState>) -> anyhow::Result<()> {
    match bound {
        #[cfg(windows)]
        IpcBound::Pipe(first) => serve_pipe(first, state).await,
        #[cfg(unix)]
        IpcBound::Unix(listener) => serve_unix(listener, state).await,
    }
}

#[cfg(windows)]
fn claim_pipe() -> anyhow::Result<IpcBound> {
    use tokio::net::windows::named_pipe::ServerOptions;
    const PIPE: &str = r"\\.\pipe\Cross-SystemConnect-core";
    match ServerOptions::new().first_pipe_instance(true).create(PIPE) {
        Ok(server) => {
            info!("IPC listening on {PIPE}");
            Ok(IpcBound::Pipe(server))
        }
        Err(err) if pipe_busy(&err) => Err(IpcBusy.into()),
        Err(err) => Err(err.into()),
    }
}

#[cfg(windows)]
fn pipe_busy(err: &std::io::Error) -> bool {
    // 5 access denied, 231 pipe busy, 183 already exists: another core owns the pipe.
    matches!(err.raw_os_error(), Some(5 | 183 | 231))
        || err.kind() == std::io::ErrorKind::AddrInUse
        || err.kind() == std::io::ErrorKind::PermissionDenied
}

#[cfg(windows)]
async fn serve_pipe(
    first: tokio::net::windows::named_pipe::NamedPipeServer,
    state: Arc<AppState>,
) -> anyhow::Result<()> {
    use tokio::net::windows::named_pipe::ServerOptions;
    const PIPE: &str = r"\\.\pipe\Cross-SystemConnect-core";
    let mut pending = Some(first);
    loop {
        let server = if let Some(server) = pending.take() {
            server
        } else {
            ServerOptions::new().create(PIPE)?
        };
        server.connect().await?;
        let st = state.clone();
        tokio::spawn(async move {
            if let Err(e) = handle_conn_windows(server, st).await {
                warn!("ipc client error: {e:#}");
            }
        });
    }
}

#[cfg(windows)]
async fn handle_conn_windows(
    mut pipe: tokio::net::windows::named_pipe::NamedPipeServer,
    state: Arc<AppState>,
) -> anyhow::Result<()> {
    let mut buf = Vec::new();
    let mut tmp = [0u8; 8192];
    loop {
        let n = pipe.read(&mut tmp).await?;
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&tmp[..n]);
        while let Some((consumed, value)) = try_decode_ipc_message(&buf)? {
            buf.drain(..consumed);
            let req: IpcRequest = serde_json::from_value(value)?;
            let resp = dispatch(&state, req).await;
            let bytes = encode_ipc_message(&resp)?;
            pipe.write_all(&bytes).await?;
            pipe.flush().await?;
        }
    }
    Ok(())
}

#[cfg(unix)]
fn bind_unix_listener() -> anyhow::Result<tokio::net::UnixListener> {
    use std::fs;
    use std::os::unix::net::UnixStream;
    use tokio::net::UnixListener;
    let path = csc_proto::core_socket_path();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    if path.exists() {
        match UnixStream::connect(&path) {
            Ok(stream) => {
                drop(stream);
                info!("IPC socket already served at {}", path.display());
                return Err(IpcBusy.into());
            }
            Err(_) => {
                let _ = fs::remove_file(&path);
            }
        }
    }
    let listener = UnixListener::bind(&path)
        .map_err(|e| anyhow::anyhow!("bind IPC {}: {e}", path.display()))?;
    info!("IPC listening on {}", path.display());
    Ok(listener)
}

#[cfg(unix)]
async fn serve_unix(
    listener: tokio::net::UnixListener,
    state: Arc<AppState>,
) -> anyhow::Result<()> {
    loop {
        let (stream, _) = listener.accept().await?;
        let st = state.clone();
        tokio::spawn(async move {
            if let Err(e) = handle_conn_unix(stream, st).await {
                warn!("ipc client error: {e:#}");
            }
        });
    }
}

#[cfg(unix)]
async fn handle_conn_unix(
    mut stream: tokio::net::UnixStream,
    state: Arc<AppState>,
) -> anyhow::Result<()> {
    let mut buf = Vec::new();
    let mut tmp = [0u8; 8192];
    loop {
        let n = stream.read(&mut tmp).await?;
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&tmp[..n]);
        while let Some((consumed, value)) = try_decode_ipc_message(&buf)? {
            buf.drain(..consumed);
            let req: IpcRequest = serde_json::from_value(value)?;
            let resp = dispatch(&state, req).await;
            let bytes = encode_ipc_message(&resp)?;
            stream.write_all(&bytes).await?;
            stream.flush().await?;
        }
    }
    Ok(())
}

async fn dispatch(state: &AppState, req: IpcRequest) -> IpcResponse {
    match req.method.as_str() {
        "get_status" | "getStatus" => {
            let mut s = state.status.read().await.clone();
            s.transfers = state.board.snapshot();
            match serde_json::to_value(s) {
                Ok(v) => IpcResponse::ok(req.id, v),
                Err(e) => IpcResponse::err(req.id, e.to_string()),
            }
        }
        "receive_dir" => {
            let dir = crate::transfer::receive_root(&*state.config.read().await);
            let _ = std::fs::create_dir_all(&dir);
            IpcResponse::ok(req.id, json!({"path": dir.display().to_string()}))
        }
        "get_config" | "getConfig" => {
            let cfg = state.config.read().await.clone();
            match serde_json::to_value(cfg) {
                Ok(v) => IpcResponse::ok(req.id, v),
                Err(e) => IpcResponse::err(req.id, e.to_string()),
            }
        }
        "save_config" | "saveConfig" => {
            let mut cfg = state.config.write().await;
            let before_role = cfg.role;
            let before_port = cfg.control_port;
            let before_data = cfg.data_port;
            let before_bind = cfg.bind_address.clone();
            let before_name = cfg.name.clone();
            let before_search = cfg.search_address.clone();
            let before_manual = cfg.network.manual_peers.clone();
            let before_layout = cfg.layout.clone();
            let before_screens = cfg.screens.clone();
            let before_log = cfg.log.clone();
            match cfg.merge_from_value(req.params) {
                Ok(()) => {
                    if (before_port != cfg.control_port && !port_free(cfg.control_port))
                        || (before_data != cfg.data_port && !port_free(cfg.data_port))
                    {
                        cfg.control_port = before_port;
                        cfg.data_port = before_data;
                        return IpcResponse::err(req.id, "端口已被占用");
                    }
                    match cfg.save(&state.config_path) {
                    Ok(()) => {
                        if before_log != cfg.log {
                            state.log.apply(&cfg.log);
                        }
                        let restart = before_role != cfg.role
                            || before_port != cfg.control_port
                            || before_bind != cfg.bind_address
                            || before_name != cfg.name
                            || before_search != cfg.search_address
                            || before_manual != cfg.network.manual_peers;
                        let layout_changed =
                            before_layout != cfg.layout || before_screens != cfg.screens;
                        crate::notice::apply(&cfg.notify);
                        crate::notice::set_language(&cfg.gui.language);
                        refresh_status_from_config(state, &cfg).await;
                        drop(cfg);
                        if restart {
                            state.net_reload.send_modify(|n| *n = n.wrapping_add(1));
                        }
                        if layout_changed {
                            info!("screen layout changed");
                        }
                        // Switch settings and hotkeys live in the same reload; it keeps sessions.
                        state.layout_reload.send_modify(|n| *n = n.wrapping_add(1));
                        IpcResponse::ok(req.id, json!({"ok": true}))
                    }
                    Err(e) => IpcResponse::err(req.id, e.to_string()),
                }
                }
                Err(e) => IpcResponse::err(req.id, e.to_string()),
            }
        }
        "resolve_peer" => {
            let decision = req
                .params
                .get("decision")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let host = req
                .params
                .get("hostName")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let code = req
                .params
                .get("code")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let choice = match decision {
                "trust" => Choice::Trust { code },
                "reject" | "cancel" | "once" => Choice::Reject,
                _ => return IpcResponse::err(req.id, "decision must be trust or reject"),
            };
            match state.prompt.resolve(&state.status, host, choice).await {
                Ok(()) => IpcResponse::ok(req.id, json!({"ok": true})),
                Err(e) => IpcResponse::err(req.id, e),
            }
        }
        "forget_peer" => {
            let host = req
                .params
                .get("hostName")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .trim()
                .to_string();
            if host.is_empty() {
                return IpcResponse::err(req.id, "hostName is required");
            }
            let mut cfg = state.config.write().await;
            let removed = cfg.forget_peer(&host);
            match cfg.save(&state.config_path) {
                Ok(()) => IpcResponse::ok(req.id, json!({"ok": true, "removed": removed})),
                Err(e) => IpcResponse::err(req.id, e.to_string()),
            }
        }
        "add_manual_peer" | "remove_manual_peer" => {
            let address = req
                .params
                .get("address")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .trim()
                .to_string();
            if address.is_empty() {
                return IpcResponse::err(req.id, "address is required");
            }
            let port = req
                .params
                .get("control_port")
                .or_else(|| req.params.get("controlPort"))
                .and_then(|v| v.as_u64())
                .and_then(|p| u16::try_from(p).ok())
                .unwrap_or(0);
            let mut cfg = state.config.write().await;
            let changed = if req.method == "add_manual_peer" {
                cfg.add_manual_peer(&address, port)
            } else {
                cfg.remove_manual_peer(&address)
            };
            if let Err(e) = cfg.save(&state.config_path) {
                return IpcResponse::err(req.id, e.to_string());
            }
            drop(cfg);
            if changed {
                state.net_reload.send_modify(|n| *n = n.wrapping_add(1));
            }
            IpcResponse::ok(req.id, json!({"ok": true, "changed": changed}))
        }
        "send_files" => {
            let peer = req
                .params
                .get("peer")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let paths = param_paths(&req.params);
            if peer.is_empty() || paths.is_empty() {
                return IpcResponse::err(req.id, "peer and paths are required");
            }
            info!("sending {} dropped item(s) to {peer}", paths.len());
            state
                .transfer
                .send_files(peer, paths, csc_proto::TransferPurpose::Drag)
                .await;
            IpcResponse::ok(req.id, json!({"ok": true}))
        }
        "drag_enter" => {
            let peer = req
                .params
                .get("peer")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let paths = param_paths(&req.params);
            if peer.is_empty() || paths.is_empty() {
                return IpcResponse::err(req.id, "peer and paths are required");
            }
            let _ = state.drag_enter.try_send((peer, paths));
            IpcResponse::ok(req.id, json!({"ok": true}))
        }
        "cancel_transfer" => {
            let Some(id) = param_u64(&req.params, &["id", "transferId"]) else {
                return IpcResponse::err(req.id, "id is required");
            };
            state.transfer.cancel(id).await;
            IpcResponse::ok(req.id, json!({"ok": true}))
        }
        "dismiss_transfer" => {
            let Some(id) = param_u64(&req.params, &["id", "transferId"]) else {
                return IpcResponse::err(req.id, "id is required");
            };
            state.board.dismiss(id);
            IpcResponse::ok(req.id, json!({"ok": true}))
        }
        "resolve_conflict" => {
            let Some(id) = param_u64(&req.params, &["transferId", "id"]) else {
                return IpcResponse::err(req.id, "transferId is required");
            };
            let decisions: Vec<(u32, crate::transfer::Choice)> = req
                .params
                .get("decisions")
                .and_then(|v| v.as_array())
                .map(|list| {
                    list.iter()
                        .filter_map(|d| {
                            let index = d.get("index")?.as_u64()? as u32;
                            let choice = serde_json::from_value(d.get("choice")?.clone()).ok()?;
                            Some((index, choice))
                        })
                        .collect()
                })
                .unwrap_or_default();
            if decisions.is_empty() {
                return IpcResponse::err(req.id, "decisions are required");
            }
            state.transfer.resolve(id, decisions).await;
            IpcResponse::ok(req.id, json!({"ok": true}))
        }
        "rescan" => {
            state.status.write().await.discovered.clear();
            state.net_reload.send_modify(|n| *n = n.wrapping_add(1));
            info!("rescan requested from UI");
            IpcResponse::ok(req.id, json!({"ok": true}))
        }
        "open_pairing" => {
            let until = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0)
                .saturating_add(120);
            state.status.write().await.pairing_open_until = until;
            info!("pairing window open until {until}");
            IpcResponse::ok(req.id, json!({"ok": true, "until": until}))
        }
        "reset_data" => {
            let keep = req
                .params
                .get("keepPairing")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            let mut cfg = state.config.write().await;
            let peers = cfg.trusted_peers.clone();
            let prints = cfg.peer_fingerprints.clone();
            let device = cfg.device_id.clone();
            *cfg = csc_config::Config::default();
            cfg.device_id = device;
            if keep {
                cfg.trusted_peers = peers;
                cfg.peer_fingerprints = prints;
            }
            if let Err(e) = cfg.save(&state.config_path) {
                return IpcResponse::err(req.id, e.to_string());
            }
            crate::notice::apply(&cfg.notify);
            refresh_status_from_config(state, &cfg).await;
            drop(cfg);
            state.net_reload.send_modify(|n| *n = n.wrapping_add(1));
            state.layout_reload.send_modify(|n| *n = n.wrapping_add(1));
            info!("settings reset (keep pairing: {keep})");
            IpcResponse::ok(req.id, json!({"ok": true}))
        }
        "set_sharing_enabled" => {
            let enabled = req
                .params
                .get("enabled")
                .and_then(|v| v.as_bool())
                .unwrap_or(true);
            let mut s = state.status.write().await;
            s.sharing_enabled = enabled;
            IpcResponse::ok(req.id, json!({"ok": true}))
        }
        "get_logs" | "getLogs" => {
            let Some(path) = state.log.path() else {
                return IpcResponse::ok(req.id, json!({"text": "", "path": "", "to_file": false}));
            };
            let shown = path.display().to_string();
            let text = tokio::task::spawn_blocking(move || read_log_tail(&path, 48 * 1024))
                .await
                .unwrap_or_default();
            IpcResponse::ok(
                req.id,
                json!({"text": text, "path": shown, "to_file": true}),
            )
        }
        "export_diagnostics" => {
            let cfg = state.config.read().await.clone();
            let log = state.log.path();
            match export_diagnostics(&cfg, log.as_deref()) {
                Ok(path) => IpcResponse::ok(req.id, json!({"ok": true, "path": path.display().to_string()})),
                Err(e) => IpcResponse::err(req.id, e),
            }
        }
        "paste_remote_files" => {
            if state.paste_here.try_send(()).is_err() {
                return IpcResponse::err(req.id, "paste is busy");
            }
            IpcResponse::ok(req.id, json!({"ok": true}))
        }
        "quit" => {
            let quit_tx = state.quit_tx.clone();
            tokio::spawn(async move {
                // Let this IPC response reach the UI before tearing down.
                tokio::time::sleep(std::time::Duration::from_millis(80)).await;
                let _ = quit_tx.send(true);
                // The main loop should exit on its own. If it is stuck, leave anyway.
                tokio::time::sleep(std::time::Duration::from_millis(400)).await;
                info!("quit timed out in the main loop; exiting");
                std::process::exit(0);
            });
            IpcResponse::ok(req.id, json!({"ok": true}))
        }
        other => IpcResponse::err(req.id, format!("unknown method: {other}")),
    }
}

fn port_free(port: u16) -> bool {
    std::net::TcpListener::bind(("0.0.0.0", port)).is_ok()
}

fn read_log_tail(path: &std::path::Path, max: u64) -> String {
    use std::io::{Read, Seek, SeekFrom};
    let Ok(meta) = std::fs::metadata(path) else {
        return String::new();
    };
    let Ok(mut file) = std::fs::File::open(path) else {
        return String::new();
    };
    let len = meta.len();
    let start = len.saturating_sub(max);
    if file.seek(SeekFrom::Start(start)).is_err() {
        return String::new();
    }
    let mut buf = Vec::new();
    if file.read_to_end(&mut buf).is_err() {
        return String::new();
    }
    let mut text = String::from_utf8_lossy(&buf).into_owned();
    if start > 0 {
        if let Some(i) = text.find('\n') {
            text.drain(..=i);
        }
    }
    text
}

/// Zip the log and a redacted config. Fingerprints are cut to 8 characters.
fn export_diagnostics(cfg: &Config, log: Option<&std::path::Path>) -> Result<PathBuf, String> {
    let mut redacted = cfg.clone();
    for fp in redacted.peer_fingerprints.values_mut() {
        if fp.len() > 8 {
            fp.truncate(8);
        }
    }
    let config_text = serde_json::to_vec_pretty(&redacted).map_err(|e| e.to_string())?;
    let mut files = vec![("config.json".to_string(), config_text)];
    if let Some(path) = log {
        if let Ok(bytes) = std::fs::read(path) {
            files.push(("log.txt".into(), bytes));
        }
    }
    let parent = log
        .and_then(|p| p.parent())
        .map(|p| p.to_path_buf())
        .unwrap_or_else(std::env::temp_dir);
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let dest = parent.join(format!("diagnostics-{stamp}.zip"));
    write_store_zip(&dest, &files)?;
    Ok(dest)
}

fn write_store_zip(path: &std::path::Path, files: &[(String, Vec<u8>)]) -> Result<(), String> {
    fn u16(n: usize) -> [u8; 2] {
        (n as u16).to_le_bytes()
    }
    fn u32(n: u32) -> [u8; 4] {
        n.to_le_bytes()
    }
    let mut body = Vec::new();
    let mut central = Vec::new();
    for (name, data) in files {
        let name = name.as_bytes();
        let local_at = body.len() as u32;
        body.extend_from_slice(&u32(0x04034b50));
        body.extend_from_slice(&u16(20));
        body.extend_from_slice(&u16(0));
        body.extend_from_slice(&u16(0));
        body.extend_from_slice(&u16(0));
        body.extend_from_slice(&u16(0));
        body.extend_from_slice(&u32(0));
        body.extend_from_slice(&u32(data.len() as u32));
        body.extend_from_slice(&u32(data.len() as u32));
        body.extend_from_slice(&u16(name.len()));
        body.extend_from_slice(&u16(0));
        body.extend_from_slice(name);
        body.extend_from_slice(data);
        central.extend_from_slice(&u32(0x02014b50));
        central.extend_from_slice(&u16(20));
        central.extend_from_slice(&u16(20));
        central.extend_from_slice(&u16(0));
        central.extend_from_slice(&u16(0));
        central.extend_from_slice(&u16(0));
        central.extend_from_slice(&u16(0));
        central.extend_from_slice(&u32(0));
        central.extend_from_slice(&u32(data.len() as u32));
        central.extend_from_slice(&u32(data.len() as u32));
        central.extend_from_slice(&u16(name.len()));
        central.extend_from_slice(&u16(0));
        central.extend_from_slice(&u16(0));
        central.extend_from_slice(&u16(0));
        central.extend_from_slice(&u16(0));
        central.extend_from_slice(&u32(0));
        central.extend_from_slice(&u32(local_at));
        central.extend_from_slice(name);
    }
    let central_at = body.len() as u32;
    body.extend_from_slice(&central);
    body.extend_from_slice(&u32(0x06054b50));
    body.extend_from_slice(&u16(0));
    body.extend_from_slice(&u16(0));
    body.extend_from_slice(&u16(files.len()));
    body.extend_from_slice(&u16(files.len()));
    body.extend_from_slice(&u32(central.len() as u32));
    body.extend_from_slice(&u32(central_at));
    body.extend_from_slice(&u16(0));
    std::fs::write(path, body).map_err(|e| e.to_string())
}

async fn refresh_status_from_config(state: &AppState, cfg: &Config) {
    let mut s = state.status.write().await;
    s.local_name = cfg.name.clone();
    s.role = match cfg.role {
        csc_config::Role::Server => "server".into(),
        csc_config::Role::Client => "client".into(),
    };
    s.discovery_port = cfg.control_port;
    s.link_state = match cfg.role {
        csc_config::Role::Server => "searching".into(),
        csc_config::Role::Client => "announcing".into(),
    };
    if !s.connected {
        s.active_screen = cfg.name.clone();
    }
}
