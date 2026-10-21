use crate::error::{Error, Result};
use crate::traits::PeerInfo;
use serde::{Deserialize, Serialize};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::time::{Duration, Instant};
use tokio::net::UdpSocket;

/// Default UDP discovery port (not the TLS session port).
pub const DEFAULT_DISCOVERY_PORT: u16 = 37_531;

#[derive(Debug, Clone)]
pub struct UdpBeaconConfig {
    pub bind: SocketAddr,
    pub broadcast: SocketAddr,
    /// TCP/TLS session port advertised in beacons.
    pub session_port: u16,
}

impl UdpBeaconConfig {
    pub fn loopback(session_port: u16) -> Self {
        Self {
            bind: SocketAddr::from((Ipv4Addr::LOCALHOST, 0)),
            broadcast: SocketAddr::from((Ipv4Addr::LOCALHOST, DEFAULT_DISCOVERY_PORT)),
            session_port,
        }
    }

    pub fn lan(session_port: u16) -> Self {
        Self {
            bind: SocketAddr::from((Ipv4Addr::UNSPECIFIED, DEFAULT_DISCOVERY_PORT)),
            broadcast: SocketAddr::from((Ipv4Addr::BROADCAST, DEFAULT_DISCOVERY_PORT)),
            session_port,
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct BeaconV0 {
    v: u16,
    device_id: String,
    alias: String,
    session_port: u16,
}

/// Bound UDP socket used to send or receive beacons (one role per socket).
pub struct UdpDiscoverer {
    sock: UdpSocket,
    cfg: UdpBeaconConfig,
}

impl UdpDiscoverer {
    pub async fn bind(cfg: UdpBeaconConfig) -> Result<Self> {
        let sock = UdpSocket::bind(cfg.bind).await?;
        sock.set_broadcast(true)?;
        Ok(Self { sock, cfg })
    }

    pub fn local_addr(&self) -> Result<SocketAddr> {
        Ok(self.sock.local_addr()?)
    }

    pub async fn advertise(&self, self_info: &PeerInfo) -> Result<()> {
        self.send_beacon(self_info, self.cfg.broadcast).await
    }

    /// Broadcast + directed subnet / gateway beacons (hotspot often filters 255.255.255.255).
    pub async fn advertise_lan(&self, self_info: &PeerInfo) -> Result<()> {
        let _ = self.advertise(self_info).await;
        for dest in directed_discovery_targets(self.cfg.broadcast.port()) {
            let _ = self.send_beacon(self_info, dest).await;
        }
        Ok(())
    }

    /// Unicast beacon (heartbeat to a known peer when LAN broadcast is filtered).
    pub async fn advertise_to(&self, self_info: &PeerInfo, dest: SocketAddr) -> Result<()> {
        self.send_beacon(self_info, dest).await
    }

    async fn send_beacon(&self, self_info: &PeerInfo, dest: SocketAddr) -> Result<()> {
        let beacon = BeaconV0 {
            v: 0,
            device_id: self_info.device_id.clone(),
            alias: self_info.alias.clone(),
            session_port: self.cfg.session_port,
        };
        let bytes = serde_json::to_vec(&beacon).map_err(|e| Error::Beacon(e.to_string()))?;
        let mut msg = b"LB1".to_vec();
        msg.extend_from_slice(&bytes);
        self.sock.send_to(&msg, dest).await?;
        Ok(())
    }

    pub async fn discover(&self, timeout: Duration) -> Result<Vec<PeerInfo>> {
        let deadline = Instant::now() + timeout;
        let mut buf = [0u8; 2048];
        let mut peers = Vec::new();

        while Instant::now() < deadline {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                break;
            }
            let recv = tokio::time::timeout(remaining, self.sock.recv_from(&mut buf)).await;
            let Ok(Ok((n, from))) = recv else {
                // Timeout / transient error: keep waiting until deadline (do not abort early).
                continue;
            };
            if n < 3 || &buf[..3] != b"LB1" {
                continue;
            }
            let beacon: BeaconV0 =
                serde_json::from_slice(&buf[3..n]).map_err(|e| Error::Beacon(e.to_string()))?;
            if beacon.v != 0 {
                continue;
            }
            let addr = SocketAddr::new(from.ip(), beacon.session_port);
            let info = PeerInfo {
                device_id: beacon.device_id,
                alias: beacon.alias,
                addr,
            };
            if !peers
                .iter()
                .any(|p: &PeerInfo| p.device_id == info.device_id)
            {
                peers.push(info);
            }
        }
        Ok(peers)
    }
}

/// Best-effort local IPv4s (UDP connect probes; no packets leave the host).
fn local_ipv4s_best_effort() -> Vec<Ipv4Addr> {
    let mut ips = Vec::new();
    let probes = [
        "1.1.1.1:80",
        "192.168.43.1:80",
        "192.168.137.1:80",
        "192.168.0.1:80",
        "10.0.0.1:80",
    ];
    for probe in probes {
        let Ok(sock) = std::net::UdpSocket::bind("0.0.0.0:0") else {
            continue;
        };
        if sock.connect(probe).is_err() {
            continue;
        }
        let Ok(addr) = sock.local_addr() else {
            continue;
        };
        if let IpAddr::V4(v4) = addr.ip() {
            if !is_usable_lan_v4(v4) || ips.contains(&v4) {
                continue;
            }
            ips.push(v4);
        }
    }
    ips
}

fn is_usable_lan_v4(v4: Ipv4Addr) -> bool {
    if v4.is_loopback() || v4.is_unspecified() || v4.is_link_local() {
        return false;
    }
    let o = v4.octets();
    if o[0] == 172 && o[1] == 17 {
        return false;
    }
    if o[0] == 100 && (64..=127).contains(&o[1]) {
        return false;
    }
    true
}

/// Directed /24 broadcasts + .1 gateway unicasts for each local IPv4.
fn directed_discovery_targets(port: u16) -> Vec<SocketAddr> {
    let mut out = Vec::new();
    for ip in local_ipv4s_best_effort() {
        let o = ip.octets();
        let bcast = Ipv4Addr::new(o[0], o[1], o[2], 255);
        out.push(SocketAddr::from((bcast, port)));
        let gw = Ipv4Addr::new(o[0], o[1], o[2], 1);
        if gw != ip {
            out.push(SocketAddr::from((gw, port)));
        }
    }
    out
}
