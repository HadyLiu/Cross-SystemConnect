//! Cross-machine control channel (HELLO + heartbeat + switch/input frames).
//!
//! The keyboard host (server) searches a UDP port and then connects to the client.
//! The client sends an announce to that same port and accepts the TCP connection.
//! UDP only carries the computer name and port. The TCP session is always TLS 1.3.

use crate::discover::{
    broadcast_targets, decode_datagram, encode_announce, encode_search, is_own_announce,
    reject_mark, same_subnet24, subnet_label, subnet_scan_targets, suppress_repeat, Announce,
    Datagram, RejectMark,
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
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream, UdpSocket};
use tokio::sync::{mpsc, watch, Mutex, RwLock};
use tokio_rustls::TlsAcceptor;
use tracing::{info, warn};

#[derive(Debug, Clone, Default)]
pub struct NetStatus {
    pub connected: bool,
    pub peer_name: String,
}

#[derive(Clone)]
pub struct PeerTable {
    txs: Arc<Mutex<HashMap<String, mpsc::Sender<Vec<u8>>>>>,
    active: Arc<Mutex<String>>,
}

impl PeerTable {
    fn new() -> Self {
        Self {
            txs: Arc::new(Mutex::new(HashMap::new())),
            active: Arc::new(Mutex::new(String::new())),
        }
    }

    async fn insert(&self, name: &str, tx: mpsc::Sender<Vec<u8>>) {
        self.txs.lock().await.insert(name.to_string(), tx);
        let mut active = self.active.lock().await;
        if active.is_empty() {
            *active = name.to_string();
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
        let txs = self.txs.lock().await;
        let tx = txs
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, tx)| tx.clone());
        drop(txs);
        if let Some(tx) = tx {
            let _ = tx.send(frame).await;
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
}

pub struct NetHub {
    pub status: Arc<RwLock<NetStatus>>,
    peers: PeerTable,
}

impl NetHub {
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

    pub async fn send_mouse_move(&self, dx: i32, dy: i32) {
        self.send_raw(encode_mouse_move_bin(&MouseMovePayload { dx, dy }))
            .await;
    }
}

pub async fn spawn_network(
    cfg: Arc<RwLock<Config>>,
    config_path: PathBuf,
    app_status: Arc<RwLock<AppStatus>>,
    prompt: Arc<PeerPrompt>,
    mut config_watch: watch::Receiver<u64>,
    inbound: mpsc::Sender<InboundEvent>,
) -> anyhow::Result<NetHub> {
    let peers = PeerTable::new();
    let status = Arc::new(RwLock::new(NetStatus::default()));
    let status2 = status.clone();
    let cfg2 = cfg.clone();
    let peers2 = peers.clone();

    tokio::spawn(async move {
        loop {
            let snapshot = cfg2.read().await.clone();
            {
                let mut s = app_status.write().await;
                s.discovery_port = snapshot.control_port;
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

    Ok(NetHub { status, peers })
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
    inbound: mpsc::Sender<InboundEvent>,
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
    MouseMove { dx: i32, dy: i32 },
    Key { hid: u32, down: bool },
    ClipOffer(serde_json::Value),
    Raw(MsgType, Vec<u8>),
    Disconnected { peer: String },
}

#[allow(clippy::too_many_arguments)]
async fn run_server(
    cfg: &Config,
    live: &Arc<RwLock<Config>>,
    config_path: &Path,
    identity: &Arc<crate::tls::Identity>,
    app_status: &Arc<RwLock<AppStatus>>,
    _prompt: &Arc<PeerPrompt>,
    local_host: &str,
    local_ips: &[String],
    status: &Arc<RwLock<NetStatus>>,
    peers: &PeerTable,
    inbound: mpsc::Sender<InboundEvent>,
) -> anyhow::Result<()> {
    let port = cfg.control_port;
    let bind = effective_bind(cfg);
    let udp = bind_udp(parse_bind(&bind, port)).await?;
    let subnet = subnet_label(&cfg.search_address).unwrap_or_else(|| "全部本机网段".into());
    info!("searching UDP port {port} on {subnet}");
    let search = encode_search();
    let targets = scan_targets(cfg, port, local_ips);
    let mut tick = tokio::time::interval(Duration::from_secs(2));
    let mut buf = [0u8; 2048];
    let (stop_tx, stop_rx) = watch::channel(false);
    let _stop = StopOnDrop(stop_tx);
    let inflight: Arc<Mutex<HashSet<String>>> = Arc::new(Mutex::new(HashSet::new()));
    let mut seen: HashMap<String, (DiscoveredPeer, Instant)> = HashMap::new();

    loop {
        tokio::select! {
            _ = tick.tick() => {
                send_to_targets(&udp, &search, &targets).await;
                let now = Instant::now();
                seen.retain(|_, (_, last)| now.duration_since(*last) < Duration::from_secs(15));
                publish_discovered(app_status, &seen).await;
            }
            res = udp.recv_from(&mut buf) => {
                let Some((n, src)) = udp_datagram(res)? else {
                    continue;
                };
                let Some(Datagram::Announce(ann)) = decode_datagram(&buf[..n]) else {
                    continue;
                };
                let src_ip = src.ip().to_string();
                if is_own_announce(&ann.host_name, local_host, &src_ip, local_ips) {
                    continue;
                }
                if !cfg.search_address.is_empty() && !same_subnet24(&src_ip, &cfg.search_address) {
                    continue;
                }
                let key = if ann.name.is_empty() { ann.host_name.clone() } else { ann.name.clone() };
                let key = if key.is_empty() { src_ip.clone() } else { key };
                let display = if ann.name.is_empty() { key.clone() } else { ann.name.clone() };
                seen.insert(key.clone(), (DiscoveredPeer {
                    name: display.clone(),
                    host_name: if ann.host_name.is_empty() { key.clone() } else { ann.host_name.clone() },
                    address: src_ip.clone(),
                    seen_unix: unix_now(),
                }, Instant::now()));
                publish_discovered(app_status, &seen).await;

                if peers.contains(&key).await || inflight.lock().await.contains(&key) {
                    continue;
                }
                let tcp_port = if ann.tcp_port == 0 { port } else { ann.tcp_port };
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
                    inflight.clone(),
                    stop_rx.clone(),
                    key,
                    peer_addr,
                );
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn spawn_server_link(
    cfg: Config,
    live: Arc<RwLock<Config>>,
    config_path: PathBuf,
    identity: Arc<crate::tls::Identity>,
    peers: PeerTable,
    inbound: mpsc::Sender<InboundEvent>,
    status: Arc<RwLock<NetStatus>>,
    inflight: Arc<Mutex<HashSet<String>>>,
    mut stop: watch::Receiver<bool>,
    key: String,
    peer_addr: SocketAddr,
) {
    tokio::spawn(async move {
        match connect_tls(&identity, peer_addr).await {
            Ok((stream, peer_cert)) => {
                info!("connected to client {key} at {peer_addr}");
                let (tx, rx) = mpsc::channel(64);
                peers.insert(&key, tx).await;
                refresh_link_status(&status, &peers).await;
                tokio::select! {
                    result = session(
                        &cfg,
                        &live,
                        &config_path,
                        stream,
                        peer_cert,
                        &status,
                        key.clone(),
                        rx,
                        inbound,
                        None,
                        None,
                    ) => {
                        if let Err(e) = result {
                            warn!("session {key} ended: {e:#}");
                        }
                    }
                    _ = stop.changed() => {}
                }
                peers.remove(&key).await;
                refresh_link_status(&status, &peers).await;
            }
            Err(e) => warn!("connect {peer_addr}: {e:#}"),
        }
        inflight.lock().await.remove(&key);
    });
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
    inbound: mpsc::Sender<InboundEvent>,
) -> anyhow::Result<()> {
    let port = cfg.control_port;
    let bind = effective_bind(cfg);
    let listener = TcpListener::bind(parse_bind(&bind, port)).await?;
    let udp = match bind_udp(parse_bind(&bind, port)).await {
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
        tcp_port: port,
    });
    let (stop_tx, stop_rx) = watch::channel(false);
    let _stop = StopOnDrop(stop_tx);
    let rejected: Arc<Mutex<HashMap<String, RejectMark>>> = Arc::new(Mutex::new(HashMap::new()));
    let targets = scan_targets(cfg, port, local_ips);
    let subnet = subnet_label(&cfg.search_address).unwrap_or_else(|| "本机网段".into());
    info!("announcing on UDP port {port} toward {subnet}");
    let mut tick = tokio::time::interval(Duration::from_secs(1));
    let mut buf = [0u8; 2048];
    loop {
        tokio::select! {
            _ = tick.tick() => {
                send_to_targets(&udp, &announce, &targets).await;
            }
            res = udp.recv_from(&mut buf) => {
                let Some((n, src)) = udp_datagram(res)? else {
                    continue;
                };
                if !cfg.search_address.is_empty() {
                    let src_ip = src.ip().to_string();
                    if !same_subnet24(&src_ip, &cfg.search_address) && src_ip != "255.255.255.255" {
                        continue;
                    }
                }
                if decode_datagram(&buf[..n]) == Some(Datagram::Search) {
                    let _ = udp.send_to(&announce, src).await;
                }
            }
            acc = listener.accept() => {
                let (tcp, peer) = acc?;
                let _ = tcp.set_nodelay(true);
                info!("keyboard host connected from {peer}");
                let key = peer.ip().to_string();
                let (tx, rx) = mpsc::channel(64);
                let cfg = cfg.clone();
                let live = live.clone();
                let config_path = config_path.to_path_buf();
                let identity = identity.clone();
                let status = status.clone();
                let peers = peers.clone();
                let inbound = inbound.clone();
                let mut stop = stop_rx.clone();
                let link = ClientLink {
                    peers: peers.clone(),
                    status: status.clone(),
                    key: key.clone(),
                    tx,
                };
                let admit = Admit {
                    app_status: app_status.clone(),
                    prompt: prompt.clone(),
                    live: live.clone(),
                    config_path: config_path.clone(),
                    rejected: rejected.clone(),
                    address: peer.to_string(),
                };
                tokio::spawn(async move {
                    let accepted = match accept_tls(&identity, tcp).await {
                        Ok(pair) => pair,
                        Err(e) => {
                            warn!("tls from {peer}: {e:#}");
                            return;
                        }
                    };
                    let (stream, peer_cert) = accepted;
                    tokio::select! {
                        result = session(
                            &cfg,
                            &live,
                            &config_path,
                            stream,
                            peer_cert,
                            &status,
                            key.clone(),
                            rx,
                            inbound,
                            Some(link),
                            Some(admit),
                        ) => {
                            if let Err(e) = result {
                                warn!("client session {key} ended: {e:#}");
                            }
                        }
                        _ = stop.changed() => {}
                    }
                    peers.remove(&key).await;
                    refresh_link_status(&status, &peers).await;
                });
            }
        }
    }
}

struct StopOnDrop(watch::Sender<bool>);

impl Drop for StopOnDrop {
    fn drop(&mut self) {
        let _ = self.0.send(true);
    }
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

async fn send_to_targets(sock: &UdpSocket, packet: &[u8], targets: &[SocketAddr]) {
    for dest in targets {
        if let Err(e) = sock.send_to(packet, dest).await {
            tracing::debug!("discovery send {dest}: {e}");
        }
    }
}

async fn connect_peer(addr: SocketAddr) -> anyhow::Result<TcpStream> {
    let stream = tokio::time::timeout(Duration::from_secs(4), TcpStream::connect(addr))
        .await
        .map_err(|_| anyhow::anyhow!("connect {addr} timed out"))??;
    let _ = stream.set_nodelay(true);
    Ok(stream)
}

async fn connect_tls(
    identity: &crate::tls::Identity,
    addr: SocketAddr,
) -> anyhow::Result<(tokio_rustls::client::TlsStream<TcpStream>, Vec<u8>)> {
    let tcp = connect_peer(addr).await?;
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
    tx: mpsc::Sender<Vec<u8>>,
}

struct Admit {
    app_status: Arc<RwLock<AppStatus>>,
    prompt: Arc<PeerPrompt>,
    live: Arc<RwLock<Config>>,
    config_path: PathBuf,
    rejected: Arc<Mutex<HashMap<String, RejectMark>>>,
    address: String,
}

#[allow(clippy::too_many_arguments)]
async fn session<S>(
    cfg: &Config,
    live: &Arc<RwLock<Config>>,
    config_path: &Path,
    stream: S,
    peer_cert: Vec<u8>,
    status: &Arc<RwLock<NetStatus>>,
    peer: String,
    mut outbound: mpsc::Receiver<Vec<u8>>,
    mut inbound: mpsc::Sender<InboundEvent>,
    link: Option<ClientLink>,
    admit: Option<Admit>,
) -> anyhow::Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let peer_fp = crate::tls::fingerprint(&peer_cert);
    let (mut reader, writer) = tokio::io::split(stream);
    let writer = Arc::new(Mutex::new(writer));

    let size = csc_platform::create_screen_info().virtual_size();
    let hello = HelloPayload {
        protocol_version: 1,
        host_name: cfg.name.clone(),
        screen_width: size.width,
        screen_height: size.height,
        capabilities: 0,
    };
    // The keyboard host speaks first. The controlled computer answers only after trust,
    // so a cancelled prompt never pins a certificate on the host.
    if admit.is_none() {
        let mut w = writer.lock().await;
        w.write_all(&encode_hello_bin(&hello)).await?;
    }

    let mut buf = BytesMut::with_capacity(64 * 1024);
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
        let announced = remote.clone();
        let mut decision = std::pin::pin!(allow_control(&admit, &remote, &peer_fp));
        let mut discard = [0u8; 4096];
        loop {
            while try_decode_frame(&mut buf)?.is_some() {}
            tokio::select! {
                allowed = &mut decision => {
                    if !allowed {
                        return Ok(());
                    }
                    break;
                }
                read = reader.read(&mut discard) => {
                    let n = read?;
                    if n == 0 {
                        info!("keyboard host {} closed while waiting", admit.address);
                        return Ok(());
                    }
                    buf.extend_from_slice(&discard[..n]);
                }
            }
        }
        {
            let mut w = writer.lock().await;
            w.write_all(&encode_hello_bin(&hello)).await?;
        }
        let _ = inbound.send(InboundEvent::Hello(announced)).await;
    } else {
        let (remote, early) = match tokio::time::timeout(
            Duration::from_secs(120),
            read_until_hello(&mut reader, &mut buf),
        )
        .await
        {
            Ok(Ok(found)) => found,
            Ok(Err(e)) => {
                info!("client {peer} closed before accepting control: {e:#}");
                return Ok(());
            }
            Err(_) => {
                info!("client {peer} did not accept control");
                return Ok(());
            }
        };
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
            if let Err(e) = saved.save(config_path) {
                warn!("save peer certificate: {e:#}");
            }
        }
        for frame in early {
            dispatch_frame(frame, status, &mut inbound).await?;
        }
        let _ = inbound.send(InboundEvent::Hello(remote)).await;
    }

    if let Some(link) = link {
        link.peers.insert(&link.key, link.tx).await;
        refresh_link_status(&link.status, &link.peers).await;
    }

    let hb_writer = writer.clone();
    let hb_enabled = cfg.network.heartbeat_enabled;
    let hb_ms = cfg.network.heartbeat_ms.max(500) as u64;
    let heartbeat = tokio::spawn(async move {
        if !hb_enabled {
            return;
        }
        let mut tick = tokio::time::interval(std::time::Duration::from_millis(hb_ms));
        loop {
            tick.tick().await;
            let frame = encode_frame(MsgType::Heartbeat, &[], 0);
            let mut w = hb_writer.lock().await;
            if w.write_all(&frame).await.is_err() {
                break;
            }
        }
    });

    let mut tmp = [0u8; 16 * 1024];
    loop {
        tokio::select! {
            read = reader.read(&mut tmp) => {
                let n = read?;
                if n == 0 {
                    let _ = inbound.send(InboundEvent::Disconnected { peer: peer.clone() }).await;
                    break;
                }
                buf.extend_from_slice(&tmp[..n]);
                loop {
                    match try_decode_frame(&mut buf)? {
                        None => break,
                        Some(frame) => {
                            dispatch_frame(frame, status, &mut inbound).await?;
                        }
                    }
                }
            }
            frame = outbound.recv() => {
                match frame {
                    Some(frame) => {
                        let mut w = writer.lock().await;
                        if w.write_all(&frame).await.is_err() {
                            let _ = inbound.send(InboundEvent::Disconnected { peer: peer.clone() }).await;
                            break;
                        }
                    }
                    None => break,
                }
            }
        }
    }

    heartbeat.abort();
    Ok(())
}

async fn read_until_hello<R>(
    reader: &mut R,
    buf: &mut BytesMut,
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
                Some(frame) => early.push(frame),
            }
        }
        let n = reader.read(&mut tmp).await?;
        if n == 0 {
            anyhow::bail!("closed before hello");
        }
        buf.extend_from_slice(&tmp[..n]);
    }
}

async fn allow_control(admit: &Admit, hello: &HelloPayload, fingerprint: &str) -> bool {
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
                return false;
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
        return true;
    }
    if admit.live.read().await.is_trusted_peer(&name) {
        info!("keyboard host {name} presented a new certificate; asking again");
    }
    info!("keyboard host {name} requested control");
    let choice = admit
        .prompt
        .ask(&admit.app_status, &name, &name, &admit.address)
        .await;
    match choice {
        Choice::Trust => {
            let mut cfg = admit.live.write().await;
            cfg.remember_peer(&name, fingerprint);
            if let Err(e) = cfg.save(&admit.config_path) {
                warn!("save trusted peer: {e:#}");
            }
            true
        }
        Choice::Reject => {
            info!("control from {name} cancelled");
            admit
                .rejected
                .lock()
                .await
                .insert(name, reject_mark(Instant::now()));
            false
        }
    }
}

async fn dispatch_frame(
    frame: csc_proto::Frame,
    status: &Arc<RwLock<NetStatus>>,
    inbound: &mut mpsc::Sender<InboundEvent>,
) -> anyhow::Result<()> {
    match frame.msg_type {
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
                let p = SwitchPayload {
                    edge,
                    position_pct: pct,
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
        MsgType::Key => {
            if frame.payload.len() >= 5 {
                let hid = u32::from_le_bytes(frame.payload[0..4].try_into()?);
                let down = frame.payload[4] != 0;
                let _ = inbound.send(InboundEvent::Key { hid, down }).await;
            }
        }
        MsgType::ClipOffer => {
            if let Ok(v) = serde_json::from_slice(&frame.payload) {
                let _ = inbound.send(InboundEvent::ClipOffer(v)).await;
            }
        }
        other => {
            let _ = inbound.send(InboundEvent::Raw(other, frame.payload)).await;
        }
    }
    Ok(())
}

#[allow(dead_code)]
pub fn encode_clip_offer(v: &impl serde::Serialize) -> Result<Vec<u8>, serde_json::Error> {
    encode_json_frame(MsgType::ClipOffer, v)
}
