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
    /// File the core logger is appending to.
    pub log_path: PathBuf,
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
            let s = state.status.read().await.clone();
            match serde_json::to_value(s) {
                Ok(v) => IpcResponse::ok(req.id, v),
                Err(e) => IpcResponse::err(req.id, e.to_string()),
            }
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
            let before_role = cfg.role.clone();
            let before_port = cfg.control_port;
            let before_bind = cfg.bind_address.clone();
            let before_name = cfg.name.clone();
            let before_search = cfg.search_address.clone();
            let before_layout = cfg.layout.clone();
            let before_screens = cfg.screens.clone();
            match cfg.merge_from_value(req.params) {
                Ok(()) => match cfg.save(&state.config_path) {
                    Ok(()) => {
                        let restart = before_role != cfg.role
                            || before_port != cfg.control_port
                            || before_bind != cfg.bind_address
                            || before_name != cfg.name
                            || before_search != cfg.search_address;
                        let layout_changed =
                            before_layout != cfg.layout || before_screens != cfg.screens;
                        refresh_status_from_config(state, &cfg).await;
                        drop(cfg);
                        if restart {
                            state.net_reload.send_modify(|n| *n = n.wrapping_add(1));
                        }
                        if layout_changed {
                            state.layout_reload.send_modify(|n| *n = n.wrapping_add(1));
                        }
                        IpcResponse::ok(req.id, json!({"ok": true}))
                    }
                    Err(e) => IpcResponse::err(req.id, e.to_string()),
                },
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
            let choice = match decision {
                "trust" => Choice::Trust,
                "reject" | "cancel" | "once" => Choice::Reject,
                _ => return IpcResponse::err(req.id, "decision must be trust or reject"),
            };
            match state.prompt.resolve(&state.status, host, choice).await {
                Ok(()) => IpcResponse::ok(req.id, json!({"ok": true})),
                Err(e) => IpcResponse::err(req.id, e),
            }
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
            let path = state.log_path.clone();
            let text = tokio::task::spawn_blocking(move || read_log_tail(&path, 48 * 1024))
                .await
                .unwrap_or_default();
            IpcResponse::ok(
                req.id,
                json!({"text": text, "path": state.log_path.display().to_string()}),
            )
        }
        "quit" => {
            let quit_tx = state.quit_tx.clone();
            tokio::spawn(async move {
                // Let this IPC response reach the UI before tearing down.
                tokio::time::sleep(std::time::Duration::from_millis(80)).await;
                let _ = quit_tx.send(true);
            });
            IpcResponse::ok(req.id, json!({"ok": true}))
        }
        other => IpcResponse::err(req.id, format!("unknown method: {other}")),
    }
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
