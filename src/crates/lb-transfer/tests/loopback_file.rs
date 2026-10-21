use lb_crypto::{generate_pairing_code, DeviceIdentity};
use lb_session::pair_as_guest;
use lb_transfer::{
    establish_receiver, establish_sender, recv_dir, recv_file, send_dir, send_file, RecvOptions,
    SendOptions,
};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::net::TcpListener;

async fn host_recv_file(
    listener: TcpListener,
    host_id: Arc<DeviceIdentity>,
    code: lb_crypto::PairingCode,
    dest_root: PathBuf,
) -> (lb_transfer::TransferSummary, PathBuf) {
    let (guest_peer, session, _) = lb_session::pair_as_host(host_id.clone(), &listener, &code)
        .await
        .unwrap();
    let mut link = establish_receiver(session, &listener, host_id, &guest_peer)
        .await
        .unwrap();
    let (summary, dest) = recv_file(
        &mut link,
        &RecvOptions {
            dest_root,
            overwrite: false,
            progress: None,
            live_name: None,
        },
    )
    .await
    .unwrap();
    (summary, dest)
}

async fn transfer_once(
    src_file: &Path,
    dst_dir: &Path,
    remote: &str,
    probe: u64,
) -> (lb_transfer::TransferSummary, PathBuf) {
    let host_id = Arc::new(DeviceIdentity::generate("host", "H").unwrap());
    let guest_id = Arc::new(DeviceIdentity::generate("guest", "G").unwrap());
    let code = generate_pairing_code();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let code_h = code.clone();
    let dest_root = dst_dir.to_path_buf();
    let remote = remote.to_string();
    let src = src_file.to_path_buf();
    let host =
        tokio::spawn(async move { host_recv_file(listener, host_id, code_h, dest_root).await });
    let (host_peer, session) = pair_as_guest(guest_id.clone(), addr, &code).await.unwrap();
    let mut link = establish_sender(session, addr, guest_id, &host_peer, probe)
        .await
        .unwrap();
    let sent = send_file(
        &mut link,
        &src,
        &SendOptions {
            remote_path: remote,
            progress: None,
            live_name: None,
        },
    )
    .await
    .unwrap();
    let _ = lb_transfer::shutdown_link(&mut link).await;
    let (recv, dest) = host.await.unwrap();
    assert_eq!(sent.bytes_transferred, recv.bytes_transferred);
    (recv, dest)
}

#[tokio::test]
async fn loopback_send_and_resume() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let root = std::env::temp_dir().join(format!("lb-xfer-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let src_dir = root.join("src");
    let dst_dir = root.join("dst");
    std::fs::create_dir_all(&src_dir).unwrap();
    std::fs::create_dir_all(&dst_dir).unwrap();
    let payload: Vec<u8> = (0..20_000u32).map(|i| (i % 251) as u8).collect();
    let src_file = src_dir.join("photo.bin");
    std::fs::write(&src_file, &payload).unwrap();

    let (summary, dest) = transfer_once(&src_file, &dst_dir, "album/photo.bin", 64 * 1024).await;
    assert_eq!(summary.bytes_transferred, payload.len() as u64);
    assert_eq!(summary.resume_offset, 0);
    assert_eq!(std::fs::read(&dest).unwrap(), payload);

    let half = payload.len() / 2;
    std::fs::write(&dest, &payload[..half]).unwrap();
    let (summary, dest2) = transfer_once(&src_file, &dst_dir, "album/photo.bin", 64 * 1024).await;
    assert_eq!(dest2, dest);
    assert_eq!(summary.resume_offset, half as u64);
    assert_eq!(std::fs::read(&dest2).unwrap(), payload);
    let _ = std::fs::remove_dir_all(&root);
}

#[tokio::test]
async fn loopback_large_payload_byte_exact() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let root = std::env::temp_dir().join(format!("lb-xfer-lg-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let src_dir = root.join("src");
    let dst_dir = root.join("dst");
    std::fs::create_dir_all(&src_dir).unwrap();
    std::fs::create_dir_all(&dst_dir).unwrap();
    // ~2.5 MiB patterned payload — catches truncation / zero-pad from set_len false positives.
    let payload: Vec<u8> = (0..2_500_000u32).map(|i| ((i * 131) % 251) as u8).collect();
    let src_file = src_dir.join("big.bin");
    std::fs::write(&src_file, &payload).unwrap();

    let (summary, dest) = transfer_once(&src_file, &dst_dir, "big.bin", 256 * 1024).await;
    assert_eq!(summary.bytes_transferred, payload.len() as u64);
    let got = std::fs::read(&dest).unwrap();
    assert_eq!(got.len(), payload.len());
    assert_eq!(got, payload);
    let _ = std::fs::remove_dir_all(&root);
}

#[tokio::test]
async fn loopback_directory() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let root = std::env::temp_dir().join(format!("lb-dir-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let src = root.join("src");
    let dst = root.join("dst");
    std::fs::create_dir_all(src.join("sub")).unwrap();
    std::fs::write(src.join("a.txt"), b"alpha").unwrap();
    std::fs::write(src.join("sub").join("b.txt"), b"beta").unwrap();

    let host_id = Arc::new(DeviceIdentity::generate("host", "H").unwrap());
    let guest_id = Arc::new(DeviceIdentity::generate("guest", "G").unwrap());
    let code = generate_pairing_code();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let code_h = code.clone();
    let dst_root = dst.clone();
    let host = tokio::spawn(async move {
        let (guest_peer, session, _) =
            lb_session::pair_as_host(host_id.clone(), &listener, &code_h)
                .await
                .unwrap();
        let mut link = establish_receiver(session, &listener, host_id, &guest_peer)
            .await
            .unwrap();
        recv_dir(
            &mut link,
            &RecvOptions {
                dest_root: dst_root,
                overwrite: false,
                progress: None,
                live_name: None,
            },
        )
        .await
        .unwrap()
    });

    let (host_peer, session) = pair_as_guest(guest_id.clone(), addr, &code).await.unwrap();
    let mut link = establish_sender(session, addr, guest_id, &host_peer, 32 * 1024)
        .await
        .unwrap();
    let n = send_dir(&mut link, &src, "inbox").await.unwrap();
    assert_eq!(n, 2);
    assert_eq!(host.await.unwrap(), 2);
    assert_eq!(std::fs::read(dst.join("inbox/a.txt")).unwrap(), b"alpha");
    assert_eq!(std::fs::read(dst.join("inbox/sub/b.txt")).unwrap(), b"beta");
    let _ = std::fs::remove_dir_all(&root);
}
