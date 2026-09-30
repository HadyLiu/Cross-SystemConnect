//! IPC client talking to Cross-SystemConnect-core.

use csc_proto::{encode_ipc_message, try_decode_ipc_message, IpcRequest, IpcResponse};
use serde_json::{json, Value};
use std::io::{Read, Write};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

static NEXT_ID: AtomicU64 = AtomicU64::new(1);

#[cfg(windows)]
pub const CORE_PIPE: &str = r"\\.\pipe\Cross-SystemConnect-core";
pub const CORE_BIN: &str = "Cross-SystemConnect-core";

fn ipc_endpoint() -> String {
    #[cfg(windows)]
    {
        CORE_PIPE.to_string()
    }
    #[cfg(unix)]
    {
        csc_proto::core_socket_path().display().to_string()
    }
}

#[cfg(windows)]
fn open_pipe() -> anyhow::Result<std::fs::File> {
    std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(CORE_PIPE)
        .map_err(|e| anyhow::anyhow!("连接内核 {} 失败: {e}", ipc_endpoint()))
}

#[cfg(unix)]
fn open_pipe() -> anyhow::Result<std::os::unix::net::UnixStream> {
    let path = csc_proto::core_socket_path();
    std::os::unix::net::UnixStream::connect(&path)
        .map_err(|e| anyhow::anyhow!("连接内核 {} 失败: {e}", path.display()))
}

fn core_is_down(err: &anyhow::Error) -> bool {
    let text = err.to_string();
    text.contains("连接内核")
        || text.contains("os error 2")
        || text.contains("os error 111")
        || text.contains("Connection refused")
        || text.contains("没有那个文件或目录")
}

fn call_raw(method: &str, params: &Value) -> anyhow::Result<IpcResponse> {
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    let req = IpcRequest {
        id,
        method: method.to_string(),
        params: params.clone(),
    };
    let bytes = encode_ipc_message(&req)?;

    let mut conn = open_pipe()?;
    conn.write_all(&bytes)?;
    let _ = conn.flush();
    // Linux: closing the write half lets the core finish the request. A blocking
    // read can also return EINTR while WebKit is delivering signals; retry that.
    #[cfg(unix)]
    {
        use std::net::Shutdown;
        let _ = conn.shutdown(Shutdown::Write);
        let _ = conn.set_read_timeout(Some(Duration::from_secs(3)));
    }
    read_ipc_response(&mut conn)
}

fn read_ipc_response(conn: &mut impl Read) -> anyhow::Result<IpcResponse> {
    let mut buf = vec![0u8; 64 * 1024];
    let mut got = Vec::new();
    for _ in 0..128 {
        let n = match conn.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) if e.kind() == std::io::ErrorKind::TimedOut => {
                anyhow::bail!("ipc timed out waiting for the core")
            }
            Err(e) => return Err(e.into()),
        };
        got.extend_from_slice(&buf[..n]);
        if let Some((_, value)) = try_decode_ipc_message(&got)? {
            return Ok(serde_json::from_value(value)?);
        }
    }
    anyhow::bail!("ipc closed without response")
}

fn decode_response(resp: IpcResponse) -> anyhow::Result<Value> {
    if resp.ok {
        Ok(resp.result.unwrap_or(Value::Null))
    } else {
        anyhow::bail!(resp.error.unwrap_or_else(|| "unknown error".into()))
    }
}

pub fn call(method: &str, params: Value) -> anyhow::Result<Value> {
    match call_raw(method, &params) {
        Ok(resp) => decode_response(resp),
        Err(err) if core_is_down(&err) => {
            ensure_core_running()
                .map_err(|start| anyhow::anyhow!("{start}（套接字 {}）", ipc_endpoint()))?;
            decode_response(call_raw(method, &params)?)
        }
        Err(err) => Err(err),
    }
}

pub fn try_connect() -> bool {
    call_raw("get_status", &json!({}))
        .and_then(decode_response)
        .is_ok()
}

struct StartFailure {
    at: Instant,
    message: String,
}

fn start_failure() -> &'static Mutex<Option<StartFailure>> {
    static SLOT: Mutex<Option<StartFailure>> = Mutex::new(None);
    &SLOT
}

pub fn ensure_core_running() -> anyhow::Result<()> {
    if try_connect() {
        return Ok(());
    }
    let mut slot = start_failure()
        .lock()
        .unwrap_or_else(|err| err.into_inner());
    if try_connect() {
        *slot = None;
        return Ok(());
    }
    if let Some(prev) = slot.as_ref() {
        if prev.at.elapsed() < Duration::from_secs(3) {
            anyhow::bail!("{}", prev.message);
        }
    }
    match spawn_core() {
        Ok(()) => {
            *slot = None;
            Ok(())
        }
        Err(err) => {
            *slot = Some(StartFailure {
                at: Instant::now(),
                message: err.to_string(),
            });
            Err(err)
        }
    }
}

fn spawn_core() -> anyhow::Result<()> {
    let exe = std::env::current_exe()?;
    let core = exe
        .parent()
        .map(|p| p.join(CORE_BIN))
        .unwrap_or_else(|| std::path::PathBuf::from(CORE_BIN));
    #[cfg(windows)]
    let core = core.with_extension("exe");

    if !core.exists() {
        anyhow::bail!(
            "找不到内核程序 {}。套接字不会被创建。请先执行 cargo build -p csc-core -p csc-ui，再重新打开界面。",
            core.display()
        );
    }

    let mut cmd = std::process::Command::new(&core);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x08000000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    let mut child = cmd
        .spawn()
        .map_err(|e| anyhow::anyhow!("failed to spawn {}: {e}", core.display()))?;

    for _ in 0..80 {
        std::thread::sleep(Duration::from_millis(100));
        if try_connect() {
            return Ok(());
        }
        if let Ok(Some(status)) = child.try_wait() {
            if try_connect() {
                return Ok(());
            }
            anyhow::bail!(
                "内核已退出 ({status})，无法读取配置、日志和计算机名。端点 {}",
                ipc_endpoint()
            );
        }
    }
    anyhow::bail!("内核在 8 秒内没有就绪。端点 {}", ipc_endpoint())
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

pub fn send_files(peer: &str, paths: &[String]) -> anyhow::Result<Value> {
    call("send_files", json!({"peer": peer, "paths": paths}))
}

pub fn drag_enter(peer: &str, paths: &[String]) -> anyhow::Result<Value> {
    call("drag_enter", json!({"peer": peer, "paths": paths}))
}

pub fn cancel_transfer(id: u64) -> anyhow::Result<Value> {
    call("cancel_transfer", json!({"id": id}))
}

pub fn dismiss_transfer(id: u64) -> anyhow::Result<Value> {
    call("dismiss_transfer", json!({"id": id}))
}

pub fn resolve_conflict(id: u64, decisions: Value) -> anyhow::Result<Value> {
    call("resolve_conflict", json!({"transferId": id, "decisions": decisions}))
}

/// Folder incoming files go to when no drop target named one.
pub fn receive_dir() -> anyhow::Result<String> {
    let v = call("receive_dir", json!({}))?;
    Ok(v.get("path").and_then(|p| p.as_str()).unwrap_or_default().to_string())
}

pub fn rescan() -> anyhow::Result<Value> {
    call("rescan", json!({}))
}

pub fn get_logs() -> anyhow::Result<Value> {
    call("get_logs", json!({}))
}

pub fn resolve_peer(host_name: &str, decision: &str) -> anyhow::Result<Value> {
    call(
        "resolve_peer",
        json!({"hostName": host_name, "decision": decision}),
    )
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
    let _ = call_raw("quit", &json!({})).and_then(decode_response);
    for _ in 0..15 {
        std::thread::sleep(Duration::from_millis(100));
        if !try_connect() {
            return Ok(());
        }
    }
    kill_core_by_lock();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::read_ipc_response;
    use csc_proto::{encode_ipc_message, IpcResponse};
    use std::io::{self, Cursor, Read};

    struct InterruptOnce<R> {
        inner: R,
        interrupted: bool,
    }

    impl<R: Read> Read for InterruptOnce<R> {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            if !self.interrupted {
                self.interrupted = true;
                return Err(io::Error::new(io::ErrorKind::Interrupted, "eintr"));
            }
            self.inner.read(buf)
        }
    }

    #[test]
    fn missing_socket_counts_as_core_down() {
        let err = anyhow::anyhow!(
            "连接内核 /run/user/1000/cross-systemconnect-core.sock 失败: No such file or directory (os error 2)"
        );
        assert!(super::core_is_down(&err));
        let other = anyhow::anyhow!("ipc timed out waiting for the core");
        assert!(!super::core_is_down(&other));
    }

    #[test]
    fn ipc_read_retries_interrupted_then_decodes() {
        let raw =
            encode_ipc_message(&IpcResponse::ok(7, serde_json::json!({"text": "line"}))).unwrap();
        let conn = InterruptOnce {
            inner: Cursor::new(raw),
            interrupted: false,
        };
        let mut conn = conn;
        let resp = read_ipc_response(&mut conn).unwrap();
        assert!(resp.ok);
        assert_eq!(resp.result.unwrap()["text"], "line");
    }
}
