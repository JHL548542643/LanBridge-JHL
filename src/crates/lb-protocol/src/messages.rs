use crate::error::{Error, Result};
use crate::features::Features;
use crate::types::MsgType;
use bytes::{BufMut, Bytes, BytesMut};

const FINGERPRINT_LEN: usize = 32;
const MAX_DEVICE_ID: usize = 64;
const MAX_ALIAS: usize = 32;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hello {
    pub protocol_version: u16,
    pub features: Features,
    pub device_id: String,
    pub fingerprint: [u8; FINGERPRINT_LEN],
    pub alias: String,
}

impl Hello {
    pub fn encode(&self) -> Result<Bytes> {
        validate_device_id(&self.device_id)?;
        if self.alias.len() > MAX_ALIAS {
            return Err(Error::InvalidPayload("alias too long"));
        }
        let mut buf =
            BytesMut::with_capacity(2 + 4 + 1 + self.device_id.len() + 32 + 1 + self.alias.len());
        buf.put_u16(self.protocol_version);
        buf.put_u32(self.features.0);
        put_len_str(&mut buf, &self.device_id)?;
        buf.extend_from_slice(&self.fingerprint);
        put_len_str(&mut buf, &self.alias)?;
        Ok(buf.freeze())
    }

    pub fn decode(payload: &[u8]) -> Result<Self> {
        let mut i = 0;
        let protocol_version = read_u16(payload, &mut i)?;
        let features = Features(read_u32(payload, &mut i)?);
        let device_id = read_len_str(payload, &mut i, 1, MAX_DEVICE_ID)?;
        let fingerprint = read_fingerprint(payload, &mut i)?;
        let alias = read_len_str(payload, &mut i, 0, MAX_ALIAS)?;
        if i != payload.len() {
            return Err(Error::InvalidPayload("trailing bytes in Hello"));
        }
        Ok(Self {
            protocol_version,
            features,
            device_id,
            fingerprint,
            alias,
        })
    }

    pub fn msg_type_hello() -> MsgType {
        MsgType::Hello
    }

    pub fn msg_type_ack() -> MsgType {
        MsgType::HelloAck
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PairAccept {
    pub device_id: String,
    pub fingerprint: [u8; FINGERPRINT_LEN],
}

impl PairAccept {
    pub fn encode(&self) -> Result<Bytes> {
        validate_device_id(&self.device_id)?;
        let mut buf = BytesMut::new();
        put_len_str(&mut buf, &self.device_id)?;
        buf.extend_from_slice(&self.fingerprint);
        Ok(buf.freeze())
    }

    pub fn decode(payload: &[u8]) -> Result<Self> {
        let mut i = 0;
        let device_id = read_len_str(payload, &mut i, 1, MAX_DEVICE_ID)?;
        let fingerprint = read_fingerprint(payload, &mut i)?;
        if i != payload.len() {
            return Err(Error::InvalidPayload("trailing bytes in PairAccept"));
        }
        Ok(Self {
            device_id,
            fingerprint,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PairConfirm {
    pub device_id: String,
    pub fingerprint: [u8; FINGERPRINT_LEN],
}

impl PairConfirm {
    pub fn encode(&self) -> Result<Bytes> {
        validate_device_id(&self.device_id)?;
        let mut buf = BytesMut::new();
        put_len_str(&mut buf, &self.device_id)?;
        buf.extend_from_slice(&self.fingerprint);
        Ok(buf.freeze())
    }

    pub fn decode(payload: &[u8]) -> Result<Self> {
        let mut i = 0;
        let device_id = read_len_str(payload, &mut i, 1, MAX_DEVICE_ID)?;
        let fingerprint = read_fingerprint(payload, &mut i)?;
        if i != payload.len() {
            return Err(Error::InvalidPayload("trailing bytes in PairConfirm"));
        }
        Ok(Self {
            device_id,
            fingerprint,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u16)]
pub enum PairRejectReason {
    BadCode = 1,
    Expired = 2,
    VersionMismatch = 3,
    FeatureMismatch = 4,
    Internal = 5,
    FingerprintMismatch = 6,
}

impl PairRejectReason {
    pub fn from_u16(v: u16) -> Option<Self> {
        Some(match v {
            1 => Self::BadCode,
            2 => Self::Expired,
            3 => Self::VersionMismatch,
            4 => Self::FeatureMismatch,
            5 => Self::Internal,
            6 => Self::FingerprintMismatch,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PairReject {
    pub reason: PairRejectReason,
}

impl PairReject {
    pub fn encode(&self) -> Bytes {
        let mut buf = BytesMut::with_capacity(2);
        buf.put_u16(self.reason as u16);
        buf.freeze()
    }

    pub fn decode(payload: &[u8]) -> Result<Self> {
        if payload.len() != 2 {
            return Err(Error::InvalidPayload("PairReject length"));
        }
        let code = u16::from_be_bytes([payload[0], payload[1]]);
        let reason = PairRejectReason::from_u16(code)
            .ok_or(Error::InvalidPayload("unknown reject reason"))?;
        Ok(Self { reason })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PairVerify {
    pub tag: [u8; FINGERPRINT_LEN],
}

impl PairVerify {
    pub fn encode(&self) -> Bytes {
        Bytes::copy_from_slice(&self.tag)
    }

    pub fn decode(payload: &[u8]) -> Result<Self> {
        if payload.len() != FINGERPRINT_LEN {
            return Err(Error::InvalidPayload("PairVerify length"));
        }
        let mut tag = [0u8; FINGERPRINT_LEN];
        tag.copy_from_slice(payload);
        Ok(Self { tag })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileOffer {
    pub path: String,
    pub file_size: u64,
}

impl FileOffer {
    pub fn encode(&self) -> Result<Bytes> {
        if self.path.is_empty() || self.path.len() > 1024 {
            return Err(Error::InvalidPayload("path length"));
        }
        let mut buf = BytesMut::new();
        buf.put_u16(self.path.len() as u16);
        buf.extend_from_slice(self.path.as_bytes());
        buf.put_u64(self.file_size);
        Ok(buf.freeze())
    }

    pub fn decode(payload: &[u8]) -> Result<Self> {
        let mut i = 0;
        let path_len = read_u16(payload, &mut i)? as usize;
        if path_len == 0 || path_len > 1024 {
            return Err(Error::InvalidPayload("path length"));
        }
        if i + path_len > payload.len() {
            return Err(Error::Truncated);
        }
        let path = std::str::from_utf8(&payload[i..i + path_len])
            .map_err(|_| Error::Utf8)?
            .to_string();
        i += path_len;
        if i + 8 > payload.len() {
            return Err(Error::Truncated);
        }
        let mut sz = [0u8; 8];
        sz.copy_from_slice(&payload[i..i + 8]);
        i += 8;
        if i != payload.len() {
            return Err(Error::InvalidPayload("trailing bytes in FileOffer"));
        }
        Ok(Self {
            path,
            file_size: u64::from_be_bytes(sz),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileAccept {
    pub resume_offset: u64,
}

impl FileAccept {
    pub fn encode(&self) -> Bytes {
        let mut buf = BytesMut::with_capacity(8);
        buf.put_u64(self.resume_offset);
        buf.freeze()
    }

    pub fn decode(payload: &[u8]) -> Result<Self> {
        if payload.len() != 8 {
            return Err(Error::InvalidPayload("FileAccept length"));
        }
        let mut n = [0u8; 8];
        n.copy_from_slice(payload);
        Ok(Self {
            resume_offset: u64::from_be_bytes(n),
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u16)]
pub enum FileRejectReason {
    BadPath = 1,
    Unsupported = 2,
    Io = 3,
    Busy = 4,
}

impl FileRejectReason {
    pub fn from_u16(v: u16) -> Option<Self> {
        Some(match v {
            1 => Self::BadPath,
            2 => Self::Unsupported,
            3 => Self::Io,
            4 => Self::Busy,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileReject {
    pub reason: FileRejectReason,
}

impl FileReject {
    pub fn encode(&self) -> Bytes {
        let mut buf = BytesMut::with_capacity(2);
        buf.put_u16(self.reason as u16);
        buf.freeze()
    }

    pub fn decode(payload: &[u8]) -> Result<Self> {
        if payload.len() != 2 {
            return Err(Error::InvalidPayload("FileReject length"));
        }
        let code = u16::from_be_bytes([payload[0], payload[1]]);
        let reason =
            FileRejectReason::from_u16(code).ok_or(Error::InvalidPayload("unknown file reject"))?;
        Ok(Self { reason })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileComplete {
    pub status: u16,
    pub bytes_transferred: u64,
}

impl FileComplete {
    pub fn encode(&self) -> Bytes {
        let mut buf = BytesMut::with_capacity(10);
        buf.put_u16(self.status);
        buf.put_u64(self.bytes_transferred);
        buf.freeze()
    }

    pub fn decode(payload: &[u8]) -> Result<Self> {
        if payload.len() != 10 {
            return Err(Error::InvalidPayload("FileComplete length"));
        }
        let status = u16::from_be_bytes([payload[0], payload[1]]);
        let mut n = [0u8; 8];
        n.copy_from_slice(&payload[2..10]);
        Ok(Self {
            status,
            bytes_transferred: u64::from_be_bytes(n),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirOffer {
    pub path: String,
    pub file_count: u32,
}

impl DirOffer {
    pub fn encode(&self) -> Result<Bytes> {
        if self.path.is_empty() || self.path.len() > 1024 {
            return Err(Error::InvalidPayload("dir path length"));
        }
        let mut buf = BytesMut::new();
        buf.put_u16(self.path.len() as u16);
        buf.extend_from_slice(self.path.as_bytes());
        buf.put_u32(self.file_count);
        Ok(buf.freeze())
    }

    pub fn decode(payload: &[u8]) -> Result<Self> {
        let mut i = 0;
        let path_len = read_u16(payload, &mut i)? as usize;
        if path_len == 0 || path_len > 1024 || i + path_len + 4 > payload.len() {
            return Err(Error::InvalidPayload("DirOffer"));
        }
        let path = std::str::from_utf8(&payload[i..i + path_len])
            .map_err(|_| Error::Utf8)?
            .to_string();
        i += path_len;
        let file_count = u32::from_be_bytes(payload[i..i + 4].try_into().unwrap());
        i += 4;
        if i != payload.len() {
            return Err(Error::InvalidPayload("trailing bytes in DirOffer"));
        }
        Ok(Self { path, file_count })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirComplete {
    pub status: u16,
    pub files_ok: u32,
}

impl DirComplete {
    pub fn encode(&self) -> Bytes {
        let mut buf = BytesMut::with_capacity(6);
        buf.put_u16(self.status);
        buf.put_u32(self.files_ok);
        buf.freeze()
    }

    pub fn decode(payload: &[u8]) -> Result<Self> {
        if payload.len() != 6 {
            return Err(Error::InvalidPayload("DirComplete length"));
        }
        Ok(Self {
            status: u16::from_be_bytes([payload[0], payload[1]]),
            files_ok: u32::from_be_bytes([payload[2], payload[3], payload[4], payload[5]]),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProbeRequest {
    pub byte_count: u64,
}

impl ProbeRequest {
    pub const MAX_BYTES: u64 = 16 * 1024 * 1024;

    pub fn encode(&self) -> Result<Bytes> {
        if self.byte_count == 0 || self.byte_count > Self::MAX_BYTES {
            return Err(Error::InvalidPayload("probe size"));
        }
        let mut buf = BytesMut::with_capacity(8);
        buf.put_u64(self.byte_count);
        Ok(buf.freeze())
    }

    pub fn decode(payload: &[u8]) -> Result<Self> {
        if payload.len() != 8 {
            return Err(Error::InvalidPayload("ProbeRequest length"));
        }
        let mut n = [0u8; 8];
        n.copy_from_slice(payload);
        let byte_count = u64::from_be_bytes(n);
        if byte_count == 0 || byte_count > Self::MAX_BYTES {
            return Err(Error::InvalidPayload("probe size"));
        }
        Ok(Self { byte_count })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProbeResult {
    pub bytes: u64,
    pub elapsed_us: u64,
}

impl ProbeResult {
    pub fn encode(&self) -> Bytes {
        let mut buf = BytesMut::with_capacity(16);
        buf.put_u64(self.bytes);
        buf.put_u64(self.elapsed_us);
        buf.freeze()
    }

    pub fn decode(payload: &[u8]) -> Result<Self> {
        if payload.len() != 16 {
            return Err(Error::InvalidPayload("ProbeResult length"));
        }
        let mut a = [0u8; 8];
        let mut b = [0u8; 8];
        a.copy_from_slice(&payload[0..8]);
        b.copy_from_slice(&payload[8..16]);
        Ok(Self {
            bytes: u64::from_be_bytes(a),
            elapsed_us: u64::from_be_bytes(b),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenChannels {
    pub streams: u16,
    pub token: u64,
}

impl OpenChannels {
    pub fn encode(&self) -> Result<Bytes> {
        if self.streams == 0 || self.streams > 8 {
            return Err(Error::InvalidPayload("stream count"));
        }
        let mut buf = BytesMut::with_capacity(10);
        buf.put_u16(self.streams);
        buf.put_u64(self.token);
        Ok(buf.freeze())
    }

    pub fn decode(payload: &[u8]) -> Result<Self> {
        if payload.len() != 10 {
            return Err(Error::InvalidPayload("OpenChannels length"));
        }
        let streams = u16::from_be_bytes([payload[0], payload[1]]);
        if streams == 0 || streams > 8 {
            return Err(Error::InvalidPayload("stream count"));
        }
        let mut t = [0u8; 8];
        t.copy_from_slice(&payload[2..10]);
        Ok(Self {
            streams,
            token: u64::from_be_bytes(t),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChannelHello {
    pub index: u16,
    pub token: u64,
}

impl ChannelHello {
    pub fn encode(&self) -> Bytes {
        let mut buf = BytesMut::with_capacity(10);
        buf.put_u16(self.index);
        buf.put_u64(self.token);
        buf.freeze()
    }

    pub fn decode(payload: &[u8]) -> Result<Self> {
        if payload.len() != 10 {
            return Err(Error::InvalidPayload("ChannelHello length"));
        }
        let index = u16::from_be_bytes([payload[0], payload[1]]);
        let mut t = [0u8; 8];
        t.copy_from_slice(&payload[2..10]);
        Ok(Self {
            index,
            token: u64::from_be_bytes(t),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SliceHeader {
    pub offset: u64,
    pub length: u64,
}

impl SliceHeader {
    pub fn encode(&self) -> Bytes {
        let mut buf = BytesMut::with_capacity(16);
        buf.put_u64(self.offset);
        buf.put_u64(self.length);
        buf.freeze()
    }

    pub fn decode(payload: &[u8]) -> Result<Self> {
        if payload.len() != 16 {
            return Err(Error::InvalidPayload("SliceHeader length"));
        }
        let mut a = [0u8; 8];
        let mut b = [0u8; 8];
        a.copy_from_slice(&payload[0..8]);
        b.copy_from_slice(&payload[8..16]);
        Ok(Self {
            offset: u64::from_be_bytes(a),
            length: u64::from_be_bytes(b),
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum ClipKind {
    TextPlain = 1,
    ImagePng = 2,
}

impl ClipKind {
    pub fn from_u8(v: u8) -> Option<Self> {
        match v {
            1 => Some(Self::TextPlain),
            2 => Some(Self::ImagePng),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClipOffer {
    pub kind: ClipKind,
    pub seq: u64,
    pub byte_len: u64,
}

impl ClipOffer {
    pub const MAX_TEXT: u64 = 1024 * 1024;
    pub const MAX_PNG: u64 = 16 * 1024 * 1024;

    pub fn encode(&self) -> Result<Bytes> {
        let max = match self.kind {
            ClipKind::TextPlain => Self::MAX_TEXT,
            ClipKind::ImagePng => Self::MAX_PNG,
        };
        if self.byte_len == 0 || self.byte_len > max {
            return Err(Error::InvalidPayload("clip size"));
        }
        let mut buf = BytesMut::with_capacity(17);
        buf.put_u8(self.kind as u8);
        buf.put_u64(self.seq);
        buf.put_u64(self.byte_len);
        Ok(buf.freeze())
    }

    pub fn decode(payload: &[u8]) -> Result<Self> {
        if payload.len() != 17 {
            return Err(Error::InvalidPayload("ClipOffer length"));
        }
        let kind = ClipKind::from_u8(payload[0]).ok_or(Error::InvalidPayload("clip kind"))?;
        let mut seq = [0u8; 8];
        let mut len = [0u8; 8];
        seq.copy_from_slice(&payload[1..9]);
        len.copy_from_slice(&payload[9..17]);
        let byte_len = u64::from_be_bytes(len);
        let max = match kind {
            ClipKind::TextPlain => Self::MAX_TEXT,
            ClipKind::ImagePng => Self::MAX_PNG,
        };
        if byte_len == 0 || byte_len > max {
            return Err(Error::InvalidPayload("clip size"));
        }
        Ok(Self {
            kind,
            seq: u64::from_be_bytes(seq),
            byte_len,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClipAck {
    pub seq: u64,
    pub status: u16,
}

impl ClipAck {
    pub fn encode(&self) -> Bytes {
        let mut buf = BytesMut::with_capacity(10);
        buf.put_u64(self.seq);
        buf.put_u16(self.status);
        buf.freeze()
    }

    pub fn decode(payload: &[u8]) -> Result<Self> {
        if payload.len() != 10 {
            return Err(Error::InvalidPayload("ClipAck length"));
        }
        let mut seq = [0u8; 8];
        seq.copy_from_slice(&payload[0..8]);
        Ok(Self {
            seq: u64::from_be_bytes(seq),
            status: u16::from_be_bytes([payload[8], payload[9]]),
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u16)]
pub enum ClipRejectReason {
    Unsupported = 1,
    TooLarge = 2,
    Busy = 3,
}

impl ClipRejectReason {
    pub fn from_u16(v: u16) -> Option<Self> {
        match v {
            1 => Some(Self::Unsupported),
            2 => Some(Self::TooLarge),
            3 => Some(Self::Busy),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClipReject {
    pub seq: u64,
    pub reason: ClipRejectReason,
}

impl ClipReject {
    pub fn encode(&self) -> Bytes {
        let mut buf = BytesMut::with_capacity(10);
        buf.put_u64(self.seq);
        buf.put_u16(self.reason as u16);
        buf.freeze()
    }

    pub fn decode(payload: &[u8]) -> Result<Self> {
        if payload.len() != 10 {
            return Err(Error::InvalidPayload("ClipReject length"));
        }
        let mut seq = [0u8; 8];
        seq.copy_from_slice(&payload[0..8]);
        let reason = ClipRejectReason::from_u16(u16::from_be_bytes([payload[8], payload[9]]))
            .ok_or(Error::InvalidPayload("clip reject"))?;
        Ok(Self {
            seq: u64::from_be_bytes(seq),
            reason,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ping {
    pub nonce: u64,
}

impl Ping {
    pub fn encode(&self) -> Bytes {
        let mut buf = BytesMut::with_capacity(8);
        buf.put_u64(self.nonce);
        buf.freeze()
    }

    pub fn decode(payload: &[u8]) -> Result<Self> {
        if payload.len() != 8 {
            return Err(Error::InvalidPayload("Ping length"));
        }
        let mut n = [0u8; 8];
        n.copy_from_slice(payload);
        Ok(Self {
            nonce: u64::from_be_bytes(n),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pong {
    pub nonce: u64,
}

impl Pong {
    pub fn encode(&self) -> Bytes {
        let mut buf = BytesMut::with_capacity(8);
        buf.put_u64(self.nonce);
        buf.freeze()
    }

    pub fn decode(payload: &[u8]) -> Result<Self> {
        if payload.len() != 8 {
            return Err(Error::InvalidPayload("Pong length"));
        }
        let mut n = [0u8; 8];
        n.copy_from_slice(payload);
        Ok(Self {
            nonce: u64::from_be_bytes(n),
        })
    }
}

/// Graceful goodbye (`0x0030`): the sender is leaving (unpaired / shutting down).
///
/// Empty payload by spec, and only meaningful inside an established trusted session — receivers
/// on an unauthenticated connection must ignore it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Bye;

impl Bye {
    pub fn encode(&self) -> Bytes {
        Bytes::new()
    }

    pub fn decode(payload: &[u8]) -> Result<Self> {
        if !payload.is_empty() {
            return Err(Error::InvalidPayload("Bye payload"));
        }
        Ok(Self)
    }
}

fn validate_device_id(id: &str) -> Result<()> {
    if id.is_empty() || id.len() > MAX_DEVICE_ID {
        return Err(Error::InvalidPayload("device_id length"));
    }
    Ok(())
}

fn put_len_str(buf: &mut BytesMut, s: &str) -> Result<()> {
    if s.len() > u8::MAX as usize {
        return Err(Error::InvalidPayload("string too long"));
    }
    buf.put_u8(s.len() as u8);
    buf.extend_from_slice(s.as_bytes());
    Ok(())
}

fn read_u16(buf: &[u8], i: &mut usize) -> Result<u16> {
    if *i + 2 > buf.len() {
        return Err(Error::Truncated);
    }
    let v = u16::from_be_bytes([buf[*i], buf[*i + 1]]);
    *i += 2;
    Ok(v)
}

fn read_u32(buf: &[u8], i: &mut usize) -> Result<u32> {
    if *i + 4 > buf.len() {
        return Err(Error::Truncated);
    }
    let v = u32::from_be_bytes([buf[*i], buf[*i + 1], buf[*i + 2], buf[*i + 3]]);
    *i += 4;
    Ok(v)
}

fn read_len_str(buf: &[u8], i: &mut usize, min: usize, max: usize) -> Result<String> {
    if *i >= buf.len() {
        return Err(Error::Truncated);
    }
    let len = buf[*i] as usize;
    *i += 1;
    if len < min || len > max {
        return Err(Error::InvalidPayload("string length"));
    }
    if *i + len > buf.len() {
        return Err(Error::Truncated);
    }
    let s = std::str::from_utf8(&buf[*i..*i + len]).map_err(|_| Error::Utf8)?;
    *i += len;
    Ok(s.to_string())
}

fn read_fingerprint(buf: &[u8], i: &mut usize) -> Result<[u8; FINGERPRINT_LEN]> {
    if *i + FINGERPRINT_LEN > buf.len() {
        return Err(Error::Truncated);
    }
    let mut fp = [0u8; FINGERPRINT_LEN];
    fp.copy_from_slice(&buf[*i..*i + FINGERPRINT_LEN]);
    *i += FINGERPRINT_LEN;
    Ok(fp)
}
