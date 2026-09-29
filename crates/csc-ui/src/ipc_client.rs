//! IPC client talking to Cross-SystemConnect-core.

use csc_proto::{encode_ipc_message, try_decode_ipc_message, IpcRequest, IpcResponse};
use serde_json::{json, Value};
use std::io::{Read, Write};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

static NEXT_ID: AtomicU64 = AtomicU64::new(1);

pub const CORE_PIPE: &str = r"\\.\pipe\Cross-SystemConnect-core";
pub const CORE_BIN: &str = "Cross-SystemConnect-core";

#[cfg(windows)]
fn open_pipe() -> std::io::Result<std::fs::File> {
    std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(CORE_PIPE)
}

#[cfg(unix)]
fn open_pipe() -> std::io::Result<std::os::unix::net::UnixStream> {
    let path = if let Ok(runtime) = std::env::var("XDG_RUNTIME_DIR") {
        std::path::PathBuf::from(runtime).join("cross-systemconnect-core.sock")
    } else {
        std::path::PathBuf::from(format!(
            "/tmp/cross-systemconnect-core-{}.sock",
            std::env::var("USER").unwrap_or_else(|_| "user".into())
        ))
    };
    std::os::unix::net::UnixStream::connect(path)
}

fn call_raw(method: &str, params: Value) -> anyhow::Result<IpcResponse> {
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    let req = IpcRequest {
        id,
        method: method.to_string(),
        params,
    };
    let bytes = encode_ipc_message(&req)?;

    let mut conn = open_pipe()?;
    conn.write_all(&bytes)?;
    let _ = conn.flush();
    let mut buf = vec![0u8; 256 * 1024];
    let mut got = Vec::new();
    // Bound wait: pipe may deliver response in one or more reads
    for _ in 0..64 {
        let n = conn.read(&mut buf)?;
        if n == 0 {
            break;
        }
        got.extend_from_slice(&buf[..n]);
        if let Some((_, value)) = try_decode_ipc_message(&got)? {
            let resp: IpcResponse = serde_json::from_value(value)?;
            return Ok(resp);
        }
    }
    anyhow::bail!("ipc closed without response")
}

pub fn call(method: &str, params: Value) -> anyhow::Result<Value> {
    let resp = call_raw(method, params)?;
    if resp.ok {
        Ok(resp.result.unwrap_or(Value::Null))
    } else {
        anyhow::bail!(resp.error.unwrap_or_else(|| "unknown error".into()))
    }
}

pub fn try_connect() -> bool {
    call("get_status", json!({})).is_ok()
}

pub fn ensure_core_running() -> anyhow::Result<()> {
    if try_connect() {
        return Ok(());
    }
    let exe = std::env::current_exe()?;
    let core = exe
        .parent()
        .map(|p| p.join(CORE_BIN))
        .unwrap_or_else(|| std::path::PathBuf::from(CORE_BIN));
    #[cfg(windows)]
    let core = core.with_extension("exe");

    if !core.exists() {
        anyhow::bail!("kernel not found next to UI: {}", core.display());
    }

    let mut cmd = std::process::Command::new(&core);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x08000000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    cmd.spawn()
        .map_err(|e| anyhow::anyhow!("failed to spawn {}: {e}", core.display()))?;

    for _ in 0..80 {
        std::thread::sleep(Duration::from_millis(100));
        if try_connect() {
            return Ok(());
        }
    }
    anyhow::bail!("Cross-SystemConnect-core did not become ready over local IPC")
}

pub fn get_status() -> anyhow::Result<Value> {
    call("get_status", json!({}))
}

pub fn get_config() -> anyhow::Result<Value> {
    call("get_config", json!({}))
}

pub fn save_config(body: Value) -> anyhow::Result<Value> {
    call("save_config", body)
}

pub fn resolve_peer(host_name: &str, decision: &str) -> anyhow::Result<Value> {
    call(
        "resolve_peer",
        json!({"hostName": host_name, "decision": decision}),
    )
}

pub fn set_sharing_enabled(enabled: bool) -> anyhow::Result<()> {
    call("set_sharing_enabled", json!({"enabled": enabled}))?;
    Ok(())
}

fn core_lock_path() -> std::path::PathBuf {
    dirs::data_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join("Cross-SystemConnect")
        .join("Cross-SystemConnect-core.lock")
}

fn kill_core_by_lock() {
    let path = core_lock_path();
    let Ok(text) = std::fs::read_to_string(&path) else {
        return;
    };
    let Ok(pid) = text.trim().parse::<u32>() else {
        return;
    };
    if pid == 0 {
        return;
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x08000000;
        let _ = std::process::Command::new("taskkill")
            .args(["/PID", &pid.to_string(), "/F"])
            .creation_flags(CREATE_NO_WINDOW)
            .status();
    }
    #[cfg(unix)]
    {
        let _ = std::process::Command::new("kill")
            .args(["-TERM", &pid.to_string()])
            .status();
    }
    let _ = std::fs::remove_file(path);
}

/// Ask the kernel to exit; if it is still alive shortly after, force-kill via lock PID.
pub fn quit_core() -> anyhow::Result<()> {
    let _ = call("quit", json!({}));
    for _ in 0..15 {
        std::thread::sleep(Duration::from_millis(100));
        if !try_connect() {
            return Ok(());
        }
    }
    kill_core_by_lock();
    Ok(())
}
