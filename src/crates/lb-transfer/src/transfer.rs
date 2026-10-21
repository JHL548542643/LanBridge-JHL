use crate::error::{Error, Result};
use crate::path::{list_files, pick_dest_for_recv, sanitize_relative_path};
use crate::plan::{plan_from_probe, split_range, TransferPlan};
use bytes::Bytes;
use futures::future::try_join_all;
use lb_crypto::DeviceIdentity;
use lb_protocol::{
    ChannelHello, DirComplete, DirOffer, FileAccept, FileComplete, FileOffer, FileReject,
    FileRejectReason, MsgType, OpenChannels, Ping, Pong, ProbeRequest, ProbeResult, SliceHeader,
};
use lb_session::{accept_tls, connect_tls, ControlSession, TlsMode, TrustedPeer};
use rand::Rng;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::fs::{File, OpenOptions};
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt, SeekFrom};
use tokio::net::{TcpListener, TcpStream};

pub const DEFAULT_PROBE_BYTES: u64 = 1024 * 1024;
const MAX_FILE_BYTES: u64 = 64 * 1024 * 1024 * 1024;
const MAX_DIR_FILES: u32 = 100_000;

/// Shared byte counters for UI progress (local side only; not on the wire).
#[derive(Debug, Default)]
pub struct ByteProgress {
    done: AtomicU64,
    total: AtomicU64,
}

impl ByteProgress {
    pub fn reset(&self, total: u64) {
        self.total.store(total, Ordering::Relaxed);
        self.done.store(0, Ordering::Relaxed);
    }

    pub fn add(&self, n: u64) {
        self.done.fetch_add(n, Ordering::Relaxed);
    }

    pub fn snapshot(&self) -> (u64, u64) {
        (
            self.done.load(Ordering::Relaxed),
            self.total.load(Ordering::Relaxed),
        )
    }
}

fn publish_live_name(slot: &Option<Arc<Mutex<String>>>, path: &str) {
    let Some(slot) = slot else {
        return;
    };
    let base = path
        .rsplit(['/', '\\'])
        .next()
        .filter(|s| !s.is_empty())
        .unwrap_or(path);
    if let Ok(mut g) = slot.lock() {
        *g = base.to_string();
    }
}

#[derive(Clone, Default)]
pub struct SendOptions {
    pub remote_path: String,
    pub progress: Option<Arc<ByteProgress>>,
    /// Optional live name for UI (basename); set by caller before send.
    pub live_name: Option<Arc<Mutex<String>>>,
}

#[derive(Clone, Default)]
pub struct RecvOptions {
    pub dest_root: PathBuf,
    pub overwrite: bool,
    pub progress: Option<Arc<ByteProgress>>,
    /// Written when FileOffer arrives (basename) for UI progress cards.
    pub live_name: Option<Arc<Mutex<String>>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransferSummary {
    pub path: String,
    pub bytes_transferred: u64,
    pub resume_offset: u64,
    pub streams: u16,
}

pub struct Link {
    control: ControlSession,
    channels: Vec<ControlSession>,
    plan: TransferPlan,
}

impl Link {
    pub fn plan(&self) -> &TransferPlan {
        &self.plan
    }
}

pub async fn establish_sender(
    mut control: ControlSession,
    addr: std::net::SocketAddr,
    identity: Arc<DeviceIdentity>,
    peer: &TrustedPeer,
    probe_bytes: u64,
) -> Result<Link> {
    control
        .send_msg(
            MsgType::ProbeRequest,
            ProbeRequest {
                byte_count: probe_bytes,
            }
            .encode()?,
        )
        .await?;
    write_probe(&mut control, probe_bytes).await?;
    let frame = control.recv_frame().await?;
    if frame.msg_type != MsgType::ProbeResult.as_u16() {
        return Err(Error::Failed("expected ProbeResult"));
    }
    let result = ProbeResult::decode(&frame.payload)?;
    let plan = plan_from_probe(result.bytes, result.elapsed_us);
    let token: u64 = rand::thread_rng().gen();
    control
        .send_msg(
            MsgType::OpenChannels,
            OpenChannels {
                streams: plan.streams,
                token,
            }
            .encode()?,
        )
        .await?;
    let ready = control.recv_frame().await?;
    if ready.msg_type != MsgType::ChannelsReady.as_u16() {
        return Err(Error::Failed("expected ChannelsReady"));
    }
    let mut channels = Vec::with_capacity(plan.streams as usize);
    for index in 0..plan.streams {
        let tcp = TcpStream::connect(addr).await?;
        let mut session = connect_tls(
            tcp,
            identity.clone(),
            TlsMode::Trusted {
                expect_fingerprint: peer.fingerprint,
            },
            Some(peer),
        )
        .await?;
        session
            .send_msg(
                MsgType::ChannelHello,
                ChannelHello { index, token }.encode(),
            )
            .await?;
        channels.push(session);
    }
    Ok(Link {
        control,
        channels,
        plan,
    })
}

pub async fn establish_receiver(
    mut control: ControlSession,
    listener: &TcpListener,
    identity: Arc<DeviceIdentity>,
    peer: &TrustedPeer,
) -> Result<Link> {
    let frame = control.recv_frame().await?;
    if frame.msg_type != MsgType::ProbeRequest.as_u16() {
        return Err(Error::Failed("expected ProbeRequest"));
    }
    let req = ProbeRequest::decode(&frame.payload)?;
    let started = std::time::Instant::now();
    read_probe(&mut control, req.byte_count).await?;
    let elapsed_us = started.elapsed().as_micros().min(u128::from(u64::MAX)) as u64;
    control
        .send_msg(
            MsgType::ProbeResult,
            ProbeResult {
                bytes: req.byte_count,
                elapsed_us,
            }
            .encode(),
        )
        .await?;
    let plan = plan_from_probe(req.byte_count, elapsed_us);
    let opened = control.recv_frame().await?;
    if opened.msg_type != MsgType::OpenChannels.as_u16() {
        return Err(Error::Failed("expected OpenChannels"));
    }
    let open = OpenChannels::decode(&opened.payload)?;
    if open.streams != plan.streams {
        return Err(Error::Failed("stream count mismatch"));
    }
    control
        .send_msg(MsgType::ChannelsReady, Bytes::new())
        .await?;
    let channels = accept_channels(listener, identity, peer, open.streams, open.token).await?;
    Ok(Link {
        control,
        channels,
        plan,
    })
}

async fn accept_channels(
    listener: &TcpListener,
    identity: Arc<DeviceIdentity>,
    peer: &TrustedPeer,
    n: u16,
    token: u64,
) -> Result<Vec<ControlSession>> {
    let mut slots: Vec<Option<ControlSession>> = (0..n).map(|_| None).collect();
    let mut left = n;
    // Bounded so a peer that never opens the channels cannot hang the transfer.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    while left > 0 {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            return Err(Error::Failed("channel accept timeout"));
        }
        let (tcp, _) = match tokio::time::timeout(remaining, listener.accept()).await {
            Ok(Ok(v)) => v,
            Ok(Err(e)) => return Err(Error::Io(e)),
            Err(_) => return Err(Error::Failed("channel accept timeout")),
        };
        let mut session = match accept_tls(
            tcp,
            identity.clone(),
            TlsMode::Trusted {
                expect_fingerprint: peer.fingerprint,
            },
        )
        .await
        {
            Ok(s) => s,
            // Aborted dial (probe / heartbeat that gave up) — not our data channel.
            Err(_) => continue,
        };
        let frame = match session.recv_frame().await {
            Ok(f) => f,
            // Probe teardown or Bye on the same pinned peer: skip, keep waiting.
            Err(_) => continue,
        };
        if frame.msg_type != MsgType::ChannelHello.as_u16() {
            // A liveness beat from the peer can land in this accept window: answer it so the
            // peer's heartbeat does not time out, then keep waiting for the real channel.
            if frame.msg_type == MsgType::Ping.as_u16() {
                if let Ok(ping) = Ping::decode(&frame.payload) {
                    let _ = session
                        .send_msg(MsgType::Pong, Pong { nonce: ping.nonce }.encode())
                        .await;
                }
                let _ = session.shutdown().await;
            }
            continue;
        }
        let hello = ChannelHello::decode(&frame.payload)?;
        if hello.token != token || hello.index >= n || slots[hello.index as usize].is_some() {
            return Err(Error::Failed("bad channel hello"));
        }
        slots[hello.index as usize] = Some(session);
        left -= 1;
    }
    Ok(slots.into_iter().map(|s| s.expect("slot filled")).collect())
}

async fn write_probe(session: &mut ControlSession, nbytes: u64) -> Result<()> {
    let mut left = nbytes;
    let buf = vec![0xA5u8; 256 * 1024];
    while left > 0 {
        let n = left.min(buf.len() as u64) as usize;
        session.write_raw(&buf[..n]).await?;
        left -= n as u64;
    }
    session.flush().await?;
    Ok(())
}

async fn read_probe(session: &mut ControlSession, nbytes: u64) -> Result<()> {
    let mut left = nbytes;
    let mut buf = vec![0u8; 256 * 1024];
    while left > 0 {
        let n = left.min(buf.len() as u64) as usize;
        session.read_exact_raw(&mut buf[..n]).await?;
        left -= n as u64;
    }
    Ok(())
}

pub async fn send_file(
    link: &mut Link,
    local_path: &Path,
    opts: &SendOptions,
) -> Result<TransferSummary> {
    let remote_path = sanitize_relative_path(&opts.remote_path)?;
    publish_live_name(&opts.live_name, &remote_path);
    let meta = tokio::fs::metadata(local_path).await?;
    if !meta.is_file() {
        return Err(Error::Failed("not a regular file"));
    }
    let file_size = meta.len();
    if file_size > MAX_FILE_BYTES {
        return Err(Error::Failed("file too large"));
    }
    link.control
        .send_msg(
            MsgType::FileOffer,
            FileOffer {
                path: remote_path.clone(),
                file_size,
            }
            .encode()?,
        )
        .await?;
    let frame = link.control.recv_frame().await?;
    match MsgType::from_u16(frame.msg_type) {
        Some(MsgType::FileAccept) => {}
        Some(MsgType::FileReject) => {
            let rej = FileReject::decode(&frame.payload)?;
            return Err(Error::Rejected(rej.reason));
        }
        _ => return Err(Error::Failed("expected FileAccept")),
    }
    let accept = FileAccept::decode(&frame.payload)?;
    if accept.resume_offset > file_size {
        return Err(Error::Failed("resume beyond file size"));
    }
    let slices = split_range(
        accept.resume_offset,
        file_size - accept.resume_offset,
        link.plan.streams,
    );
    if let Some(p) = &opts.progress {
        p.reset(file_size.saturating_sub(accept.resume_offset));
    }
    pump_slices(link, local_path, &slices, true, opts.progress.as_ref()).await?;
    let transferred = file_size - accept.resume_offset;
    link.control
        .send_msg(
            MsgType::FileComplete,
            FileComplete {
                status: 0,
                bytes_transferred: transferred,
            }
            .encode(),
        )
        .await?;
    link.control.flush().await?;
    // Brief wait for the receiver to finish draining / close. Was 180s and left
    // the sender UI stuck on「发送中」after the peer already showed success.
    // Do NOT shutdown control here — send_dir sends multiple files on one link.
    let _ = tokio::time::timeout(std::time::Duration::from_secs(3), async {
        // Drain whatever the receiver still sends until it closes (timeout bounds it).
        while link.control.recv_frame().await.is_ok() {}
    })
    .await;
    Ok(TransferSummary {
        path: remote_path,
        bytes_transferred: transferred,
        resume_offset: accept.resume_offset,
        streams: link.plan.streams,
    })
}

/// Graceful TLS close on control + data channels (after the last FileComplete / DirComplete).
pub async fn shutdown_link(link: &mut Link) -> Result<()> {
    let _ = link.control.shutdown().await;
    for ch in &mut link.channels {
        let _ = ch.shutdown().await;
    }
    Ok(())
}

pub async fn recv_file(link: &mut Link, opts: &RecvOptions) -> Result<(TransferSummary, PathBuf)> {
    let frame = link.control.recv_frame().await?;
    if frame.msg_type != MsgType::FileOffer.as_u16() {
        return Err(Error::Failed("expected FileOffer"));
    }
    let offer = FileOffer::decode(&frame.payload)?;
    publish_live_name(&opts.live_name, &offer.path);
    if offer.file_size > MAX_FILE_BYTES {
        link.control
            .send_msg(
                MsgType::FileReject,
                FileReject {
                    reason: FileRejectReason::Unsupported,
                }
                .encode(),
            )
            .await?;
        return Err(Error::Failed("file too large"));
    }
    let relative = match sanitize_relative_path(&offer.path) {
        Ok(p) => p,
        Err(_) => {
            link.control
                .send_msg(
                    MsgType::FileReject,
                    FileReject {
                        reason: FileRejectReason::BadPath,
                    }
                    .encode(),
                )
                .await?;
            return Err(Error::BadPath("rejected offer path"));
        }
    };
    let (dest, resume_offset) =
        pick_dest_for_recv(&opts.dest_root, &relative, offer.file_size, opts.overwrite)?;
    link.control
        .send_msg(MsgType::FileAccept, FileAccept { resume_offset }.encode())
        .await?;
    {
        let file = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(false)
            .open(&dest)
            .await?;
        // Pre-size for parallel slice writes. Do NOT use metadata().len() as an
        // integrity check — after set_len it always equals file_size even when
        // only partial payload was written (zeros / holes).
        file.set_len(offer.file_size).await?;
    }
    let expected = offer.file_size.saturating_sub(resume_offset);
    let slices = split_range(resume_offset, expected, link.plan.streams);
    let progress = opts
        .progress
        .clone()
        .unwrap_or_else(|| Arc::new(ByteProgress::default()));
    progress.reset(expected);
    match pump_slices(link, &dest, &slices, false, Some(&progress)).await {
        Ok(()) => {}
        Err(e) if is_abrupt_peer_close(&e) => {
            let (done, _) = progress.snapshot();
            if done != expected {
                // Never leave set_len-sized zero-padded garbage for the user to open.
                let _ = tokio::fs::remove_file(&dest).await;
                return Err(Error::Incomplete { done, expected });
            }
            tracing::warn!(
                error = %e,
                done,
                expected,
                "data plane closed abruptly after all slice bytes received; accepting"
            );
        }
        Err(e) => {
            let _ = tokio::fs::remove_file(&dest).await;
            return Err(e);
        }
    }
    let (done, _) = progress.snapshot();
    if done != expected {
        let _ = tokio::fs::remove_file(&dest).await;
        return Err(Error::Incomplete { done, expected });
    }
    // Prefer a clean FileComplete; if the sender already closed after a lost flush,
    // slice bytes are already verified — do not hang the phone UI forever.
    let done_frame = match tokio::time::timeout(
        std::time::Duration::from_secs(15),
        link.control.recv_frame(),
    )
    .await
    {
        Ok(Ok(frame)) => Some(frame),
        Ok(Err(e)) => {
            tracing::warn!(
                error = %e,
                "FileComplete recv failed after verified pump; accepting file"
            );
            None
        }
        Err(_) => {
            tracing::warn!("FileComplete timed out after verified pump; accepting file");
            None
        }
    };
    if let Some(frame) = done_frame {
        if frame.msg_type != MsgType::FileComplete.as_u16() {
            tracing::warn!(
                msg = frame.msg_type,
                "expected FileComplete after verified pump; accepting file"
            );
        } else {
            match FileComplete::decode(&frame.payload) {
                Ok(complete) if complete.status == 0 && complete.bytes_transferred == expected => {}
                Ok(complete) => {
                    tracing::warn!(
                        status = complete.status,
                        done = complete.bytes_transferred,
                        expected,
                        "FileComplete mismatch after verified pump; accepting file"
                    );
                }
                Err(e) => {
                    tracing::warn!(
                        error = %e,
                        "FileComplete decode failed after verified pump; accepting file"
                    );
                }
            }
        }
    }
    Ok((
        TransferSummary {
            path: relative,
            bytes_transferred: expected,
            resume_offset,
            streams: link.plan.streams,
        },
        dest,
    ))
}

pub async fn send_dir(link: &mut Link, local_dir: &Path, remote_prefix: &str) -> Result<u32> {
    let prefix = sanitize_relative_path(remote_prefix)?;
    let files = list_files(local_dir)?;
    if files.len() > MAX_DIR_FILES as usize {
        return Err(Error::Failed("too many files"));
    }
    link.control
        .send_msg(
            MsgType::DirOffer,
            DirOffer {
                path: prefix.clone(),
                file_count: files.len() as u32,
            }
            .encode()?,
        )
        .await?;
    for (rel, path) in &files {
        let remote = format!("{prefix}/{rel}");
        send_file(
            link,
            path,
            &SendOptions {
                remote_path: remote,
                progress: None,
                live_name: None,
            },
        )
        .await?;
    }
    link.control
        .send_msg(
            MsgType::DirComplete,
            DirComplete {
                status: 0,
                files_ok: files.len() as u32,
            }
            .encode(),
        )
        .await?;
    link.control.flush().await?;
    let _ = shutdown_link(link).await;
    Ok(files.len() as u32)
}

pub async fn recv_dir(link: &mut Link, opts: &RecvOptions) -> Result<u32> {
    let frame = link.control.recv_frame().await?;
    if frame.msg_type != MsgType::DirOffer.as_u16() {
        return Err(Error::Failed("expected DirOffer"));
    }
    let offer = DirOffer::decode(&frame.payload)?;
    if offer.file_count > MAX_DIR_FILES {
        return Err(Error::Failed("too many files"));
    }
    let _prefix = sanitize_relative_path(&offer.path)?;
    for _ in 0..offer.file_count {
        recv_file(link, opts).await?;
    }
    let done = link.control.recv_frame().await?;
    if done.msg_type != MsgType::DirComplete.as_u16() {
        return Err(Error::Failed("expected DirComplete"));
    }
    let complete = DirComplete::decode(&done.payload)?;
    if complete.status != 0 || complete.files_ok != offer.file_count {
        return Err(Error::Failed("dir complete mismatch"));
    }
    Ok(offer.file_count)
}

async fn pump_slices(
    link: &mut Link,
    path: &Path,
    slices: &[(u64, u64)],
    sending: bool,
    progress: Option<&Arc<ByteProgress>>,
) -> Result<()> {
    let channels = std::mem::take(&mut link.channels);
    let chunk = link.plan.chunk_size;
    let path = path.to_path_buf();
    let progress = progress.cloned();
    // Do NOT tokio::spawn here. lb-ffi drives this via Runtime::block_on from a
    // NAPI async_work thread; spawn+join under that pattern can stall on OH when
    // streams>1 (PC finishes FileComplete while host JoinHandles never run).
    // try_join_all polls concurrent slice IO on the same task instead.
    let futs = channels
        .into_iter()
        .zip(slices.iter().copied())
        .map(|(mut channel, slice)| {
            let path = path.clone();
            let progress = progress.clone();
            async move {
                if sending {
                    send_slice(
                        &mut channel,
                        &path,
                        slice.0,
                        slice.1,
                        chunk,
                        progress.as_ref(),
                    )
                    .await?;
                } else {
                    recv_slice(
                        &mut channel,
                        &path,
                        slice.0,
                        slice.1,
                        chunk,
                        progress.as_ref(),
                    )
                    .await?;
                }
                Ok::<_, Error>(channel)
            }
        });
    link.channels = try_join_all(futs).await?;
    Ok(())
}

fn is_abrupt_peer_close(e: &Error) -> bool {
    let s = e.to_string();
    s.contains("close_notify")
        || s.contains("unexpected eof")
        || s.contains("UnexpectedEof")
        || s.contains("connection closed")
        || s.contains("Connection reset")
        || s.contains("强迫关闭")
        || s.contains("forcibly closed")
        || s.contains("os error 10054")
}

async fn send_slice(
    session: &mut ControlSession,
    path: &Path,
    offset: u64,
    length: u64,
    chunk: usize,
    progress: Option<&Arc<ByteProgress>>,
) -> Result<()> {
    session
        .send_msg(
            MsgType::SliceHeader,
            SliceHeader { offset, length }.encode(),
        )
        .await?;
    if length == 0 {
        session.flush().await?;
        return Ok(());
    }
    let mut file = File::open(path).await?;
    file.seek(SeekFrom::Start(offset)).await?;
    let mut left = length;
    let mut buf = vec![0u8; chunk];
    while left > 0 {
        let want = left.min(chunk as u64) as usize;
        let n = file.read(&mut buf[..want]).await?;
        if n == 0 {
            return Err(Error::Failed("unexpected eof"));
        }
        session.write_raw(&buf[..n]).await?;
        if let Some(p) = progress {
            p.add(n as u64);
        }
        left -= n as u64;
    }
    session.flush().await?;
    Ok(())
}

async fn recv_slice(
    session: &mut ControlSession,
    path: &Path,
    offset: u64,
    length: u64,
    chunk: usize,
    progress: Option<&Arc<ByteProgress>>,
) -> Result<()> {
    let frame = session.recv_frame().await?;
    if frame.msg_type != MsgType::SliceHeader.as_u16() {
        return Err(Error::Failed("expected SliceHeader"));
    }
    let header = SliceHeader::decode(&frame.payload)?;
    if header.offset != offset || header.length != length {
        return Err(Error::Failed("slice mismatch"));
    }
    if length == 0 {
        return Ok(());
    }
    let mut file = OpenOptions::new().write(true).open(path).await?;
    file.seek(SeekFrom::Start(offset)).await?;
    let mut left = length;
    let mut buf = vec![0u8; chunk];
    while left > 0 {
        let want = left.min(chunk as u64) as usize;
        session.read_exact_raw(&mut buf[..want]).await?;
        file.write_all(&buf[..want]).await?;
        if let Some(p) = progress {
            p.add(want as u64);
        }
        left -= want as u64;
    }
    file.flush().await?;
    Ok(())
}
