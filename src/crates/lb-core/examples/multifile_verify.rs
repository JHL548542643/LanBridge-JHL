use lb_core::{DeviceSession, InboundEvent};
use std::path::PathBuf;
use std::time::Duration;
use tokio::time::sleep;

#[tokio::main]
async fn main() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../.tmp/lb-multifile-verify");
    let _ = std::fs::remove_dir_all(&root);
    let host_dir = root.join("host");
    let guest_dir = root.join("guest");
    let dst = root.join("dst");
    std::fs::create_dir_all(&dst).unwrap();
    let mut host = DeviceSession::open(host_dir, "lanbridge-win", "PC").unwrap();
    let mut guest = DeviceSession::open(guest_dir, "lanbridge-ohos", "Phone").unwrap();
    let port = host.go_online(37721).await.unwrap();
    let code = host.arm_pair_wait().unwrap();
    sleep(Duration::from_millis(100)).await;
    guest.go_online(37722).await.unwrap();
    guest.pair("127.0.0.1", port, &code).await.unwrap();
    sleep(Duration::from_millis(200)).await;
    let _ = host.poll_pair_wait();
    assert_eq!(host.status().connection, "connected");
    assert!(!host.status().nearby.is_empty());
    let f1 = root.join("a.txt");
    let f2 = root.join("b.txt");
    std::fs::write(&f1, b"aaaa").unwrap();
    std::fs::write(&f2, b"bbbbbb").unwrap();
    let h = tokio::spawn(async move {
        let mut got = 0u64;
        for _ in 0..8 {
            match host.serve_once(dst.clone()).await {
                Ok(InboundEvent::File { bytes, .. }) => {
                    got += bytes;
                    if got >= 10 {
                        return got;
                    }
                }
                Err(_) => {}
                _ => {}
            }
        }
        got
    });
    sleep(Duration::from_millis(50)).await;
    let n1 = guest.send_file(&f1, "a.txt").await.unwrap();
    let n2 = guest.send_file(&f2, "b.txt").await.unwrap();
    let got = h.await.unwrap();
    assert_eq!(n1 + n2, 10);
    assert!(got >= 10, "got={got}");
    println!(
        "MULTIFILE_OK n1={n1} n2={n2} got={got} banner={}",
        guest.status().banner
    );
}
