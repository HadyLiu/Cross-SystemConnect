//! Local IPC server (Named Pipe / Unix socket).

use crate::prompt::{Choice, PeerPrompt};
use csc_config::Config;
use csc_proto::{
    encode_ipc_message, try_decode_ipc_message, AppStatus, IpcRequest, IpcResponse,
};
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
}

pub async fn run_ipc(state: Arc<AppState>) -> anyhow::Result<()> {
    #[cfg(windows)]
    {
        run_ipc_windows(state).await
    }
    #[cfg(unix)]
    {
        run_ipc_unix(state).await
    }
}

#[cfg(windows)]
async fn run_ipc_windows(state: Arc<AppState>) -> anyhow::Result<()> {
    use tokio::net::windows::named_pipe::ServerOptions;
    const PIPE: &str = r"\\.\pipe\Cross-SystemConnect-core";
    info!("IPC listening on {PIPE}");
    let mut first = true;
    loop {
        let server = if first {
            first = false;
            ServerOptions::new().first_pipe_instance(true).create(PIPE)?
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
        }
    }
    Ok(())
}

#[cfg(unix)]
async fn run_ipc_unix(state: Arc<AppState>) -> anyhow::Result<()> {
    use std::fs;
    use tokio::net::UnixListener;
    let path = unix_socket_path();
    if path.exists() {
        let _ = fs::remove_file(&path);
    }
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    let listener = UnixListener::bind(&path)?;
    info!("IPC listening on {}", path.display());
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
fn unix_socket_path() -> PathBuf {
    if let Ok(runtime) = std::env::var("XDG_RUNTIME_DIR") {
        return PathBuf::from(runtime).join("cross-systemconnect-core.sock");
    }
    PathBuf::from(format!(
        "/tmp/cross-systemconnect-core-{}.sock",
        std::env::var("USER").unwrap_or_else(|_| "user".into())
    ))
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
                "once" => Choice::Once,
                "reject" => Choice::Reject,
                _ => return IpcResponse::err(req.id, "decision must be trust, once, or reject"),
            };
            match state.prompt.resolve(host, choice).await {
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
