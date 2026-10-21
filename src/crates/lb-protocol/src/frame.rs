use crate::error::{Error, Result};
use crate::types::{MsgType, PROTOCOL_VERSION};
use bytes::{BufMut, Bytes, BytesMut};

/// ASCII `LBfr`
pub const MAGIC: [u8; 4] = *b"LBfr";
pub const HEADER_LEN: usize = 12;
pub const MAX_PAYLOAD_LEN: u32 = 1_048_576;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    pub version: u16,
    pub msg_type: u16,
    pub payload: Bytes,
}

impl Frame {
    pub fn new(msg_type: MsgType, payload: impl Into<Bytes>) -> Self {
        Self {
            version: PROTOCOL_VERSION,
            msg_type: msg_type.as_u16(),
            payload: payload.into(),
        }
    }

    pub fn known_type(&self) -> Option<MsgType> {
        MsgType::from_u16(self.msg_type)
    }
}

pub fn encode_frame(frame: &Frame, dst: &mut BytesMut) -> Result<()> {
    if frame.payload.len() > MAX_PAYLOAD_LEN as usize {
        return Err(Error::PayloadTooLarge(frame.payload.len() as u32));
    }
    dst.reserve(HEADER_LEN + frame.payload.len());
    dst.extend_from_slice(&MAGIC);
    dst.put_u16(frame.version);
    dst.put_u16(frame.msg_type);
    dst.put_u32(frame.payload.len() as u32);
    dst.extend_from_slice(&frame.payload);
    Ok(())
}

/// Decode one frame; returns `None` if more bytes are needed.
pub fn try_decode(src: &[u8]) -> Result<Option<(Frame, usize)>> {
    if src.len() < HEADER_LEN {
        return Ok(None);
    }
    if src[0..4] != MAGIC {
        return Err(Error::BadMagic);
    }
    let version = u16::from_be_bytes([src[4], src[5]]);
    if version != PROTOCOL_VERSION {
        return Err(Error::BadVersion(version));
    }
    let msg_type = u16::from_be_bytes([src[6], src[7]]);
    let payload_len = u32::from_be_bytes([src[8], src[9], src[10], src[11]]);
    if payload_len > MAX_PAYLOAD_LEN {
        return Err(Error::PayloadTooLarge(payload_len));
    }
    let total = HEADER_LEN + payload_len as usize;
    if src.len() < total {
        return Ok(None);
    }
    let payload = Bytes::copy_from_slice(&src[HEADER_LEN..total]);
    Ok(Some((
        Frame {
            version,
            msg_type,
            payload,
        },
        total,
    )))
}

pub fn decode_frame(src: &[u8]) -> Result<(Frame, usize)> {
    match try_decode(src)? {
        Some(v) => Ok(v),
        None => Err(Error::Truncated),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::MsgType;

    #[test]
    fn roundtrip_empty_ping() {
        let frame = Frame::new(MsgType::Ping, Bytes::new());
        let mut buf = BytesMut::new();
        encode_frame(&frame, &mut buf).unwrap();
        let (decoded, n) = decode_frame(&buf).unwrap();
        assert_eq!(n, buf.len());
        assert_eq!(decoded, frame);
    }
}
