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
mod tls;

use crate::clipboard::ClipboardSync;
use crate::file_transfer::FileTransfer;
use crate::ipc::{claim_ipc, serve_ipc, AppState, IpcBusy};
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
use std::collections::HashMap;
use std::fs::OpenOptions;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{mpsc, RwLock};
use tracing::{debug, info, warn};
use tracing_subscriber::EnvFilter;

fn peer_size(
    sizes: &HashMap<String, csc_platform::ScreenSize>,
    name: &str,
    fallback: csc_platform::ScreenSize,
) -> csc_platform::ScreenSize {
    sizes
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case(name))
        .map(|(_, size)| *size)
        .filter(|size| size.width > 1 && size.height > 1)
        .unwrap_or(fallback)
}

/// Where the local pointer reappears after it leaves a remote screen.
/// `leave` is the edge of the remote screen that was crossed, `pct` the
/// position along it, and `m` the local monitor the pointer returns to.
fn reentry_point(leave: csc_proto::Edge, pct: f32, m: csc_platform::Rect) -> (i32, i32) {
    let inset = 12;
    let pct = pct.clamp(0.0, 1.0);
    let lo_x = m.left + inset;
    let hi_x = (m.right - 1 - inset).max(lo_x);
    let lo_y = m.top + inset;
    let hi_y = (m.bottom - 1 - inset).max(lo_y);
    let along_x = (m.left + (m.width() as f32 * pct) as i32).clamp(lo_x, hi_x);
    let along_y = (m.top + (m.height() as f32 * pct) as i32).clamp(lo_y, hi_y);
    match leave.opposite() {
        csc_proto::Edge::Left => (lo_x, along_y),
        csc_proto::Edge::Right => (hi_x, along_y),
        csc_proto::Edge::Top => (along_x, lo_y),
        csc_proto::Edge::Bottom => (along_x, hi_y),
        csc_proto::Edge::TopLeft => (lo_x, lo_y),
        csc_proto::Edge::TopRight => (hi_x, lo_y),
        csc_proto::Edge::BottomLeft => (lo_x, hi_y),
        csc_proto::Edge::BottomRight => (hi_x, hi_y),
    }
}

/// Give input back to this computer. `target` is where the cursor reappears,
/// in desktop coordinates relative to the virtual origin.
fn release_pointer(grab: &mut bool, target: Option<(i32, i32)>) {
    if !*grab {
        return;
    }
    *grab = false;
    csc_platform::capture_set_grab(false, None);
    if let Some((x, y)) = target {
        let (ox, oy) = csc_platform::virtual_origin();
        csc_platform::warp_cursor(ox + x, oy + y);
    }
}

/// Keys and buttons the remote computer currently sees as held down.
#[derive(Default)]
struct HeldInput {
    keys: std::collections::HashSet<u32>,
    buttons: std::collections::HashSet<u32>,
}

impl HeldInput {
    fn track(&mut self, ev: csc_platform::Captured) {
        use csc_platform::Captured;
        match ev {
            Captured::Key { hid, down: true } => {
                self.keys.insert(hid);
            }
            Captured::Key { hid, down: false } => {
                self.keys.remove(&hid);
            }
            Captured::Button { button, down: true } => {
                self.buttons.insert(button);
            }
            Captured::Button { button, down: false } => {
                self.buttons.remove(&button);
            }
            _ => {}
        }
    }

    /// Release everything on the remote, so no key stays stuck after the pointer leaves.
    async fn release_all(&mut self, net: &crate::net::NetHub) {
        use csc_platform::Captured;
        for hid in self.keys.drain() {
            net.send_captured(Captured::Key { hid, down: false }).await;
        }
        for button in self.buttons.drain() {
            net.send_captured(Captured::Button { button, down: false }).await;
        }
    }

    /// Release everything this computer injected on behalf of a remote host.
    fn release_injected(&mut self, injector: &mut dyn csc_platform::InputInjector) {
        for hid in self.keys.drain() {
            injector.key(hid, false);
        }
        for button in self.buttons.drain() {
            injector.mouse_button(button, false);
        }
    }
}

/// A key combination such as `Ctrl+Alt+Home`, as HID usages.
/// Modifiers are stored left-handed; either hand matches.
#[derive(Debug, Clone, PartialEq)]
struct Hotkey {
    mods: Vec<u32>,
    key: u32,
}

impl Hotkey {
    fn parse(text: &str) -> Option<Self> {
        use csc_platform::keymap::{fold_modifier, hid_from_name};
        let mut usages = text
            .split('+')
            .filter(|p| !p.trim().is_empty())
            .map(|p| hid_from_name(p).map(fold_modifier))
            .collect::<Option<Vec<u32>>>()?;
        let key = usages.pop()?;
        Some(Self { mods: usages, key })
    }

    /// True when `hid` going down completes the combination, given the keys already held.
    fn completes(&self, hid: u32, held: &std::collections::HashSet<u32>) -> bool {
        use csc_platform::keymap::fold_modifier;
        fold_modifier(hid) == self.key
            && self
                .mods
                .iter()
                .all(|m| held.iter().any(|k| fold_modifier(*k) == *m))
    }
}

/// Keep the pointer just inside the edge it crossed, so the physical mouse
/// stays where the hand is and a small move back returns to this computer.
fn park_inside(
    edge: csc_proto::Edge,
    x: i32,
    y: i32,
    screen: csc_platform::ScreenSize,
) -> (i32, i32) {
    let w = screen.width.max(2);
    let h = screen.height.max(2);
    let inset = 24;
    let cx = x.clamp(inset, w - 1 - inset);
    let cy = y.clamp(inset, h - 1 - inset);
    match edge {
        csc_proto::Edge::Left => (inset, cy),
        csc_proto::Edge::Right => (w - 1 - inset, cy),
        csc_proto::Edge::Top => (cx, inset),
        csc_proto::Edge::Bottom => (cx, h - 1 - inset),
        csc_proto::Edge::TopLeft => (inset, inset),
        csc_proto::Edge::TopRight => (w - 1 - inset, inset),
        csc_proto::Edge::BottomLeft => (inset, h - 1 - inset),
        csc_proto::Edge::BottomRight => (w - 1 - inset, h - 1 - inset),
    }
}

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

    csc_platform::init_process();
    let screen_info = create_screen_info();
    let mut screen = screen_info.virtual_size();
    let mut monitors = screen_info.monitors();
    info!(
        "screen {}x{}, monitors {:?}",
        screen.width, screen.height, monitors
    );
    let mut switch = SwitchController::default();
    switch.configure(
        cfg.name.clone(),
        screen,
        cfg.switch_cfg.clone(),
        ScreenLayout::load(&cfg.layout),
    );
    switch.set_screen(screen, monitors.clone());
    let hooked = csc_platform::capture_start();

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
    // Bind IPC before discovery. A second core must exit so it does not also take port 24900.
    let ipc_bound = match claim_ipc() {
        Ok(bound) => bound,
        Err(err) if err.downcast_ref::<IpcBusy>().is_some() => {
            info!("core already running, exiting");
            std::process::exit(0);
        }
        Err(err) => return Err(err),
    };
    let ipc_state = state.clone();
    tokio::spawn(async move {
        if let Err(e) = serve_ipc(ipc_bound, ipc_state).await {
            warn!("IPC server stopped: {e:#}");
        }
    });
    // GetAdaptersAddresses must not run on a Tokio worker after the pipe is on IOCP.
    let host = tokio::task::spawn_blocking(query_host_net_info)
        .await
        .unwrap_or_else(|e| {
            warn!("host net info: {e}");
            csc_platform::HostNetInfo::default()
        });
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
    let mut peer_screens: HashMap<String, csc_platform::ScreenSize> = HashMap::new();
    let mut grab = false;
    let mut grab_armed = false;
    let mut park_x = 0i32;
    let mut park_y = 0i32;
    let mut enter_point = (0i32, 0i32);
    // Just inside the edge the pointer left through, for returns that cross no edge.
    let mut home_point = (screen.width / 2, screen.height / 2);
    let mut held = HeldInput::default();
    // Keys and buttons this computer injected for a remote host, released if that host goes away.
    let mut injected = HeldInput::default();
    let hotkey_of = |cfg: &Config| {
        let text = cfg.hotkeys.return_to_server.clone();
        let parsed = Hotkey::parse(&text);
        if parsed.is_none() && !text.trim().is_empty() {
            warn!("return hotkey {text:?} not understood");
        }
        parsed
    };
    let mut return_hotkey = hotkey_of(&*config.read().await);
    let mut last_miss = Instant::now() - Duration::from_secs(5);
    let mut tick = tokio::time::interval(Duration::from_millis(8));
    let mut next_nic = Instant::now();

    loop {
        tokio::select! {
            _ = quit_rx.changed() => {
                if *quit_rx.borrow_and_update() {
                    info!("quit requested");
                    held.release_all(&net).await;
                    release_pointer(&mut grab, None);
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
                    switch.set_screen(screen, monitors.clone());
                    return_hotkey = hotkey_of(&cfg);
                }
                if grab && !net.has_peer(switch.active_remote()).await {
                    warn!(
                        "session to {} is gone; returning input to this computer",
                        switch.active_remote()
                    );
                    switch.unlock_to_local();
                    held = HeldInput::default();
                    release_pointer(&mut grab, Some(home_point));
                    grab_armed = false;
                    let mut s = status.write().await;
                    s.active_screen = s.local_name.clone();
                }
                if Instant::now() >= next_nic {
                    next_nic = Instant::now() + Duration::from_secs(3);
                    if !grab {
                        let now_screen = screen_info.virtual_size();
                        let now_monitors = screen_info.monitors();
                        if now_monitors != monitors
                            || now_screen.width != screen.width
                            || now_screen.height != screen.height
                        {
                            info!(
                                "screen changed to {}x{}, monitors {:?}",
                                now_screen.width, now_screen.height, now_monitors
                            );
                            screen = now_screen;
                            monitors = now_monitors;
                            switch.set_screen(screen, monitors.clone());
                        }
                    }
                    let Ok(host) = tokio::task::spawn_blocking(query_host_net_info).await else {
                        continue;
                    };
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

                if screen.width > 1 && screen.height > 1 {
                    if grab && switch.state() == crate::switch::SwitchState::Remote {
                        // With hooks, local input arrives already swallowed. Without them,
                        // poll the cursor and pull it back to the park point each tick.
                        let steps: Vec<csc_platform::Captured> = if hooked {
                            csc_platform::capture_drain()
                        } else {
                            let mut polled = Vec::new();
                            if let Some((ax, ay)) = csc_platform::cursor_pos() {
                                let (ox, oy) = csc_platform::virtual_origin();
                                let x = ax - ox;
                                let y = ay - oy;
                                if !grab_armed {
                                    // Use the cursor's real position. Forcing it back to a
                                    // mismatched point makes the physical mouse feel stuck.
                                    park_x = x;
                                    park_y = y;
                                    grab_armed = true;
                                } else if x != park_x || y != park_y {
                                    csc_platform::warp_cursor(ox + park_x, oy + park_y);
                                    polled.push(csc_platform::Captured::Move {
                                        dx: x - park_x,
                                        dy: y - park_y,
                                    });
                                }
                            }
                            polled
                        };
                        for step in steps {
                            if let csc_platform::Captured::Key { hid, down: true } = step {
                                if return_hotkey
                                    .as_ref()
                                    .is_some_and(|k| k.completes(hid, &held.keys))
                                {
                                    let current = switch.active_remote().to_string();
                                    held.release_all(&net).await;
                                    let leave_msg = SwitchPayload {
                                        edge: csc_proto::Edge::Left,
                                        position_pct: 0.5,
                                    };
                                    net.send_to(&current, encode_switch_leave_bin(&leave_msg)).await;
                                    switch.unlock_to_local();
                                    release_pointer(&mut grab, Some(home_point));
                                    grab_armed = false;
                                    let mut s = status.write().await;
                                    s.active_screen = s.local_name.clone();
                                    info!("return hotkey: input back from {current}");
                                    break;
                                }
                            }
                            if let csc_platform::Captured::Key { hid, down: false } = step {
                                if !held.keys.contains(&hid) {
                                    // Release of the hotkey's own key, which the remote never saw.
                                    continue;
                                }
                            }
                            let csc_platform::Captured::Move { dx, dy } = step else {
                                held.track(step);
                                net.send_captured(step).await;
                                continue;
                            };
                            let dx = dx.clamp(-300, 300);
                            let dy = dy.clamp(-300, 300);
                            let current = switch.active_remote().to_string();
                            let remote = peer_size(&peer_screens, &current, screen);
                            let Some(leave) = switch.on_remote_relative(
                                dx,
                                dy,
                                &mut remote_x,
                                &mut remote_y,
                                remote,
                            ) else {
                                net.send_mouse_move(dx, dy).await;
                                continue;
                            };
                            held.release_all(&net).await;
                            let local_name = status.read().await.local_name.clone();
                            let next = switch.neighbor_from_active(leave.edge);
                            let hop = next.as_ref().filter(|name| {
                                !name.eq_ignore_ascii_case(&local_name)
                                    && !name.eq_ignore_ascii_case(&current)
                            });
                            let leave_msg = SwitchPayload {
                                edge: leave.edge,
                                position_pct: leave.position_pct,
                            };
                            if let Some(next) = hop {
                                net.send_to(&current, encode_switch_leave_bin(&leave_msg)).await;
                                let enter = SwitchPayload {
                                    edge: leave.edge.opposite(),
                                    position_pct: leave.position_pct,
                                };
                                net.send_to(next, csc_proto::encode_switch_bin(&enter)).await;
                                let remote = peer_size(&peer_screens, next, screen);
                                switch.confirm_entered(
                                    enter.edge,
                                    enter.position_pct,
                                    remote,
                                    &mut remote_x,
                                    &mut remote_y,
                                    next,
                                );
                                status.write().await.active_screen = next.clone();
                                info!("switch hop -> {next}");
                            } else {
                                if current.is_empty() {
                                    net.send_raw(encode_switch_leave_bin(&leave_msg)).await;
                                } else {
                                    net.send_to(&current, encode_switch_leave_bin(&leave_msg)).await;
                                }
                                switch.confirm_returned();
                                let m = switch.monitor_at(enter_point.0, enter_point.1);
                                let back = reentry_point(leave.edge, leave.position_pct, m);
                                release_pointer(&mut grab, Some(back));
                                grab_armed = false;
                                let mut s = status.write().await;
                                s.active_screen = s.local_name.clone();
                                info!("switch return");
                                break;
                            }
                        }
                    } else if grab {
                        release_pointer(&mut grab, None);
                        grab_armed = false;
                    } else if switch.enabled() && switch.state() == crate::switch::SwitchState::Local {
                        if let Some((ax, ay)) = csc_platform::cursor_pos() {
                            let (ox, oy) = csc_platform::virtual_origin();
                            let x = ax - ox;
                            let y = ay - oy;
                            if let Some(dec) = switch.on_local_cursor(x, y, Instant::now()) {
                                let known_screen = peer_screens.iter().any(|(k, size)| {
                                    k.eq_ignore_ascii_case(&dec.neighbor)
                                        && size.width > 1
                                        && size.height > 1
                                });
                                let has_peer = net.has_peer(&dec.neighbor).await;
                                if has_peer && known_screen {
                                    let entry = dec.edge.opposite();
                                    let payload = SwitchPayload {
                                        edge: entry,
                                        position_pct: dec.position_pct,
                                    };
                                    net.send_to(&dec.neighbor, csc_proto::encode_switch_bin(&payload)).await;
                                    let remote = peer_size(&peer_screens, &dec.neighbor, screen);
                                    switch.confirm_entered(
                                        entry,
                                        dec.position_pct,
                                        remote,
                                        &mut remote_x,
                                        &mut remote_y,
                                        &dec.neighbor,
                                    );
                                    enter_point = (x, y);
                                    home_point = reentry_point(
                                        entry,
                                        dec.position_pct,
                                        switch.monitor_at(x, y),
                                    );
                                    held = HeldInput::default();
                                    if hooked {
                                        // Moves are measured from the park point, and Windows
                                        // clips the cursor at the desktop edge. Park in the
                                        // middle of this monitor so no motion is lost.
                                        let (cx, cy) = switch.monitor_at(x, y).center();
                                        csc_platform::capture_set_grab(true, Some((ox + cx, oy + cy)));
                                    } else {
                                        let parked = park_inside(dec.edge, x, y, screen);
                                        park_x = parked.0;
                                        park_y = parked.1;
                                        csc_platform::warp_cursor(ox + park_x, oy + park_y);
                                    }
                                    grab = true;
                                    grab_armed = false;
                                    let mut s = status.write().await;
                                    s.active_screen = dec.neighbor;
                                    info!("switch enter -> {}", s.active_screen);
                                } else if last_miss.elapsed() > Duration::from_secs(2) {
                                    last_miss = Instant::now();
                                    if !has_peer {
                                        warn!(
                                            "pointer hit {:?} but {} has no session; place that computer on an edge in the layout",
                                            dec.edge, dec.neighbor
                                        );
                                    } else {
                                        warn!(
                                            "pointer hit {:?} but {} has not reported its screen size; leaving the local pointer alone",
                                            dec.edge, dec.neighbor
                                        );
                                    }
                                }
                            }
                        }
                    }
                }
            }
            ev = inbound_rx.recv() => {
                match ev {
                    None => break,
                    Some(InboundEvent::Disconnected { peer }) => {
                        info!("session {peer} disconnected");
                        let mut s = status.write().await;
                        let active = grab
                            && (switch.active_remote().eq_ignore_ascii_case(&peer)
                                || s.active_screen.eq_ignore_ascii_case(&peer));
                        if active {
                            switch.unlock_to_local();
                            held = HeldInput::default();
                            release_pointer(&mut grab, Some(home_point));
                            grab_armed = false;
                            s.active_screen = s.local_name.clone();
                        }
                        if !grab {
                            injected.release_injected(injector.as_mut());
                        }
                        let left: Vec<&str> = s.peer_name.split(',').map(str::trim).filter(|n| !n.is_empty() && !n.eq_ignore_ascii_case(&peer)).collect();
                        s.peer_name = left.join(", ");
                        s.connected = !s.peer_name.is_empty();
                    }
                    Some(InboundEvent::Hello(h)) => {
                        info!("HELLO from {} ({}x{})", h.host_name, h.screen_width, h.screen_height);
                        if h.screen_width > 1 && h.screen_height > 1 {
                            peer_screens.insert(
                                h.host_name,
                                csc_platform::ScreenSize {
                                    width: h.screen_width,
                                    height: h.screen_height,
                                },
                            );
                        }
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
                        if grab {
                            held.release_all(&net).await;
                            let m = switch.monitor_at(enter_point.0, enter_point.1);
                            release_pointer(&mut grab, Some(reentry_point(p.edge, p.position_pct, m)));
                        }
                        grab_armed = false;
                        let mut s = status.write().await;
                        s.active_screen = s.local_name.clone();
                    }
                    Some(InboundEvent::MouseMove { dx, dy }) => {
                        if !grab {
                            let w = screen.width.max(1);
                            let h = screen.height.max(1);
                            remote_x = remote_x.saturating_add(dx).clamp(0, w - 1);
                            remote_y = remote_y.saturating_add(dy).clamp(0, h - 1);
                            injector.mouse_move_abs(remote_x, remote_y);
                        }
                    }
                    Some(InboundEvent::MouseButton { button, down }) => {
                        if !grab {
                            injected.track(csc_platform::Captured::Button { button, down });
                            injector.mouse_button(button, down);
                        }
                    }
                    Some(InboundEvent::MouseWheel { v, h }) => {
                        if !grab {
                            injector.mouse_wheel(v, h);
                        }
                    }
                    Some(InboundEvent::Key { hid, down }) => {
                        if !grab {
                            injected.track(csc_platform::Captured::Key { hid, down });
                            injector.key(hid, down);
                        }
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

#[cfg(test)]
mod tests {
    use super::Hotkey;
    use std::collections::HashSet;

    #[test]
    fn return_hotkey_matches_either_hand() {
        let hk = Hotkey::parse("Ctrl+Alt+Home").expect("parse");
        let home = 0x4A;
        let held: HashSet<u32> = [0xE4, 0xE2].into_iter().collect();
        assert!(hk.completes(home, &held));
        let only_ctrl: HashSet<u32> = [0xE0].into_iter().collect();
        assert!(!hk.completes(home, &only_ctrl));
        assert!(!hk.completes(0x4D, &held));
        assert!(Hotkey::parse("Ctrl+Nope").is_none());
    }
}
