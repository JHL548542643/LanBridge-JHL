use crate::error::{Error, Result};
use bytes::{Bytes, BytesMut};
use lb_crypto::{fingerprint_sha256, Fingerprint};
use lb_protocol::{encode_frame, Frame, MsgType, MAX_PAYLOAD_LEN};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio_rustls::TlsStream;

/// Framed control channel over an established TLS stream.
pub struct ControlSession {
    stream: TlsStream<TcpStream>,
    read_buf: BytesMut,
    /// One-frame pushback for inbound demux (ProbeRequest vs ClipOffer).
    pushed: Option<Frame>,
}

impl ControlSession {
    pub fn new(stream: TlsStream<TcpStream>) -> Self {
        Self {
            stream,
            read_buf: BytesMut::with_capacity(4096),
            pushed: None,
        }
    }

    /// Push a frame so the next [`recv_frame`] returns it (inbound demux).
    pub fn push_frame(&mut self, frame: Frame) {
        self.pushed = Some(frame);
    }

    /// SHA-256 fingerprint of the peer's end-entity certificate.
    pub fn peer_fingerprint(&self) -> Result<Fingerprint> {
        let certs = match &self.stream {
            TlsStream::Client(s) => s.get_ref().1.peer_certificates(),
            TlsStream::Server(s) => s.get_ref().1.peer_certificates(),
        };
        let Some(certs) = certs else {
            return Err(Error::Tls("missing peer certificates".into()));
        };
        let Some(end) = certs.first() else {
            return Err(Error::Tls("empty peer certificate chain".into()));
        };
        Ok(fingerprint_sha256(end.as_ref()))
    }

    pub async fn send_msg(&mut self, msg_type: MsgType, payload: Bytes) -> Result<()> {
        let frame = Frame::new(msg_type, payload);
        let mut buf = BytesMut::new();
        encode_frame(&frame, &mut buf)?;
        self.stream.write_all(&buf).await?;
        self.stream.flush().await?;
        Ok(())
    }

    /// Send `Bye` and flush it. The caller must close **after** this: closing first leaves the
    /// peer with a bare EOF and it cannot tell "goodbye" from "crashed".
    pub async fn send_bye(&mut self) -> Result<()> {
        self.send_msg(MsgType::Bye, Bytes::new()).await
    }

    pub async fn send_frame(&mut self, frame: &Frame) -> Result<()> {
        let mut buf = BytesMut::new();
        encode_frame(frame, &mut buf)?;
        self.stream.write_all(&buf).await?;
        self.stream.flush().await?;
        Ok(())
    }

    /// Read one frame.
    ///
    /// `Bye` is interpreted here rather than in every caller: a peer that says goodbye is
    /// **leaving**, so the reader gets [`Error::PeerLeft`] and must drop trust. Use
    /// [`Self::recv_frame_raw`] when the raw frame is needed (pairing, tests).
    pub async fn recv_frame(&mut self) -> Result<Frame> {
        let frame = self.recv_frame_raw().await?;
        if frame.msg_type == MsgType::Bye.as_u16() {
            return Err(Error::PeerLeft);
        }
        Ok(frame)
    }

    /// Read one frame without interpreting `Bye`.
    pub async fn recv_frame_raw(&mut self) -> Result<Frame> {
        if let Some(frame) = self.pushed.take() {
            return Ok(frame);
        }
        loop {
            if let Some((frame, n)) = lb_protocol::try_decode(&self.read_buf)? {
                if MsgType::from_u16(frame.msg_type).is_none() {
                    let _ = self.read_buf.split_to(n);
                    continue;
                }
                let _ = self.read_buf.split_to(n);
                return Ok(frame);
            }
            let mut tmp = vec![0u8; 8192];
            let n = self.stream.read(&mut tmp).await?;
            if n == 0 {
                return Err(Error::PeerClosed);
            }
            if self.read_buf.len() + n > HEADER_CAP {
                return Err(Error::Protocol("read buffer overflow"));
            }
            self.read_buf.extend_from_slice(&tmp[..n]);
        }
    }

    /// Write raw bytes (file / probe data plane) — not LBfr-framed.
    /// Caller flushes once per slice or probe, not per small chunk.
    pub async fn write_raw(&mut self, data: &[u8]) -> Result<()> {
        self.stream.write_all(data).await?;
        Ok(())
    }

    pub async fn flush(&mut self) -> Result<()> {
        self.stream.flush().await?;
        Ok(())
    }

    /// Send TLS close_notify then shut down the write half (avoid abrupt EOF on peer).
    pub async fn shutdown(&mut self) -> Result<()> {
        let _ = self.stream.flush().await;
        self.stream.shutdown().await?;
        Ok(())
    }

    /// Read exactly `buf.len()` raw bytes, consuming any leftover control buffer first.
    pub async fn read_exact_raw(&mut self, buf: &mut [u8]) -> Result<()> {
        let mut filled = 0;
        while filled < buf.len() && !self.read_buf.is_empty() {
            let n = (buf.len() - filled).min(self.read_buf.len());
            buf[filled..filled + n].copy_from_slice(&self.read_buf[..n]);
            let _ = self.read_buf.split_to(n);
            filled += n;
        }
        while filled < buf.len() {
            let n = self.stream.read(&mut buf[filled..]).await?;
            if n == 0 {
                return Err(Error::PeerClosed);
            }
            filled += n;
        }
        Ok(())
    }
}

const HEADER_CAP: usize = MAX_PAYLOAD_LEN as usize + 64 * 1024;
