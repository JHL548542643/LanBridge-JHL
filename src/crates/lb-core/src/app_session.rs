//! App-facing device session: pair once, fixed-port listen, bidirectional send/recv.
//!
//! Product contract: one fixed control port, JSON status, prepare/execute split for long work.

use crate::{
    accept_trusted, await_host_pair, connect_trusted, establish_receiver, establish_sender,
    generate_pairing_code, pair_as_guest, push_clipboard, recv_clipboard, recv_file, send_file,
    shutdown_link, ByteProgress, ClipboardItem, DeviceIdentity, Error, HostPairOutcome, MsgType,
    PairingCode, PeerInfo, RecvOptions, Result, SendOptions, TrustStore, TrustedPeer,
    UdpBeaconConfig, UdpDiscoverer, DEFAULT_DISCOVERY_PORT, DEFAULT_PROBE_BYTES,
};
use lb_protocol::Frame;
use std::collections::HashMap;
use std::net::{Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicU8, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio::net::TcpListener;
use tokio::time::timeout;

/// Default control listen port (discovery uses 37531).
pub const DEFAULT_CONTROL_PORT: u16 = 37_532;

const PAIR_WAIT: Duration = Duration::from_secs(180);
const GUEST_PAIR_WAIT: Duration = Duration::from_secs(45);
/// Short idle accept so send/status are not blocked for minutes (shell loops serve_once).
const SERVE_IDLE_WAIT: Duration = Duration::from_millis(800);
const ACCEPT_WAIT: Duration = Duration::from_secs(300);
/// Outbound TLS connect must not hang forever (SYN-drop / peer not accepting).
const CONNECT_OUT_WAIT: Duration = Duration::from_secs(5);
/// Reachability probe — keep short so UI/session lock is not starved.
const PROBE_CONNECT_WAIT: Duration = Duration::from_secs(2);
/// Session-live window for UI「已连接」. UDP beacons do **not** extend this — only an
/// authenticated control exchange (heartbeat / probe / transfer / clipboard / inbound accept) via
/// [`DeviceSession::touch_peer_alive`] does.
///
/// 12 s = four missed 3 s heartbeats, inside the ≤15 s acceptance bound of
/// (acceptance: the peer reads as offline well inside 15 s).
const PEER_LIVE_MS: u64 = 12_000;
/// Heartbeat cadence on an established pair: `Ping` → `Pong` over a trusted session.
const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(3);
/// A half-open network must fail the dial fast enough to keep the 3 s cadence, but a phone that
/// has just woken from doze needs more than 1.5 s to finish the TLS handshake — aborting it
/// produces "tls handshake eof" on the peer's accept loop.
const HEARTBEAT_DIAL_TIMEOUT: Duration = Duration::from_millis(2_500);
/// Wait for `Pong` once connected: longer than a LAN RTT, shorter than the next beat.
const HEARTBEAT_RTT_TIMEOUT: Duration = Duration::from_millis(2_000);
/// Unpair handshake: `Bye` must reach the peer, but must not block the UI forever.
const UNPAIR_BYE_WAIT: Duration = Duration::from_secs(2);

fn identity_dir(data_dir: &Path) -> PathBuf {
    data_dir.join("identity")
}

fn trust_path(data_dir: &Path) -> PathBuf {
    data_dir.join("trust.json")
}

fn endpoint_path(data_dir: &Path) -> PathBuf {
    data_dir.join("endpoint.json")
}

fn listen_port_path(data_dir: &Path) -> PathBuf {
    data_dir.join("listen_port.json")
}

fn load_or_create_identity(
    data_dir: &Path,
    device_id: &str,
    alias: &str,
) -> Result<Arc<DeviceIdentity>> {
    let dir = identity_dir(data_dir);
    if dir.exists() {
        if let Ok(id) = DeviceIdentity::load_from_dir(&dir) {
            return Ok(Arc::new(id));
        }
    }
    let id = DeviceIdentity::generate(device_id.to_string(), alias.to_string())?;
    std::fs::create_dir_all(&dir)?;
    id.save_to_dir(&dir)?;
    Ok(Arc::new(id))
}

fn load_trust(data_dir: &Path) -> TrustStore {
    TrustStore::load(&trust_path(data_dir)).unwrap_or_default()
}

fn save_trust(data_dir: &Path, store: &TrustStore) -> Result<()> {
    store.save(&trust_path(data_dir))?;
    Ok(())
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct EndpointFile {
    host: String,
    port: u16,
    peer_device_id: String,
    /// Friendly name from Hello / discovery (e.g. "PC" / "Phone").
    #[serde(default)]
    peer_alias: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct ListenPortFile {
    port: u16,
}

fn save_endpoint(
    data_dir: &Path,
    host: &str,
    port: u16,
    peer_device_id: &str,
    peer_alias: Option<&str>,
) -> Result<()> {
    std::fs::create_dir_all(data_dir)?;
    let prev = load_endpoint(data_dir);
    let alias = peer_alias
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .or_else(|| prev.and_then(|p| p.peer_alias));
    let f = EndpointFile {
        host: host.to_string(),
        port,
        peer_device_id: peer_device_id.to_string(),
        peer_alias: alias,
    };
    let path = endpoint_path(data_dir);
    let tmp = path.with_extension("tmp");
    let bytes = serde_json::to_vec_pretty(&f)
        .map_err(|e| Error::Session(lb_session::Error::Store(e.to_string())))?;
    std::fs::write(&tmp, bytes)?;
    let _ = std::fs::remove_file(&path);
    std::fs::rename(tmp, path)?;
    Ok(())
}

fn load_endpoint(data_dir: &Path) -> Option<EndpointFile> {
    let bytes = std::fs::read(endpoint_path(data_dir)).ok()?;
    serde_json::from_slice(&bytes).ok()
}

fn save_listen_port(data_dir: &Path, port: u16) -> Result<()> {
    std::fs::create_dir_all(data_dir)?;
    let path = listen_port_path(data_dir);
    let tmp = path.with_extension("tmp");
    let bytes = serde_json::to_vec_pretty(&ListenPortFile { port })
        .map_err(|e| Error::Session(lb_session::Error::Store(e.to_string())))?;
    std::fs::write(&tmp, bytes)?;
    let _ = std::fs::remove_file(&path);
    std::fs::rename(tmp, path)?;
    Ok(())
}

fn load_listen_port(data_dir: &Path) -> Option<u16> {
    let bytes = std::fs::read(listen_port_path(data_dir)).ok()?;
    serde_json::from_slice::<ListenPortFile>(&bytes)
        .ok()
        .map(|f| f.port)
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn peer_ip_string(addr: SocketAddr) -> String {
    match addr.ip() {
        std::net::IpAddr::V4(v) => v.to_string(),
        std::net::IpAddr::V6(v) => v.to_string(),
    }
}

fn ipv4_slash24(ip: &str) -> Option<(u8, u8, u8)> {
    let v4: Ipv4Addr = ip.parse().ok()?;
    let o = v4.octets();
    Some((o[0], o[1], o[2]))
}

fn socket_slash24(addr: SocketAddr) -> Option<(u8, u8, u8)> {
    match addr.ip() {
        std::net::IpAddr::V4(v4) => {
            let o = v4.octets();
            Some((o[0], o[1], o[2]))
        }
        _ => None,
    }
}

/// Reject loopback / link-local / common virtual / CGNAT (not for LAN pairing UI).
fn is_usable_lan_v4(v4: Ipv4Addr) -> bool {
    if v4.is_loopback() || v4.is_unspecified() || v4.is_link_local() {
        return false;
    }
    let o = v4.octets();
    // Harmony anconwlan / Docker-style bridge.
    if o[0] == 172 && o[1] == 17 {
        return false;
    }
    // CGNAT / Tailscale — wrong for same-LAN hand-fill.
    if o[0] == 100 && (64..=127).contains(&o[1]) {
        return false;
    }
    true
}

fn lan_v4_rank(v4: Ipv4Addr) -> u8 {
    let o = v4.octets();
    // Phone SoftAP / Win Mobile Hotspot gateway addresses — prefer over STA Wi‑Fi.
    if o[0] == 192 && o[1] == 168 && (o[2] == 43 || o[2] == 137) && o[3] == 1 {
        return 0;
    }
    if o[0] == 192 && o[1] == 168 {
        return 1;
    }
    if o[0] == 10 {
        return 2;
    }
    if o[0] == 172 && (16..=31).contains(&o[1]) {
        return 3;
    }
    4
}

/// Best-effort primary LAN IPv4 (UDP connect trick; no packets leave the host).
/// Prefers SoftAP / hotspot-style subnets and RFC1918 Wi‑Fi over virtual NICs.
fn primary_lan_ipv4() -> Option<String> {
    let probes = [
        "192.168.43.1:80",
        "192.168.137.1:80",
        "192.168.0.1:80",
        "10.0.0.1:80",
        "1.1.1.1:80",
    ];
    let mut best: Option<(u8, Ipv4Addr)> = None;
    for probe in probes {
        let Ok(sock) = std::net::UdpSocket::bind("0.0.0.0:0") else {
            continue;
        };
        if sock.connect(probe).is_err() {
            continue;
        }
        if let Ok(addr) = sock.local_addr() {
            if let std::net::IpAddr::V4(v4) = addr.ip() {
                if !is_usable_lan_v4(v4) {
                    continue;
                }
                let rank = lan_v4_rank(v4);
                if best.map(|(br, _)| rank < br).unwrap_or(true) {
                    best = Some((rank, v4));
                }
            }
        }
    }
    best.map(|(_, v)| v.to_string())
}

/// Session state for shells (matches session-ux-v1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionState {
    Unpaired,
    PairedOffline,
    /// Trusted and listening, but the peer stopped answering (bye / timeout / closed).
    Disconnected,
    Online,
    Pairing,
    TransferringIn,
    TransferringOut,
}

/// Snapshot returned by [`DeviceSession::status`] (JSON-friendly).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SessionStatus {
    pub state: SessionState,
    pub paired: bool,
    pub listen_port: u16,
    pub listening: bool,
    pub pairing_code: Option<String>,
    pub peer_device_id: Option<String>,
    pub peer_alias: Option<String>,
    pub peer_host: Option<String>,
    pub peer_port: Option<u16>,
    pub device_id: String,
    pub alias: String,
    /// Primary LAN IPv4 for fallback / display (not required for pairing UX).
    pub local_ipv4: Option<String>,
    /// Live connection: `disconnected` | `searching` | `pairing` | `connected`.
    pub connection: String,
    /// Why the link is down: `unpaired` | `timeout` | `closed` (absent while connected).
    #[serde(default)]
    pub disconnect_reason: Option<DisconnectReason>,
    /// Nearby devices from UDP discovery (excludes self).
    pub nearby: Vec<NearbyPeer>,
    pub banner: String,
    /// Bytes completed in the active (or last) file transfer.
    #[serde(default)]
    pub transfer_done: u64,
    /// Total bytes for the active (or last) file transfer; 0 if unknown.
    #[serde(default)]
    pub transfer_total: u64,
    /// Approximate bytes/sec while transferring; 0 when idle.
    #[serde(default)]
    pub transfer_bps: u64,
    /// Basename of the active file transfer (empty when idle / unknown).
    #[serde(default)]
    pub transfer_name: String,
    /// Last completed outbound send: probe bps (same session as transfer).
    #[serde(default)]
    pub last_send_probe_bps: u64,
    #[serde(default)]
    pub last_send_probe_elapsed_us: u64,
    #[serde(default)]
    pub last_send_transfer_bps: u64,
    #[serde(default)]
    pub last_send_transfer_elapsed_us: u64,
    #[serde(default)]
    pub last_send_bytes: u64,
}

/// One row in the discovery list (Bluetooth-like).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct NearbyPeer {
    pub device_id: String,
    pub alias: String,
    pub host: String,
    pub port: u16,
    pub trusted: bool,
    pub last_seen_ms: u64,
}

struct DiscoveryShare {
    nearby: Mutex<HashMap<String, NearbyPeer>>,
    stop: AtomicBool,
    /// Alias of trusted peer when last seen via beacon.
    peer_alias: Mutex<Option<String>>,
    peer_seen_ms: Mutex<Option<u64>>,
    trusted_id: Mutex<Option<String>>,
    /// Unicast heartbeat target (known endpoint IP).
    peer_unicast: Mutex<Option<std::net::IpAddr>>,
}

/// Cloned listen handles so accept can run without holding the session mutex.
#[derive(Clone)]
pub struct ServeListenParts {
    pub listener: Arc<TcpListener>,
    pub peer: TrustedPeer,
    pub identity: Arc<DeviceIdentity>,
}

/// File recv work that runs **without** holding the session mutex (live progress polls).
pub struct InboundFileWork {
    pub listener: Arc<TcpListener>,
    pub peer: TrustedPeer,
    pub identity: Arc<DeviceIdentity>,
    pub progress: Arc<ByteProgress>,
    pub live_name: Arc<Mutex<String>>,
}

/// File send work that runs **without** holding the session mutex (live progress polls).
pub struct OutboundFileWork {
    pub local: PathBuf,
    pub remote: String,
    pub progress: Arc<ByteProgress>,
    pub live_name: Arc<Mutex<String>>,
    pub identity: Arc<DeviceIdentity>,
    pub peer: TrustedPeer,
    pub peer_addr: SocketAddr,
}

/// TLS probe without holding the session mutex (avoids UI / serve starvation).
pub struct ProbeWork {
    pub identity: Arc<DeviceIdentity>,
    pub peer: TrustedPeer,
    pub peer_addr: SocketAddr,
}

/// Outbound clipboard without holding the session mutex.
pub struct ClipboardOutWork {
    pub identity: Arc<DeviceIdentity>,
    pub peer: TrustedPeer,
    pub peer_addr: SocketAddr,
    pub item: ClipboardItem,
}

/// Why the session with a paired peer ended (surfaced as `disconnect_reason`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DisconnectReason {
    /// Peer sent `Bye` and dropped the pair: trust is gone, pair again.
    Unpaired,
    /// No authenticated frame inside the live window (crash / sleep / network loss).
    Timeout,
    /// Peer finished the TLS handshake then hung up without a frame (serve loop gone).
    Closed,
}

/// One inbound event from [`DeviceSession::serve_once`].
#[derive(Debug, Clone, serde::Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum InboundEvent {
    File {
        bytes: u64,
        path: String,
    },
    ClipboardText {
        bytes: usize,
        text: String,
    },
    ClipboardPng {
        bytes: usize,
        png_base64: String,
    },
    /// Peer said `Bye`: it unpaired. Receivers drop trust too instead of keeping「已连接」.
    PeerLeft {
        reason: String,
    },
    /// Peer hung up right after the handshake (probe / inbound accept teardown).
    PeerClosed,
    /// Liveness exchange; nothing for the UI to render.
    Heartbeat {
        nonce: u64,
    },
    Error {
        message: String,
    },
}

/// Outcome of the first frame read on an accepted session.
pub enum InboundRead {
    /// A real frame: continue with [`DeviceSession::dispatch_inbound`].
    Frame(Frame),
    /// The session ended before any payload frame; the event is already decided.
    Done(InboundEvent),
}

/// Bidirectional device session: one identity, fixed listen port, send + accept loop.
pub struct DeviceSession {
    pub identity: Arc<DeviceIdentity>,
    data_dir: PathBuf,
    peer: Option<TrustedPeer>,
    peer_addr: Option<SocketAddr>,
    /// Last known friendly name for the trusted peer (endpoint / discovery).
    peer_alias_saved: Option<String>,
    /// Wall-clock ms of last evidence that peer process is alive (beacon / transfer / pair).
    peer_alive_ms: Option<u64>,
    listener: Option<Arc<TcpListener>>,
    listen_port: u16,
    code: PairingCode,
    pairing: bool,
    transferring: TransferKind,
    /// TLS reachability probe in flight (must not look like a file transfer).
    probing: bool,
    /// Short clipboard push/pull in flight (orthogonal to file `transferring`).
    clip_inflight: bool,
    transfer_progress: Arc<ByteProgress>,
    /// 0 = idle; otherwise wall-clock ms when current transfer started (lock-free for UI).
    transfer_started_ms: Arc<AtomicU64>,
    /// 0 = idle, 1 = inbound, 2 = outbound (lock-free; pairs with `transfer_started_ms`).
    transfer_kind_live: Arc<AtomicU8>,
    /// Basename of active file (lock-free for UI while session mutex held).
    transfer_name: Arc<Mutex<String>>,
    discovery: Option<Arc<DiscoveryShare>>,
    /// Last observed local /24 for network-switch detection (clear nearby on change).
    last_local_subnet: Mutex<Option<(u8, u8, u8)>>,
    /// Background first-pair accept result (set by arm_pair_wait).
    pair_wait: Option<Arc<Mutex<Option<PairWaitOutcome>>>>,
    /// Metrics from the last successful outbound file send (probe + transfer).
    last_send: Option<SendFileResult>,
    /// Background heartbeat (Ping every [`HEARTBEAT_INTERVAL`]) for the current pair.
    heartbeat: Option<Arc<HeartbeatShare>>,
    /// Wall-clock ms of the last `Bye` from the peer (drives「对端已取消配对」).
    peer_left_ms: Option<u64>,
    /// Name of the peer that left, kept after trust is wiped so the banner can still name it.
    peer_left_label: Option<String>,
    /// Reason recorded for the current disconnect (cleared by the next authenticated frame).
    disconnect_reason: Option<DisconnectReason>,
}

/// Outcome of one outbound file send including same-session probe (85% bench).
#[derive(Debug, Clone, Copy)]
pub struct SendFileResult {
    pub bytes: u64,
    pub probe_bps: u64,
    pub probe_elapsed_us: u64,
    pub transfer_bps: u64,
    pub transfer_elapsed_us: u64,
}

enum PairWaitOutcome {
    Ok {
        peer: TrustedPeer,
        host: String,
        port: u16,
    },
    /// A pinned peer reconnected while we were waiting for a 6-digit code.
    Reconnect {
        peer: TrustedPeer,
        host: String,
        port: u16,
    },
    Err(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TransferKind {
    None,
    In,
    Out,
}

/// Dial target for the heartbeat task (cloned, so the session lock is never held while dialing).
#[derive(Clone)]
struct HeartbeatTarget {
    identity: Arc<DeviceIdentity>,
    peer: TrustedPeer,
    addr: SocketAddr,
}

/// Result of one heartbeat beat, drained by [`DeviceSession::poll_heartbeat`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HeartbeatOutcome {
    /// `Pong` arrived: the peer is authenticated and alive.
    Ok,
    /// Peer answered our dial with `Bye`.
    PeerLeft,
    /// Peer completed TLS then closed without a frame.
    Closed,
    /// Refused / timed out / protocol error: liveness now depends on the staleness window.
    Failed,
}

/// Shared state for the background heartbeat task.
struct HeartbeatShare {
    stop: AtomicBool,
    /// False while pairing / probing / transferring owns the listener.
    allowed: AtomicBool,
    target: Mutex<Option<HeartbeatTarget>>,
    outcome: Mutex<Option<HeartbeatOutcome>>,
}

impl DeviceSession {
    /// Load identity/trust/endpoint. Does not bind a port until [`go_online`].
    pub fn open(data_dir: PathBuf, device_id: &str, alias: &str) -> Result<Self> {
        std::fs::create_dir_all(&data_dir)?;
        let identity = load_or_create_identity(&data_dir, device_id, alias)?;
        let trust = load_trust(&data_dir);
        let endpoint = load_endpoint(&data_dir);
        let listen_port = load_listen_port(&data_dir).unwrap_or(DEFAULT_CONTROL_PORT);
        let (peer, peer_addr, peer_alias_saved) = if let Some(ep) = endpoint {
            let peer = trust
                .get(&ep.peer_device_id)
                .cloned()
                .or_else(|| trust.peers().first().cloned());
            let addr = format!("{}:{}", ep.host, ep.port).parse().ok();
            (peer, addr, ep.peer_alias)
        } else {
            (trust.peers().first().cloned(), None, None)
        };
        Ok(Self {
            identity,
            data_dir,
            peer,
            peer_addr,
            peer_alias_saved,
            peer_alive_ms: None,
            listener: None,
            listen_port,
            code: generate_pairing_code(),
            pairing: false,
            transferring: TransferKind::None,
            probing: false,
            clip_inflight: false,
            transfer_progress: Arc::new(ByteProgress::default()),
            transfer_started_ms: Arc::new(AtomicU64::new(0)),
            transfer_kind_live: Arc::new(AtomicU8::new(0)),
            transfer_name: Arc::new(Mutex::new(String::new())),
            discovery: None,
            last_local_subnet: Mutex::new(None),
            pair_wait: None,
            last_send: None,
            heartbeat: None,
            peer_left_ms: None,
            peer_left_label: None,
            disconnect_reason: None,
        })
    }

    /// Shared byte counters — readable while send/recv holds the session mutex.
    pub fn progress_handle(&self) -> Arc<ByteProgress> {
        self.transfer_progress.clone()
    }

    /// Shared start timestamp (0 = idle).
    pub fn started_ms_handle(&self) -> Arc<AtomicU64> {
        self.transfer_started_ms.clone()
    }

    /// Shared direction: 0 idle, 1 inbound, 2 outbound.
    pub fn kind_live_handle(&self) -> Arc<AtomicU8> {
        self.transfer_kind_live.clone()
    }

    /// Shared transfer basename for UI.
    pub fn name_live_handle(&self) -> Arc<Mutex<String>> {
        self.transfer_name.clone()
    }

    fn mark_transfer_start(&self, kind: TransferKind) {
        let k = match kind {
            TransferKind::In => 1u8,
            TransferKind::Out => 2u8,
            TransferKind::None => 0u8,
        };
        self.transfer_kind_live.store(k, Ordering::Relaxed);
        if let Ok(mut g) = self.transfer_name.lock() {
            g.clear();
        }
        self.transfer_started_ms.store(now_ms(), Ordering::Relaxed);
    }

    fn mark_transfer_end(&self) {
        self.transfer_started_ms.store(0, Ordering::Relaxed);
        self.transfer_kind_live.store(0, Ordering::Relaxed);
        if let Ok(mut g) = self.transfer_name.lock() {
            g.clear();
        }
        // Clear counters so UI does not keep showing "发送中" after completion.
        self.transfer_progress.reset(0);
    }

    /// Overlay live transfer fields onto a status snapshot (atomics; no session lock needed).
    pub fn apply_live_transfer(
        status: &mut SessionStatus,
        progress: &ByteProgress,
        started_ms: u64,
        kind: u8,
        name: &str,
    ) {
        if started_ms == 0 {
            status.transfer_done = 0;
            status.transfer_total = 0;
            status.transfer_bps = 0;
            status.transfer_name.clear();
            if matches!(
                status.state,
                SessionState::TransferringIn | SessionState::TransferringOut
            ) {
                // Stale cache while lock was held; transfer already finished.
                status.state = SessionState::Online;
            }
            return;
        }
        let (done, total) = progress.snapshot();
        status.transfer_done = done;
        status.transfer_total = total;
        status.transfer_name = name.to_string();
        let elapsed_ms = now_ms().saturating_sub(started_ms).max(1);
        status.transfer_bps = if total > 0 {
            done.saturating_mul(1000) / elapsed_ms
        } else {
            0
        };
        // Prefer live direction atom — cached status.state is often still Online while lock is held.
        status.state = match kind {
            1 => SessionState::TransferringIn,
            2 => SessionState::TransferringOut,
            _ if matches!(
                status.state,
                SessionState::TransferringIn | SessionState::TransferringOut
            ) =>
            {
                status.state
            }
            _ => SessionState::TransferringOut,
        };
    }

    fn touch_peer_alive(&mut self) {
        let now = now_ms();
        self.peer_alive_ms = Some(now);
        self.disconnect_reason = None;
        self.peer_left_ms = None;
        self.peer_left_label = None;
        self.note_peer_reachable_at(now);
    }

    /// Update discovery-side live stamp without `&mut self` (e.g. after TLS connect).
    fn note_peer_reachable_at(&self, now: u64) {
        if let Some(d) = &self.discovery {
            if let Ok(mut t) = d.peer_seen_ms.lock() {
                *t = Some(now);
            }
        }
    }

    /// Clear TLS/session live evidence so UI leaves「已连接」.
    /// Does not wipe trust; nearby beacons may still list the peer.
    pub fn expire_peer_session_live(&mut self) {
        self.peer_alive_ms = None;
        if let Some(d) = &self.discovery {
            if let Ok(mut t) = d.peer_seen_ms.lock() {
                *t = None;
            }
        }
    }

    /// Test helper: same as [`expire_peer_session_live`].
    pub fn debug_expire_peer_live(&mut self) {
        self.expire_peer_session_live();
        if let Some(d) = &self.discovery {
            if let Ok(mut map) = d.nearby.lock() {
                for n in map.values_mut() {
                    n.last_seen_ms = 0;
                }
            }
        }
    }

    /// True only when a recent **session** (TLS) interaction succeeded.
    /// UDP discovery must not fake「已连接」— one-sided trust + beacon caused that bug.
    fn peer_is_live(&self) -> bool {
        // An in-flight transfer is itself proof of life; heartbeat beats are gated off during it.
        if self.transfer_started_ms.load(Ordering::Relaxed) != 0 {
            return true;
        }
        let now = now_ms();
        if let Some(ms) = self.peer_alive_ms {
            if now.saturating_sub(ms) < PEER_LIVE_MS {
                return true;
            }
        }
        false
    }

    fn sync_peer_unicast_target(&self) {
        let Some(d) = &self.discovery else {
            return;
        };
        let ip = self.peer_addr.map(|a| a.ip());
        if let Ok(mut g) = d.peer_unicast.lock() {
            *g = ip;
        }
    }

    pub fn status(&self) -> SessionStatus {
        let paired = self.peer.is_some() && self.peer_addr.is_some();
        let listening = self.listener.is_some();
        let local_ipv4 = primary_lan_ipv4();
        // Router → hotspot (or any net change): old peer IP is on another /24.
        let subnet_mismatch = match (&local_ipv4, self.peer_addr) {
            (Some(local), Some(addr)) => {
                // Loopback peers are same-host tests / local tools — not a net switch.
                if addr.ip().is_loopback() {
                    false
                } else {
                    ipv4_slash24(local)
                        .zip(socket_slash24(addr))
                        .map(|(a, b)| a != b)
                        == Some(true)
                }
            }
            _ => false,
        };
        // Drop stale nearby + unicast targets when our /24 changes.
        if let Some(ref lip) = local_ipv4 {
            if let Some(sub) = ipv4_slash24(lip) {
                if let Ok(mut prev) = self.last_local_subnet.lock() {
                    match *prev {
                        Some(p) if p != sub => {
                            *prev = Some(sub);
                            if let Some(d) = &self.discovery {
                                if let Ok(mut map) = d.nearby.lock() {
                                    map.clear();
                                }
                                if let Ok(mut t) = d.peer_seen_ms.lock() {
                                    *t = None;
                                }
                                if let Ok(mut u) = d.peer_unicast.lock() {
                                    *u = None;
                                }
                            }
                        }
                        None => *prev = Some(sub),
                        _ => {}
                    }
                }
            }
        }
        let mut nearby = self.nearby_snapshot();
        // Surface trusted endpoint in list, but do NOT fake "just seen" after net switch.
        if let (Some(peer), Some(addr)) = (&self.peer, self.peer_addr) {
            if !nearby.iter().any(|n| n.device_id == peer.device_id) {
                let alias = self
                    .peer_alias_saved
                    .clone()
                    .or_else(|| {
                        self.discovery
                            .as_ref()
                            .and_then(|d| d.peer_alias.lock().ok().and_then(|g| g.clone()))
                    })
                    .unwrap_or_else(|| peer.device_id.clone());
                let last_seen_ms = if subnet_mismatch {
                    0
                } else {
                    self.peer_alive_ms
                        .or_else(|| {
                            self.discovery
                                .as_ref()
                                .and_then(|d| d.peer_seen_ms.lock().ok().and_then(|g| *g))
                        })
                        .unwrap_or(0)
                };
                nearby.insert(
                    0,
                    NearbyPeer {
                        device_id: peer.device_id.clone(),
                        alias,
                        host: peer_ip_string(addr),
                        port: addr.port(),
                        trusted: true,
                        last_seen_ms,
                    },
                );
            }
        }
        let peer_alias = self
            .peer_alias_saved
            .clone()
            .or_else(|| {
                self.discovery
                    .as_ref()
                    .and_then(|d| d.peer_alias.lock().ok().and_then(|g| g.clone()))
            })
            .or_else(|| {
                nearby
                    .iter()
                    .find(|n| {
                        Some(n.device_id.as_str())
                            == self.peer.as_ref().map(|p| p.device_id.as_str())
                    })
                    .map(|n| n.alias.clone())
            });
        let peer_label = peer_alias
            .clone()
            .or_else(|| self.peer.as_ref().map(|p| p.device_id.clone()))
            .unwrap_or_else(|| "对端".into());
        let peer_live = self.peer_is_live() && !subnet_mismatch;
        // 「已连接」= recent authenticated control traffic; a UDP beacon must never fake it.
        // A pending Bye reads as "disconnected" (banner:「对端已取消配对」), not "searching".
        let peer_left_pending = self.peer_left_ms.is_some();
        let connection = if self.pairing {
            "pairing".to_string()
        } else if peer_left_pending {
            "disconnected".to_string()
        } else if paired && listening && peer_live {
            "connected".to_string()
        } else if paired && listening {
            "disconnected".to_string()
        } else if listening {
            "searching".to_string()
        } else {
            "disconnected".to_string()
        };
        // 三态口径：已连接 / 对端离线 / 未配对。`unpaired` survives the trust wipe so the banner
        // can still say「对端已取消配对」instead of a bare「未配对」.
        let disconnect_reason = if peer_live {
            None
        } else if self.peer_left_ms.is_some() {
            Some(DisconnectReason::Unpaired)
        } else if !paired || !listening || self.pairing {
            None
        } else {
            Some(self.disconnect_reason.unwrap_or(DisconnectReason::Timeout))
        };
        let state = if self.transferring == TransferKind::In {
            SessionState::TransferringIn
        } else if self.transferring == TransferKind::Out {
            SessionState::TransferringOut
        } else if self.pairing {
            SessionState::Pairing
        } else if listening && peer_live {
            SessionState::Online
        } else if paired && listening {
            SessionState::Disconnected
        } else if listening {
            SessionState::Online
        } else if self.peer.is_some() {
            SessionState::PairedOffline
        } else {
            SessionState::Unpaired
        };
        let left_label = self
            .peer_left_label
            .clone()
            .unwrap_or_else(|| peer_label.clone());
        let banner = match connection.as_str() {
            "connected" => format!("已连接 · {peer_label}"),
            "pairing" => format!("等待对方填码 · {}", self.code.as_str()),
            "searching" => "等待配对：让对方填本机码，或填对方的 6 位码".into(),
            _ => {
                if paired && listening && subnet_mismatch {
                    format!(
                        "网络已切换 · {peer_label} 仍是旧网段 · 请重试连接或清除配对后手填新 IP"
                    )
                } else if disconnect_reason == Some(DisconnectReason::Unpaired) {
                    format!("对端已取消配对 · {left_label}（请重新配对）")
                } else if paired && listening {
                    // UDP often filtered on SoftAP; TCP may still work — auto/manual probe.
                    format!("对端离线 · {peer_label}（将自动重试 / 可点重试连接）")
                } else if paired {
                    "本机未上线（打开应用会自动上线）".into()
                } else {
                    "未配对".into()
                }
            }
        };
        let started = self.transfer_started_ms.load(Ordering::Relaxed);
        let transfer_name = if started > 0 {
            self.transfer_name
                .lock()
                .ok()
                .map(|g| g.clone())
                .unwrap_or_default()
        } else {
            String::new()
        };
        let (transfer_done, transfer_total, transfer_bps) =
            if self.transferring != TransferKind::None && started > 0 {
                let (done, total) = self.transfer_progress.snapshot();
                let elapsed_ms = now_ms().saturating_sub(started).max(1);
                let bps = if total > 0 {
                    done.saturating_mul(1000) / elapsed_ms
                } else {
                    0
                };
                (done, total, bps)
            } else {
                (0, 0, 0)
            };
        SessionStatus {
            state,
            paired,
            listen_port: self.listen_port,
            listening,
            pairing_code: if self.pairing || (!paired && listening) {
                Some(self.code.as_str().to_string())
            } else {
                None
            },
            peer_device_id: self.peer.as_ref().map(|p| p.device_id.clone()),
            peer_alias,
            peer_host: self.peer_addr.map(peer_ip_string),
            peer_port: self.peer_addr.map(|a| a.port()),
            device_id: self.identity.device_id.clone(),
            alias: self.identity.alias.clone(),
            local_ipv4,
            connection,
            disconnect_reason,
            nearby,
            banner,
            transfer_done,
            transfer_total,
            transfer_bps,
            transfer_name,
            last_send_probe_bps: self.last_send.map(|s| s.probe_bps).unwrap_or(0),
            last_send_probe_elapsed_us: self.last_send.map(|s| s.probe_elapsed_us).unwrap_or(0),
            last_send_transfer_bps: self.last_send.map(|s| s.transfer_bps).unwrap_or(0),
            last_send_transfer_elapsed_us: self
                .last_send
                .map(|s| s.transfer_elapsed_us)
                .unwrap_or(0),
            last_send_bytes: self.last_send.map(|s| s.bytes).unwrap_or(0),
        }
    }

    fn nearby_snapshot(&self) -> Vec<NearbyPeer> {
        let Some(disc) = &self.discovery else {
            return Vec::new();
        };
        let Ok(map) = disc.nearby.lock() else {
            return Vec::new();
        };
        let mut v: Vec<_> = map.values().cloned().collect();
        v.sort_by(|a, b| a.alias.cmp(&b.alias));
        v
    }

    /// Bind fixed (or override) port. `port == 0` → saved/default 37532.
    /// Retries briefly on `AddrInUse` (stale process dying / TIME_WAIT race).
    pub async fn go_online(&mut self, port: u16) -> Result<u16> {
        if self.listener.is_some() {
            return Ok(self.listen_port);
        }
        let bind_port = if port == 0 { self.listen_port } else { port };
        let addr = SocketAddr::from((Ipv4Addr::UNSPECIFIED, bind_port));
        let mut last_in_use = false;
        let listener = {
            let mut bound: Option<TcpListener> = None;
            for attempt in 0..8u32 {
                match TcpListener::bind(addr).await {
                    Ok(l) => {
                        bound = Some(l);
                        break;
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => {
                        last_in_use = true;
                        if attempt + 1 < 8 {
                            tokio::time::sleep(std::time::Duration::from_millis(
                                80 + u64::from(attempt) * 40,
                            ))
                            .await;
                            continue;
                        }
                    }
                    Err(e) => return Err(Error::Io(e)),
                }
            }
            match bound {
                Some(l) => l,
                None if last_in_use => {
                    return Err(Error::Session(lb_session::Error::Protocol("port_in_use")));
                }
                None => {
                    return Err(Error::Session(lb_session::Error::Protocol("port_in_use")));
                }
            }
        };
        let actual = listener.local_addr()?.port();
        self.listen_port = actual;
        save_listen_port(&self.data_dir, actual)?;
        self.listener = Some(Arc::new(listener));
        self.pairing = false;
        self.start_discovery_loop();
        self.start_heartbeat_loop();
        self.sync_heartbeat_target();
        tracing::info!(port = actual, "session online");
        Ok(actual)
    }

    pub fn go_offline(&mut self) {
        self.stop_discovery_loop();
        self.stop_heartbeat_loop();
        self.listener = None;
        self.pairing = false;
        self.pair_wait = None;
        self.transferring = TransferKind::None;
        self.disconnect_reason = None;
        tracing::info!("session offline");
    }

    /// Forget trusted peer + endpoint so both ends can pair again with a fresh code.
    pub fn clear_pair(&mut self) -> Result<()> {
        self.peer = None;
        self.peer_addr = None;
        self.peer_alias_saved = None;
        self.peer_alive_ms = None;
        self.pairing = false;
        self.pair_wait = None;
        self.transferring = TransferKind::None;
        if let Some(d) = &self.discovery {
            if let Ok(mut t) = d.trusted_id.lock() {
                *t = None;
            }
            if let Ok(mut a) = d.peer_alias.lock() {
                *a = None;
            }
            if let Ok(mut s) = d.peer_seen_ms.lock() {
                *s = None;
            }
            if let Ok(mut u) = d.peer_unicast.lock() {
                *u = None;
            }
            if let Ok(mut map) = d.nearby.lock() {
                for n in map.values_mut() {
                    n.trusted = false;
                }
            }
        }
        save_trust(&self.data_dir, &TrustStore::new())?;
        let _ = std::fs::remove_file(endpoint_path(&self.data_dir));
        self.code = generate_pairing_code();
        self.disconnect_reason = None;
        self.sync_heartbeat_target();
        tracing::info!("trust cleared locally");
        Ok(())
    }

    /// Unpair by telling the peer first: Bye, flush, close, then wipe local trust.
    ///
    /// Without this the peer keeps「已连接」until the heartbeat window expires — the one-sided
    /// unpair case (one side cleared trust, the other still showed「已连接」).
    pub async fn unpair(&mut self) -> Result<()> {
        let peer = self.peer.as_ref().map(|p| p.device_id.clone());
        self.send_bye_to_peer().await;
        self.clear_pair()?;
        self.peer_left_ms = None;
        self.peer_left_label = None;
        tracing::info!(
            peer = peer.as_deref().unwrap_or("-"),
            "unpair: bye sent, trust cleared"
        );
        Ok(())
    }

    /// Best-effort Bye on a fresh trusted session.
    ///
    /// Errors are ignored: local trust is removed either way, and an unreachable peer falls back
    /// to the heartbeat timeout. Closing **after** the bye is what lets the peer tell a goodbye
    /// from a crash.
    async fn send_bye_to_peer(&self) {
        let (Some(peer), Some(addr)) = (self.peer.clone(), self.peer_addr) else {
            return;
        };
        let dial = timeout(
            UNPAIR_BYE_WAIT,
            connect_trusted(self.identity.clone(), addr, &peer),
        )
        .await;
        if let Ok(Ok(mut session)) = dial {
            let _ = crate::send_bye(&mut session).await;
        }
    }

    /// Peer said Bye: drop trust locally too, and remember who left for the banner.
    fn note_peer_left(&mut self) {
        let label = self
            .peer_alias_saved
            .clone()
            .or_else(|| self.peer.as_ref().map(|p| p.device_id.clone()))
            .or_else(|| {
                self.discovery
                    .as_ref()
                    .and_then(|d| d.peer_alias.lock().ok().and_then(|g| g.clone()))
            });
        let _ = self.clear_pair();
        tracing::warn!(
            peer = label.as_deref().unwrap_or("-"),
            "peer left (bye): trust dropped"
        );
        self.peer_left_ms = Some(now_ms());
        self.peer_left_label = label;
        self.disconnect_reason = Some(DisconnectReason::Unpaired);
    }

    fn heartbeat_target(&self) -> Option<HeartbeatTarget> {
        match (self.peer.clone(), self.peer_addr) {
            (Some(peer), Some(addr)) => Some(HeartbeatTarget {
                identity: self.identity.clone(),
                peer,
                addr,
            }),
            _ => None,
        }
    }

    /// Point the heartbeat task at the current peer (it idles when unpaired).
    fn sync_heartbeat_target(&mut self) {
        self.sync_heartbeat_gate();
        let target = self.heartbeat_target();
        if let Some(share) = self.heartbeat.clone() {
            if let Ok(mut g) = share.target.lock() {
                *g = target;
            }
        }
    }

    /// Pause heartbeats while pairing / probing / transferring owns the listener.
    fn sync_heartbeat_gate(&self) {
        let Some(share) = &self.heartbeat else {
            return;
        };
        let busy = self.pairing || self.probing || self.transferring != TransferKind::None;
        share.allowed.store(!busy, Ordering::Relaxed);
    }

    fn start_heartbeat_loop(&mut self) {
        self.stop_heartbeat_loop();
        let share = Arc::new(HeartbeatShare {
            stop: AtomicBool::new(false),
            allowed: AtomicBool::new(true),
            target: Mutex::new(self.heartbeat_target()),
            outcome: Mutex::new(None),
        });
        // Same rule as the discovery loop: FFI/NAPI threads are not on the Tokio runtime.
        let Ok(handle) = tokio::runtime::Handle::try_current() else {
            tracing::warn!("heartbeat spawn skipped: no tokio runtime");
            return;
        };
        let worker = share.clone();
        handle.spawn(async move { heartbeat_loop(worker).await });
        self.heartbeat = Some(share);
    }

    fn stop_heartbeat_loop(&mut self) {
        if let Some(share) = self.heartbeat.take() {
            share.stop.store(true, Ordering::Relaxed);
        }
    }

    /// Apply a finished heartbeat beat. Call from every mutable status poll.
    pub fn poll_heartbeat(&mut self) {
        self.sync_heartbeat_gate();
        let Some(share) = self.heartbeat.clone() else {
            return;
        };
        let outcome = {
            let Ok(mut g) = share.outcome.lock() else {
                return;
            };
            g.take()
        };
        match outcome {
            None => {}
            Some(HeartbeatOutcome::Ok) => {
                tracing::debug!("heartbeat ok");
                self.touch_peer_alive();
            }
            Some(HeartbeatOutcome::PeerLeft) => self.note_peer_left(),
            Some(HeartbeatOutcome::Closed) => {
                // TLS completed, then the peer hung up without a frame: its serve loop is gone.
                tracing::warn!("heartbeat: peer closed the session without a frame");
                self.expire_peer_session_live();
                self.disconnect_reason = Some(DisconnectReason::Closed);
            }
            Some(HeartbeatOutcome::Failed) => {
                // Half-open / unreachable: liveness goes stale within PEER_LIVE_MS.
                tracing::debug!("heartbeat failed (half-open / busy / refusing)");
            }
        }
    }

    /// Test hook: age the last liveness evidence instead of sleeping the whole window.
    pub fn debug_age_peer_evidence(&mut self, age_ms: u64) {
        if let Some(ms) = self.peer_alive_ms {
            self.peer_alive_ms = Some(ms.saturating_sub(age_ms));
        }
    }

    fn start_discovery_loop(&mut self) {
        self.stop_discovery_loop();
        let share = Arc::new(DiscoveryShare {
            nearby: Mutex::new(HashMap::new()),
            stop: AtomicBool::new(false),
            peer_alias: Mutex::new(self.peer_alias_saved.clone()),
            peer_seen_ms: Mutex::new(self.peer_alive_ms),
            trusted_id: Mutex::new(self.peer.as_ref().map(|p| p.device_id.clone())),
            peer_unicast: Mutex::new(self.peer_addr.map(|a| a.ip())),
        });
        self.discovery = Some(share.clone());
        let self_info = PeerInfo {
            device_id: self.identity.device_id.clone(),
            alias: self.identity.alias.clone(),
            addr: SocketAddr::from((Ipv4Addr::UNSPECIFIED, self.listen_port)),
        };
        let session_port = self.listen_port;
        let Ok(handle) = tokio::runtime::Handle::try_current() else {
            tracing::error!("discovery spawn skipped: no tokio runtime");
            return;
        };
        handle.spawn(async move {
            let cfg = UdpBeaconConfig::lan(session_port);
            let disc = match UdpDiscoverer::bind(cfg).await {
                Ok(d) => d,
                Err(e) => {
                    tracing::warn!(error = %e, "discovery bind failed");
                    return;
                }
            };
            while !share.stop.load(Ordering::Relaxed) {
                let _ = disc.advertise_lan(&self_info).await;
                // Unicast heartbeat to known peer + reply to every recently seen nearby
                // host. SoftAP/hotspot often drops client→host broadcast; unicast works.
                let mut unicast_ips: Vec<std::net::IpAddr> = Vec::new();
                if let Some(ip) = share.peer_unicast.lock().ok().and_then(|g| *g) {
                    unicast_ips.push(ip);
                }
                if let Ok(map) = share.nearby.lock() {
                    for n in map.values() {
                        if let Ok(ip) = n.host.parse::<std::net::IpAddr>() {
                            if !unicast_ips.contains(&ip) {
                                unicast_ips.push(ip);
                            }
                        }
                    }
                }
                for ip in unicast_ips {
                    let dest = SocketAddr::new(ip, DEFAULT_DISCOVERY_PORT);
                    let _ = disc.advertise_to(&self_info, dest).await;
                }
                match disc.discover(Duration::from_millis(900)).await {
                    Ok(peers) => {
                        let trusted_id = share.trusted_id.lock().ok().and_then(|g| g.clone());
                        let mut map = match share.nearby.lock() {
                            Ok(m) => m,
                            Err(_) => break,
                        };
                        let now = now_ms();
                        for p in peers {
                            if p.device_id == self_info.device_id {
                                continue;
                            }
                            let trusted = trusted_id.as_ref() == Some(&p.device_id);
                            if trusted {
                                if let Ok(mut a) = share.peer_alias.lock() {
                                    *a = Some(p.alias.clone());
                                }
                                if let Ok(mut t) = share.peer_seen_ms.lock() {
                                    *t = Some(now);
                                }
                            }
                            map.insert(
                                p.device_id.clone(),
                                NearbyPeer {
                                    device_id: p.device_id,
                                    alias: p.alias,
                                    host: peer_ip_string(p.addr),
                                    port: p.addr.port(),
                                    trusted,
                                    last_seen_ms: now,
                                },
                            );
                        }
                        map.retain(|_, n| now.saturating_sub(n.last_seen_ms) < 20_000);
                    }
                    Err(e) => tracing::debug!(error = %e, "discover tick"),
                }
                tokio::time::sleep(Duration::from_millis(400)).await;
            }
        });
    }

    fn stop_discovery_loop(&mut self) {
        if let Some(d) = self.discovery.take() {
            d.stop.store(true, Ordering::Relaxed);
        }
    }

    /// Apply beacon address for the trusted peer when seen (silent reconnect).
    pub fn apply_discovery_endpoints(&mut self) {
        let Some(peer) = self.peer.clone() else {
            return;
        };
        let Some(disc) = &self.discovery else {
            return;
        };
        let Ok(map) = disc.nearby.lock() else {
            return;
        };
        if let Some(n) = map.get(&peer.device_id) {
            if let Ok(addr) = format!("{}:{}", n.host, n.port).parse::<SocketAddr>() {
                self.peer_addr = Some(addr);
                let alias = if !n.alias.is_empty() {
                    Some(n.alias.clone())
                } else {
                    None
                };
                let host = n.host.clone();
                let port = n.port;
                drop(map);
                if let Some(a) = alias {
                    self.remember_peer_alias(&a);
                } else {
                    let _ = save_endpoint(
                        &self.data_dir,
                        &host,
                        port,
                        &peer.device_id,
                        self.peer_alias_saved.as_deref(),
                    );
                }
                // A UDP beacon updates the endpoint but is NOT liveness evidence: that was the
                // 「空闲也显示已连接」bug. Only authenticated control traffic keeps the link green.
                self.sync_peer_unicast_target();
                self.sync_heartbeat_target();
            }
        }
    }

    /// Pair using a discovered nearby device (no manual IP).
    pub async fn pair_nearby(&mut self, device_id: &str, code: &str) -> Result<TrustedPeer> {
        self.apply_discovery_endpoints();
        let nearby = self.nearby_snapshot();
        let target = nearby
            .iter()
            .find(|n| n.device_id == device_id)
            .ok_or(Error::Session(lb_session::Error::Protocol(
                "peer_not_found",
            )))?;
        let host = target.host.clone();
        let port = target.port;
        self.pair(&host, port, code).await
    }

    /// Non-blocking: wait for inbound pair in background using the **current** pairing code.
    ///
    /// Does not remint — UI must show [`status`].pairing_code / [`pairing_code`] first.
    /// Mint only via [`mint_pairing_code`] / [`clear_pair`] / successful pair.
    pub fn arm_pair_wait(&mut self) -> Result<String> {
        let listener = self
            .listener
            .clone()
            .ok_or(Error::Session(lb_session::Error::Protocol("not_online")))?;
        if self.peer.is_some() {
            return Ok(self.code.as_str().to_string());
        }
        if self.pairing {
            return Ok(self.code.as_str().to_string());
        }
        let code_str = self.code.as_str().to_string();
        self.pairing = true;
        let slot = Arc::new(Mutex::new(None));
        self.pair_wait = Some(slot.clone());
        let identity = self.identity.clone();
        let code = self.code.clone();
        // Already-pinned peers must be recognised while we wait for the code, otherwise a
        // reconnecting phone is rejected until the window closes ("PC 连接中 / 手机已连接").
        let trusted = load_trust(&self.data_dir).peers().to_vec();
        // Prefer Handle::try_current — bare tokio::spawn aborts the process when
        // called from FFI/NAPI worker threads outside a runtime (OH launch crash).
        let handle = tokio::runtime::Handle::try_current()
            .map_err(|_| Error::Session(lb_session::Error::Protocol("no_tokio_runtime")))?;
        handle.spawn(async move {
            let result = timeout(
                PAIR_WAIT,
                await_host_pair(identity, listener.as_ref(), &code, &trusted),
            )
            .await;
            let outcome = match result {
                Ok(Ok(HostPairOutcome::Paired(peer, _session, sock))) => {
                    let host = peer_ip_string(sock);
                    PairWaitOutcome::Ok {
                        peer,
                        host,
                        port: DEFAULT_CONTROL_PORT,
                    }
                }
                Ok(Ok(HostPairOutcome::Reconnect(peer, sock))) => {
                    let host = peer_ip_string(sock);
                    PairWaitOutcome::Reconnect {
                        peer,
                        host,
                        port: DEFAULT_CONTROL_PORT,
                    }
                }
                Ok(Err(e)) => PairWaitOutcome::Err(e.to_string()),
                Err(_) => PairWaitOutcome::Err("pair_timeout".into()),
            };
            if let Ok(mut g) = slot.lock() {
                *g = Some(outcome);
            }
        });
        Ok(code_str)
    }

    /// Apply finished background pair if ready. Returns error string if pair failed.
    pub fn poll_pair_wait(&mut self) -> Option<std::result::Result<(), String>> {
        let slot = self.pair_wait.clone()?;
        let outcome = {
            let mut g = slot.lock().ok()?;
            g.take()?
        };
        self.pairing = false;
        self.pair_wait = None;
        match outcome {
            PairWaitOutcome::Ok { peer, host, port } => {
                let mut store = TrustStore::new();
                store.insert(peer.clone());
                let _ = save_trust(&self.data_dir, &store);
                let _ = save_endpoint(&self.data_dir, &host, port, &peer.device_id, None);
                self.peer = Some(peer.clone());
                if let Ok(ip) = host.parse::<std::net::IpAddr>() {
                    self.peer_addr = Some(SocketAddr::new(ip, port));
                }
                self.set_trusted_discovery_id(&peer.device_id);
                self.touch_peer_alive();
                self.sync_peer_unicast_target();
                self.code = generate_pairing_code();
                self.sync_heartbeat_target();
                tracing::info!(peer = %peer.device_id, "paired as host");
                Some(Ok(()))
            }
            PairWaitOutcome::Reconnect { peer, host, port } => {
                // Trust is already pinned: only the endpoint needs re-binding.
                let _ = save_endpoint(&self.data_dir, &host, port, &peer.device_id, None);
                self.peer = Some(peer.clone());
                if let Ok(ip) = host.parse::<std::net::IpAddr>() {
                    self.peer_addr = Some(SocketAddr::new(ip, port));
                }
                self.set_trusted_discovery_id(&peer.device_id);
                self.touch_peer_alive();
                self.sync_peer_unicast_target();
                self.sync_heartbeat_target();
                tracing::info!(peer = %peer.device_id, "trusted peer reconnected");
                Some(Ok(()))
            }
            PairWaitOutcome::Err(e) => Some(Err(e)),
        }
    }

    /// Wait for peer with pairing code; persist trust + peer endpoint (peer IP, default control port).
    ///
    /// Prefer [`arm_pair_wait`] + [`poll_pair_wait`] in UI shells so status stays live.
    pub async fn begin_pair(&mut self) -> Result<TrustedPeer> {
        let _ = self.arm_pair_wait()?;
        loop {
            tokio::time::sleep(Duration::from_millis(200)).await;
            match self.poll_pair_wait() {
                None => {
                    if !self.pairing {
                        return Err(Error::Session(lb_session::Error::Protocol("pair_timeout")));
                    }
                }
                Some(Ok(())) => {
                    return self
                        .peer
                        .clone()
                        .ok_or(Error::Session(lb_session::Error::Protocol("pair_failed")));
                }
                Some(Err(e)) => {
                    let static_msg: &'static str = if e.contains("timeout") {
                        "pair_timeout"
                    } else {
                        "pair_failed"
                    };
                    return Err(Error::Session(lb_session::Error::Protocol(static_msg)));
                }
            }
        }
    }

    /// Mint a new pairing code for the next [`begin_pair`] wait window.
    pub fn mint_pairing_code(&mut self) -> String {
        self.code = generate_pairing_code();
        self.code.as_str().to_string()
    }

    pub fn pairing_code(&self) -> String {
        self.code.as_str().to_string()
    }

    /// Connect to peer with code; persist trust + endpoint.
    pub async fn pair(&mut self, host: &str, port: u16, code: &str) -> Result<TrustedPeer> {
        let code = PairingCode::parse(code)?;
        let port = if port == 0 {
            DEFAULT_CONTROL_PORT
        } else {
            port
        };
        let addr: SocketAddr = format!("{host}:{port}")
            .parse()
            .map_err(|_| Error::Session(lb_session::Error::Protocol("bad host/port")))?;
        let (peer, _session) = match timeout(
            GUEST_PAIR_WAIT,
            pair_as_guest(self.identity.clone(), addr, &code),
        )
        .await
        {
            Ok(Ok(v)) => v,
            Ok(Err(e)) => return Err(Error::Session(e)),
            Err(_) => {
                return Err(Error::Session(lb_session::Error::Protocol("pair_timeout")));
            }
        };
        let mut store = TrustStore::new();
        store.insert(peer.clone());
        save_trust(&self.data_dir, &store)?;
        save_endpoint(
            &self.data_dir,
            host,
            port,
            &peer.device_id,
            self.peer_alias_saved.as_deref(),
        )?;
        self.peer = Some(peer.clone());
        self.peer_addr = Some(addr);
        self.set_trusted_discovery_id(&peer.device_id);
        self.touch_peer_alive();
        self.sync_peer_unicast_target();
        self.sync_heartbeat_target();
        tracing::info!(peer = %peer.device_id, host, port, "paired as guest");
        Ok(peer)
    }

    fn set_trusted_discovery_id(&self, id: &str) {
        if let Some(d) = &self.discovery {
            if let Ok(mut g) = d.trusted_id.lock() {
                *g = Some(id.to_string());
            }
        }
    }

    fn remember_peer_alias(&mut self, alias: &str) {
        let a = alias.trim();
        if a.is_empty() {
            return;
        }
        if self.peer_alias_saved.as_deref() == Some(a) {
            return;
        }
        self.peer_alias_saved = Some(a.to_string());
        if let (Some(peer), Some(addr)) = (&self.peer, self.peer_addr) {
            let _ = save_endpoint(
                &self.data_dir,
                &peer_ip_string(addr),
                addr.port(),
                &peer.device_id,
                Some(a),
            );
        }
        if let Some(d) = &self.discovery {
            if let Ok(mut g) = d.peer_alias.lock() {
                *g = Some(a.to_string());
            }
        }
    }

    pub fn set_peer(&mut self, host: &str, port: u16) -> Result<()> {
        let peer_id = self
            .peer
            .as_ref()
            .map(|p| p.device_id.clone())
            .ok_or(Error::Session(lb_session::Error::Protocol("not_paired")))?;
        let port = if port == 0 {
            DEFAULT_CONTROL_PORT
        } else {
            port
        };
        let addr: SocketAddr = format!("{host}:{port}")
            .parse()
            .map_err(|_| Error::Session(lb_session::Error::Protocol("bad host/port")))?;
        save_endpoint(
            &self.data_dir,
            host,
            port,
            &peer_id,
            self.peer_alias_saved.as_deref(),
        )?;
        self.peer_addr = Some(addr);
        self.sync_peer_unicast_target();
        self.sync_heartbeat_target();
        Ok(())
    }

    /// Begin reachability probe; caller should release the session lock, run
    /// [`execute_probe`], then [`finish_probe`].
    pub fn prepare_probe(&mut self) -> Result<ProbeWork> {
        self.apply_discovery_endpoints();
        if self.peer.is_none() || self.peer_addr.is_none() {
            return Err(Error::Session(lb_session::Error::Protocol("not_paired")));
        }
        if self.listener.is_none() {
            return Err(Error::Session(lb_session::Error::Protocol("not_online")));
        }
        if self.transferring != TransferKind::None || self.pairing || self.probing {
            return Err(Error::Session(lb_session::Error::Protocol("busy")));
        }
        self.probing = true;
        Ok(ProbeWork {
            identity: self.identity.clone(),
            peer: self.peer.clone().expect("checked"),
            peer_addr: self.peer_addr.expect("checked"),
        })
    }

    /// TLS connect + soft-close; safe without session mutex.
    pub async fn execute_probe(work: ProbeWork) -> Result<()> {
        let session = match timeout(
            PROBE_CONNECT_WAIT,
            connect_trusted(work.identity, work.peer_addr, &work.peer),
        )
        .await
        {
            Ok(Ok(s)) => s,
            Ok(Err(_)) | Err(_) => {
                return Err(Error::Session(lb_session::Error::Protocol(
                    "peer_unreachable",
                )));
            }
        };
        let mut session = session;
        let _ = session.shutdown().await;
        Ok(())
    }

    /// Clear probing flag; mark live or expire based on result.
    pub fn finish_probe(&mut self, ok: bool) {
        tracing::debug!(ok, "reachability probe finished");
        self.probing = false;
        if ok {
            self.touch_peer_alive();
        } else {
            self.expire_peer_session_live();
        }
    }

    /// Convenience for tests / single-threaded callers (holds `&mut self` during connect).
    pub async fn probe_peer(&mut self) -> Result<()> {
        let work = self.prepare_probe()?;
        let result = Self::execute_probe(work).await;
        self.finish_probe(result.is_ok());
        result
    }

    pub async fn send_file(&mut self, local: &Path, remote_name: &str) -> Result<u64> {
        let work = self.prepare_send_file(local, remote_name)?;
        let result = Self::execute_send_file(work).await;
        self.finish_transfer(result.is_ok());
        if let Ok(ref r) = result {
            self.record_last_send(*r);
        }
        result.map(|r| r.bytes)
    }

    /// Begin outbound file transfer bookkeeping; caller should release session lock
    /// before [`execute_send_file`], then call [`finish_transfer`].
    pub fn prepare_send_file(
        &mut self,
        local: &Path,
        remote_name: &str,
    ) -> Result<OutboundFileWork> {
        if self.transferring != TransferKind::None {
            return Err(Error::Session(lb_session::Error::Protocol("busy")));
        }
        self.apply_discovery_endpoints();
        let peer = self
            .peer
            .clone()
            .ok_or(Error::Session(lb_session::Error::Protocol("not_paired")))?;
        let peer_addr = self
            .peer_addr
            .ok_or(Error::Session(lb_session::Error::Protocol("no_endpoint")))?;
        if !local.is_file() {
            return Err(Error::Session(lb_session::Error::Protocol("not_a_file")));
        }
        self.transferring = TransferKind::Out;
        self.mark_transfer_start(TransferKind::Out);
        tracing::info!("outbound transfer start");
        self.transfer_progress.reset(0);
        if let Ok(meta) = std::fs::metadata(local) {
            if meta.is_file() {
                self.transfer_progress.reset(meta.len());
                if let Some(name) = local.file_name().and_then(|s| s.to_str()) {
                    if let Ok(mut g) = self.transfer_name.lock() {
                        *g = name.to_string();
                    }
                }
            }
        }
        let remote = if remote_name.trim().is_empty() {
            local
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or("file.bin")
                .replace('\\', "/")
        } else {
            remote_name.trim().replace('\\', "/")
        };
        Ok(OutboundFileWork {
            local: local.to_path_buf(),
            remote,
            progress: self.transfer_progress.clone(),
            live_name: self.transfer_name.clone(),
            identity: self.identity.clone(),
            peer,
            peer_addr,
        })
    }

    /// Data-plane send; safe without session mutex (progress atomics update live).
    pub async fn execute_send_file(work: OutboundFileWork) -> Result<SendFileResult> {
        let session = match timeout(
            CONNECT_OUT_WAIT,
            connect_trusted(work.identity.clone(), work.peer_addr, &work.peer),
        )
        .await
        {
            Ok(Ok(s)) => s,
            Ok(Err(_)) | Err(_) => {
                return Err(Error::Session(lb_session::Error::Protocol(
                    "peer_unreachable",
                )));
            }
        };
        let mut link = establish_sender(
            session,
            work.peer_addr,
            work.identity,
            &work.peer,
            DEFAULT_PROBE_BYTES,
        )
        .await?;
        let plan = *link.plan();
        let started = Instant::now();
        let summary = send_file(
            &mut link,
            &work.local,
            &SendOptions {
                remote_path: work.remote,
                progress: Some(work.progress),
                live_name: Some(work.live_name),
            },
        )
        .await?;
        let transfer_elapsed_us = started.elapsed().as_micros().max(1) as u64;
        let transfer_bps =
            summary.bytes_transferred.saturating_mul(1_000_000) / transfer_elapsed_us;
        let _ = shutdown_link(&mut link).await;
        Ok(SendFileResult {
            bytes: summary.bytes_transferred,
            probe_bps: plan.probe_bps,
            probe_elapsed_us: plan.probe_elapsed_us,
            transfer_bps,
            transfer_elapsed_us,
        })
    }

    /// Clear transferring flags after unlocked send/recv completes.
    pub fn finish_transfer(&mut self, ok: bool) {
        let (done, total) = self.transfer_progress.snapshot();
        let inbound = self.transfer_kind_live.load(Ordering::Relaxed) == 1;
        self.transferring = TransferKind::None;
        self.mark_transfer_end();
        if ok {
            tracing::info!(inbound, bytes = done, total, "transfer finished");
            self.touch_peer_alive();
        } else {
            tracing::warn!(inbound, bytes = done, total, "transfer failed");
        }
    }

    pub fn record_last_send(&mut self, result: SendFileResult) {
        self.last_send = Some(result);
    }

    pub fn prepare_send_clipboard(&mut self, item: ClipboardItem) -> Result<ClipboardOutWork> {
        if self.pairing || self.clip_inflight {
            return Err(Error::Session(lb_session::Error::Protocol("busy")));
        }
        self.apply_discovery_endpoints();
        let peer = self
            .peer
            .clone()
            .ok_or(Error::Session(lb_session::Error::Protocol("not_paired")))?;
        let peer_addr = self
            .peer_addr
            .ok_or(Error::Session(lb_session::Error::Protocol("not_paired")))?;
        self.clip_inflight = true;
        Ok(ClipboardOutWork {
            identity: self.identity.clone(),
            peer,
            peer_addr,
            item,
        })
    }

    pub async fn execute_send_clipboard(work: ClipboardOutWork) -> Result<()> {
        let session = match timeout(
            CONNECT_OUT_WAIT,
            connect_trusted(work.identity, work.peer_addr, &work.peer),
        )
        .await
        {
            Ok(Ok(s)) => s,
            Ok(Err(_)) | Err(_) => {
                return Err(Error::Session(lb_session::Error::Protocol(
                    "peer_unreachable",
                )));
            }
        };
        let mut session = session;
        push_clipboard(&mut session, &work.item).await?;
        let _ = session.shutdown().await;
        Ok(())
    }

    pub fn finish_send_clipboard(&mut self, ok: bool) {
        self.clip_inflight = false;
        if ok {
            self.touch_peer_alive();
        }
    }

    pub async fn send_clipboard(&mut self, item: ClipboardItem) -> Result<()> {
        let bytes = match &item {
            ClipboardItem::Text(t) => t.len(),
            ClipboardItem::ImagePng(p) => p.len(),
        };
        let work = self.prepare_send_clipboard(item)?;
        let result = Self::execute_send_clipboard(work).await;
        self.finish_send_clipboard(result.is_ok());
        tracing::debug!(bytes, ok = result.is_ok(), "clipboard sent");
        result
    }

    /// Mark inbound clipboard; release session lock before [`execute_clip_inbound`].
    pub fn prepare_clip_inbound(&mut self) -> Result<()> {
        if self.pairing || self.clip_inflight || self.transferring != TransferKind::None {
            return Err(Error::Session(lb_session::Error::Protocol("busy")));
        }
        self.clip_inflight = true;
        Ok(())
    }

    pub async fn execute_clip_inbound(
        mut session: crate::ControlSession,
        frame: Frame,
    ) -> Result<InboundEvent> {
        session.push_frame(frame);
        let (_seq, item) =
            match timeout(Duration::from_secs(30), recv_clipboard(&mut session)).await {
                Ok(Ok(v)) => v,
                Ok(Err(e)) => return Err(Error::Clipboard(e)),
                Err(_) => {
                    return Err(Error::Session(lb_session::Error::Protocol(
                        "clipboard recv timeout",
                    )));
                }
            };
        Ok(match item {
            ClipboardItem::Text(t) => InboundEvent::ClipboardText {
                bytes: t.len(),
                text: t,
            },
            ClipboardItem::ImagePng(p) => InboundEvent::ClipboardPng {
                bytes: p.len(),
                png_base64: base64_encode(&p),
            },
        })
    }

    pub fn finish_clip_inbound(&mut self, ok: bool) {
        self.clip_inflight = false;
        if ok {
            self.touch_peer_alive();
        }
    }

    /// Accept one inbound trusted transfer (file or clipboard). Shell loops while online.
    pub async fn serve_once(&mut self, dest_root: PathBuf) -> Result<InboundEvent> {
        let parts = self.take_serve_parts()?;
        let session = Self::accept_once(&parts).await?;
        self.handle_inbound(session, dest_root).await
    }

    /// Snapshot listen handles; call [`accept_once`] **without** holding the session lock.
    pub fn take_serve_parts(&mut self) -> Result<ServeListenParts> {
        let listener = self
            .listener
            .clone()
            .ok_or(Error::Session(lb_session::Error::Protocol("not_online")))?;
        let peer = self
            .peer
            .clone()
            .ok_or(Error::Session(lb_session::Error::Protocol("not_paired")))?;
        if self.pairing || self.transferring != TransferKind::None || self.probing {
            // File data plane also accept()s on this listener — concurrent serve
            // would steal those connections and surface as「入站异常」.
            return Err(Error::Session(lb_session::Error::Protocol("busy")));
        }
        Ok(ServeListenParts {
            listener,
            peer,
            identity: self.identity.clone(),
        })
    }

    /// Blocking accept (short idle timeout). Safe to run while session mutex is released.
    pub async fn accept_once(parts: &ServeListenParts) -> Result<crate::ControlSession> {
        match timeout(
            SERVE_IDLE_WAIT,
            accept_trusted(parts.identity.clone(), parts.listener.as_ref(), &parts.peer),
        )
        .await
        {
            Ok(Ok(s)) => Ok(s),
            Ok(Err(e)) => Err(Error::Session(e)),
            Err(_) => Err(Error::Session(lb_session::Error::Protocol(
                "waiting for trusted peer timed out",
            ))),
        }
    }

    /// Process one accepted control session (file probe path or clipboard).
    pub async fn handle_inbound(
        &mut self,
        mut session: crate::ControlSession,
        dest_root: PathBuf,
    ) -> Result<InboundEvent> {
        self.touch_peer_alive();
        if self.pairing || self.transferring != TransferKind::None {
            return Err(Error::Session(lb_session::Error::Protocol("busy")));
        }
        match self.read_inbound(&mut session).await? {
            InboundRead::Done(ev) => Ok(ev),
            InboundRead::Frame(frame) => self.dispatch_inbound(session, frame, dest_root).await,
        }
    }

    /// Read the first frame of an accepted session and classify how it ended.
    ///
    /// One read loop for every entry point (`serve_once`, FFI, Tauri shells), so Bye / EOF behave
    /// the same everywhere instead of surfacing as「入站异常」.
    pub async fn read_inbound(
        &mut self,
        session: &mut crate::ControlSession,
    ) -> Result<InboundRead> {
        match session.recv_frame().await {
            Ok(frame) => Ok(InboundRead::Frame(frame)),
            Err(lb_session::Error::PeerLeft) => {
                // Bye is only reachable here on a fingerprint-pinned session (accept_trusted /
                // pairing window), so an unknown peer can never clear trust this way.
                tracing::info!("inbound: peer sent Bye — dropping trust");
                self.note_peer_left();
                Ok(InboundRead::Done(InboundEvent::PeerLeft {
                    reason: "unpaired".into(),
                }))
            }
            Err(lb_session::Error::PeerClosed) => {
                // Handshake done, then a bare close: that is the peer's probe / teardown, which
                // still proves it is alive and answering.
                self.touch_peer_alive();
                Ok(InboundRead::Done(InboundEvent::PeerClosed))
            }
            Err(e) => Err(Error::Session(e)),
        }
    }

    /// After first control frame is read — file path can run unlocked via prepare/execute.
    pub async fn dispatch_inbound(
        &mut self,
        mut session: crate::ControlSession,
        frame: Frame,
        dest_root: PathBuf,
    ) -> Result<InboundEvent> {
        self.touch_peer_alive();
        match MsgType::from_u16(frame.msg_type) {
            Some(MsgType::Ping) => {
                // Heartbeat from the peer: answer Pong so both ends see authenticated liveness.
                let ping = lb_protocol::Ping::decode(&frame.payload)?;
                let _ = session
                    .send_msg(
                        MsgType::Pong,
                        lb_protocol::Pong { nonce: ping.nonce }.encode(),
                    )
                    .await;
                let _ = session.shutdown().await;
                self.touch_peer_alive();
                Ok(InboundEvent::Heartbeat { nonce: ping.nonce })
            }
            Some(MsgType::ProbeRequest) => {
                let work = self.prepare_inbound_file()?;
                let result = Self::execute_inbound_file(work, session, frame, dest_root).await;
                self.finish_transfer(matches!(&result, Ok(InboundEvent::File { .. })));
                result
            }
            Some(MsgType::ClipOffer) => {
                self.prepare_clip_inbound()?;
                let result = Self::execute_clip_inbound(session, frame).await;
                self.finish_clip_inbound(matches!(
                    &result,
                    Ok(InboundEvent::ClipboardText { .. }) | Ok(InboundEvent::ClipboardPng { .. })
                ));
                result
            }
            other => {
                tracing::warn!(msg_type = ?other, "unexpected inbound frame");
                Ok(InboundEvent::Error {
                    message: format!("unexpected inbound msg: {other:?}"),
                })
            }
        }
    }

    /// Mark inbound file transfer; release session lock before [`execute_inbound_file`].
    pub fn prepare_inbound_file(&mut self) -> Result<InboundFileWork> {
        if self.transferring != TransferKind::None || self.pairing {
            return Err(Error::Session(lb_session::Error::Protocol("busy")));
        }
        self.touch_peer_alive();
        let listener = self
            .listener
            .clone()
            .ok_or(Error::Session(lb_session::Error::Protocol("not_online")))?;
        let peer = self
            .peer
            .clone()
            .ok_or(Error::Session(lb_session::Error::Protocol("not_paired")))?;
        self.transferring = TransferKind::In;
        self.mark_transfer_start(TransferKind::In);
        self.transfer_progress.reset(0);
        Ok(InboundFileWork {
            listener,
            peer,
            identity: self.identity.clone(),
            progress: self.transfer_progress.clone(),
            live_name: self.transfer_name.clone(),
        })
    }

    /// Data-plane recv; safe without session mutex (progress atomics update live).
    pub async fn execute_inbound_file(
        work: InboundFileWork,
        mut session: crate::ControlSession,
        frame: Frame,
        dest_root: PathBuf,
    ) -> Result<InboundEvent> {
        session.push_frame(frame);
        let mut link =
            establish_receiver(session, work.listener.as_ref(), work.identity, &work.peer).await?;
        std::fs::create_dir_all(&dest_root)?;
        let (summary, path) = recv_file(
            &mut link,
            &RecvOptions {
                dest_root,
                overwrite: false,
                progress: Some(work.progress),
                live_name: Some(work.live_name),
            },
        )
        .await?;
        let _ = shutdown_link(&mut link).await;
        Ok(InboundEvent::File {
            bytes: summary.bytes_transferred,
            path: path.display().to_string(),
        })
    }
}

/// Heartbeat driver: one Ping/Pong beat every [`HEARTBEAT_INTERVAL`] while a pair exists.
///
/// Runs without the session mutex (dialing a dead peer must never freeze the UI) and only
/// publishes an outcome; [`DeviceSession::poll_heartbeat`] applies it on the next status poll.
async fn heartbeat_loop(share: Arc<HeartbeatShare>) {
    let mut nonce = 0u64;
    loop {
        tokio::time::sleep(HEARTBEAT_INTERVAL).await;
        if share.stop.load(Ordering::Relaxed) {
            break;
        }
        if !share.allowed.load(Ordering::Relaxed) {
            continue;
        }
        let target = share.target.lock().ok().and_then(|g| g.clone());
        let Some(target) = target else {
            continue;
        };
        nonce = nonce.wrapping_add(1);
        let outcome = heartbeat_once(&target, nonce).await;
        if share.stop.load(Ordering::Relaxed) {
            break;
        }
        if let Ok(mut g) = share.outcome.lock() {
            *g = Some(outcome);
        }
    }
}

/// One beat: dial a trusted session, Ping, expect Pong, close.
async fn heartbeat_once(target: &HeartbeatTarget, nonce: u64) -> HeartbeatOutcome {
    let dialed = timeout(
        HEARTBEAT_DIAL_TIMEOUT,
        connect_trusted(target.identity.clone(), target.addr, &target.peer),
    )
    .await;
    let mut session = match dialed {
        Ok(Ok(s)) => s,
        Ok(Err(lb_session::Error::PeerLeft)) => return HeartbeatOutcome::PeerLeft,
        Ok(Err(lb_session::Error::PeerClosed)) => return HeartbeatOutcome::Closed,
        // Refused / half-open / not accepting: liveness now depends on the staleness window.
        Ok(Err(_)) | Err(_) => return HeartbeatOutcome::Failed,
    };
    let beat = timeout(HEARTBEAT_RTT_TIMEOUT, async {
        session
            .send_msg(MsgType::Ping, lb_protocol::Ping { nonce }.encode())
            .await?;
        session.recv_frame().await
    })
    .await;
    let outcome = match beat {
        Ok(Ok(frame)) if frame.msg_type == MsgType::Pong.as_u16() => HeartbeatOutcome::Ok,
        Ok(Err(lb_session::Error::PeerLeft)) => HeartbeatOutcome::PeerLeft,
        Ok(Err(lb_session::Error::PeerClosed)) => HeartbeatOutcome::Closed,
        _ => HeartbeatOutcome::Failed,
    };
    let _ = session.shutdown().await;
    outcome
}

fn base64_encode(data: &[u8]) -> String {
    const T: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = Vec::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let mut n = (chunk[0] as u32) << 16;
        if chunk.len() > 1 {
            n |= (chunk[1] as u32) << 8;
        }
        if chunk.len() > 2 {
            n |= chunk[2] as u32;
        }
        out.push(T[((n >> 18) & 63) as usize]);
        out.push(T[((n >> 12) & 63) as usize]);
        out.push(if chunk.len() > 1 {
            T[((n >> 6) & 63) as usize]
        } else {
            b'='
        });
        out.push(if chunk.len() > 2 {
            T[(n & 63) as usize]
        } else {
            b'='
        });
    }
    String::from_utf8(out).unwrap_or_default()
}

// --- Legacy thin wrappers (examples / transition). Prefer DeviceSession. ---

/// Host: persistent listen socket + optional trusted peer.
pub struct HostService {
    pub identity: Arc<DeviceIdentity>,
    pub listener: TcpListener,
    pub code: PairingCode,
    pub peer: Option<TrustedPeer>,
    pub port: u16,
    data_dir: PathBuf,
}

impl HostService {
    pub async fn start(data_dir: PathBuf, alias: &str) -> Result<(Self, u16)> {
        std::fs::create_dir_all(&data_dir)?;
        let identity = load_or_create_identity(&data_dir, "lanbridge-host", alias)?;
        let trust = load_trust(&data_dir);
        let peer = trust.peers().first().cloned();
        let code = generate_pairing_code();
        // Legacy HostService: ephemeral port (tests / old ABI). Product path uses DeviceSession @ 37532.
        let listener = TcpListener::bind(SocketAddr::from((Ipv4Addr::UNSPECIFIED, 0))).await?;
        let port = listener.local_addr()?.port();
        Ok((
            Self {
                identity,
                listener,
                code,
                peer,
                port,
                data_dir,
            },
            port,
        ))
    }

    pub fn pairing_code(&self) -> String {
        self.code.as_str().to_string()
    }

    pub fn is_paired(&self) -> bool {
        self.peer.is_some()
    }

    pub fn peer_device_id(&self) -> Option<String> {
        self.peer.as_ref().map(|p| p.device_id.clone())
    }

    pub async fn await_pair(&mut self) -> Result<TrustedPeer> {
        let trusted = load_trust(&self.data_dir).peers().to_vec();
        let outcome = match timeout(
            PAIR_WAIT,
            await_host_pair(self.identity.clone(), &self.listener, &self.code, &trusted),
        )
        .await
        {
            Ok(v) => v?,
            Err(_) => {
                return Err(Error::Session(lb_session::Error::Protocol(
                    "pairing accept timeout",
                )))
            }
        };
        let (peer, sock, reconnected) = match outcome {
            HostPairOutcome::Paired(peer, _session, sock) => (peer, sock, false),
            HostPairOutcome::Reconnect(peer, sock) => (peer, sock, true),
        };
        if !reconnected {
            let mut store = TrustStore::new();
            store.insert(peer.clone());
            save_trust(&self.data_dir, &store)?;
        }
        let host = peer_ip_string(sock);
        save_endpoint(
            &self.data_dir,
            &host,
            DEFAULT_CONTROL_PORT,
            &peer.device_id,
            None,
        )?;
        self.peer = Some(peer.clone());
        self.code = generate_pairing_code();
        Ok(peer)
    }

    async fn accept_control(&self) -> Result<(TrustedPeer, crate::ControlSession)> {
        let peer = self
            .peer
            .clone()
            .ok_or(Error::Session(lb_session::Error::Protocol(
                "not paired — call await_pair first",
            )))?;
        let session = match timeout(
            ACCEPT_WAIT,
            accept_trusted(self.identity.clone(), &self.listener, &peer),
        )
        .await
        {
            Ok(v) => v?,
            Err(_) => {
                return Err(Error::Session(lb_session::Error::Protocol(
                    "waiting for trusted peer timed out",
                )))
            }
        };
        Ok((peer, session))
    }

    pub async fn recv_file(&mut self, dest_root: PathBuf) -> Result<(u64, PathBuf)> {
        std::fs::create_dir_all(&dest_root)?;
        let (peer, session) = self.accept_control().await?;
        let mut link =
            establish_receiver(session, &self.listener, self.identity.clone(), &peer).await?;
        let (summary, path) = recv_file(
            &mut link,
            &RecvOptions {
                dest_root,
                overwrite: false,
                progress: None,
                live_name: None,
            },
        )
        .await?;
        Ok((summary.bytes_transferred, path))
    }

    pub async fn recv_clipboard(&mut self) -> Result<ClipboardItem> {
        let (_peer, mut session) = self.accept_control().await?;
        let item = match timeout(Duration::from_secs(120), recv_clipboard(&mut session)).await {
            Ok(v) => v?,
            Err(_) => {
                return Err(Error::Session(lb_session::Error::Protocol(
                    "clipboard recv timeout",
                )))
            }
        };
        Ok(item.1)
    }
}

/// Guest: trusted peer + remembered host endpoint.
pub struct GuestService {
    pub identity: Arc<DeviceIdentity>,
    pub peer: Option<TrustedPeer>,
    pub addr: Option<SocketAddr>,
    data_dir: PathBuf,
}

impl GuestService {
    pub fn open(data_dir: PathBuf, alias: &str) -> Result<Self> {
        std::fs::create_dir_all(&data_dir)?;
        let identity = load_or_create_identity(&data_dir, "lanbridge-guest", alias)?;
        let trust = load_trust(&data_dir);
        let endpoint = load_endpoint(&data_dir);
        let (peer, addr) = if let Some(ep) = endpoint {
            let peer = trust
                .get(&ep.peer_device_id)
                .cloned()
                .or_else(|| trust.peers().first().cloned());
            let addr = format!("{}:{}", ep.host, ep.port).parse().ok();
            (peer, addr)
        } else {
            (trust.peers().first().cloned(), None)
        };
        Ok(Self {
            identity,
            peer,
            addr,
            data_dir,
        })
    }

    pub fn is_paired(&self) -> bool {
        self.peer.is_some() && self.addr.is_some()
    }

    pub fn remembered_host(&self) -> Option<String> {
        self.addr.map(peer_ip_string)
    }

    pub fn remembered_port(&self) -> Option<u16> {
        self.addr.map(|a| a.port())
    }

    pub fn peer_device_id(&self) -> Option<String> {
        self.peer.as_ref().map(|p| p.device_id.clone())
    }

    pub async fn pair(&mut self, host: &str, port: u16, code: &str) -> Result<TrustedPeer> {
        let code = PairingCode::parse(code)?;
        let addr: SocketAddr = format!("{host}:{port}")
            .parse()
            .map_err(|_| Error::Session(lb_session::Error::Protocol("bad host/port")))?;
        let (peer, session) = match timeout(
            GUEST_PAIR_WAIT,
            pair_as_guest(self.identity.clone(), addr, &code),
        )
        .await
        {
            Ok(v) => v?,
            Err(_) => {
                return Err(Error::Session(lb_session::Error::Protocol(
                    "pairing connect timeout — is the peer in「等待配对」?",
                )))
            }
        };
        drop(session);
        let mut store = TrustStore::new();
        store.insert(peer.clone());
        save_trust(&self.data_dir, &store)?;
        save_endpoint(&self.data_dir, host, port, &peer.device_id, None)?;
        self.peer = Some(peer.clone());
        self.addr = Some(addr);
        Ok(peer)
    }

    async fn connect(&self) -> Result<(SocketAddr, TrustedPeer, crate::ControlSession)> {
        let peer = self
            .peer
            .clone()
            .ok_or(Error::Session(lb_session::Error::Protocol(
                "not paired — call pair first",
            )))?;
        let addr = self.addr.ok_or(Error::Session(lb_session::Error::Protocol(
            "no remembered endpoint",
        )))?;
        let session = connect_trusted(self.identity.clone(), addr, &peer).await?;
        Ok((addr, peer, session))
    }

    pub async fn send_file(&self, local: &Path, remote_name: &str) -> Result<u64> {
        let (addr, peer, session) = self.connect().await?;
        let mut link = establish_sender(
            session,
            addr,
            self.identity.clone(),
            &peer,
            DEFAULT_PROBE_BYTES,
        )
        .await?;
        let remote = if remote_name.trim().is_empty() {
            local
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or("file.bin")
                .replace('\\', "/")
        } else {
            remote_name.trim().replace('\\', "/")
        };
        let summary = send_file(
            &mut link,
            local,
            &SendOptions {
                remote_path: remote,
                progress: None,
                live_name: None,
            },
        )
        .await?;
        let _ = shutdown_link(&mut link).await;
        Ok(summary.bytes_transferred)
    }

    pub async fn send_clipboard(&self, item: ClipboardItem) -> Result<()> {
        let (_addr, _peer, mut session) = self.connect().await?;
        push_clipboard(&mut session, &item).await?;
        let _ = session.shutdown().await;
        Ok(())
    }

    pub fn update_endpoint(&mut self, host: &str, port: u16) -> Result<()> {
        let peer_id = self
            .peer
            .as_ref()
            .map(|p| p.device_id.clone())
            .ok_or(Error::Session(lb_session::Error::Protocol("not paired")))?;
        let addr: SocketAddr = format!("{host}:{port}")
            .parse()
            .map_err(|_| Error::Session(lb_session::Error::Protocol("bad host/port")))?;
        save_endpoint(&self.data_dir, host, port, &peer_id, None)?;
        self.addr = Some(addr);
        Ok(())
    }
}
