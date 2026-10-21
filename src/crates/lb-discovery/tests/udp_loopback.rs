use lb_discovery::{PeerInfo, UdpBeaconConfig, UdpDiscoverer};
use std::net::{Ipv4Addr, SocketAddr};
use std::time::Duration;

#[tokio::test]
async fn udp_beacon_reaches_peer() {
    let listener = UdpDiscoverer::bind(UdpBeaconConfig {
        bind: SocketAddr::from((Ipv4Addr::LOCALHOST, 0)),
        broadcast: SocketAddr::from((Ipv4Addr::LOCALHOST, 9)),
        session_port: 1,
    })
    .await
    .unwrap();
    let dest = listener.local_addr().unwrap();

    let sender = UdpDiscoverer::bind(UdpBeaconConfig {
        bind: SocketAddr::from((Ipv4Addr::LOCALHOST, 0)),
        broadcast: dest,
        session_port: 4444,
    })
    .await
    .unwrap();

    sender
        .advertise(&PeerInfo {
            device_id: "phone".into(),
            alias: "Phone".into(),
            addr: SocketAddr::from((Ipv4Addr::LOCALHOST, 4444)),
        })
        .await
        .unwrap();

    let peers = listener.discover(Duration::from_millis(500)).await.unwrap();
    assert_eq!(peers.len(), 1);
    assert_eq!(peers[0].device_id, "phone");
    assert_eq!(peers[0].alias, "Phone");
    assert_eq!(peers[0].addr.port(), 4444);
    assert_eq!(peers[0].addr.ip().to_string(), "127.0.0.1");
}
