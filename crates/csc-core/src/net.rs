//! Cross-machine control channel (HELLO + heartbeat + switch/input frames).
//!
//! The keyboard host (server) searches a UDP port and then connects to the client.
//! The client sends an announce to that same port and accepts the TCP connection.
//! The client also dials the host back when it hears a search, because a firewall
//! often drops inbound TCP on one side only. Each side keeps one link per peer.
//! UDP only carries the computer name and port. The TCP session is always TLS 1.3,
//! with the client as the TLS server whichever side opened the TCP connection.

use crate::discover::{
    broadcast_targets, decode_datagram, encode_announce, encode_ping, encode_pong, encode_search,
    is_own_announce, reject_mark, same_subnet24, subnet_label, subnet_scan_targets,
    suppress_repeat, Announce, Datagram, RejectMark,
};
use crate::prompt::{Choice, PeerPrompt};
use bytes::BytesMut;
use csc_config::{Config, Role};
use csc_proto::{
    decode_hello_bin, encode_frame, encode_hello_bin, encode_json_frame, encode_mouse_move_bin,
    try_decode_frame, AppStatus, DiscoveredPeer, HelloPayload, MouseMovePayload, MsgType,
    SwitchPayload,
};
use std::collections::{HashMap, HashSet};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, ToSocketAddrs};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream, UdpSocket};
use tokio::sync::{mpsc, watch, Mutex, RwLock};
use tokio_rustls::TlsAcceptor;
use tracing::{debug, info, warn};

#[derive(Debug, Clone, Default)]
pub struct NetStatus {
    pub connected: bool,
    pub peer_name: String,
}

/// What the other side said about itself in HELLO.
#[derive(Debug, Clone, Default)]
pub struct PeerInfo {
    pub host_name: String,
    pub capabilities: u32,
}

/// Channels into one running session.
#[derive(Clone)]
pub struct Link {
    /// Input and control frames, sent before anything on `bulk`.
    tx: mpsc::Sender<Vec<u8>>,
    /// File data. Small queue, so a transfer never delays the pointer by much.
    bulk: mpsc::Sender<Vec<u8>>,
    kill: watch::Sender<bool>,
    since: Instant,
    info: Arc<std::sync::Mutex<PeerInfo>>,
}

/// The session end of a `Link`.
pub struct LinkRx {
    rx: mpsc::Receiver<Vec<u8>>,
    bulk: mpsc::Receiver<Vec<u8>>,
    kill: watch::Receiver<bool>,
    info: Arc<std::sync::Mutex<PeerInfo>>,
}

fn new_link() -> (Link, LinkRx) {
    let (tx, rx) = mpsc::channel(64);
    let (btx, brx) = mpsc::channel(4);
    let (ktx, krx) = watch::channel(false);
    let info = Arc::new(std::sync::Mutex::new(PeerInfo::default()));
    (
        Link {
            tx,
            bulk: btx,
            kill: ktx,
            since: Instant::now(),
            info: info.clone(),
        },
        LinkRx {
            rx,
            bulk: brx,
            kill: krx,
            info,
        },
    )
}

/// Where sessions deliver what they receive, plus state kept across network restarts.
#[derive(Clone)]
pub struct Wires {
    events: mpsc::Sender<InboundEvent>,
    transfer: crate::transfer::TransferHandle,
    /// Keyboard host: last TCP address of each controlled computer, for redialing.
    last_peers: Arc<Mutex<HashMap<String, SocketAddr>>>,
    /// Names/IPs the user disconnected; do not auto-redial until they pair again.
    held_off: Arc<Mutex<HashSet<String>>>,
    /// Live control sessions (shared so HELLO can rekey IP → hostname).
    peers: PeerTable,
    /// A session ended: search or announce right away.
    kick: Arc<tokio::sync::Notify>,
    /// Bump to restart discovery after an automatic role change.
    net_reload: watch::Sender<u64>,
    /// Bump when desk/layout changes after trust so edge switch reloads neighbors.
    layout_reload: watch::Sender<u64>,
}

impl Link {
    fn same(&self, other: &Link) -> bool {
        self.tx.same_channel(&other.tx)
    }
}

#[derive(Clone)]
pub struct PeerTable {
    txs: Arc<Mutex<HashMap<String, Link>>>,
    active: Arc<Mutex<String>>,
}

impl PeerTable {
    fn new() -> Self {
        Self {
            txs: Arc::new(Mutex::new(HashMap::new())),
            active: Arc::new(Mutex::new(String::new())),
        }
    }

    /// Keep `link` as the session for `name`.
    /// A different session younger than `min_age` stays, and this call returns
    /// false: that is the other computer dialing at the same moment.
    /// An older session is replaced, because the peer has given it up.
    async fn adopt(&self, name: &str, link: Link, min_age: Duration) -> bool {
        {
            let mut txs = self.txs.lock().await;
            let old = txs
                .iter()
                .find(|(k, _)| k.eq_ignore_ascii_case(name))
                .map(|(k, l)| (k.clone(), l.clone()));
            if let Some((key, old)) = old {
                if !old.same(&link) && old.since.elapsed() < min_age {
                    return false;
                }
                if !old.same(&link) {
                    let _ = old.kill.send(true);
                }
                txs.remove(&key);
            }
            txs.insert(name.to_string(), link);
        }
        let mut active = self.active.lock().await;
        if active.is_empty() {
            *active = name.to_string();
        }
        true
    }

    /// Insert only when no link to `name` exists yet.
    async fn insert_new(&self, name: &str, link: Link) -> bool {
        {
            let mut txs = self.txs.lock().await;
            if txs.keys().any(|k| k.eq_ignore_ascii_case(name)) {
                return false;
            }
            txs.insert(name.to_string(), link);
        }
        let mut active = self.active.lock().await;
        if active.is_empty() {
            *active = name.to_string();
        }
        true
    }

    /// Move a live session from `from` to `to` (IP dial → hostname after HELLO).
    /// Returns false when `to` already has a different live session: the newcomer
    /// is dropped and the existing hostname session is kept.
    async fn rekey(&self, from: &str, to: &str) -> bool {
        if from.eq_ignore_ascii_case(to) || to.is_empty() {
            return true;
        }
        let mut txs = self.txs.lock().await;
        let Some(link) = txs
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(from))
            .map(|(k, l)| (k.clone(), l.clone()))
        else {
            return true;
        };
        if let Some(old) = txs
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(to))
            .map(|(k, l)| (k.clone(), l.clone()))
        {
            if !old.1.same(&link.1) {
                // Keep the session already keyed by hostname; drop this dial.
                txs.remove(&link.0);
                drop(txs);
                let mut active = self.active.lock().await;
                if active.eq_ignore_ascii_case(from) {
                    *active = to.to_string();
                }
                return false;
            }
            txs.remove(&old.0);
        }
        txs.remove(&link.0);
        txs.insert(to.to_string(), link.1);
        drop(txs);
        let mut active = self.active.lock().await;
        if active.eq_ignore_ascii_case(from) {
            *active = to.to_string();
        }
        true
    }

    /// Remove `name` only if it still points at this link, not a newer one.
    async fn remove_owned(&self, name: &str, link: &Link) {
        let key = {
            let txs = self.txs.lock().await;
            txs.iter()
                .find(|(k, cur)| k.eq_ignore_ascii_case(name) || cur.same(link))
                .filter(|(_, cur)| cur.same(link))
                .map(|(k, _)| k.clone())
        };
        if let Some(key) = key {
            self.remove(&key).await;
        }
    }

    fn find(txs: &HashMap<String, Link>, name: &str) -> Option<Link> {
        txs.iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, l)| l.clone())
    }

    /// Queue file data; waits while the peer's bulk queue is full.
    pub async fn send_bulk(&self, name: &str, frame: Vec<u8>) -> bool {
        let link = Self::find(&*self.txs.lock().await, name);
        match link {
            Some(link) => link.bulk.send(frame).await.is_ok(),
            None => false,
        }
    }

    async fn remove(&self, name: &str) {
        let next = {
            let mut txs = self.txs.lock().await;
            txs.remove(name);
            txs.keys().next().cloned()
        };
        let mut active = self.active.lock().await;
        if active.eq_ignore_ascii_case(name) {
            *active = next.unwrap_or_default();
        }
    }

    /// Kill the session task and drop it from the table (pairing-code takeover).
    async fn kill_remove(&self, name: &str) {
        let link = {
            let txs = self.txs.lock().await;
            Self::find(&txs, name)
        };
        if let Some(link) = link {
            let _ = link.kill.send(true);
        }
        self.remove(name).await;
    }

    async fn contains(&self, name: &str) -> bool {
        self.txs
            .lock()
            .await
            .keys()
            .any(|k| k.eq_ignore_ascii_case(name))
    }

    async fn set_active(&self, name: &str) {
        if self.contains(name).await {
            *self.active.lock().await = name.to_string();
        }
    }

    pub async fn send_to(&self, name: &str, frame: Vec<u8>) {
        let link = Self::find(&*self.txs.lock().await, name);
        if let Some(link) = link {
            // Never wait: a stalled writer would freeze the grab loop and
            // hold the pointer. A full or closed queue means the session is
            // already stuck, so end it and let Disconnected hand input back.
            if link.tx.try_send(frame).is_err() {
                let _ = link.kill.send(true);
            }
        }
    }

    pub async fn send_active(&self, frame: Vec<u8>) {
        let active = self.active.lock().await.clone();
        if !active.is_empty() {
            self.send_to(&active, frame).await;
        }
    }

    async fn names(&self) -> Vec<String> {
        let mut names: Vec<String> = self.txs.lock().await.keys().cloned().collect();
        names.sort();
        names
    }

    /// Sessions whose HELLO has arrived. Before that a link may still be pairing.
    async fn greeted_names(&self) -> Vec<String> {
        let mut names: Vec<String> = self
            .txs
            .lock()
            .await
            .iter()
            .filter(|(_, l)| l.info.lock().is_ok_and(|i| !i.host_name.is_empty()))
            .map(|(k, _)| k.clone())
            .collect();
        names.sort();
        names
    }

    async fn info(&self, name: &str) -> Option<PeerInfo> {
        let link = Self::find(&*self.txs.lock().await, name)?;
        let info = link.info.lock().map(|i| i.clone()).unwrap_or_default();
        Some(info)
    }
}

#[derive(Clone)]
pub struct NetHub {
    peers: PeerTable,
    last_peers: Arc<Mutex<HashMap<String, SocketAddr>>>,
    held_off: Arc<Mutex<HashSet<String>>>,
}

impl NetHub {
    /// Send to `peer` without making it the target of forwarded input.
    pub async fn send_control(&self, peer: &str, frame: Vec<u8>) {
        self.peers.send_to(peer, frame).await;
    }

    pub async fn send_bulk(&self, peer: &str, frame: Vec<u8>) -> bool {
        self.peers.send_bulk(peer, frame).await
    }

    pub async fn peer_names(&self) -> Vec<String> {
        self.peers.names().await
    }

    pub async fn greeted_names(&self) -> Vec<String> {
        self.peers.greeted_names().await
    }

    pub async fn send_raw(&self, frame: Vec<u8>) {
        self.peers.send_active(frame).await;
    }

    pub async fn send_to(&self, peer: &str, frame: Vec<u8>) {
        self.peers.set_active(peer).await;
        self.peers.send_to(peer, frame).await;
    }

    pub async fn has_peer(&self, name: &str) -> bool {
        self.peers.contains(name).await
    }

    /// Capability bits from the peer's HELLO; 0 for older versions or no session.
    pub async fn peer_caps(&self, name: &str) -> u32 {
        self.peers
            .info(name)
            .await
            .map(|i| i.capabilities)
            .unwrap_or(0)
    }

    /// Computer name to show for a session key (the controlled side keys by address).
    pub async fn peer_label(&self, name: &str) -> String {
        self.peers
            .info(name)
            .await
            .map(|i| i.host_name)
            .filter(|n| !n.is_empty())
            .unwrap_or_else(|| name.to_string())
    }

    /// Drop a live session without forgetting trust, and do not auto-redial
    /// that computer until the user types a pairing code again.
    pub async fn disconnect_peer(&self, name: &str) -> bool {
        let name = name.trim();
        if name.is_empty() {
            return false;
        }
        let had = self.peers.contains(name).await;
        let info = self.peers.info(name).await;
        self.peers.kill_remove(name).await;

        let mut aliases = Vec::new();
        if let Some(info) = &info {
            if !info.host_name.is_empty() && !info.host_name.eq_ignore_ascii_case(name) {
                aliases.push(info.host_name.clone());
            }
        }
        for alias in &aliases {
            self.peers.kill_remove(alias).await;
        }

        let mut ips = Vec::new();
        {
            let mut last = self.last_peers.lock().await;
            last.retain(|k, addr| {
                let hit = k.eq_ignore_ascii_case(name)
                    || aliases.iter().any(|a| k.eq_ignore_ascii_case(a));
                if hit {
                    ips.push(addr.ip());
                }
                !hit
            });
            last.retain(|k, addr| {
                !ips.iter()
                    .any(|ip| addr.ip() == *ip || k == &ip.to_string())
            });
        }
        for ip in &ips {
            self.peers.kill_remove(&ip.to_string()).await;
        }

        let mut held = self.held_off.lock().await;
        held.insert(name.to_ascii_lowercase());
        for alias in &aliases {
            held.insert(alias.to_ascii_lowercase());
        }
        for ip in ips {
            held.insert(ip.to_string().to_ascii_lowercase());
        }
        had
    }

    pub async fn send_mouse_move(&self, dx: i32, dy: i32) {
        self.send_raw(encode_mouse_move_bin(&MouseMovePayload { dx, dy }))
            .await;
    }

    /// Forward one captured input event to the computer that has the pointer.
    pub async fn send_captured(&self, ev: csc_platform::Captured) {
        use csc_platform::Captured;
        let frame = match ev {
            Captured::Move { dx, dy } => encode_mouse_move_bin(&MouseMovePayload { dx, dy }),
            Captured::Button { button, down } => {
                csc_proto::encode_mouse_button_bin(&csc_proto::MouseButtonPayload {
                    button,
                    down: down as u8,
                })
            }
            Captured::Wheel { v, h } => {
                csc_proto::encode_mouse_wheel_bin(&csc_proto::MouseWheelPayload {
                    wheel_v: v,
                    wheel_h: h,
                })
            }
            Captured::Key { hid, down } => csc_proto::encode_key_bin(&csc_proto::KeyPayload {
                hid_usage: hid,
                down: down as u8,
                modifiers: 0,
            }),
        };
        self.send_raw(frame).await;
    }
}

#[allow(clippy::too_many_arguments)]
pub async fn spawn_network(
    cfg: Arc<RwLock<Config>>,
    config_path: PathBuf,
    app_status: Arc<RwLock<AppStatus>>,
    prompt: Arc<PeerPrompt>,
    mut config_watch: watch::Receiver<u64>,
    net_reload: watch::Sender<u64>,
    layout_reload: watch::Sender<u64>,
    events: mpsc::Sender<InboundEvent>,
    transfer: crate::transfer::TransferHandle,
) -> anyhow::Result<NetHub> {
    let peers = PeerTable::new();
    let last_peers = Arc::new(Mutex::new(HashMap::new()));
    let held_off = Arc::new(Mutex::new(HashSet::new()));
    let inbound = Wires {
        events,
        transfer,
        last_peers: last_peers.clone(),
        held_off: held_off.clone(),
        peers: peers.clone(),
        kick: Arc::new(tokio::sync::Notify::new()),
        net_reload,
        layout_reload,
    };
    let status = Arc::new(RwLock::new(NetStatus::default()));
    let status2 = status.clone();
    let cfg2 = cfg.clone();
    let peers2 = peers.clone();

    tokio::spawn(async move {
        loop {
            let snapshot = cfg2.read().await.clone();
            {
                let mut s = app_status.write().await;
                s.discovery_port = udp_port(&snapshot);
                s.role = match snapshot.role {
                    Role::Server => "server".into(),
                    Role::Client => "client".into(),
                };
                s.link_state = match snapshot.role {
                    Role::Server => "searching".into(),
                    Role::Client => "announcing".into(),
                };
                s.search_subnet = snapshot
                    .search_address
                    .is_empty()
                    .then(String::new)
                    .unwrap_or_else(|| subnet_label(&snapshot.search_address).unwrap_or_default());
            }
            let (local_host, local_ips) = {
                let s = app_status.read().await;
                (s.host_name.clone(), s.local_ipv4.clone())
            };
            let restarted = tokio::select! {
                _ = config_watch.changed() => {
                    info!("network settings changed, restarting discovery");
                    true
                }
                result = run_role(
                    &snapshot,
                    &cfg2,
                    &config_path,
                    &app_status,
                    &prompt,
                    &local_host,
                    &local_ips,
                    &status2,
                    &peers2,
                    inbound.clone(),
                ) => {
            if let Err(e) = result {
                warn!("network session ended: {e:#}");
                    }
                    false
                }
            };
            {
                let mut s = app_status.write().await;
                s.pending_peer = None;
                s.being_controlled = false;
            }
            {
                let mut s = status2.write().await;
                s.connected = false;
                s.peer_name.clear();
            }
            if restarted {
                continue;
            }
            tokio::select! {
                _ = config_watch.changed() => {}
                _ = tokio::time::sleep(Duration::from_secs(2)) => {}
            }
        }
    });

    Ok(NetHub {
        peers,
        last_peers,
        held_off,
    })
}

#[allow(clippy::too_many_arguments)]
async fn run_role(
    snapshot: &Config,
    cfg: &Arc<RwLock<Config>>,
    config_path: &Path,
    app_status: &Arc<RwLock<AppStatus>>,
    prompt: &Arc<PeerPrompt>,
    local_host: &str,
    local_ips: &[String],
    status: &Arc<RwLock<NetStatus>>,
    peers: &PeerTable,
    inbound: Wires,
) -> anyhow::Result<()> {
    let identity = Arc::new(crate::tls::Identity::load(
        config_path.parent().unwrap_or_else(|| Path::new(".")),
    )?);
    info!("control channel TLS fingerprint {}", identity.fingerprint);
    match snapshot.role {
        Role::Server => {
            run_server(
                snapshot,
                cfg,
                config_path,
                &identity,
                app_status,
                prompt,
                local_host,
                local_ips,
                status,
                peers,
                inbound,
            )
            .await
        }
        Role::Client => {
            run_client(
                snapshot,
                cfg,
                config_path,
                &identity,
                app_status,
                prompt,
                local_host,
                local_ips,
                status,
                peers,
                inbound,
            )
            .await
        }
    }
}

#[derive(Debug)]
pub enum InboundEvent {
    Hello(HelloPayload),
    Heartbeat,
    SwitchEnter(SwitchPayload),
    SwitchLeave(SwitchPayload),
    MouseMove {
        dx: i32,
        dy: i32,
    },
    MouseButton {
        button: u32,
        down: bool,
    },
    MouseWheel {
        v: i32,
        h: i32,
    },
    Key {
        hid: u32,
        down: bool,
    },
    /// JSON message whose sender matters: ClipOffer, EdgeHit, PasteRequest.
    From {
        peer: String,
        msg: MsgType,
        payload: Vec<u8>,
    },
    Raw(MsgType, Vec<u8>),
    Disconnected {
        peer: String,
    },
}

#[allow(clippy::too_many_arguments)]
async fn run_server(
    cfg: &Config,
    live: &Arc<RwLock<Config>>,
    config_path: &Path,
    identity: &Arc<crate::tls::Identity>,
    app_status: &Arc<RwLock<AppStatus>>,
    prompt: &Arc<PeerPrompt>,
    local_host: &str,
    local_ips: &[String],
    status: &Arc<RwLock<NetStatus>>,
    peers: &PeerTable,
    inbound: Wires,
) -> anyhow::Result<()> {
    let port = cfg.control_port;
    let discover = udp_port(cfg);
    let bind = effective_bind(cfg);
    remember_lan_bind(parse_bind(&bind, port));
    let udp = bind_udp(parse_bind(&bind, discover)).await?;
    spawn_data_listener(identity, parse_bind(&bind, cfg.data_port), cfg.data_port).await;
    let listener = match TcpListener::bind(parse_bind(&bind, port)).await {
        Ok(l) => Some(l),
        Err(e) => {
            warn!("listen TCP {port} failed ({e:#}); clients cannot dial back");
            None
        }
    };
    let subnet = subnet_label(&cfg.search_address).unwrap_or_else(|| "全部本机网段".into());
    info!("searching UDP port {discover} on {subnet}");
    let search = encode_search(cfg.control_port, cfg.data_port, &cfg.name);
    let search_unlinked =
        crate::discover::encode_search_unlinked(cfg.control_port, cfg.data_port, &cfg.name);
    let targets = scan_targets(cfg, discover, local_ips);
    let mut tick = tokio::time::interval(Duration::from_secs(2));
    // Typed pairing codes encode the peer host octet — poll often so we dial
    // that IP right away instead of waiting for a full /24 UDP sweep.
    let mut offer_poll = tokio::time::interval(Duration::from_millis(100));
    let mut last_offer_gen = 0u64;
    let mut buf = [0u8; 2048];
    let (stop_tx, stop_rx) = watch::channel(false);
    let _stop = StopOnDrop(stop_tx);
    let inflight: Arc<Mutex<HashSet<String>>> = Arc::new(Mutex::new(HashSet::new()));
    let mut seen: HashMap<String, (DiscoveredPeer, Instant)> = HashMap::new();
    let mut udp_fails = 0u32;
    let mut redial = Redial::default();
    let mut latency = LatencyProbes::default();
    let mut udp_peers: HashMap<IpAddr, SocketAddr> = HashMap::new();
    let kick = inbound.kick.clone();

    loop {
        tokio::select! {
            _ = offer_poll.tick() => {
                expire_stale_offer(app_status).await;
                let gen = app_status.read().await.offer_gen;
                if gen != last_offer_gen {
                    last_offer_gen = gen;
                    poke_offer_peer(&udp, &search_unlinked, cfg, app_status, discover).await;
                    spawn_try_dial_offer_ip(
                        cfg.clone(),
                        live.clone(),
                        config_path.to_path_buf(),
                        identity.clone(),
                        peers.clone(),
                        inbound.clone(),
                        status.clone(),
                        app_status.clone(),
                        prompt.clone(),
                        inflight.clone(),
                        stop_rx.clone(),
                    );
                }
            }
            _ = tick.tick() => {
                expire_stale_offer(app_status).await;
                udp_fails = count_udp_failure(udp_fails, send_to_targets(&udp, &search, &targets).await)?;
                let now = Instant::now();
                seen.retain(|_, (_, last)| now.duration_since(*last) < Duration::from_secs(15));
                publish_discovered(app_status, &seen).await;
                ping_linked_peers(
                    &udp,
                    cfg,
                    discover,
                    peers,
                    &inbound,
                    &mut latency,
                    &udp_peers,
                    &search_unlinked,
                )
                .await;
                poke_offer_peer(&udp, &search_unlinked, cfg, app_status, discover).await;
                spawn_try_dial_offer_ip(
                    cfg.clone(),
                    live.clone(),
                    config_path.to_path_buf(),
                    identity.clone(),
                    peers.clone(),
                    inbound.clone(),
                    status.clone(),
                    app_status.clone(),
                    prompt.clone(),
                    inflight.clone(),
                    stop_rx.clone(),
                );
                // Controlled computers that were linked before are dialed directly,
                // without waiting for their announce.
                let known: Vec<(String, SocketAddr)> = inbound
                    .last_peers
                    .lock()
                    .await
                    .iter()
                    .map(|(k, a)| (k.clone(), *a))
                    .collect();
                for (key, addr) in known {
                    if is_held_off(&inbound, &[&key, &addr.ip().to_string()]).await {
                        continue;
                    }
                    if peers.contains(&key).await
                        || already_linked_to_ip(peers, &inbound, addr.ip()).await
                    {
                        redial.reset(&key);
                        continue;
                    }
                    // Typed pairing code owns initiation to this IP.
                    if offer_ready(app_status).await {
                        let offer = current_offer(app_status).await;
                        if let Some(want) = peer_addr_from_offer(cfg, &offer, local_ips) {
                            if want.ip() == addr.ip() {
                                continue;
                            }
                        }
                    }
                    if !redial.due(&key, now) || inflight.lock().await.contains(&key) {
                        continue;
                    }
                    let ip_key = addr.ip().to_string();
                    if inflight.lock().await.contains(&ip_key) {
                        continue;
                    }
                    debug!("redialing {key} at {addr}");
                    inflight.lock().await.insert(key.clone());
                    spawn_server_link(
                        cfg.clone(),
                        live.clone(),
                        config_path.to_path_buf(),
                        identity.clone(),
                        peers.clone(),
                        inbound.clone(),
                        status.clone(),
                        app_status.clone(),
                        prompt.clone(),
                        inflight.clone(),
                        stop_rx.clone(),
                        key,
                        ServerLink::Dial(addr),
                    false);
                }
                let manuals = live.read().await.network.manual_peers.clone();
                for peer in manuals {
                    let port = if peer.control_port == 0 {
                        cfg.control_port
                    } else {
                        peer.control_port
                    };
                    let Ok(mut addrs) = (peer.address.as_str(), port).to_socket_addrs() else {
                        continue;
                    };
                    let Some(addr) = addrs.next() else { continue };
                    let key = addr.ip().to_string();
                    if !cfg.search_address.is_empty()
                        && !same_subnet24(&key, &cfg.search_address)
                    {
                        continue;
                    }
                    if peers.contains(&key).await {
                        redial.reset(&key);
                        continue;
                    }
                    if !redial.due(&key, now) || inflight.lock().await.contains(&key) {
                        continue;
                    }
                    debug!("dialing manual peer {addr}");
                    inflight.lock().await.insert(key.clone());
                    spawn_server_link(
                        cfg.clone(),
                        live.clone(),
                        config_path.to_path_buf(),
                        identity.clone(),
                        peers.clone(),
                        inbound.clone(),
                        status.clone(),
                        app_status.clone(),
                        prompt.clone(),
                        inflight.clone(),
                        stop_rx.clone(),
                        key,
                        ServerLink::Dial(addr),
                    false);
                }
            }
            _ = kick.notified() => {
                send_to_targets(&udp, &search, &targets).await;
            }
            res = udp.recv_from(&mut buf) => {
                let Some((n, src)) = udp_datagram(res)? else {
                    continue;
                };
                udp_peers.insert(src.ip(), src);
                match decode_datagram(&buf[..n]) {
                    Some(Datagram::Ping(echo)) => {
                        let _ = udp.send_to(&discovery_pong(cfg, echo.nonce), src).await;
                        continue;
                    }
                    Some(Datagram::Pong(echo)) => {
                        if let Some((peer, ms)) = latency.finish_ping(echo.nonce) {
                            set_peer_latency_for_ip(
                                app_status,
                                peers,
                                &inbound,
                                src.ip(),
                                &peer,
                                ms,
                            )
                            .await;
                        }
                        continue;
                    }
                    Some(Datagram::Announce(ann)) => {
                        // fall through with ann
                        let src_ip = src.ip().to_string();
                if is_own_announce(&ann.host_name, local_host, &src_ip, local_ips)
                    || is_own_announce(&ann.name, &cfg.name, &src_ip, local_ips)
                    || is_own_announce(&ann.host_name, &cfg.name, &src_ip, local_ips)
                {
                    continue;
                }
                if !cfg.search_address.is_empty() && !same_subnet24(&src_ip, &cfg.search_address) {
                    continue;
                }
                let key = if ann.name.is_empty() { ann.host_name.clone() } else { ann.name.clone() };
                let key = if key.is_empty() { src_ip.clone() } else { key };
                let display = if ann.name.is_empty() { key.clone() } else { ann.name.clone() };
                if let Some((peer, ms)) = latency.finish_search(src.ip()) {
                    set_peer_latency_for_ip(app_status, peers, &inbound, src.ip(), &peer, ms)
                        .await;
                }
                seen.insert(key.clone(), (DiscoveredPeer {
                    name: display.clone(),
                    host_name: if ann.host_name.is_empty() { key.clone() } else { ann.host_name.clone() },
                    address: src_ip.clone(),
                    seen_unix: unix_now(),
                }, Instant::now()));
                publish_discovered(app_status, &seen).await;

                if peers.contains(&key).await
                    || already_linked_to_ip(peers, &inbound, src.ip()).await
                {
                    // Still poke so the client can dial back if our inbound is blocked.
                    let _ = udp.send_to(&search_unlinked, src).await;
                    continue;
                }
                // A direct reply passes firewalls that drop unsolicited datagrams,
                // so the client dials back now instead of after its own refresh.
                let _ = udp.send_to(&search_unlinked, src).await;
                if is_held_off(&inbound, &[&key, &display, &ann.host_name, &src_ip]).await {
                    continue;
                }
                let eager = offer_ready(app_status).await;
                // Who typed the code dials out — discovery must not race that IP.
                if eager {
                    let offer = current_offer(app_status).await;
                    if let Some(want) = peer_addr_from_offer(cfg, &offer, local_ips) {
                        if want.ip() == src.ip() {
                            continue;
                        }
                    }
                }
                let known = {
                    let saved = live.read().await;
                    saved.is_trusted_peer(&display) || saved.peer_fingerprint(&display).is_some()
                };
                // No code yet: try once, then back off. A typed code dials immediately.
                if !eager && !known && !redial.due(&key, Instant::now()) {
                    continue;
                }
                if inflight.lock().await.contains(&key) {
                    continue;
                }
                let tcp_port = ann.control_port;
                let peer_addr = SocketAddr::new(src.ip(), tcp_port);
                inflight.lock().await.insert(key.clone());
                spawn_server_link(
                    cfg.clone(),
                    live.clone(),
                    config_path.to_path_buf(),
                    identity.clone(),
                    peers.clone(),
                    inbound.clone(),
                    status.clone(),
                    app_status.clone(),
                    prompt.clone(),
                    inflight.clone(),
                    stop_rx.clone(),
                    key,
                    ServerLink::Dial(peer_addr),
                false);
                    }
                    _ => {}
                }
            }
            acc = accept_opt(listener.as_ref()) => {
                let (tcp, addr) = match acc {
                    Ok(pair) => pair,
                    Err(e) => {
                        warn!("accept client: {e}");
                        continue;
                    }
                };
                prepare_tcp(&tcp);
                let ip = addr.ip().to_string();
                if !cfg.search_address.is_empty() && !same_subnet24(&ip, &cfg.search_address) {
                    info!(
                        "ignoring TCP from {addr}: outside selected subnet {}",
                        subnet_label(&cfg.search_address).unwrap_or_default()
                    );
                    continue;
                }
                // Only computers that announced on the selected LAN, or were linked
                // before, may dial in.
                let known = inbound
                    .last_peers
                    .lock()
                    .await
                    .iter()
                    .find(|(_, a)| a.ip() == addr.ip())
                    .map(|(k, _)| k.clone());
                let Some(key) = seen
                    .iter()
                    .find(|(_, (p, _))| p.address == ip)
                    .map(|(k, _)| k.clone())
                    .or(known)
                else {
                    info!("ignoring TCP from {addr}: no announce seen yet");
                    continue;
                };
                if is_held_off(&inbound, &[&key, &ip]).await {
                    info!("ignoring TCP from {addr}: user disconnected this computer");
                    continue;
                }
                spawn_server_link(
                    cfg.clone(),
                    live.clone(),
                    config_path.to_path_buf(),
                    identity.clone(),
                    peers.clone(),
                    inbound.clone(),
                    status.clone(),
                    app_status.clone(),
                    prompt.clone(),
                    inflight.clone(),
                    stop_rx.clone(),
                    key,
                    ServerLink::Accepted(tcp, addr),
                false);
            }
        }
    }
}

async fn accept_opt(listener: Option<&TcpListener>) -> std::io::Result<(TcpStream, SocketAddr)> {
    match listener {
        Some(l) => l.accept().await,
        None => std::future::pending().await,
    }
}

enum ServerLink {
    Dial(SocketAddr),
    Accepted(TcpStream, SocketAddr),
}

/// Address used to redial a peer's *listening* control port.
/// Never store an inbound connection's ephemeral source port — that produced
/// redials to ports such as 7672 after a client dialed back.
fn peer_control_addr(ip: IpAddr, advertised: u16, fallback: u16) -> SocketAddr {
    let port = if advertised != 0 {
        advertised
    } else {
        fallback
    };
    SocketAddr::new(ip, port)
}

fn remember_control_addr(
    last: &mut HashMap<String, SocketAddr>,
    key: &str,
    ip: IpAddr,
    advertised: u16,
    fallback: u16,
) {
    last.insert(key.to_string(), peer_control_addr(ip, advertised, fallback));
}

fn known_control_port(last: &HashMap<String, SocketAddr>, ip: IpAddr, fallback: u16) -> u16 {
    last.iter()
        .find(|(_, a)| a.ip() == ip && a.port() != 0)
        .map(|(_, a)| a.port())
        .unwrap_or(fallback)
}

async fn set_peer_note(app_status: &Arc<RwLock<AppStatus>>, note: impl Into<String>) {
    let note = note.into();
    let mut s = app_status.write().await;
    if s.peer_note != note {
        s.peer_note = note;
    }
}

async fn expire_stale_offer(app_status: &Arc<RwLock<AppStatus>>) {
    let mut s = app_status.write().await;
    if s.offer_code.len() != 6 || s.offer_until == 0 || s.offer_until > unix_now() {
        return;
    }
    s.offer_code.clear();
    s.offer_until = 0;
    if s.peer_note.contains("正在连接") || s.peer_note.is_empty() {
        s.peer_note = "连接超时，请重新输入配对码".into();
    }
}

async fn fail_offer(app_status: &Arc<RwLock<AppStatus>>, note: &str) {
    let mut s = app_status.write().await;
    s.offer_code.clear();
    s.offer_until = 0;
    let keep_existing = !s.peer_note.is_empty()
        && !s.peer_note.contains("正在连接")
        && (s.peer_note.contains("配对")
            || s.peer_note.contains("取消")
            || s.peer_note.contains("超时")
            || s.peer_note.contains("失败"));
    if !keep_existing {
        s.peer_note = note.to_string();
    }
}

async fn note_waiting_dialback(app_status: &Arc<RwLock<AppStatus>>) {
    if offer_ready(app_status).await {
        set_peer_note(app_status, "正在连接，等待对方回拨…").await;
    }
}

async fn note_handshake_failed(app_status: &Arc<RwLock<AppStatus>>, err: &str) {
    if offer_ready(app_status).await {
        warn!("pairing handshake failed during offer: {err}");
        fail_offer(app_status, "连接握手失败，请重新输入配对码").await;
    }
}

async fn ip_looks_trusted(live: &Arc<RwLock<Config>>, inbound: &Wires, ip: IpAddr) -> bool {
    let mut names: Vec<String> = vec![ip.to_string()];
    {
        let last = inbound.last_peers.lock().await;
        for (k, a) in last.iter() {
            if a.ip() == ip {
                names.push(k.clone());
            }
        }
    }
    let saved = live.read().await;
    names
        .iter()
        .any(|n| saved.is_trusted_peer(n) || saved.peer_fingerprint(n).is_some())
}

/// Pairing code `XXXYYY`: `YYY` is the peer’s IPv4 last octet on the selected /24.
/// When `search_address` was cleared (settings reset), fall back to a local IPv4.
fn peer_addr_from_offer(cfg: &Config, offer: &str, local_ipv4: &[String]) -> Option<SocketAddr> {
    let digits: String = offer.chars().filter(|c| c.is_ascii_digit()).collect();
    if digits.len() != 6 {
        debug!(
            offer = %offer,
            digits = %digits,
            "peer_addr_from_offer: need exactly 6 digits"
        );
        return None;
    }
    let octet: u8 = match digits[3..6].parse() {
        Ok(o) => o,
        Err(e) => {
            debug!(suffix = &digits[3..6], error = %e, "peer_addr_from_offer: bad host octet");
            return None;
        }
    };
    if octet == 0 || octet == 255 {
        debug!(octet, "peer_addr_from_offer: host octet cannot be 0/255");
        return None;
    }
    let base = crate::discover::ipv4_octets(&cfg.search_address).or_else(|| {
        let hit = local_ipv4
            .iter()
            .find_map(|ip| crate::discover::ipv4_octets(ip));
        if let Some(b) = hit {
            debug!(
                search = %cfg.search_address,
                fallback = %format!("{}.{}.{}.{}", b[0], b[1], b[2], b[3]),
                "peer_addr_from_offer: search_address empty — using local IPv4"
            );
        }
        hit
    });
    let Some(base) = base else {
        debug!(
            search = %cfg.search_address,
            ?local_ipv4,
            "peer_addr_from_offer: no IPv4 base (search_address and local_ipv4 empty)"
        );
        return None;
    };
    if base[3] == octet {
        debug!(
            base0 = base[0],
            base1 = base[1],
            base2 = base[2],
            octet,
            "peer_addr_from_offer: refusing to dial self"
        );
        return None;
    }
    let addr = SocketAddr::new(
        IpAddr::V4(Ipv4Addr::new(base[0], base[1], base[2], octet)),
        cfg.control_port,
    );
    debug!(%addr, offer = %digits, "peer_addr_from_offer: ok");
    Some(addr)
}

/// Unicast a search probe to the IP encoded in the typed code so a firewalled
/// client dials back without waiting for a full /24 sweep.
async fn poke_offer_peer(
    udp: &UdpSocket,
    search: &[u8],
    cfg: &Config,
    app_status: &Arc<RwLock<AppStatus>>,
    discover: u16,
) {
    if !offer_ready(app_status).await {
        return;
    }
    let (offer, local_ips) = {
        let s = app_status.read().await;
        (s.offer_code.clone(), s.local_ipv4.clone())
    };
    if offer.len() != 6 {
        return;
    }
    let Some(tcp) = peer_addr_from_offer(cfg, &offer, &local_ips) else {
        return;
    };
    let dest = SocketAddr::new(tcp.ip(), discover);
    if let Err(e) = udp.send_to(search, dest).await {
        debug!("offer poke {dest}: {e}");
    }
}

/// True when we already have a live session to this IP (under IP or hostname key).
async fn already_linked_to_ip(peers: &PeerTable, inbound: &Wires, ip: std::net::IpAddr) -> bool {
    if peers.contains(&ip.to_string()).await {
        return true;
    }
    let keys: Vec<String> = inbound
        .last_peers
        .lock()
        .await
        .iter()
        .filter(|(_, a)| a.ip() == ip)
        .map(|(k, _)| k.clone())
        .collect();
    for k in keys {
        if peers.contains(&k).await {
            return true;
        }
    }
    false
}

/// Keys currently holding a control session to `ip`.
async fn session_keys_for_ip(peers: &PeerTable, inbound: &Wires, ip: IpAddr) -> Vec<String> {
    let mut keys = Vec::new();
    let ip_s = ip.to_string();
    if peers.contains(&ip_s).await {
        keys.push(ip_s.clone());
    }
    // Collect candidates then drop the mutex before any further .await.
    let candidates: Vec<String> = inbound
        .last_peers
        .lock()
        .await
        .iter()
        .filter(|(_, a)| a.ip() == ip)
        .map(|(k, _)| k.clone())
        .collect();
    for k in candidates {
        if keys.iter().any(|x| x.eq_ignore_ascii_case(&k)) {
            continue;
        }
        if peers.contains(&k).await {
            keys.push(k);
        }
    }
    keys
}

/// True when a session to `ip` has finished HELLO (usable for switching / UI).
async fn already_greeted_at_ip(peers: &PeerTable, inbound: &Wires, ip: IpAddr) -> bool {
    let greeted = peers.greeted_names().await;
    if greeted.iter().any(|n| n == &ip.to_string()) {
        debug!(%ip, ?greeted, "already_greeted_at_ip: yes (ip key)");
        return true;
    }
    let names: Vec<String> = inbound
        .last_peers
        .lock()
        .await
        .iter()
        .filter(|(_, a)| a.ip() == ip)
        .map(|(k, _)| k.clone())
        .collect();
    for k in &names {
        if greeted.iter().any(|n| n.eq_ignore_ascii_case(k)) {
            debug!(%ip, key = %k, ?greeted, "already_greeted_at_ip: yes (hostname)");
            return true;
        }
    }
    debug!(%ip, ?greeted, "already_greeted_at_ip: no");
    false
}

/// Drop incomplete (or any) sessions to `ip` so a typed pairing-code dial can own the slot.
async fn drop_sessions_to_ip(peers: &PeerTable, inbound: &Wires, ip: IpAddr) {
    for key in session_keys_for_ip(peers, inbound, ip).await {
        info!("replacing session {key} so pairing-code dial can initiate to {ip}");
        peers.kill_remove(&key).await;
    }
}

/// Fire-and-forget dial so the discovery select loop never blocks on locks/TLS.
#[allow(clippy::too_many_arguments)]
fn spawn_try_dial_offer_ip(
    cfg: Config,
    live: Arc<RwLock<Config>>,
    config_path: PathBuf,
    identity: Arc<crate::tls::Identity>,
    peers: PeerTable,
    inbound: Wires,
    status: Arc<RwLock<NetStatus>>,
    app_status: Arc<RwLock<AppStatus>>,
    prompt: Arc<PeerPrompt>,
    inflight: Arc<Mutex<HashSet<String>>>,
    stop_rx: watch::Receiver<bool>,
) {
    tokio::spawn(async move {
        try_dial_offer_ip(
            &cfg,
            &live,
            &config_path,
            &identity,
            &peers,
            &inbound,
            &status,
            &app_status,
            &prompt,
            &inflight,
            &stop_rx,
        )
        .await;
    });
}

/// Dial the IP encoded in the typed pairing code (skip subnet search wait).
#[allow(clippy::too_many_arguments)]
async fn try_dial_offer_ip(
    cfg: &Config,
    live: &Arc<RwLock<Config>>,
    config_path: &Path,
    identity: &Arc<crate::tls::Identity>,
    peers: &PeerTable,
    inbound: &Wires,
    status: &Arc<RwLock<NetStatus>>,
    app_status: &Arc<RwLock<AppStatus>>,
    prompt: &Arc<PeerPrompt>,
    inflight: &Arc<Mutex<HashSet<String>>>,
    stop_rx: &watch::Receiver<bool>,
) {
    if !offer_ready(app_status).await {
        debug!("try_dial_offer_ip: skip — offer not ready");
        return;
    }
    let offer = current_offer(app_status).await;
    let local_ips = app_status.read().await.local_ipv4.clone();
    debug!(
        offer = %offer,
        search = %cfg.search_address,
        ?local_ips,
        "try_dial_offer_ip: offer ready"
    );
    let Some(addr) = peer_addr_from_offer(cfg, &offer, &local_ips) else {
        warn!(
            offer = %offer,
            search = %cfg.search_address,
            ?local_ips,
            "try_dial_offer_ip: cannot map pairing code to peer IP"
        );
        return;
    };
    debug!(%addr, "try_dial_offer_ip: resolving session key");
    let key = session_key_for_ip(inbound, app_status, addr.ip()).await;
    debug!(%addr, %key, "try_dial_offer_ip: session key ready");
    let greeted = already_greeted_at_ip(peers, inbound, addr.ip()).await;
    let keys = session_keys_for_ip(peers, inbound, addr.ip()).await;
    let ip = addr.ip().to_string();
    let mut hold_names: Vec<&str> = vec![&key, &ip];
    hold_names.extend(keys.iter().map(|s| s.as_str()));
    release_hold(inbound, &hold_names).await;
    let trusted = {
        let saved = live.read().await;
        let mut hit = saved.is_trusted_peer(&key) || saved.peer_fingerprint(&key).is_some();
        for k in &keys {
            if saved.is_trusted_peer(k) || saved.peer_fingerprint(k).is_some() {
                hit = true;
                break;
            }
        }
        hit
    };
    debug!(%addr, %key, greeted, trusted, ?keys, "try_dial_offer_ip: link state");
    // Who typed initiates. Only skip when HELLO finished AND peer is trusted.
    if greeted && trusted {
        info!(
            %addr,
            %key,
            "try_dial_offer_ip: already greeted+trusted — clear offer, no redial"
        );
        clear_offer(app_status).await;
        return;
    }
    // A typed code takes over the IP once. Later ticks must leave that session
    // alone while the peer is clicking confirm, and only redial once it is gone.
    static LAST_OFFER_DIAL: Mutex<u64> = Mutex::const_new(0);
    let mut last_gen = LAST_OFFER_DIAL.lock().await;
    let gen = app_status.read().await.offer_gen;
    let first = *last_gen != gen;
    let ip_key = addr.ip().to_string();
    if !first {
        let busy = {
            let flight = inflight.lock().await;
            flight.contains(&key) || flight.contains(&ip_key)
        };
        if busy || already_linked_to_ip(peers, inbound, addr.ip()).await {
            debug!(%addr, %key, "try_dial_offer_ip: pairing session for this code still open");
            return;
        }
    } else {
        if greeted && !trusted {
            debug!(%addr, %key, "try_dial_offer_ip: greeted but not trusted — replace");
        } else if already_linked_to_ip(peers, inbound, addr.ip()).await {
            debug!(%addr, %key, "try_dial_offer_ip: incomplete session — replace");
        }
        drop_sessions_to_ip(peers, inbound, addr.ip()).await;
        debug!(%addr, %key, "try_dial_offer_ip: stale sessions dropped");
    }
    {
        let mut flight = inflight.lock().await;
        if first {
            flight.remove(&ip_key);
            flight.remove(&key);
        }
        if !flight.insert(key.clone()) {
            debug!(%key, "try_dial_offer_ip: inflight race — skip duplicate spawn");
            return;
        }
        debug!(%key, "try_dial_offer_ip: inflight marked");
    }
    *last_gen = gen;
    drop(last_gen);
    info!("pairing-code initiate: dialing {addr} as {key} (who typed, dials out)");
    spawn_server_link(
        cfg.clone(),
        live.clone(),
        config_path.to_path_buf(),
        identity.clone(),
        peers.clone(),
        inbound.clone(),
        status.clone(),
        app_status.clone(),
        prompt.clone(),
        inflight.clone(),
        stop_rx.clone(),
        key,
        ServerLink::Dial(addr),
        first,
    );
}

/// Prefer a hostname already known for `ip` (last_peers / discovered); else the IP.
async fn session_key_for_ip(
    inbound: &Wires,
    app_status: &Arc<RwLock<AppStatus>>,
    ip: IpAddr,
) -> String {
    let from_last = {
        let guard = inbound.last_peers.lock().await;
        guard
            .iter()
            .find(|(_, a)| a.ip() == ip)
            .map(|(k, _)| k.clone())
    }; // mutex dropped here
    if let Some(name) = from_last {
        if name.parse::<IpAddr>().is_err() {
            return name;
        }
    }
    let ip_s = ip.to_string();
    let discovered = app_status.read().await.discovered.clone();
    for peer in &discovered {
        if peer.address == ip_s {
            if !peer.name.is_empty() {
                return peer.name.clone();
            }
            if !peer.host_name.is_empty() {
                return peer.host_name.clone();
            }
        }
    }
    ip_s
}

#[allow(clippy::too_many_arguments)]
fn spawn_server_link(
    cfg: Config,
    live: Arc<RwLock<Config>>,
    config_path: PathBuf,
    identity: Arc<crate::tls::Identity>,
    peers: PeerTable,
    inbound: Wires,
    status: Arc<RwLock<NetStatus>>,
    app_status: Arc<RwLock<AppStatus>>,
    prompt: Arc<PeerPrompt>,
    inflight: Arc<Mutex<HashSet<String>>>,
    mut stop: watch::Receiver<bool>,
    key: String,
    link: ServerLink,
    // Typed pairing-code dial: replace any incomplete session to this IP.
    replace: bool,
) {
    tokio::spawn(async move {
        let dialed = matches!(link, ServerLink::Dial(_));
        match link {
            ServerLink::Dial(addr) => match connect_tls(&identity, addr).await {
                Ok((stream, cert)) => {
                    let token = export_client(&stream);
                    host_link_session(
                        &cfg,
                        &live,
                        &config_path,
                        &identity,
                        &peers,
                        inbound,
                        &status,
                        &app_status,
                        &prompt,
                        &inflight,
                        &mut stop,
                        &key,
                        stream,
                        cert,
                        token,
                        addr,
                        true,
                        replace,
                    )
                    .await;
                }
                Err(e) => {
                    warn!(
                        "connect {addr}: {e:#} (the client's firewall may block inbound TCP; waiting for it to dial back)"
                    );
                    let text = format!("{e:#}");
                    if text.contains("timed out")
                        || text.contains("Connection refused")
                        || text.contains("Network is unreachable")
                        || text.contains("No route to host")
                    {
                        note_waiting_dialback(&app_status).await;
                    } else {
                        note_handshake_failed(&app_status, &text).await;
                    }
                }
            },
            ServerLink::Accepted(tcp, addr) => match accept_tls(&identity, tcp).await {
                Ok((stream, cert)) => {
                    let token = export_server(&stream);
                    host_link_session(
                        &cfg,
                        &live,
                        &config_path,
                        &identity,
                        &peers,
                        inbound,
                        &status,
                        &app_status,
                        &prompt,
                        &inflight,
                        &mut stop,
                        &key,
                        stream,
                        cert,
                        token,
                        addr,
                        false,
                        replace,
                    )
                    .await;
                }
                Err(e) => {
                    warn!("client {key} dialed back from {addr}: {e:#}");
                    note_handshake_failed(&app_status, &format!("{e:#}")).await;
                }
            },
        }
        if dialed {
            inflight.lock().await.remove(&key);
        }
    });
}

/// One host-side control session after TLS (outbound = TLS client, inbound = TLS server).
#[allow(clippy::too_many_arguments)]
async fn host_link_session<S>(
    cfg: &Config,
    live: &Arc<RwLock<Config>>,
    config_path: &Path,
    identity: &Arc<crate::tls::Identity>,
    peers: &PeerTable,
    inbound: Wires,
    status: &Arc<RwLock<NetStatus>>,
    app_status: &Arc<RwLock<AppStatus>>,
    prompt: &Arc<PeerPrompt>,
    inflight: &Arc<Mutex<HashSet<String>>>,
    stop: &mut watch::Receiver<bool>,
    key: &str,
    stream: S,
    peer_cert: Vec<u8>,
    data_token: [u8; 32],
    peer_addr: SocketAddr,
    dialed: bool,
    replace: bool,
) where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let (tx, rx) = new_link();
    if replace {
        // Who typed the code initiates — own the slot.
        drop_sessions_to_ip(peers, &inbound, peer_addr.ip()).await;
    } else if already_linked_to_ip(peers, &inbound, peer_addr.ip()).await {
        // Discovery/redial must not fight an existing (or offer) session.
        debug!(
            "already linked to {} via another name; dropping {key} at {peer_addr}",
            peer_addr.ip()
        );
        if dialed {
            inflight.lock().await.remove(key);
        }
        return;
    }
    if dialed {
        if replace {
            // Force-take the key even if a racing insert landed.
            let _ = peers.adopt(key, tx.clone(), Duration::ZERO).await;
        } else if !peers.insert_new(key, tx.clone()).await {
            debug!("already linked to {key}; dropping extra connection {peer_addr}");
            inflight.lock().await.remove(key);
            return;
        }
    } else if !peers.adopt(key, tx.clone(), DUPLICATE_LINK).await {
        debug!("already linked to {key}; dropping extra connection {peer_addr}");
        return;
    }
    let how = if dialed {
        "dialed"
    } else {
        "dialed back by client"
    };
    info!("connected to client {key} at {peer_addr} ({how})");
    let tcp_addr = if dialed {
        peer_addr
    } else {
        let known = {
            let last = inbound.last_peers.lock().await;
            known_control_port(&last, peer_addr.ip(), cfg.control_port)
        };
        peer_control_addr(peer_addr.ip(), known, cfg.control_port)
    };
    inbound
        .last_peers
        .lock()
        .await
        .insert(key.to_string(), tcp_addr);
    refresh_link_status(status, peers).await;
    let kick = inbound.kick.clone();
    let events = inbound.clone();
    tokio::select! {
        result = session(
            cfg,
            live,
            config_path,
            stream,
            peer_cert,
            status,
            app_status,
            prompt,
            key.to_string(),
            rx,
            inbound,
            None,
            None,
            peer_addr.ip(),
            identity,
            data_token,
        ) => {
            if let Err(e) = result {
                warn!("session {key} ended: {e:#}");
            }
        }
        _ = stop.changed() => {}
    }
    peers.remove_owned(key, &tx).await;
    report_gone(peers, &events, key).await;
    refresh_link_status(status, peers).await;
    kick.notify_one();
}

#[allow(clippy::too_many_arguments)]
async fn run_client(
    cfg: &Config,
    live: &Arc<RwLock<Config>>,
    config_path: &Path,
    identity: &Arc<crate::tls::Identity>,
    app_status: &Arc<RwLock<AppStatus>>,
    prompt: &Arc<PeerPrompt>,
    local_host: &str,
    local_ips: &[String],
    status: &Arc<RwLock<NetStatus>>,
    peers: &PeerTable,
    inbound: Wires,
) -> anyhow::Result<()> {
    let port = cfg.control_port;
    let discover = udp_port(cfg);
    let bind = effective_bind(cfg);
    remember_lan_bind(parse_bind(&bind, port));
    spawn_data_listener(identity, parse_bind(&bind, cfg.data_port), cfg.data_port).await;
    let listener = TcpListener::bind(parse_bind(&bind, port)).await?;
    let udp = match bind_udp(parse_bind(&bind, discover)).await {
        Ok(sock) => sock,
        Err(e) => {
            warn!("bind UDP {port} failed ({e:#}); announcing from an ephemeral port");
            bind_udp(SocketAddr::from(([0, 0, 0, 0], 0))).await?
        }
    };
    let host_name = if local_host.is_empty() {
        cfg.name.clone()
    } else {
        local_host.to_string()
    };
    let announce = encode_announce(&Announce {
        name: cfg.name.clone(),
        host_name,
        control_port: cfg.control_port,
        data_port: cfg.data_port,
    });
    let (stop_tx, stop_rx) = watch::channel(false);
    let _stop = StopOnDrop(stop_tx);
    let rejected: Arc<Mutex<HashMap<String, RejectMark>>> = Arc::new(Mutex::new(HashMap::new()));
    let targets = scan_targets(cfg, discover, local_ips);
    let subnet = subnet_label(&cfg.search_address).unwrap_or_else(|| "本机网段".into());
    info!("announcing on UDP port {discover} toward {subnet}");
    let ctx = ClientCtx {
        cfg: cfg.clone(),
        live: live.clone(),
        config_path: config_path.to_path_buf(),
        identity: identity.clone(),
        app_status: app_status.clone(),
        prompt: prompt.clone(),
        status: status.clone(),
        peers: peers.clone(),
        inbound: inbound.clone(),
        rejected: rejected.clone(),
        stop: stop_rx.clone(),
    };
    let dialing: Arc<Mutex<HashSet<String>>> = Arc::new(Mutex::new(HashSet::new()));
    // Inbound TCP that has not finished yet. Do not dial the host again meanwhile.
    let incoming: Arc<Mutex<HashMap<String, u32>>> = Arc::new(Mutex::new(HashMap::new()));
    // Dial the host only if its own connection has not arrived within a second.
    // Dialing at the same moment replaces the session that just came up.
    let mut host_wait: HashMap<String, (Instant, SocketAddr)> = HashMap::new();
    let mut tick = tokio::time::interval(Duration::from_secs(1));
    let mut manual_tick = 0u8;
    let mut buf = [0u8; 2048];
    let mut udp_fails = 0u32;
    let mut latency = LatencyProbes::default();
    let mut udp_peers: HashMap<IpAddr, SocketAddr> = HashMap::new();
    let kick = inbound.kick.clone();
    loop {
        tokio::select! {
            _ = tick.tick() => {
                expire_stale_offer(app_status).await;
                manual_tick = manual_tick.wrapping_add(1);
                if manual_tick % 15 == 1 {
                    dial_client_manual(&ctx, &dialing).await;
                }
                udp_fails = count_udp_failure(udp_fails, send_to_targets(&udp, &announce, &targets).await)?;
                // Probe about every 2s while linked (client tick is 1s).
                if manual_tick.is_multiple_of(2) {
                    ping_linked_peers(
                        &udp,
                        cfg,
                        discover,
                        peers,
                        &inbound,
                        &mut latency,
                        &udp_peers,
                        &announce,
                    )
                    .await;
                }
                let eager = offer_ready(&ctx.app_status).await;
                let due: Vec<(String, SocketAddr)> = host_wait
                    .iter()
                    .filter(|(_, (at, _))| {
                        eager || at.elapsed() >= Duration::from_secs(1)
                    })
                    .map(|(key, (_, addr))| (key.clone(), *addr))
                    .collect();
                for (key, addr) in due {
                    host_wait.remove(&key);
                    let incoming_now = incoming.lock().await.contains_key(&key);
                    if incoming_now || peers.contains(&key).await {
                        continue;
                    }
                    if dialing.lock().await.insert(key.clone()) {
                        spawn_client_dial(ctx.clone(), dialing.clone(), key, addr);
                    }
                }
            }
            _ = kick.notified() => {
                send_to_targets(&udp, &announce, &targets).await;
            }
            res = udp.recv_from(&mut buf) => {
                let Some((n, src)) = udp_datagram(res)? else {
                    continue;
                };
                udp_peers.insert(src.ip(), src);
                let src_ip = src.ip().to_string();
                if is_own_announce("", local_host, &src_ip, local_ips) {
                    continue;
                }
                if !cfg.search_address.is_empty()
                    && !same_subnet24(&src_ip, &cfg.search_address)
                    && src_ip != "255.255.255.255"
                {
                    continue;
                }
                match decode_datagram(&buf[..n]) {
                    Some(Datagram::Ping(echo)) => {
                        let _ = udp.send_to(&discovery_pong(cfg, echo.nonce), src).await;
                    }
                    Some(Datagram::Pong(echo)) => {
                        if let Some((peer, ms)) = latency.finish_ping(echo.nonce) {
                            set_peer_latency_for_ip(
                                app_status,
                                peers,
                                &inbound,
                                src.ip(),
                                &peer,
                                ms,
                            )
                            .await;
                        }
                    }
                    Some(Datagram::Search(probe)) => {
                        // Host Search after our announce/probe → discovery RTT.
                        if let Some((peer, ms)) = latency.finish_search(src.ip()) {
                            set_peer_latency_for_ip(
                                app_status,
                                peers,
                                &inbound,
                                src.ip(),
                                &peer,
                                ms,
                            )
                            .await;
                        }
                        let _ = udp.send_to(&announce, src).await;
                        let key = src.ip().to_string();
                        let incoming_now = incoming.lock().await.contains_key(&key);
                        if incoming_now || peers.contains(&key).await {
                            host_wait.remove(&key);
                        } else {
                            host_wait.entry(key).or_insert((
                                Instant::now(),
                                SocketAddr::new(src.ip(), probe.control_port),
                            ));
                        }
                    }
                    _ => {}
                }
            }
            acc = listener.accept() => {
                let (tcp, peer) = acc?;
                prepare_tcp(&tcp);
                let ip = peer.ip().to_string();
                if !cfg.search_address.is_empty() && !same_subnet24(&ip, &cfg.search_address) {
                    info!(
                        "ignoring keyboard host from {peer}: outside selected subnet {}",
                        subnet_label(&cfg.search_address).unwrap_or_default()
                    );
                    continue;
                }
                info!("keyboard host connected from {peer}");
                let key = ip;
                *incoming
                    .lock()
                    .await
                    .entry(key.clone())
                    .or_insert(0) += 1;
                let ctx = ctx.clone();
                let incoming = incoming.clone();
                tokio::spawn(async move {
                    client_link(ctx, tcp, peer, false).await;
                    let mut n = incoming.lock().await;
                    if let Some(left) = n.get_mut(&key) {
                        *left = left.saturating_sub(1);
                        if *left == 0 {
                            n.remove(&key);
                        }
                    }
                });
            }
        }
    }
}

#[derive(Clone)]
struct ClientCtx {
    cfg: Config,
    live: Arc<RwLock<Config>>,
    config_path: PathBuf,
    identity: Arc<crate::tls::Identity>,
    app_status: Arc<RwLock<AppStatus>>,
    prompt: Arc<PeerPrompt>,
    status: Arc<RwLock<NetStatus>>,
    peers: PeerTable,
    inbound: Wires,
    rejected: Arc<Mutex<HashMap<String, RejectMark>>>,
    stop: watch::Receiver<bool>,
}

async fn dial_client_manual(ctx: &ClientCtx, dialing: &Arc<Mutex<HashSet<String>>>) {
    let saved = ctx.live.read().await;
    let manuals = saved.network.manual_peers.clone();
    let control = saved.control_port;
    drop(saved);
    for peer in manuals {
        let port = if peer.control_port == 0 {
            control
        } else {
            peer.control_port
        };
        let Ok(mut addrs) = (peer.address.as_str(), port).to_socket_addrs() else {
            warn!("manual address {} is not usable", peer.address);
            continue;
        };
        let Some(addr) = addrs.next() else { continue };
        let key = addr.ip().to_string();
        if !ctx.cfg.search_address.is_empty() && !same_subnet24(&key, &ctx.cfg.search_address) {
            continue;
        }
        if ctx.peers.contains(&key).await || dialing.lock().await.contains(&key) {
            continue;
        }
        if dialing.lock().await.insert(key.clone()) {
            info!("dialing manual address {addr}");
            spawn_client_dial(ctx.clone(), dialing.clone(), key, addr);
        }
    }
}

fn spawn_client_dial(
    ctx: ClientCtx,
    dialing: Arc<Mutex<HashSet<String>>>,
    key: String,
    addr: SocketAddr,
) {
    tokio::spawn(async move {
        match connect_peer(addr).await {
            Ok(tcp) => {
                info!("dialed keyboard host at {addr}");
                client_link(ctx, tcp, addr, true).await;
            }
            Err(e) => debug!("dial keyboard host {addr}: {e:#}"),
        }
        dialing.lock().await.remove(&key);
    });
}

/// Run one control session with the keyboard host, whichever side opened the TCP.
/// Outbound dials are the TLS client; inbound accepts are the TLS server.
async fn client_link(ctx: ClientCtx, tcp: TcpStream, peer: SocketAddr, dialed: bool) {
    let key = peer.ip().to_string();
    let (tx, rx) = new_link();
    let registered = Arc::new(AtomicBool::new(false));
    let link = ClientLink {
        peers: ctx.peers.clone(),
        status: ctx.status.clone(),
        key: key.clone(),
        tx: tx.clone(),
        registered: registered.clone(),
    };
    let admit = Admit {
        app_status: ctx.app_status.clone(),
        prompt: ctx.prompt.clone(),
        live: ctx.live.clone(),
        config_path: ctx.config_path.clone(),
        rejected: ctx.rejected.clone(),
        address: peer.to_string(),
    };
    if dialed {
        match tls_client(&ctx.identity, tcp, peer).await {
            Ok((stream, cert)) => {
                let token = export_client(&stream);
                client_link_session(
                    ctx, stream, cert, token, peer, key, link, admit, tx, rx, registered, true,
                )
                .await;
            }
            Err(e) => warn!("tls to {peer}: {e:#}"),
        }
    } else {
        match accept_tls(&ctx.identity, tcp).await {
            Ok((stream, cert)) => {
                let token = export_server(&stream);
                client_link_session(
                    ctx, stream, cert, token, peer, key, link, admit, tx, rx, registered, false,
                )
                .await;
            }
            Err(e) => {
                warn!("tls from {peer}: {e:#}");
                if !ip_looks_trusted(&ctx.live, &ctx.inbound, peer.ip()).await {
                    begin_pair_attempt(&ctx.app_status).await;
                    set_peer_note(&ctx.app_status, format!("{peer} 正在尝试连接，但握手失败"))
                        .await;
                    finish_pair_attempt(&ctx.app_status).await;
                }
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn client_link_session<S>(
    ctx: ClientCtx,
    stream: S,
    peer_cert: Vec<u8>,
    data_token: [u8; 32],
    peer: SocketAddr,
    key: String,
    link: ClientLink,
    admit: Admit,
    tx: Link,
    rx: LinkRx,
    registered: Arc<AtomicBool>,
    dialed: bool,
) where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    // Remember the peer's listening control port, never the ephemeral source port.
    {
        let mut last = ctx.inbound.last_peers.lock().await;
        if dialed {
            last.insert(key.clone(), peer);
        } else {
            let port = known_control_port(&last, peer.ip(), ctx.cfg.control_port);
            remember_control_addr(&mut last, &key, peer.ip(), port, ctx.cfg.control_port);
        }
    }
    let targeted = !dialed && !ip_looks_trusted(&ctx.live, &ctx.inbound, peer.ip()).await;
    if targeted {
        begin_pair_attempt(&ctx.app_status).await;
        set_peer_note(&ctx.app_status, format!("{} 正在尝试连接…", peer.ip())).await;
    }
    let mut stop = ctx.stop.clone();
    tokio::select! {
        result = session(
            &ctx.cfg,
            &ctx.live,
            &ctx.config_path,
            stream,
            peer_cert,
            &ctx.status,
            &ctx.app_status,
            &ctx.prompt,
            key.clone(),
            rx,
            ctx.inbound.clone(),
            Some(link),
            Some(admit),
            peer.ip(),
            &ctx.identity,
            data_token,
        ) => {
            if let Err(e) = result {
                warn!("client session {key} ended: {e:#}");
            }
        }
        _ = stop.changed() => {}
    }
    if targeted {
        finish_pair_attempt(&ctx.app_status).await;
    }
    ctx.peers.remove_owned(&key, &tx).await;
    // A second connection that never got past HELLO or trust was never this peer's session.
    if registered.load(Ordering::Acquire) {
        report_gone(&ctx.peers, &ctx.inbound, &key).await;
    }
    refresh_link_status(&ctx.status, &ctx.peers).await;
    ctx.inbound.kick.notify_one();
}

/// Tell the main loop `key` is gone, unless another session to it has taken over.
/// Both sides dial at once when they reconnect; the extra link ending is not a disconnect.
/// Also suppresses the IP-key hangup that follows an IP→hostname rekey while the
/// hostname session is still live (otherwise the UI flashes 断开/已连接).
async fn report_gone(peers: &PeerTable, wires: &Wires, key: &str) {
    if peers.contains(key).await {
        debug!("extra session to {key} closed; another one is still up");
        return;
    }
    if let Ok(ip) = key.parse::<IpAddr>() {
        if already_linked_to_ip(peers, wires, ip).await {
            debug!("session {key} closed; still linked under another name");
            return;
        }
    } else {
        let addr = wires.last_peers.lock().await.get(key).copied();
        if let Some(addr) = addr {
            if peers.contains(&addr.ip().to_string()).await
                || already_linked_to_ip(peers, wires, addr.ip()).await
            {
                debug!("session {key} closed; still linked via {}", addr.ip());
                return;
            }
        }
    }
    let _ = wires
        .events
        .send(InboundEvent::Disconnected {
            peer: key.to_string(),
        })
        .await;
}

/// Consecutive discovery rounds where no datagram could be sent. The socket is
/// probably bound to an address that went away; after a few, restart the network.
fn count_udp_failure(fails: u32, sent_any: bool) -> anyhow::Result<u32> {
    if sent_any {
        return Ok(0);
    }
    let fails = fails + 1;
    if fails >= 3 {
        anyhow::bail!("discovery could not send for {fails} rounds; rebuilding sockets");
    }
    Ok(fails)
}

fn hold_key(name: &str) -> String {
    name.trim().to_ascii_lowercase()
}

async fn is_held_off(inbound: &Wires, keys: &[&str]) -> bool {
    let held = inbound.held_off.lock().await;
    keys.iter().any(|k| {
        let k = k.trim();
        !k.is_empty() && held.contains(&hold_key(k))
    })
}

async fn release_hold(inbound: &Wires, keys: &[&str]) {
    let mut held = inbound.held_off.lock().await;
    for k in keys {
        let k = k.trim();
        if !k.is_empty() {
            held.remove(&hold_key(k));
        }
    }
}

/// Back-off for redialing a controlled computer that was linked before.
#[derive(Default)]
struct Redial {
    next: HashMap<String, (Instant, Duration)>,
}

impl Redial {
    const FIRST: Duration = Duration::from_millis(500);
    const MAX: Duration = Duration::from_secs(10);

    fn due(&mut self, key: &str, now: Instant) -> bool {
        let entry = self
            .next
            .entry(key.to_ascii_lowercase())
            .or_insert((now, Self::FIRST));
        if now < entry.0 {
            return false;
        }
        entry.0 = now + entry.1;
        entry.1 = (entry.1 * 2).min(Self::MAX);
        true
    }

    fn reset(&mut self, key: &str) {
        self.next.remove(&key.to_ascii_lowercase());
    }
}

struct StopOnDrop(watch::Sender<bool>);

impl Drop for StopOnDrop {
    fn drop(&mut self) {
        let _ = self.0.send(true);
    }
}

fn both_hosts(local: Role, remote_role: &str) -> bool {
    local == Role::Server && remote_role.eq_ignore_ascii_case("server")
}

async fn note_role_conflict(app_status: &Arc<RwLock<AppStatus>>, peer: &str) {
    warn!("both computers are keyboard hosts; not connecting to {peer}");
    app_status.write().await.peer_note = format!("{peer} 也是键鼠主机，没有建立连接");
    crate::notice::tell(
        crate::notice::Kind::Error,
        "Cross-SystemConnect",
        &crate::notice::text(
            &format!("{peer} 也是键鼠主机，请把其中一台改成被控端"),
            &format!("{peer} is also a keyboard host. Make one of them controlled."),
        ),
        true,
    );
}

/// Become the controlled computer and restart discovery so the host can dial in.
async fn demote_to_client(
    live: &Arc<RwLock<Config>>,
    config_path: &Path,
    app_status: &Arc<RwLock<AppStatus>>,
    net_reload: &watch::Sender<u64>,
    peer: &str,
) {
    {
        let mut saved = live.write().await;
        if saved.role == Role::Client {
            return;
        }
        info!("{peer} is the keyboard host; switching this computer to client");
        saved.role = Role::Client;
        if let Err(e) = saved.save(config_path) {
            warn!("save client role: {e:#}");
        }
    }
    app_status.write().await.role = "client".into();
    app_status.write().await.peer_note = format!("已自动切换为从机，以便接受 {peer} 的控制");
    net_reload.send_modify(|n| *n = n.wrapping_add(1));
}

fn effective_bind(cfg: &Config) -> String {
    if crate::discover::ipv4_octets(&cfg.search_address).is_some() {
        cfg.search_address.clone()
    } else if cfg.bind_address.is_empty() || cfg.bind_address.starts_with("127.") {
        "0.0.0.0".into()
    } else {
        cfg.bind_address.clone()
    }
}

fn scan_targets(cfg: &Config, port: u16, local_ips: &[String]) -> Vec<SocketAddr> {
    if crate::discover::ipv4_octets(&cfg.search_address).is_some() {
        subnet_scan_targets(&cfg.search_address, port)
    } else {
        broadcast_targets(port, local_ips)
    }
}

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

async fn publish_discovered(
    status: &Arc<RwLock<AppStatus>>,
    seen: &HashMap<String, (DiscoveredPeer, Instant)>,
) {
    let mut list: Vec<DiscoveredPeer> = seen.values().map(|(p, _)| p.clone()).collect();
    list.sort_by(|a, b| a.name.cmp(&b.name));
    status.write().await.discovered = list;
}

async fn refresh_link_status(status: &Arc<RwLock<NetStatus>>, peers: &PeerTable) {
    let names = peers.names().await;
    let mut s = status.write().await;
    s.connected = !names.is_empty();
    s.peer_name = names.join(", ");
}

fn parse_bind(bind: &str, port: u16) -> SocketAddr {
    if bind.is_empty() || bind == "0.0.0.0" {
        SocketAddr::from(([0, 0, 0, 0], port))
    } else {
        format!("{bind}:{port}")
            .parse()
            .unwrap_or_else(|_| SocketAddr::from(([0, 0, 0, 0], port)))
    }
}

static LAN_BIND: std::sync::Mutex<Option<std::net::Ipv4Addr>> = std::sync::Mutex::new(None);

fn remember_lan_bind(addr: SocketAddr) {
    let ip = match addr.ip() {
        std::net::IpAddr::V4(ip) if !ip.is_unspecified() => Some(ip),
        _ => None,
    };
    if let Ok(mut slot) = LAN_BIND.lock() {
        *slot = ip;
    }
}

fn pin_udp(sock: &UdpSocket, addr: SocketAddr) {
    #[cfg(windows)]
    {
        use std::os::windows::io::AsRawSocket;
        csc_platform::pin_ipv4_egress(sock.as_raw_socket(), addr);
    }
    #[cfg(not(windows))]
    {
        let _ = (sock, addr);
    }
}

async fn bind_udp(addr: SocketAddr) -> anyhow::Result<UdpSocket> {
    let sock = UdpSocket::bind(addr).await?;
    sock.set_broadcast(true)?;
    // Windows turns ICMP port-unreachable into WSAECONNRESET on the next recv.
    // Scanning a /24 then kills discovery. Ignore that error on the socket.
    #[cfg(windows)]
    {
        use std::os::windows::io::AsRawSocket;
        csc_platform::tune_udp_socket(sock.as_raw_socket());
    }
    pin_udp(&sock, addr);
    Ok(sock)
}

fn udp_datagram(
    res: std::io::Result<(usize, SocketAddr)>,
) -> std::io::Result<Option<(usize, SocketAddr)>> {
    match res {
        Ok(packet) => Ok(Some(packet)),
        Err(err)
            if err.kind() == std::io::ErrorKind::ConnectionReset
                || err.raw_os_error() == Some(10054) =>
        {
            Ok(None)
        }
        Err(err) => Err(err),
    }
}

/// Send to every target. True when at least one datagram went out.
async fn send_to_targets(sock: &UdpSocket, packet: &[u8], targets: &[SocketAddr]) -> bool {
    let mut any = targets.is_empty();
    let mut first_err = None;
    for dest in targets {
        match sock.send_to(packet, dest).await {
            Ok(_) => any = true,
            Err(e) => {
                tracing::debug!("discovery send {dest}: {e}");
                first_err.get_or_insert(e);
            }
        }
    }
    if let (false, Some(e)) = (any, first_err) {
        warn!("discovery send failed on every target: {e}");
    }
    any
}

async fn connect_peer(addr: SocketAddr) -> anyhow::Result<TcpStream> {
    let local = LAN_BIND.lock().ok().and_then(|g| *g);
    let stream = if cfg!(windows) && local.is_some() {
        let std_stream = tokio::task::spawn_blocking(move || {
            csc_platform::connect_tcp_from(local, addr, Duration::from_secs(4))
        })
        .await
        .map_err(|e| anyhow::anyhow!("connect {addr}: {e}"))??;
        // Tokio needs a non-blocking socket. A blocking one stalls the reader
        // and starves the writer, so HELLO and heartbeats never go out.
        std_stream.set_nonblocking(true)?;
        TcpStream::from_std(std_stream)?
    } else {
        tokio::time::timeout(Duration::from_secs(4), TcpStream::connect(addr))
            .await
            .map_err(|_| anyhow::anyhow!("connect {addr} timed out"))??
    };
    prepare_tcp(&stream);
    Ok(stream)
}

/// Keep the socket from sitting half-dead when a VPN/TUN like Clash
/// black-holes an established connection: no delay, and a short keepalive
/// so the session fails in a few seconds instead of stalling the pointer.
fn prepare_tcp(stream: &TcpStream) {
    let _ = stream.set_nodelay(true);
    let sock = socket2::SockRef::from(stream);
    let ka = socket2::TcpKeepalive::new()
        .with_time(Duration::from_secs(1))
        .with_interval(Duration::from_secs(1));
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    let ka = ka.with_retries(3);
    if let Err(e) = sock.set_tcp_keepalive(&ka) {
        debug!("tcp keepalive: {e}");
    }
}

async fn connect_tls(
    identity: &crate::tls::Identity,
    addr: SocketAddr,
) -> anyhow::Result<(tokio_rustls::client::TlsStream<TcpStream>, Vec<u8>)> {
    let tcp = connect_peer(addr).await?;
    tls_client(identity, tcp, addr).await
}

async fn tls_client(
    identity: &crate::tls::Identity,
    tcp: TcpStream,
    addr: SocketAddr,
) -> anyhow::Result<(tokio_rustls::client::TlsStream<TcpStream>, Vec<u8>)> {
    let peer = crate::tls::PeerCert::new();
    let config = identity.client_config(peer.clone())?;
    let name = rustls::pki_types::ServerName::try_from("csc.local")
        .map_err(|e| anyhow::anyhow!("server name: {e}"))?;
    let connector = tokio_rustls::TlsConnector::from(Arc::new(config));
    let stream = tokio::time::timeout(Duration::from_secs(5), connector.connect(name, tcp))
        .await
        .map_err(|_| anyhow::anyhow!("tls handshake with {addr} timed out"))??;
    let der = peer.take()?;
    Ok((stream, der))
}

async fn accept_tls(
    identity: &crate::tls::Identity,
    tcp: TcpStream,
) -> anyhow::Result<(tokio_rustls::server::TlsStream<TcpStream>, Vec<u8>)> {
    let peer = crate::tls::PeerCert::new();
    let config = identity.server_config(peer.clone())?;
    let stream = tokio::time::timeout(
        Duration::from_secs(5),
        TlsAcceptor::from(Arc::new(config)).accept(tcp),
    )
    .await
    .map_err(|_| anyhow::anyhow!("tls handshake timed out"))??;
    let der = peer.take()?;
    Ok((stream, der))
}

struct ClientLink {
    peers: PeerTable,
    status: Arc<RwLock<NetStatus>>,
    key: String,
    tx: Link,
    /// Set once the session is in the peer table.
    registered: Arc<AtomicBool>,
}

struct Admit {
    app_status: Arc<RwLock<AppStatus>>,
    prompt: Arc<PeerPrompt>,
    live: Arc<RwLock<Config>>,
    config_path: PathBuf,
    rejected: Arc<Mutex<HashMap<String, RejectMark>>>,
    address: String,
}

struct AbortOnDrop(tokio::task::JoinHandle<()>);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// Runs one control session. The caller reports `Disconnected` (see `report_gone`),
/// whether the peer closed, reset, timed out or sent garbage.
#[allow(clippy::too_many_arguments)]
async fn session<S>(
    cfg: &Config,
    live: &Arc<RwLock<Config>>,
    config_path: &Path,
    stream: S,
    peer_cert: Vec<u8>,
    status: &Arc<RwLock<NetStatus>>,
    app_status: &Arc<RwLock<AppStatus>>,
    prompt: &Arc<PeerPrompt>,
    mut peer: String,
    mut outbound: LinkRx,
    mut inbound: Wires,
    link: Option<ClientLink>,
    admit: Option<Admit>,
    remote_ip: std::net::IpAddr,
    identity: &Arc<crate::tls::Identity>,
    data_token: [u8; 32],
) -> anyhow::Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let peer_fp = crate::tls::fingerprint(&peer_cert);
    let _data_reg = DataReg::insert(DataSlot {
        token: data_token,
        peer: peer.clone(),
        wires: inbound.clone(),
        is_host: admit.is_none(),
        status: status.clone(),
        bulk: std::sync::Mutex::new(None),
        fingerprint: peer_fp.clone(),
    });
    let is_host = admit.is_none();
    let (mut reader, writer) = tokio::io::split(stream);
    let writer = Arc::new(Mutex::new(writer));

    let size = csc_platform::create_screen_info().virtual_size();
    let hello = HelloPayload {
        protocol_version: 2,
        host_name: cfg.name.clone(),
        role: match cfg.role {
            Role::Server => "server".into(),
            Role::Client => "client".into(),
        },
        app_version: env!("CARGO_PKG_VERSION").into(),
        device_id: cfg.device_id.clone(),
        screen_width: size.width,
        screen_height: size.height,
        capabilities: local_capabilities(),
        ports: csc_proto::HelloPorts {
            control: cfg.control_port,
            data: cfg.data_port,
        },
        ..HelloPayload::default()
    };
    let remember = |remote: &HelloPayload| {
        if let Ok(mut info) = outbound.info.lock() {
            info.host_name = remote.host_name.clone();
            info.capabilities = remote.capabilities;
        }
    };
    // The keyboard host speaks first. The controlled computer answers only after trust,
    // so a cancelled prompt never pins a certificate on the host.
    if admit.is_none() {
        // Flush. A TLS write left in the buffer is invisible to the other side,
        // and both then wait until this 10s hello limit and drop the link.
        write_locked(&writer, &encode_hello_bin(&hello)).await?;
    }

    let mut buf = BytesMut::with_capacity(64 * 1024);
    let peer_data_port;
    if let Some(admit) = admit {
        let (remote, _early) = match tokio::time::timeout(
            Duration::from_secs(5),
            read_until_hello(&mut reader, &mut buf),
        )
        .await
        {
            Ok(Ok(found)) => found,
            Ok(Err(e)) => {
                warn!("control request from {}: {e:#}", admit.address);
                return Ok(());
            }
            Err(_) => {
                warn!("control request from {} timed out", admit.address);
                return Ok(());
            }
        };
        // Keep reading while the user decides, so the host's send buffer cannot fill.
        // Heartbeats tell the host this computer is still here and only waiting.
        peer_data_port = remote.ports.data;
        if both_hosts(cfg.role, &remote.role) {
            let who = if remote.host_name.is_empty() {
                admit.address.clone()
            } else {
                remote.host_name.clone()
            };
            if offer_ready(app_status).await {
                // We typed a code — stay host; the other side should demote.
                note_role_conflict(app_status, &who).await;
                return Ok(());
            }
            // Peer is dialing as host (entered our code). Become client and retry.
            demote_to_client(
                &admit.live,
                &admit.config_path,
                app_status,
                &inbound.net_reload,
                &who,
            )
            .await;
            return Ok(());
        }
        let announced = remote.clone();
        let name = if remote.host_name.is_empty() {
            admit.address.clone()
        } else {
            remote.host_name.clone()
        };
        let trusted = {
            let saved = admit.live.read().await;
            saved.certificate_trusted(&name, &peer_fp)
                || saved.certificate_trusted(&remote.host_name, &peer_fp)
        };
        if trusted {
            match allow_control(&admit, &remote, &peer_fp, None).await {
                Gate::Ok => {}
                Gate::Rejected | Gate::Mismatch => return Ok(()),
            }
        } else if !pairing_window_open(app_status).await {
            warn!("pairing window is closed on this computer");
            return Ok(());
        } else {
            let who = if remote.host_name.is_empty() {
                name.clone()
            } else {
                remote.host_name.clone()
            };
            set_peer_note(app_status, format!("{who} 正在尝试连接…")).await;
            match client_pair(
                &mut reader,
                &writer,
                &mut buf,
                app_status,
                prompt,
                &cfg.name,
                &cfg.device_id,
                &remote.device_id,
                &who,
                &admit.address,
                &data_token,
            )
            .await
            {
                Ok(()) => {
                    let mut saved = admit.live.write().await;
                    saved.remember_peer(&name, &peer_fp);
                    let desk = saved.ensure_trusted_screen_places();
                    if let Err(e) = saved.save(&admit.config_path) {
                        warn!("save trusted peer: {e:#}");
                    }
                    if desk {
                        inbound
                            .layout_reload
                            .send_modify(|n| *n = n.wrapping_add(1));
                    }
                }
                Err(e) => {
                    let text = format!("{e:#}");
                    if text.contains("no_pair_code") {
                        debug!("pairing with {} is waiting for a code", admit.address);
                    } else {
                        info!("pairing with {} failed: {text}", admit.address);
                    }
                    return Ok(());
                }
            }
        }
        write_locked(&writer, &encode_hello_bin(&hello)).await?;
        remember(&announced);
        // Point last_peers at the hostname and the advertised control port.
        {
            let mut last = inbound.last_peers.lock().await;
            if !name.eq_ignore_ascii_case(&peer) {
                last.remove(&peer);
            }
            remember_control_addr(
                &mut last,
                &name,
                remote_ip,
                announced.ports.control,
                cfg.control_port,
            );
        }
        let _ = inbound.events.send(InboundEvent::Hello(announced)).await;
    } else {
        // The controlled computer sends heartbeats while its user decides, so
        // silence means the connection is dead, not that someone is thinking.
        let (remote, early) = match host_wait_hello(
            &mut reader,
            &writer,
            &mut buf,
            app_status,
            &cfg.device_id,
            &data_token,
            prompt,
            &peer,
        )
        .await
        {
            Ok(found) => found,
            Err(e) => {
                let text = format!("{e:#}");
                if text.contains("no_pair_code") || text.contains("waiting_for_offer") {
                    let note = format!(
                        "{peer} 还没有信任本机。请在下方「添加对方配对码」输入 {peer} 首页上的六位配对码。"
                    );
                    let mut s = app_status.write().await;
                    if s.peer_note != note {
                        info!("client {peer} does not trust this host; enter its pairing code");
                        s.peer_note = note;
                    }
                } else {
                    info!("client {peer} did not accept control: {text}");
                    if offer_ready(app_status).await
                        || app_status.read().await.peer_note.contains("正在连接")
                    {
                        if text.contains("timed out") || text.contains("no data") {
                            fail_offer(app_status, "连接超时，请重新输入配对码").await;
                        } else if text.contains("cancelled") || text.contains("pairing cancelled") {
                            // host_wait_hello already set 对方取消了这次配对
                        } else if text.contains("pairing rejected") {
                            // already set 配对码不对 / 已取消这次配对
                        } else if text.contains("pairing refused") {
                            fail_offer(app_status, "对方拒绝了这次连接").await;
                        } else {
                            fail_offer(app_status, "连接失败，请重新输入配对码").await;
                        }
                    }
                }
                return Ok(());
            }
        };
        peer_data_port = remote.ports.data;
        if both_hosts(cfg.role, &remote.role) {
            let who = if remote.host_name.is_empty() {
                peer.clone()
            } else {
                remote.host_name.clone()
            };
            if offer_ready(app_status).await {
                // We typed their code — keep hosting; they should auto-demote and
                // accept on the next dial.
                info!("{who} is still a host; waiting for it to switch to client");
                app_status.write().await.peer_note = format!("{who} 正在切换为从机…");
                return Ok(());
            }
            // Another host dialed us (or we crossed). Become client and retry.
            demote_to_client(live, config_path, app_status, &inbound.net_reload, &who).await;
            return Ok(());
        }
        remember(&remote);
        let name = if remote.host_name.is_empty() {
            peer.clone()
        } else {
            remote.host_name.clone()
        };
        {
            let mut saved = live.write().await;
            if let Err(msg) = saved.pin_peer(&name, &peer_fp) {
                warn!("{msg}");
                return Ok(());
            }
            let desk = saved.ensure_trusted_screen_places();
            if let Err(e) = saved.save(config_path) {
                warn!("save peer certificate: {e:#}");
            }
            if desk {
                inbound
                    .layout_reload
                    .send_modify(|n| *n = n.wrapping_add(1));
            }
        }
        // Offer dials may use the IP as the session key; layout and pointer
        // switch need the hostname. Rekey once HELLO names the peer. If a
        // hostname session is already live, drop this duplicate dial.
        if !name.eq_ignore_ascii_case(&peer) {
            if !inbound.peers.rekey(&peer, &name).await {
                info!("session {name} already live; dropping duplicate dial via {peer}");
                {
                    let mut last = inbound.last_peers.lock().await;
                    last.remove(&peer);
                    remember_control_addr(
                        &mut last,
                        &name,
                        remote_ip,
                        remote.ports.control,
                        cfg.control_port,
                    );
                } // last_peers dropped before clear_offer .await
                clear_offer(app_status).await;
                return Ok(());
            }
            {
                let mut last = inbound.last_peers.lock().await;
                last.remove(&peer);
                remember_control_addr(
                    &mut last,
                    &name,
                    remote_ip,
                    remote.ports.control,
                    cfg.control_port,
                );
            }
            info!("session {peer} is now {name}");
            peer = name.clone();
        } else {
            let mut last = inbound.last_peers.lock().await;
            remember_control_addr(
                &mut last,
                &name,
                remote_ip,
                remote.ports.control,
                cfg.control_port,
            );
        }
        // Connected — stop offer redials that would open a second link.
        clear_offer(app_status).await;
        for frame in early {
            dispatch_frame(frame, &name, status, &mut inbound, is_host).await?;
        }
        let _ = inbound.events.send(InboundEvent::Hello(remote)).await;
    }

    if is_host && peer_data_port != cfg.control_port {
        let addr = SocketAddr::new(remote_ip, peer_data_port);
        let identity = identity.clone();
        let token = data_token;
        let who = peer_fp.clone();
        tokio::spawn(async move {
            if let Err(e) = dial_data(&identity, addr, &token, &who).await {
                warn!("data connection to {addr}: {e:#}");
            }
        });
    }

    if let Some(link) = link {
        if !link
            .peers
            .adopt(&link.key, link.tx.clone(), DUPLICATE_LINK)
            .await
        {
            debug!("already linked to {}; dropping extra connection", link.key);
            return Ok(());
        }
        link.registered.store(true, Ordering::Release);
        refresh_link_status(&link.status, &link.peers).await;
    }

    let hb_writer = writer.clone();
    // Always probe, even if the user turned the setting off: a black-holed
    // socket can still accept tiny writes into the kernel buffer, so only a
    // missing reply tells us the pointer must come back.
    let probe_ms = if cfg.network.heartbeat_enabled {
        (cfg.network.heartbeat_ms as u64).clamp(200, 500)
    } else {
        500
    };
    let (write_fail_tx, mut write_fail_rx) = watch::channel(false);
    let hb_fail = write_fail_tx.clone();
    let _heartbeat = AbortOnDrop(tokio::spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_millis(probe_ms));
        loop {
            tick.tick().await;
            let frame = encode_frame(MsgType::Heartbeat, &[], 0);
            if write_locked(&hb_writer, &frame).await.is_err() {
                let _ = hb_fail.send(true);
                break;
            }
        }
    }));
    // Two missed probes (~1 s) is long enough to ignore a brief hitch and
    // short enough that a dead link cannot hold the pointer.
    let idle_limit = Duration::from_millis(probe_ms * 2);

    let mut last_rx = tokio::time::Instant::now();
    // One transfer stays on one connection: data port once it is up, otherwise control.
    let mut held_bulk: Option<Vec<u8>> = None;
    let mut bulk_on_control = false;
    let mut bulk_on_data = false;
    let bulk_deadline = tokio::time::Instant::now() + Duration::from_secs(2);

    let mut tmp = [0u8; 16 * 1024];
    loop {
        tokio::select! {
            // Input frames go out before queued file data.
            biased;
            read = reader.read(&mut tmp) => {
                let n = read?;
                if n == 0 {
                    break;
                }
                last_rx = tokio::time::Instant::now();
                buf.extend_from_slice(&tmp[..n]);
                loop {
                    match try_decode_frame(&mut buf)? {
                        None => break,
                        Some(frame) => {
                            if let Some(reply) = bench_answer(&frame) {
                                if write_locked(&writer, &reply).await.is_err() {
                                    break;
                                }
                            } else {
                                dispatch_frame(frame, &peer, status, &mut inbound, is_host).await?;
                            }
                        }
                    }
                }
            }
            _ = outbound.kill.changed() => {
                anyhow::bail!("replaced by a newer session");
            }
            _ = write_fail_rx.changed() => {
                if *write_fail_rx.borrow() {
                    anyhow::bail!("control write failed");
                }
            }
            frame = outbound.rx.recv() => {
                match frame {
                    Some(frame) => {
                        let frame = if rides_data(&frame) && !bulk_on_control {
                            match send_on_data(&peer_fp, frame) {
                                Ok(()) => continue,
                                Err(frame) => frame,
                            }
                        } else {
                            frame
                        };
                        if write_locked(&writer, &frame).await.is_err() {
                            break;
                        }
                    }
                    None => break,
                }
            }
            Some(frame) = outbound.bulk.recv(), if held_bulk.is_none() => {
                if bulk_on_control {
                    if write_locked(&writer, &frame).await.is_err() {
                        break;
                    }
                } else if bulk_on_data {
                    if send_on_data(&peer_fp, frame).is_err() {
                        break;
                    }
                } else {
                    held_bulk = Some(frame);
                }
            }
            _ = tokio::time::sleep(Duration::from_millis(40)), if held_bulk.is_some() => {
                let frame = held_bulk.take().unwrap_or_default();
                match send_on_data(&peer_fp, frame) {
                    Ok(()) => bulk_on_data = true,
                    Err(frame) => {
                        if tokio::time::Instant::now() >= bulk_deadline {
                            bulk_on_control = true;
                            if write_locked(&writer, &frame).await.is_err() {
                                break;
                            }
                        } else {
                            held_bulk = Some(frame);
                        }
                    }
                }
            }
            _ = tokio::time::sleep_until(last_rx + idle_limit) => {
                anyhow::bail!("no data from {peer} for {}s", idle_limit.as_secs());
            }
        }
    }
    Ok(())
}

/// What this build can do here. Cross-screen virtual drag is retired.
fn local_capabilities() -> u32 {
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        csc_proto::CAPABILITIES | csc_proto::CAP_CLIP_PLACEHOLDER
    }
    #[cfg(not(all(unix, not(target_os = "macos"))))]
    {
        csc_proto::CAPABILITIES
    }
}

/// A second TCP opened this soon is the other computer dialing at the same time.
/// Keep the session that is already up.
const DUPLICATE_LINK: Duration = Duration::from_secs(3);
/// Controlled computer: heartbeat interval while its trust prompt is open.
const HELLO_KEEPALIVE: Duration = Duration::from_secs(2);
/// Keyboard host: give up on a controlled computer silent this long before its HELLO.
const HELLO_IDLE: Duration = Duration::from_secs(10);

async fn read_until_hello<R>(
    reader: &mut R,
    buf: &mut BytesMut,
) -> anyhow::Result<(HelloPayload, Vec<csc_proto::Frame>)>
where
    R: AsyncRead + Unpin,
{
    read_until_hello_idle(reader, buf, Duration::MAX).await
}

/// Read up to HELLO, failing when nothing at all arrives for `idle`.
/// Heartbeats before HELLO only prove the other side is alive.
async fn read_until_hello_idle<R>(
    reader: &mut R,
    buf: &mut BytesMut,
    idle: Duration,
) -> anyhow::Result<(HelloPayload, Vec<csc_proto::Frame>)>
where
    R: AsyncRead + Unpin,
{
    let mut early = Vec::new();
    let mut tmp = [0u8; 4096];
    loop {
        loop {
            match try_decode_frame(buf)? {
                None => break,
                Some(frame) if frame.msg_type == MsgType::Hello => {
                    let hello = decode_hello_bin(&frame.payload)
                        .ok_or_else(|| anyhow::anyhow!("bad hello"))?;
                    return Ok((hello, early));
                }
                Some(frame) if frame.msg_type == MsgType::Heartbeat => {}
                Some(frame) => early.push(frame),
            }
        }
        let n = if idle == Duration::MAX {
            reader.read(&mut tmp).await?
        } else {
            tokio::time::timeout(idle, reader.read(&mut tmp))
                .await
                .map_err(|_| anyhow::anyhow!("no data for {}s", idle.as_secs()))??
        };
        if n == 0 {
            anyhow::bail!("closed before hello");
        }
        buf.extend_from_slice(&tmp[..n]);
    }
}

enum Gate {
    Ok,
    Rejected,
    Mismatch,
}

fn udp_port(cfg: &Config) -> u16 {
    if cfg.network.discovery_port == 0 {
        cfg.control_port
    } else {
        cfg.network.discovery_port
    }
}

async fn pairing_window_open(app_status: &Arc<RwLock<AppStatus>>) -> bool {
    let s = app_status.read().await;
    // 0 = always open (stable home pairing code). Non-zero is a timed window.
    if s.pairing_open_until == 0 {
        return !s.home_code.is_empty();
    }
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    s.pairing_open_until > now
}

fn random_nonce() -> [u8; 32] {
    let mut buf = [0u8; 32];
    let _ = getrandom::getrandom(&mut buf);
    buf
}

/// Floor for a stalled write. Grows with payload so a clipboard image
/// on a slow link is not cut off; mouse and heartbeats stay at the floor.
fn write_budget(len: usize) -> Duration {
    Duration::from_millis(300 + len as u64 / 1024)
}

async fn write_locked<W>(writer: &Mutex<W>, frame: &[u8]) -> anyhow::Result<()>
where
    W: AsyncWrite + Unpin,
{
    let mut w = writer.lock().await;
    let budget = write_budget(frame.len());
    tokio::time::timeout(budget, async {
        w.write_all(frame).await?;
        w.flush().await?;
        Ok::<(), std::io::Error>(())
    })
    .await
    .map_err(|_| anyhow::anyhow!("control write timed out"))??;
    Ok(())
}

async fn write_timeout<S>(stream: &mut S, frame: &[u8]) -> anyhow::Result<()>
where
    S: AsyncWrite + Unpin,
{
    let budget = write_budget(frame.len());
    tokio::time::timeout(budget, async {
        stream.write_all(frame).await?;
        stream.flush().await?;
        Ok::<(), std::io::Error>(())
    })
    .await
    .map_err(|_| anyhow::anyhow!("data write timed out"))??;
    Ok(())
}

/// Read one frame, skipping heartbeats. `idle` with no bytes at all is a failure.
async fn read_typed<R>(
    reader: &mut R,
    buf: &mut BytesMut,
    idle: Duration,
    want: MsgType,
) -> anyhow::Result<csc_proto::Frame>
where
    R: AsyncRead + Unpin,
{
    let mut tmp = [0u8; 4096];
    let mut last = tokio::time::Instant::now();
    loop {
        if let Some(frame) = try_decode_frame(buf)? {
            if frame.msg_type == MsgType::Heartbeat {
                last = tokio::time::Instant::now();
                continue;
            }
            if frame.msg_type == want {
                return Ok(frame);
            }
            anyhow::bail!("expected {want:?}, got {:?}", frame.msg_type);
        }
        let left = idle.saturating_sub(last.elapsed());
        if left.is_zero() {
            anyhow::bail!("pairing timed out");
        }
        let n = tokio::time::timeout(left, reader.read(&mut tmp))
            .await
            .map_err(|_| anyhow::anyhow!("pairing timed out"))??;
        if n == 0 {
            anyhow::bail!("closed during pairing");
        }
        last = tokio::time::Instant::now();
        buf.extend_from_slice(&tmp[..n]);
    }
}

async fn offer_ready(status: &Arc<RwLock<AppStatus>>) -> bool {
    let s = status.read().await;
    s.offer_until > unix_now() && s.offer_code.len() == 6
}

async fn current_offer(status: &Arc<RwLock<AppStatus>>) -> String {
    if offer_ready(status).await {
        status.read().await.offer_code.clone()
    } else {
        String::new()
    }
}

async fn clear_offer(status: &Arc<RwLock<AppStatus>>) {
    debug!("clear_offer");
    let mut s = status.write().await;
    s.offer_code.clear();
    s.offer_until = 0;
}

async fn home_hit(status: &Arc<RwLock<AppStatus>>, typed: &str) -> bool {
    let s = status.read().await;
    let hit = |shown: &str| shown.len() == 6 && code_matches(typed, shown);
    hit(&s.home_code) || hit(&s.home_code_prev) || hit(&s.home_code_inflight)
}

/// Keep the socket moving while the user clicks confirm.
async fn wait_choice<R, W>(
    reader: &mut R,
    writer: &Mutex<W>,
    buf: &mut BytesMut,
    choice: impl std::future::Future<Output = Choice>,
) -> anyhow::Result<Choice>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let mut choice = std::pin::pin!(choice);
    let mut discard = [0u8; 4096];
    let mut waiting = tokio::time::interval(HELLO_KEEPALIVE);
    loop {
        while try_decode_frame(buf)?.is_some() {}
        tokio::select! {
            biased;
            picked = &mut choice => return Ok(picked),
            _ = waiting.tick() => {
                write_locked(writer, &encode_frame(MsgType::Heartbeat, &[], 0)).await?;
            }
            read = reader.read(&mut discard) => {
                let n = read?;
                if n == 0 {
                    anyhow::bail!("closed during pairing");
                }
                buf.extend_from_slice(&discard[..n]);
            }
        }
    }
}

/// Send the code this computer typed. The computer that owns the code must click confirm.
#[allow(clippy::too_many_arguments)]
async fn client_pair<R, W>(
    reader: &mut R,
    writer: &Mutex<W>,
    buf: &mut BytesMut,
    app_status: &Arc<RwLock<AppStatus>>,
    prompt: &PeerPrompt,
    local_name: &str,
    local_device: &str,
    host_device_id: &str,
    host_name: &str,
    address: &str,
    exporter: &[u8; 32],
) -> anyhow::Result<()>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let offer = current_offer(app_status).await;
    let req = csc_proto::PairRequestPayload {
        device_id: local_device.to_string(),
        name: local_name.to_string(),
        role: "client".into(),
        protocol_version: 2,
        code: offer.clone(),
    };
    write_locked(writer, &encode_json_frame(MsgType::PairRequest, &req)?).await?;
    let commit_frame = read_typed(reader, buf, HELLO_IDLE, MsgType::PairCommit).await?;
    let commit: csc_proto::PairCommitPayload = serde_json::from_slice(&commit_frame.payload)?;
    let nb = random_nonce();
    let nonce = csc_proto::PairNoncePayload {
        nonce: hex_encode(&nb),
    };
    write_locked(writer, &encode_json_frame(MsgType::PairNonce, &nonce)?).await?;
    let na_frame = read_typed(reader, buf, HELLO_IDLE, MsgType::PairNonce).await?;
    let na_msg: csc_proto::PairNoncePayload = serde_json::from_slice(&na_frame.payload)?;
    let na = unhex(&na_msg.nonce).unwrap_or_default();
    let got = unhex(&commit.commit).unwrap_or_default();
    if !csc_proto::pair_commit_matches(&got, &na, host_device_id) {
        anyhow::bail!("bad_commit");
    }
    let _sas = csc_proto::pair_code(exporter, &na, &nb);
    debug!(
        commit_code_len = commit.code.len(),
        local_offer_len = offer.len(),
        host = %host_name,
        "client_pair: got PairCommit"
    );
    // Host typed OUR home code → show confirm. Empty commit means host has not
    // typed yet (should not happen once host waits for offer). Never sit 1.5s
    // waiting for a local offer and then deny — that raced the initiator.
    let typed_ours = home_hit(app_status, &commit.code).await;
    let confirm = if typed_ours {
        info!(
            host = %host_name,
            "client_pair: PairCommit matches this computer’s home code — confirm"
        );
        let choice = wait_choice(
            reader,
            writer,
            buf,
            prompt.ask(app_status, host_name, host_name, address, true),
        )
        .await?;
        let accepted = matches!(choice, Choice::Trust { .. });
        csc_proto::PairConfirmPayload {
            accepted,
            code: if accepted {
                String::new()
            } else {
                "cancel".into()
            },
        }
    } else if commit.code.is_empty() {
        warn!(
            host = %host_name,
            "client_pair: PairCommit has empty code — host must enter our pairing code"
        );
        let deny = csc_proto::PairConfirmPayload {
            accepted: false,
            code: String::new(),
        };
        write_locked(writer, &encode_json_frame(MsgType::PairConfirm, &deny)?).await?;
        set_peer_note(app_status, "配对码不对").await;
        anyhow::bail!("no_pair_code");
    } else if offer.len() == 6 {
        // We also typed a code (rare both-sides case): accept with our offer.
        debug!("client_pair: using local offer as PairConfirm code");
        csc_proto::PairConfirmPayload {
            accepted: true,
            code: offer,
        }
    } else {
        warn!(
            commit = %commit.code,
            host = %host_name,
            "client_pair: PairCommit code is not this computer’s home code"
        );
        let deny = csc_proto::PairConfirmPayload {
            accepted: false,
            code: String::new(),
        };
        write_locked(writer, &encode_json_frame(MsgType::PairConfirm, &deny)?).await?;
        set_peer_note(app_status, "配对码不对").await;
        anyhow::bail!("no_pair_code");
    };
    write_locked(writer, &encode_json_frame(MsgType::PairConfirm, &confirm)?).await?;
    if !confirm.accepted {
        if typed_ours {
            set_peer_note(app_status, "已取消这次配对").await;
        } else {
            set_peer_note(app_status, "配对码不对").await;
        }
        anyhow::bail!("pairing cancelled");
    }
    let answer = read_typed(reader, buf, Duration::from_secs(120), MsgType::PairResult).await?;
    let result: csc_proto::PairResultPayload = serde_json::from_slice(&answer.payload)?;
    if result.status != "ok" {
        clear_offer(app_status).await;
        set_peer_note(app_status, "对方没有确认这次配对").await;
        anyhow::bail!("pairing rejected");
    }
    clear_offer(app_status).await;
    if typed_ours {
        burn_home_code(app_status).await;
    }
    Ok(())
}

/// The peer just paired with this computer's code: replace it so it works only once.
async fn burn_home_code(status: &Arc<RwLock<AppStatus>>) {
    crate::burn_home_code(&mut *status.write().await);
    info!("pairing code used once; a new one is shown");
}

async fn begin_pair_attempt(status: &Arc<RwLock<AppStatus>>) {
    crate::begin_pair_attempt(&mut *status.write().await);
}

async fn finish_pair_attempt(status: &Arc<RwLock<AppStatus>>) {
    crate::finish_pair_attempt(&mut *status.write().await);
    info!("pairing attempt finished; home code rotated if this computer was targeted");
}

#[allow(clippy::too_many_arguments)]
async fn host_wait_hello<R, W>(
    reader: &mut R,
    writer: &Mutex<W>,
    buf: &mut BytesMut,
    app_status: &Arc<RwLock<AppStatus>>,
    device_id: &str,
    exporter: &[u8; 32],
    prompt: &PeerPrompt,
    peer_label: &str,
) -> anyhow::Result<(HelloPayload, Vec<csc_proto::Frame>)>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let mut early = Vec::new();
    let mut tmp = [0u8; 4096];
    let mut last = tokio::time::Instant::now();
    let result = async {
        loop {
            if let Some(frame) = try_decode_frame(buf)? {
                last = tokio::time::Instant::now();
                if frame.msg_type == MsgType::Heartbeat {
                    continue;
                }
                if frame.msg_type == MsgType::PairRequest {
                    if !pairing_window_open(app_status).await {
                        warn!("host_wait_hello: pairing window closed — reject PairRequest");
                        let result = csc_proto::PairResultPayload {
                            status: "rejected".into(),
                        };
                        write_locked(writer, &encode_json_frame(MsgType::PairResult, &result)?)
                            .await?;
                        anyhow::bail!("pairing window closed");
                    }
                    let ask: csc_proto::PairRequestPayload = serde_json::from_slice(&frame.payload)
                        .unwrap_or(csc_proto::PairRequestPayload {
                            device_id: String::new(),
                            name: String::new(),
                            role: "client".into(),
                            protocol_version: 2,
                            code: String::new(),
                        });
                    debug!(
                        peer = %peer_label,
                        ask_code_len = ask.code.len(),
                        "host_wait_hello: PairRequest"
                    );
                    // Whoever types the 6-digit code is the initiator and must
                    // Dial a fresh control session. Discovery PairRequest must
                    // not sit here waiting (that occupied the hostname slot,
                    // dropped peer dial-backs, and sent PairCommit on a dying
                    // discovery socket). Reject immediately when no offer is
                    // ready; the offer dial path owns PairCommit.
                    let offer = current_offer(app_status).await;
                    if offer.len() != 6 {
                        debug!(
                            peer = %peer_label,
                            "host_wait_hello: no typed offer — reject discovery PairRequest (initiator must Dial)"
                        );
                        let result = csc_proto::PairResultPayload {
                            status: "rejected".into(),
                        };
                        write_locked(writer, &encode_json_frame(MsgType::PairResult, &result)?)
                            .await?;
                        anyhow::bail!("waiting_for_offer");
                    }
                    info!(
                        peer = %peer_label,
                        "host_wait_hello: offer already present — PairCommit on this session"
                    );
                    let na = random_nonce();
                    let commit = csc_proto::PairCommitPayload {
                        commit: hex_encode(&csc_proto::pair_commit(&na, device_id)),
                        code: offer.clone(),
                    };
                    write_locked(writer, &encode_json_frame(MsgType::PairCommit, &commit)?).await?;
                    let nb_frame = read_typed(reader, buf, HELLO_IDLE, MsgType::PairNonce).await?;
                    let nb_msg: csc_proto::PairNoncePayload =
                        serde_json::from_slice(&nb_frame.payload)?;
                    let nb = unhex(&nb_msg.nonce).unwrap_or_default();
                    let reveal = csc_proto::PairNoncePayload {
                        nonce: hex_encode(&na),
                    };
                    write_locked(writer, &encode_json_frame(MsgType::PairNonce, &reveal)?).await?;
                    let _sas = csc_proto::pair_code(exporter, &na, &nb);
                    let confirm =
                        read_typed(reader, buf, Duration::from_secs(120), MsgType::PairConfirm)
                            .await?;
                    let ok: csc_proto::PairConfirmPayload =
                        serde_json::from_slice(&confirm.payload)?;
                    if ok.code == "cancel" {
                        clear_offer(app_status).await;
                        app_status.write().await.peer_note = "对方取消了这次配对".into();
                        let result = csc_proto::PairResultPayload {
                            status: "rejected".into(),
                        };
                        write_locked(writer, &encode_json_frame(MsgType::PairResult, &result)?)
                            .await?;
                        anyhow::bail!("pairing cancelled");
                    }
                    if !ok.accepted {
                        warn!(
                            peer = %peer_label,
                            "host_wait_hello: peer PairConfirm accepted=false (no_pair_code)"
                        );
                        anyhow::bail!("no_pair_code");
                    }
                    let typed_ours = home_hit(app_status, &ok.code).await
                        || home_hit(app_status, &ask.code).await;
                    let confirmed_ours =
                        !offer.is_empty() && (ok.code.is_empty() || code_matches(&ok.code, &offer));
                    if typed_ours {
                        begin_pair_attempt(app_status).await;
                    }
                    let choice_result = if typed_ours {
                        let who = if ask.name.is_empty() {
                            peer_label.to_string()
                        } else {
                            ask.name.clone()
                        };
                        wait_choice(
                            reader,
                            writer,
                            buf,
                            prompt.ask(app_status, &who, &who, peer_label, true),
                        )
                        .await
                    } else {
                        Ok(Choice::Reject)
                    };
                    let accept = if typed_ours {
                        match &choice_result {
                            Ok(choice) => matches!(choice, Choice::Trust { .. }),
                            Err(_) => false,
                        }
                    } else {
                        confirmed_ours
                    };
                    let result = csc_proto::PairResultPayload {
                        status: if accept {
                            "ok".into()
                        } else {
                            "rejected".into()
                        },
                    };
                    write_locked(writer, &encode_json_frame(MsgType::PairResult, &result)?).await?;
                    if !accept {
                        if typed_ours {
                            app_status.write().await.peer_note = "已取消这次配对".into();
                        } else {
                            clear_offer(app_status).await;
                            app_status.write().await.peer_note = "配对码不对".into();
                        }
                        if typed_ours {
                            finish_pair_attempt(app_status).await;
                        }
                        choice_result?;
                        anyhow::bail!("pairing rejected");
                    }
                    clear_offer(app_status).await;
                    if typed_ours {
                        burn_home_code(app_status).await;
                        finish_pair_attempt(app_status).await;
                    }
                    last = tokio::time::Instant::now();
                    continue;
                }
                if frame.msg_type == MsgType::PairResult {
                    anyhow::bail!("pairing refused");
                }
                if frame.msg_type == MsgType::Hello {
                    let hello = decode_hello_bin(&frame.payload)
                        .ok_or_else(|| anyhow::anyhow!("bad hello"))?;
                    return Ok((hello, early));
                }
                early.push(frame);
                continue;
            }
            let left = HELLO_IDLE.saturating_sub(last.elapsed());
            if left.is_zero() {
                anyhow::bail!("no data for {}s", HELLO_IDLE.as_secs());
            }
            let n = tokio::time::timeout(left, reader.read(&mut tmp))
                .await
                .map_err(|_| anyhow::anyhow!("no data for {}s", HELLO_IDLE.as_secs()))??;
            if n == 0 {
                anyhow::bail!("closed before hello");
            }
            last = tokio::time::Instant::now();
            buf.extend_from_slice(&tmp[..n]);
        }
    }
    .await;
    result
}

/// Clear the displayed pairing code only if it is still the one this attempt showed.
#[cfg(test)]
fn release_code(slot: &mut String, mine: &str) {
    if !mine.is_empty() && slot == mine {
        slot.clear();
    }
}

async fn allow_control(
    admit: &Admit,
    hello: &HelloPayload,
    fingerprint: &str,
    expect_code: Option<String>,
) -> Gate {
    let name = if hello.host_name.is_empty() {
        admit.address.clone()
    } else {
        hello.host_name.clone()
    };
    {
        let mut rejected = admit.rejected.lock().await;
        if let Some(mark) = rejected.get_mut(&name) {
            if suppress_repeat(mark, Instant::now()) {
                info!("ignoring {name} after cancel");
                return Gate::Rejected;
            }
            rejected.remove(&name);
        }
    }
    let trusted = {
        let cfg = admit.live.read().await;
        cfg.certificate_trusted(&name, fingerprint)
            || cfg.certificate_trusted(&hello.host_name, fingerprint)
    };
    if trusted {
        info!("trusted keyboard host {name}");
        return Gate::Ok;
    }
    if admit.live.read().await.is_trusted_peer(&name) {
        info!("keyboard host {name} presented a new certificate; asking again");
    }
    info!("keyboard host {name} requested control");
    let mut misses = 0u8;
    loop {
        let choice = admit
            .prompt
            .ask(&admit.app_status, &name, &name, &admit.address, false)
            .await;
        match choice {
            Choice::Trust { code } => {
                let digits: String = code.chars().filter(|c| c.is_ascii_digit()).collect();
                if expect_code.is_some() && digits.len() != 6 {
                    misses += 1;
                    info!("pairing code from {name} was not six digits ({misses}/3)");
                    if misses >= 3 {
                        return Gate::Mismatch;
                    }
                    continue;
                }
                return Gate::Ok;
            }
            Choice::Reject => {
                info!("control from {name} cancelled");
                admit
                    .rejected
                    .lock()
                    .await
                    .insert(name, reject_mark(Instant::now()));
                return Gate::Rejected;
            }
        }
    }
}

fn code_matches(typed: &str, expect: &str) -> bool {
    let digits: String = typed.chars().filter(|c| c.is_ascii_digit()).collect();
    if digits.len() != expect.len() {
        return false;
    }
    ct_eq(digits.as_bytes(), expect.as_bytes())
}

static BENCH_ON: AtomicBool = AtomicBool::new(false);

pub fn set_bench_enabled(on: bool) {
    BENCH_ON.store(on, Ordering::Relaxed);
}

fn bench_answer(frame: &csc_proto::Frame) -> Option<Vec<u8>> {
    if frame.msg_type != MsgType::BenchSync || !BENCH_ON.load(Ordering::Relaxed) {
        return None;
    }
    let msg: csc_proto::BenchSyncPayload = serde_json::from_slice(&frame.payload).ok()?;
    if msg.t2.is_some() {
        return None;
    }
    let now = mono_ns();
    let reply = csc_proto::BenchSyncPayload {
        seq: msg.seq,
        t1: msg.t1,
        t2: Some(now),
        t3: Some(now),
    };
    encode_json_frame(MsgType::BenchSync, &reply).ok()
}

/// Write the same RTT under every live session key for `ip` (hostname + IP).
async fn set_peer_latency_for_ip(
    app_status: &Arc<RwLock<AppStatus>>,
    peers: &PeerTable,
    inbound: &Wires,
    ip: IpAddr,
    fallback: &str,
    ms: u32,
) {
    let mut keys = session_keys_for_ip(peers, inbound, ip).await;
    if keys.is_empty() {
        keys.push(fallback.to_string());
    }
    if !keys.iter().any(|k| k.eq_ignore_ascii_case(fallback)) {
        keys.push(fallback.to_string());
    }
    let ip_s = ip.to_string();
    if !keys.iter().any(|k| k == &ip_s) {
        keys.push(ip_s);
    }
    let mut s = app_status.write().await;
    for k in keys {
        s.peer_latency_ms.insert(k, ms);
    }
}

/// Outstanding discovery-channel RTT probes.
#[derive(Default)]
struct LatencyProbes {
    next: u64,
    /// Ping/Pong nonce → (sent, peer label).
    pending_ping: HashMap<u64, (Instant, String)>,
    /// Unicast Search → Announce RTT (works with older peers).
    pending_search: HashMap<IpAddr, (Instant, String)>,
}

impl LatencyProbes {
    fn begin_ping(&mut self, peer: String) -> u64 {
        self.pending_ping
            .retain(|_, (sent, _)| sent.elapsed() < Duration::from_secs(3));
        let nonce = self.next;
        self.next = self.next.wrapping_add(1);
        self.pending_ping.insert(nonce, (Instant::now(), peer));
        nonce
    }

    fn finish_ping(&mut self, nonce: u64) -> Option<(String, u32)> {
        let (sent, peer) = self.pending_ping.remove(&nonce)?;
        let ms = sent.elapsed().as_millis().min(u128::from(u32::MAX)) as u32;
        Some((peer, ms))
    }

    fn begin_search(&mut self, ip: IpAddr, peer: String) {
        self.pending_search
            .retain(|_, (sent, _)| sent.elapsed() < Duration::from_secs(3));
        self.pending_search.insert(ip, (Instant::now(), peer));
    }

    fn finish_search(&mut self, ip: IpAddr) -> Option<(String, u32)> {
        let (sent, peer) = self.pending_search.remove(&ip)?;
        let ms = sent.elapsed().as_millis().min(u128::from(u32::MAX)) as u32;
        Some((peer, ms))
    }
}

fn discovery_pong(cfg: &Config, nonce: u64) -> Vec<u8> {
    encode_pong(cfg.control_port, cfg.data_port, &cfg.name, nonce)
}

fn discovery_ping(cfg: &Config, nonce: u64) -> Vec<u8> {
    encode_ping(cfg.control_port, cfg.data_port, &cfg.name, nonce)
}

async fn latency_label_for_ip(
    peers: &PeerTable,
    inbound: &Wires,
    ip: IpAddr,
    fallback: &str,
) -> String {
    session_keys_for_ip(peers, inbound, ip)
        .await
        .into_iter()
        .find(|k| k.parse::<IpAddr>().is_err())
        .unwrap_or_else(|| fallback.to_string())
}

#[allow(clippy::too_many_arguments)]
async fn ping_linked_peers(
    udp: &UdpSocket,
    cfg: &Config,
    discover: u16,
    peers: &PeerTable,
    inbound: &Wires,
    probes: &mut LatencyProbes,
    udp_peers: &HashMap<IpAddr, SocketAddr>,
    search: &[u8],
) {
    let known: Vec<(String, SocketAddr)> = inbound
        .last_peers
        .lock()
        .await
        .iter()
        .map(|(k, a)| (k.clone(), *a))
        .collect();
    let mut seen_ip = HashSet::new();
    for (key, addr) in known {
        let linked =
            peers.contains(&key).await || already_linked_to_ip(peers, inbound, addr.ip()).await;
        if !linked || !seen_ip.insert(addr.ip()) {
            continue;
        }
        let label = latency_label_for_ip(peers, inbound, addr.ip(), &key).await;
        let dest = udp_peers
            .get(&addr.ip())
            .copied()
            .unwrap_or_else(|| SocketAddr::new(addr.ip(), discover));
        // Search↔Announce works with older peers that ignore Ping.
        probes.begin_search(addr.ip(), label.clone());
        let _ = udp.send_to(search, dest).await;
        let nonce = probes.begin_ping(label);
        let _ = udp.send_to(&discovery_ping(cfg, nonce), dest).await;
    }
}

fn mono_ns() -> u64 {
    static START: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
    START
        .get_or_init(Instant::now)
        .elapsed()
        .as_nanos()
        .min(u128::from(u64::MAX)) as u64
}

async fn dispatch_frame(
    frame: csc_proto::Frame,
    peer: &str,
    status: &Arc<RwLock<NetStatus>>,
    wires: &mut Wires,
    is_host: bool,
) -> anyhow::Result<()> {
    let role = if is_host {
        csc_proto::SessionRole::Host
    } else {
        csc_proto::SessionRole::Controlled
    };
    if matches!(frame.msg_type, MsgType::BenchSync | MsgType::BenchEcho)
        && !BENCH_ON.load(Ordering::Relaxed)
    {
        return Ok(());
    }
    if csc_proto::bad_direction(role, frame.msg_type) {
        anyhow::bail!("bad_direction {:?}", frame.msg_type);
    }
    let inbound = &wires.events;
    match frame.msg_type {
        // File frames go straight to the transfer engine, so a busy transfer
        // never queues up behind pointer input or the other way round.
        MsgType::FileOffer
        | MsgType::FileRequest
        | MsgType::FileBegin
        | MsgType::FileData
        | MsgType::FileBatch
        | MsgType::FileEnd
        | MsgType::FileDone
        | MsgType::FileError
        | MsgType::FileCancel
        | MsgType::FileAck
        | MsgType::DragCancel => {
            wires
                .transfer
                .frame(peer.to_string(), frame.msg_type, frame.payload)
                .await;
        }
        MsgType::ClipOffer
        | MsgType::ClipData
        | MsgType::EdgeHit
        | MsgType::PasteRequest
        | MsgType::PasteLocated
        | MsgType::FocusInfo
        | MsgType::PeerState
        | MsgType::ControlPause
        | MsgType::InputReset
        | MsgType::LockScreen
        | MsgType::Activity => {
            let _ = inbound
                .send(InboundEvent::From {
                    peer: peer.to_string(),
                    msg: frame.msg_type,
                    payload: frame.payload,
                })
                .await;
        }
        // Retired cross-screen virtual drag: decode so direction checks pass, then discard.
        MsgType::DragBegin | MsgType::DragRetarget => {}
        MsgType::Hello => {
            if let Some(h) = decode_hello_bin(&frame.payload) {
                {
                    let mut s = status.write().await;
                    s.connected = true;
                }
                let _ = inbound.send(InboundEvent::Hello(h)).await;
            }
        }
        MsgType::Heartbeat => {
            let _ = inbound.send(InboundEvent::Heartbeat).await;
        }
        MsgType::SwitchEnter | MsgType::SwitchLeave => {
            if frame.payload.len() >= 5 {
                let edge = match frame.payload[0] {
                    0 => csc_proto::Edge::Left,
                    1 => csc_proto::Edge::Right,
                    2 => csc_proto::Edge::Top,
                    3 => csc_proto::Edge::Bottom,
                    4 => csc_proto::Edge::TopLeft,
                    5 => csc_proto::Edge::TopRight,
                    6 => csc_proto::Edge::BottomLeft,
                    7 => csc_proto::Edge::BottomRight,
                    _ => return Ok(()),
                };
                let pct = f32::from_le_bytes(frame.payload[1..5].try_into()?);
                let locks = frame.payload.get(5).and_then(|flags| {
                    if flags & 0x80 == 0 {
                        return None;
                    }
                    Some(csc_proto::LockState {
                        caps: flags & 1 != 0,
                        num: flags & 2 != 0,
                        scroll: flags & 4 != 0,
                    })
                });
                let p = SwitchPayload {
                    edge,
                    position_pct: pct,
                    locks,
                };
                if frame.msg_type == MsgType::SwitchEnter {
                    let _ = inbound.send(InboundEvent::SwitchEnter(p)).await;
                } else {
                    let _ = inbound.send(InboundEvent::SwitchLeave(p)).await;
                }
            }
        }
        MsgType::MouseMove => {
            if frame.payload.len() >= 8 {
                let dx = i32::from_le_bytes(frame.payload[0..4].try_into()?);
                let dy = i32::from_le_bytes(frame.payload[4..8].try_into()?);
                let _ = inbound.send(InboundEvent::MouseMove { dx, dy }).await;
            }
        }
        MsgType::MouseButton => {
            if let Some(b) = csc_proto::decode_mouse_button_bin(&frame.payload) {
                let _ = inbound
                    .send(InboundEvent::MouseButton {
                        button: b.button,
                        down: b.down != 0,
                    })
                    .await;
            }
        }
        MsgType::MouseWheel => {
            if let Some(w) = csc_proto::decode_mouse_wheel_bin(&frame.payload) {
                let _ = inbound
                    .send(InboundEvent::MouseWheel {
                        v: w.wheel_v,
                        h: w.wheel_h,
                    })
                    .await;
            }
        }
        MsgType::Key => {
            if frame.payload.len() >= 5 {
                let hid = u32::from_le_bytes(frame.payload[0..4].try_into()?);
                let down = frame.payload[4] != 0;
                let _ = inbound.send(InboundEvent::Key { hid, down }).await;
            }
        }
        other => {
            let _ = inbound.send(InboundEvent::Raw(other, frame.payload)).await;
        }
    }
    Ok(())
}

struct DataSlot {
    fingerprint: String,
    token: [u8; 32],
    peer: String,
    wires: Wires,
    is_host: bool,
    status: Arc<RwLock<NetStatus>>,
    bulk: std::sync::Mutex<Option<mpsc::UnboundedSender<Vec<u8>>>>,
}

fn data_open() -> &'static std::sync::Mutex<HashMap<String, DataSlot>> {
    static CELL: std::sync::OnceLock<std::sync::Mutex<HashMap<String, DataSlot>>> =
        std::sync::OnceLock::new();
    CELL.get_or_init(|| std::sync::Mutex::new(HashMap::new()))
}

struct DataReg(String);

impl DataReg {
    fn insert(slot: DataSlot) -> Self {
        let fingerprint = slot.fingerprint.clone();
        data_open()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(fingerprint.clone(), slot);
        DataReg(fingerprint)
    }
}

/// The control connection keeps input, switching, and session upkeep. Clipboard,
/// paste, file, lock-screen and activity messages go on the data connection.
fn rides_data(frame: &[u8]) -> bool {
    let Some(kind) = frame.get(3).copied().and_then(MsgType::from_u8) else {
        return false;
    };
    matches!(
        kind,
        MsgType::ClipOffer
            | MsgType::ClipRequest
            | MsgType::ClipData
            | MsgType::PasteRequest
            | MsgType::PasteLocated
            | MsgType::ClipEnd
            | MsgType::DragBegin
            | MsgType::DragCancel
            | MsgType::DragRetarget
            | MsgType::FileRequest
            | MsgType::FileData
            | MsgType::FileEnd
            | MsgType::FileCancel
            | MsgType::FileError
            | MsgType::FileOffer
            | MsgType::FileBegin
            | MsgType::FileDone
            | MsgType::FileAck
            | MsgType::FileBatch
            | MsgType::LockScreen
            | MsgType::Activity
    )
}

/// Queue `frame` on the data connection. Gives it back when that connection is not up yet.
fn send_on_data(fingerprint: &str, frame: Vec<u8>) -> Result<(), Vec<u8>> {
    let map = data_open().lock().unwrap_or_else(|e| e.into_inner());
    let Some(slot) = map.get(fingerprint) else {
        return Err(frame);
    };
    let guard = slot.bulk.lock().unwrap_or_else(|e| e.into_inner());
    match guard.as_ref() {
        Some(tx) => tx.send(frame).map_err(|e| e.0),
        None => Err(frame),
    }
}

fn attach_bulk(fingerprint: &str, tx: mpsc::UnboundedSender<Vec<u8>>) -> bool {
    let map = data_open().lock().unwrap_or_else(|e| e.into_inner());
    let Some(slot) = map.get(fingerprint) else {
        return false;
    };
    *slot.bulk.lock().unwrap_or_else(|e| e.into_inner()) = Some(tx);
    true
}

fn detach_bulk(fingerprint: &str) {
    let map = data_open().lock().unwrap_or_else(|e| e.into_inner());
    if let Some(slot) = map.get(fingerprint) {
        *slot.bulk.lock().unwrap_or_else(|e| e.into_inner()) = None;
    }
}

impl Drop for DataReg {
    fn drop(&mut self) {
        data_open()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&self.0);
    }
}

fn export_client(stream: &tokio_rustls::client::TlsStream<TcpStream>) -> [u8; 32] {
    match stream
        .get_ref()
        .1
        .export_keying_material([0u8; 32], b"EXPORTER-CSC-DATA-v1", None)
    {
        Ok(token) => token,
        Err(e) => {
            warn!("export data token: {e}");
            [0u8; 32]
        }
    }
}

fn export_server(stream: &tokio_rustls::server::TlsStream<TcpStream>) -> [u8; 32] {
    match stream
        .get_ref()
        .1
        .export_keying_material([0u8; 32], b"EXPORTER-CSC-DATA-v1", None)
    {
        Ok(token) => token,
        Err(e) => {
            warn!("export data token: {e}");
            [0u8; 32]
        }
    }
}

async fn spawn_data_listener(identity: &Arc<crate::tls::Identity>, addr: SocketAddr, port: u16) {
    match TcpListener::bind(addr).await {
        Ok(listener) => {
            info!("data port {port} listening");
            let identity = identity.clone();
            tokio::spawn(async move {
                loop {
                    let Ok((tcp, from)) = listener.accept().await else {
                        break;
                    };
                    prepare_tcp(&tcp);
                    let identity = identity.clone();
                    tokio::spawn(async move {
                        if let Err(e) = take_data(&identity, tcp, port).await {
                            debug!("data connection from {from}: {e:#}");
                        }
                    });
                }
            });
        }
        Err(e) => warn!("listen TCP data port {port} failed ({e:#})"),
    }
}

async fn dial_data(
    identity: &crate::tls::Identity,
    addr: SocketAddr,
    token: &[u8; 32],
    fingerprint: &str,
) -> anyhow::Result<()> {
    let (mut stream, _) = connect_tls(identity, addr).await?;
    let attach = csc_proto::DataAttachPayload {
        device_id: fingerprint.to_string(),
        session_token: hex_encode(token),
        data_port: addr.port(),
    };
    let frame = encode_json_frame(MsgType::DataAttach, &attach)?;
    write_timeout(&mut stream, &frame).await?;
    serve_data(stream, fingerprint, BytesMut::new()).await
}

/// File frames travel on this socket once DATA_ATTACH has succeeded.
async fn serve_data<S>(mut stream: S, fingerprint: &str, mut buf: BytesMut) -> anyhow::Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let (tx, mut rx) = mpsc::unbounded_channel();
    if !attach_bulk(fingerprint, tx) {
        anyhow::bail!("no control session for data channel");
    }
    struct Detach<'a>(&'a str);
    impl Drop for Detach<'_> {
        fn drop(&mut self) {
            detach_bulk(self.0);
        }
    }
    let _detach = Detach(fingerprint);
    let mut tmp = [0u8; 16 * 1024];
    loop {
        tokio::select! {
            outgoing = rx.recv() => {
                match outgoing {
                    Some(frame) => write_timeout(&mut stream, &frame).await?,
                    None => break,
                }
            }
            read = stream.read(&mut tmp) => {
                let n = read?;
                if n == 0 {
                    break;
                }
                buf.extend_from_slice(&tmp[..n]);
                loop {
                    match try_decode_frame(&mut buf)? {
                        None => break,
                        Some(frame) => {
                            if let Some((peer, mut wires, is_host, status)) = data_target(fingerprint) {
                                dispatch_frame(frame, &peer, &status, &mut wires, is_host).await?;
                            }
                        }
                    }
                }
            }
        }
    }
    Ok(())
}

fn data_target(fingerprint: &str) -> Option<(String, Wires, bool, Arc<RwLock<NetStatus>>)> {
    let map = data_open().lock().unwrap_or_else(|e| e.into_inner());
    let slot = map.get(fingerprint)?;
    Some((
        slot.peer.clone(),
        slot.wires.clone(),
        slot.is_host,
        slot.status.clone(),
    ))
}

async fn take_data(
    identity: &crate::tls::Identity,
    tcp: TcpStream,
    local_port: u16,
) -> anyhow::Result<()> {
    let (mut stream, der) = accept_tls(identity, tcp).await?;
    let fp = crate::tls::fingerprint(&der);
    let mut buf = BytesMut::new();
    let mut tmp = [0u8; 2048];
    let frame = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Some(frame) = try_decode_frame(&mut buf)? {
                return Ok::<_, anyhow::Error>(frame);
            }
            let n = stream.read(&mut tmp).await?;
            if n == 0 {
                anyhow::bail!("closed before DATA_ATTACH");
            }
            buf.extend_from_slice(&tmp[..n]);
        }
    })
    .await
    .map_err(|_| anyhow::anyhow!("handshake_timeout"))??;
    if frame.msg_type != MsgType::DataAttach {
        anyhow::bail!("expected DATA_ATTACH");
    }
    let attach: csc_proto::DataAttachPayload =
        serde_json::from_slice(&frame.payload).map_err(|_| anyhow::anyhow!("bad DATA_ATTACH"))?;
    if attach.data_port != local_port {
        anyhow::bail!("bad_data_port");
    }
    let expect = data_open()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(&fp)
        .map(|slot| slot.token);
    let Some(expect) = expect else {
        anyhow::bail!("fingerprint_mismatch");
    };
    let got = unhex(&attach.session_token).unwrap_or_default();
    if !ct_eq(&got, &expect) {
        anyhow::bail!("bad_session_token");
    }
    info!("data channel attached for {fp}");
    serve_data(stream, &fp, buf).await
}

fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 0xf) as usize] as char);
    }
    out
}

fn unhex(text: &str) -> Option<Vec<u8>> {
    if !text.len().is_multiple_of(2) {
        return None;
    }
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len() / 2);
    let mut i = 0;
    while i < bytes.len() {
        let hi = hex_val(bytes[i])?;
        let lo = hex_val(bytes[i + 1])?;
        out.push((hi << 4) | lo);
        i += 2;
    }
    Some(out)
}

fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

#[allow(dead_code)]
pub fn encode_clip_offer(v: &impl serde::Serialize) -> Result<Vec<u8>, serde_json::Error> {
    encode_json_frame(MsgType::ClipOffer, v)
}

#[cfg(test)]
mod tests {

    #[test]
    fn a_failed_attempt_keeps_another_attempts_code() {
        let mut slot = "111111".to_string();
        // An attempt that showed nothing, or another code, leaves it alone.
        super::release_code(&mut slot, "");
        super::release_code(&mut slot, "222222");
        assert_eq!(slot, "111111");
        super::release_code(&mut slot, "111111");
        assert!(slot.is_empty());
    }

    use super::*;

    #[tokio::test]
    async fn a_young_session_is_not_replaced_by_the_other_dial() {
        let peers = PeerTable::new();
        let (first, rx1) = new_link();
        assert!(peers.adopt("hady", first, DUPLICATE_LINK).await);
        let (second, rx2) = new_link();
        assert!(!peers.adopt("hady", second, DUPLICATE_LINK).await);
        assert!(!*rx1.kill.borrow());
        assert!(!*rx2.kill.borrow());
        assert!(peers.contains("hady").await);
    }

    #[tokio::test]
    async fn an_old_session_is_replaced_when_the_peer_dials_again() {
        let peers = PeerTable::new();
        let (first, rx1) = new_link();
        assert!(peers.adopt("hady", first, Duration::ZERO).await);
        let (second, _rx2) = new_link();
        assert!(peers.adopt("hady", second, Duration::ZERO).await);
        assert!(*rx1.kill.borrow());
        assert!(peers.contains("hady").await);
    }

    #[test]
    fn redial_backs_off_to_ten_seconds() {
        let mut r = Redial::default();
        let t0 = Instant::now();
        assert!(r.due("B", t0));
        assert!(!r.due("b", t0 + Duration::from_millis(499)));
        assert!(r.due("B", t0 + Duration::from_millis(500)));
        // Next wait is 1 s, then 2 s, 4 s, 8 s, then capped at 10 s.
        assert!(!r.due("B", t0 + Duration::from_millis(1499)));
        assert!(r.due("B", t0 + Duration::from_millis(1500)));
        assert!(r.due("B", t0 + Duration::from_millis(3500)));
        assert!(!r.due("B", t0 + Duration::from_millis(7499)));
        assert!(r.due("B", t0 + Duration::from_millis(7500)));
        r.reset("B");
        assert!(r.due("B", t0 + Duration::from_secs(25)));
    }

    #[test]
    fn three_silent_rounds_restart_discovery() {
        let n = count_udp_failure(0, false).unwrap();
        let n = count_udp_failure(n, false).unwrap();
        assert_eq!(count_udp_failure(n, true).unwrap(), 0);
        assert!(count_udp_failure(n, false).is_err());
    }

    #[test]
    fn redial_address_ignores_missing_advertised_port() {
        let ip: std::net::IpAddr = "10.10.10.10".parse().unwrap();
        assert_eq!(peer_control_addr(ip, 0, 45277).port(), 45277);
        assert_eq!(peer_control_addr(ip, 45277, 1).port(), 45277);
        // Callers must not pass an inbound ephemeral port as `advertised`.
        assert_eq!(
            peer_control_addr(ip, 0, 45277),
            "10.10.10.10:45277".parse().unwrap()
        );
    }

    #[test]
    fn pairing_code_suffix_maps_to_peer_ip() {
        let mut cfg = Config {
            search_address: "192.168.1.90".into(),
            control_port: 45433,
            ..Default::default()
        };
        let addr = peer_addr_from_offer(&cfg, "582243", &[]).unwrap();
        assert_eq!(addr, "192.168.1.243:45433".parse().unwrap());
        // Own host octet must not dial self.
        assert!(peer_addr_from_offer(&cfg, "111090", &[]).is_none());
        assert!(peer_addr_from_offer(&cfg, "12", &[]).is_none());
        // Settings reset clears search_address: fall back to the local IPv4.
        cfg.search_address.clear();
        assert!(peer_addr_from_offer(&cfg, "176026", &[]).is_none());
        let local = ["192.168.1.90".to_string()];
        let addr = peer_addr_from_offer(&cfg, "176026", &local).unwrap();
        assert_eq!(addr, "192.168.1.26:45433".parse().unwrap());
    }
    #[tokio::test]
    async fn rekey_keeps_existing_hostname_session() {
        let peers = PeerTable::new();
        let (host_link, host_rx) = new_link();
        assert!(peers.insert_new("Desk", host_link).await);
        let (ip_link, ip_rx) = new_link();
        assert!(peers.insert_new("192.168.1.26", ip_link).await);
        // Newcomer must not kill the hostname session.
        assert!(!peers.rekey("192.168.1.26", "Desk").await);
        assert!(peers.contains("Desk").await);
        assert!(!peers.contains("192.168.1.26").await);
        assert!(!*host_rx.kill.borrow());
        // Dropped dial should not leave an orphan under the IP key.
        assert!(!*ip_rx.kill.borrow() || !peers.contains("192.168.1.26").await);
    }

    #[tokio::test]
    async fn rekey_moves_ip_session_to_hostname() {
        let peers = PeerTable::new();
        let (ip_link, rx) = new_link();
        assert!(peers.insert_new("192.168.1.26", ip_link).await);
        assert!(peers.rekey("192.168.1.26", "Desk").await);
        assert!(peers.contains("Desk").await);
        assert!(!peers.contains("192.168.1.26").await);
        assert!(!*rx.kill.borrow());
    }

    #[test]
    fn only_input_and_session_frames_stay_on_control() {
        let on_data = |t| rides_data(&encode_frame(t, b"{}", 0));
        for t in [
            MsgType::Hello,
            MsgType::Heartbeat,
            MsgType::PairRequest,
            MsgType::SwitchEnter,
            MsgType::SwitchLeave,
            MsgType::EdgeHit,
            MsgType::ScreenUpdate,
            MsgType::MouseMove,
            MsgType::MouseButton,
            MsgType::MouseWheel,
            MsgType::Key,
            MsgType::InputReset,
        ] {
            assert!(!on_data(t), "{t:?} must stay on control");
        }
        for t in [
            MsgType::ClipOffer,
            MsgType::ClipRequest,
            MsgType::ClipData,
            MsgType::PasteRequest,
            MsgType::PasteLocated,
            MsgType::FileOffer,
            MsgType::FileCancel,
            MsgType::FileAck,
            MsgType::LockScreen,
            MsgType::Activity,
        ] {
            assert!(on_data(t), "{t:?} must go on data");
        }
        assert!(!rides_data(&[]));
    }

    #[tokio::test]
    async fn hello_wait_gives_up_after_silence_only() {
        let (mut a, mut b) = tokio::io::duplex(1024);
        let mut buf = BytesMut::new();
        let res = read_until_hello_idle(&mut b, &mut buf, Duration::from_millis(50)).await;
        assert!(res.is_err());

        let writer = tokio::spawn(async move {
            for _ in 0..4 {
                tokio::time::sleep(Duration::from_millis(30)).await;
                a.write_all(&encode_frame(MsgType::Heartbeat, &[], 0))
                    .await
                    .unwrap();
            }
            let hello = HelloPayload {
                protocol_version: 2,
                host_name: "B".into(),
                screen_width: 10,
                screen_height: 10,
                capabilities: csc_proto::CAPABILITIES,
                ..HelloPayload::default()
            };
            a.write_all(&encode_hello_bin(&hello)).await.unwrap();
            a
        });
        let mut buf = BytesMut::new();
        let (hello, early) = read_until_hello_idle(&mut b, &mut buf, Duration::from_millis(80))
            .await
            .expect("heartbeats keep it waiting");
        assert_eq!(hello.host_name, "B");
        assert!(early.is_empty());
        let _ = writer.await;
    }
}
