#![cfg_attr(windows, windows_subsystem = "windows")]

mod clipboard;
mod discover;
mod file_transfer;
mod ipc;
mod layout;
mod net;
mod path_safe;
mod prompt;
mod switch;

use crate::clipboard::ClipboardSync;
use crate::file_transfer::FileTransfer;
use crate::ipc::{run_ipc, AppState};
use crate::layout::ScreenLayout;
use crate::net::{spawn_network, InboundEvent};
use crate::prompt::PeerPrompt;
use crate::switch::SwitchController;
use anyhow::Context;
use csc_config::{default_config_path, default_log_path, Config};
use csc_platform::{
    create_clipboard, create_input_injector, create_screen_info, query_host_net_info,
};
use csc_proto::{encode_switch_leave_bin, AppStatus, ClipOfferPayload, LocalNic, SwitchPayload};
use std::fs::OpenOptions;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{mpsc, RwLock};
use tracing::{debug, info, warn};
use tracing_subscriber::EnvFilter;

fn log_path_of(cfg: &Config) -> PathBuf {
    if !cfg.log.file.is_empty() {
        PathBuf::from(&cfg.log.file)
    } else {
        default_log_path()
    }
}

fn open_log_file(path: &std::path::Path) -> anyhow::Result<std::fs::File> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("create log dir {}", parent.display()))?;
        }
    }
    OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .with_context(|| format!("open log {}", path.display()))
}

/// Open the configured log, or the default path when that file cannot be created.
/// A missing directory must not stop the process before IPC is listening.
fn init_logging(cfg: &Config) -> anyhow::Result<PathBuf> {
    let level = if !cfg.log.level.is_empty() {
        cfg.log.level.as_str()
    } else {
        cfg.log_level.as_str()
    };
    let filter = EnvFilter::try_new(format!("info,csc_core={level}")).unwrap_or_default();
    let preferred = log_path_of(cfg);
    let fallback = default_log_path();
    let (path, file) = match open_log_file(&preferred) {
        Ok(file) => (preferred, file),
        Err(err) if preferred != fallback => {
            eprintln!("log path {} unavailable: {err:#}", preferred.display());
            (fallback.clone(), open_log_file(&fallback)?)
        }
        Err(err) => return Err(err),
    };
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(file)
        .with_ansi(false)
        .init();
    info!("logging to {}", path.display());
    Ok(path)
}

fn single_instance_lock() -> anyhow::Result<std::fs::File> {
    let dir = default_config_path()
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| PathBuf::from("."));
    std::fs::create_dir_all(&dir)?;
    let lock_path = dir.join("Cross-SystemConnect-core.lock");
    let file = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(&lock_path)?;
    #[cfg(windows)]
    {
        use std::os::windows::io::AsRawHandle;
        // Best-effort: write PID; full exclusive lock via fs2 optional — use try exclusive by opening.
        let _ = file.as_raw_handle();
        std::fs::write(&lock_path, format!("{}", std::process::id()))?;
    }
    #[cfg(unix)]
    {
        std::fs::write(&lock_path, format!("{}", std::process::id()))?;
    }
    Ok(file)
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let config_path = std::env::args()
        .position(|a| a == "--config")
        .and_then(|i| std::env::args().nth(i + 1))
        .map(PathBuf::from)
        .unwrap_or_else(default_config_path);

    let cfg = Config::load_or_create(&config_path).unwrap_or_else(|e| {
        eprintln!("config load failed, using defaults: {e:#}");
        Config::default()
    });
    let log_path = match init_logging(&cfg) {
        Ok(path) => path,
        Err(e) => {
            eprintln!("logging disabled: {e:#}");
            let _ = tracing_subscriber::fmt()
                .with_env_filter(EnvFilter::try_new("info").unwrap_or_default())
                .with_ansi(false)
                .try_init();
            log_path_of(&cfg)
        }
    };
    let _lock = match single_instance_lock() {
        Ok(file) => Some(file),
        Err(e) => {
            eprintln!("single instance lock skipped: {e:#}");
            None
        }
    };

    info!("Cross-SystemConnect-core starting name={}", cfg.name);

    let screen = create_screen_info().virtual_size();
    let mut switch = SwitchController::default();
    switch.configure(
        cfg.name.clone(),
        screen,
        cfg.switch_cfg.clone(),
        ScreenLayout::load(&cfg.layout),
    );

    let status = AppStatus {
        connected: false,
        peer_name: String::new(),
        active_screen: cfg.name.clone(),
        sharing_enabled: true,
        role: match cfg.role {
            csc_config::Role::Server => "server".into(),
            csc_config::Role::Client => "client".into(),
        },
        local_name: cfg.name.clone(),
        launch_at_login: cfg.gui.launch_at_login,
        host_name: cfg.name.clone(),
        local_ipv4: Vec::new(),
        local_nics: Vec::new(),
        discovery_port: cfg.control_port,
        link_state: match cfg.role {
            csc_config::Role::Server => "searching".into(),
            csc_config::Role::Client => "announcing".into(),
        },
        pending_peer: None,
        discovered: Vec::new(),
        search_subnet: crate::discover::subnet_label(&cfg.search_address).unwrap_or_default(),
    };

    let config = Arc::new(RwLock::new(cfg));
    let status = Arc::new(RwLock::new(status));
    let (quit_tx, mut quit_rx) = tokio::sync::watch::channel(false);
    let (net_reload, net_reload_rx) = tokio::sync::watch::channel(0u64);
    let (layout_reload, mut layout_rx) = tokio::sync::watch::channel(0u64);
    let prompt = Arc::new(PeerPrompt::default());

    let state = Arc::new(AppState {
        config: config.clone(),
        config_path: config_path.clone(),
        status: status.clone(),
        quit_tx,
        prompt: prompt.clone(),
        net_reload,
        layout_reload,
        log_path,
    });

    let (inbound_tx, mut inbound_rx) = mpsc::channel::<InboundEvent>(256);
    // The socket must exist before this function returns to the rest of startup.
    // A later failure must not leave the UI connecting to a path that was never created.
    run_ipc(state.clone())
        .await
        .context("start IPC")?;

    let host = query_host_net_info();
    {
        let mut cfg = config.write().await;
        if cfg.search_address.is_empty() {
            if let Some(nic) = host
                .nics
                .iter()
                .find(|n| n.kind == "ethernet")
                .or(host.nics.first())
            {
                cfg.search_address = nic.address.clone();
                if let Err(e) = cfg.save(&config_path) {
                    warn!("save default search address: {e:#}");
                }
            }
        }
        info!("host={} ips={:?}", host.host_name, host.ipv4_addresses);
    }
    let search = config.read().await.search_address.clone();
    {
        let mut s = status.write().await;
        s.host_name = host.host_name;
        s.local_ipv4 = host.ipv4_addresses;
        s.local_nics = host
            .nics
            .into_iter()
            .map(|n| LocalNic {
                address: n.address,
                kind: n.kind,
                name: n.name,
            })
            .collect();
        s.search_subnet = crate::discover::subnet_label(&search).unwrap_or_default();
    }

    let net = spawn_network(
        config.clone(),
        config_path.clone(),
        status.clone(),
        prompt,
        net_reload_rx,
        inbound_tx,
    )
    .await?;

    let mut injector = create_input_injector();
    let mut clipboard = create_clipboard();
    let mut clip_sync = ClipboardSync::default();
    let receive_root = {
        let cfg = config.read().await;
        if cfg.file_transfer.receive_dir.is_empty() {
            default_config_path()
                .parent()
                .map(|p| p.join("received"))
                .unwrap_or_else(|| PathBuf::from("received"))
        } else {
            PathBuf::from(&cfg.file_transfer.receive_dir)
        }
    };
    let _file_xfer = FileTransfer::new(receive_root);
    let mut remote_x = 0i32;
    let mut remote_y = 0i32;
    let mut tick = tokio::time::interval(Duration::from_millis(8));
    let mut next_nic = Instant::now();

    loop {
        tokio::select! {
            _ = quit_rx.changed() => {
                if *quit_rx.borrow_and_update() {
                    info!("quit requested");
                    break;
                }
            }
            _ = tick.tick() => {
                if layout_rx.has_changed().unwrap_or(false) {
                    let _ = layout_rx.borrow_and_update();
                    let cfg = config.read().await;
                    switch.configure(
                        cfg.name.clone(),
                        screen,
                        cfg.switch_cfg.clone(),
                        ScreenLayout::load(&cfg.layout),
                    );
                }
                if Instant::now() >= next_nic {
                    next_nic = Instant::now() + Duration::from_secs(3);
                    let host = query_host_net_info();
                    let mut s = status.write().await;
                    s.host_name = host.host_name;
                    s.local_ipv4 = host.ipv4_addresses;
                    s.local_nics = host
                        .nics
                        .into_iter()
                        .map(|n| LocalNic {
                            address: n.address,
                            kind: n.kind,
                            name: n.name,
                        })
                        .collect();
                }
                {
                    let ns = net.status.read().await.clone();
                    let mut s = status.write().await;
                    s.connected = ns.connected;
                    s.peer_name = ns.peer_name.clone();
                    switch.set_enabled(s.sharing_enabled);
                }

                #[cfg(windows)]
                {
                    if switch.enabled() && switch.state() == crate::switch::SwitchState::Local {
                        let (x, y) = csc_platform::win::cursor_pos();
                        if let Some(dec) = switch.on_local_cursor(x, y, Instant::now()) {
                            let payload = SwitchPayload {
                                edge: dec.edge,
                                position_pct: dec.position_pct,
                            };
                            net.send_to(&dec.neighbor, csc_proto::encode_switch_bin(&payload)).await;
                            switch.confirm_entered(
                                dec.edge,
                                dec.position_pct,
                                screen,
                                &mut remote_x,
                                &mut remote_y,
                                &dec.neighbor,
                            );
                            let mut s = status.write().await;
                            s.active_screen = dec.neighbor;
                            info!("switch enter -> {}", s.active_screen);
                        }
                    }
                }
            }
            ev = inbound_rx.recv() => {
                match ev {
                    None => break,
                    Some(InboundEvent::Disconnected { peer }) => {
                        let mut s = status.write().await;
                        if s.active_screen.eq_ignore_ascii_case(&peer) || s.peer_name.eq_ignore_ascii_case(&peer) {
                            switch.unlock_to_local();
                            s.active_screen = s.local_name.clone();
                        }
                        if !s.peer_name.split(',').any(|n| n.trim().eq_ignore_ascii_case(&peer)) {
                            // peer list is refreshed from the network task; drop this name locally
                        }
                        let left: Vec<&str> = s.peer_name.split(',').map(str::trim).filter(|n| !n.is_empty() && !n.eq_ignore_ascii_case(&peer)).collect();
                        s.peer_name = left.join(", ");
                        s.connected = !s.peer_name.is_empty();
                    }
                    Some(InboundEvent::Hello(h)) => {
                        info!("HELLO from {}", h.host_name);
                    }
                    Some(InboundEvent::Heartbeat) => {}
                    Some(InboundEvent::SwitchEnter(p)) => {
                        switch.confirm_entered(p.edge, p.position_pct, screen, &mut remote_x, &mut remote_y, "");
                        injector.mouse_move_abs(remote_x, remote_y);
                    }
                    Some(InboundEvent::SwitchLeave(p)) => {
                        debug!(edge = ?p.edge, "switch leave");
                        let current = switch.active_remote().to_string();
                        let local = status.read().await.local_name.clone();
                        let next = switch.neighbor_from_active(p.edge);
                        if let Some(next) = next {
                            if !next.eq_ignore_ascii_case(&local) && !next.eq_ignore_ascii_case(&current) {
                                let enter = SwitchPayload {
                                    edge: p.edge.opposite(),
                                    position_pct: p.position_pct,
                                };
                                net.send_to(&next, csc_proto::encode_switch_bin(&enter)).await;
                                switch.confirm_entered(
                                    enter.edge,
                                    enter.position_pct,
                                    screen,
                                    &mut remote_x,
                                    &mut remote_y,
                                    &next,
                                );
                                let mut s = status.write().await;
                                s.active_screen = next;
                                continue;
                            }
                        }
                        switch.confirm_returned();
                        let mut s = status.write().await;
                        s.active_screen = s.local_name.clone();
                    }
                    Some(InboundEvent::MouseMove { dx, dy }) => {
                        if switch.state() == crate::switch::SwitchState::Remote {
                            if let Some(leave) =
                                switch.on_remote_relative(dx, dy, &mut remote_x, &mut remote_y, screen)
                            {
                                let payload = SwitchPayload {
                                    edge: leave.edge,
                                    position_pct: leave.position_pct,
                                };
                                net.send_raw(encode_switch_leave_bin(&payload)).await;
                                switch.confirm_returned();
                            } else {
                                injector.mouse_move_abs(remote_x, remote_y);
                            }
                        } else {
                            net.send_mouse_move(dx, dy).await;
                            injector.mouse_move_rel(dx, dy);
                        }
                    }
                    Some(InboundEvent::Key { hid, down }) => {
                        injector.key(hid, down);
                    }
                    Some(InboundEvent::ClipOffer(v)) => {
                        match serde_json::from_value::<ClipOfferPayload>(v) {
                            Ok(offer) => {
                                let _ = ClipboardSync::offer_msg_type();
                                clip_sync.apply_remote_offer(clipboard.as_mut(), &offer);
                            }
                            Err(e) => warn!("bad clip offer: {e}"),
                        }
                    }
                    Some(InboundEvent::Raw(ty, payload)) => {
                        debug!(?ty, len = payload.len(), "unhandled frame");
                    }
                }
            }
        }
    }

    info!("Cross-SystemConnect-core exiting");
    // Ensure network/IPC tasks die with the process (do not leave a stray kernel).
    std::process::exit(0);
}
