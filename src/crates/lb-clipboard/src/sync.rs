use crate::error::{Error, Result};
use bytes::Bytes;
use lb_protocol::{ClipAck, ClipKind, ClipOffer, ClipReject, ClipRejectReason, MsgType};
use lb_session::ControlSession;
use std::sync::atomic::{AtomicU64, Ordering};

static SEQ: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClipboardItem {
    Text(String),
    ImagePng(Bytes),
}

impl ClipboardItem {
    pub fn kind(&self) -> ClipKind {
        match self {
            Self::Text(_) => ClipKind::TextPlain,
            Self::ImagePng(_) => ClipKind::ImagePng,
        }
    }

    pub fn as_bytes(&self) -> &[u8] {
        match self {
            Self::Text(s) => s.as_bytes(),
            Self::ImagePng(b) => b.as_ref(),
        }
    }

    pub fn from_wire(kind: ClipKind, data: Bytes) -> Result<Self> {
        match kind {
            ClipKind::TextPlain => {
                let s = std::str::from_utf8(&data).map_err(|_| Error::Failed("utf8"))?;
                Ok(Self::Text(s.to_string()))
            }
            ClipKind::ImagePng => {
                if data.len() < 8 || &data[..8] != b"\x89PNG\r\n\x1a\n" {
                    return Err(Error::Failed("not png"));
                }
                Ok(Self::ImagePng(data))
            }
        }
    }
}

/// Push an item: ClipOffer → raw bytes → ClipAck.
pub async fn push_clipboard(session: &mut ControlSession, item: &ClipboardItem) -> Result<u64> {
    let data = item.as_bytes();
    let byte_len = data.len() as u64;
    let max = match item.kind() {
        ClipKind::TextPlain => ClipOffer::MAX_TEXT,
        ClipKind::ImagePng => ClipOffer::MAX_PNG,
    };
    if byte_len == 0 || byte_len > max {
        return Err(Error::Failed("size"));
    }
    let seq = SEQ.fetch_add(1, Ordering::Relaxed);
    session
        .send_msg(
            MsgType::ClipOffer,
            ClipOffer {
                kind: item.kind(),
                seq,
                byte_len,
            }
            .encode()?,
        )
        .await?;
    // Large buffer write without per-chunk flush.
    let mut offset = 0;
    while offset < data.len() {
        let end = (offset + 256 * 1024).min(data.len());
        session.write_raw(&data[offset..end]).await?;
        offset = end;
    }
    session.flush().await?;

    let frame = session.recv_frame().await?;
    match MsgType::from_u16(frame.msg_type) {
        Some(MsgType::ClipAck) => {
            let ack = ClipAck::decode(&frame.payload)?;
            if ack.seq != seq || ack.status != 0 {
                return Err(Error::Failed("bad ack"));
            }
        }
        Some(MsgType::ClipReject) => {
            let rej = ClipReject::decode(&frame.payload)?;
            return Err(Error::Rejected(rej.reason));
        }
        _ => return Err(Error::Failed("expected ClipAck")),
    }
    Ok(seq)
}

/// Receive one clipboard push (blocks until ClipOffer).
pub async fn recv_clipboard(session: &mut ControlSession) -> Result<(u64, ClipboardItem)> {
    let frame = session.recv_frame().await?;
    if frame.msg_type != MsgType::ClipOffer.as_u16() {
        return Err(Error::Failed("expected ClipOffer"));
    }
    let offer = ClipOffer::decode(&frame.payload)?;
    let max = match offer.kind {
        ClipKind::TextPlain => ClipOffer::MAX_TEXT,
        ClipKind::ImagePng => ClipOffer::MAX_PNG,
    };
    if offer.byte_len > max {
        session
            .send_msg(
                MsgType::ClipReject,
                ClipReject {
                    seq: offer.seq,
                    reason: ClipRejectReason::TooLarge,
                }
                .encode(),
            )
            .await?;
        return Err(Error::Rejected(ClipRejectReason::TooLarge));
    }

    let mut buf = vec![0u8; offer.byte_len as usize];
    session.read_exact_raw(&mut buf).await?;
    let item = match ClipboardItem::from_wire(offer.kind, Bytes::from(buf)) {
        Ok(item) => item,
        Err(e) => {
            session
                .send_msg(
                    MsgType::ClipReject,
                    ClipReject {
                        seq: offer.seq,
                        reason: ClipRejectReason::Unsupported,
                    }
                    .encode(),
                )
                .await?;
            return Err(e);
        }
    };

    session
        .send_msg(
            MsgType::ClipAck,
            ClipAck {
                seq: offer.seq,
                status: 0,
            }
            .encode(),
        )
        .await?;
    Ok((offer.seq, item))
}
