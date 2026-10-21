use crate::error::{Error, Result};
use crate::stream::ControlSession;
use crate::trust::TrustedPeer;
use lb_crypto::{fingerprint_sha256, DeviceIdentity, Fingerprint};
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::server::danger::{ClientCertVerified, ClientCertVerifier};
use rustls::{
    ClientConfig, DigitallySignedStruct, DistinguishedName, Error as RustlsError, ServerConfig,
    SignatureScheme,
};
use std::sync::Arc;
use tokio::net::TcpStream;
use tokio_rustls::{TlsAcceptor, TlsConnector};

#[derive(Debug, Clone, Copy)]
pub enum TlsMode {
    /// Accept any self-signed peer cert (pairing only); still requires peer present (mTLS).
    Pairing,
    /// Require exact SHA-256 fingerprint match.
    Trusted { expect_fingerprint: Fingerprint },
}

pub async fn accept_tls(
    tcp: TcpStream,
    identity: Arc<DeviceIdentity>,
    mode: TlsMode,
) -> Result<ControlSession> {
    let config = make_server_config(identity, mode)?;
    let acceptor = TlsAcceptor::from(config);
    let tls = acceptor
        .accept(tcp)
        .await
        .map_err(|e| Error::Tls(e.to_string()))?;
    Ok(ControlSession::new(tokio_rustls::TlsStream::Server(tls)))
}

pub async fn connect_tls(
    tcp: TcpStream,
    identity: Arc<DeviceIdentity>,
    mode: TlsMode,
    _peer: Option<&TrustedPeer>,
) -> Result<ControlSession> {
    let config = make_client_config(identity, mode)?;
    let connector = TlsConnector::from(config);
    let server_name = ServerName::try_from("lanbridge.device")
        .map_err(|e| Error::Tls(format!("server name: {e}")))?;
    let tls = connector
        .connect(server_name, tcp)
        .await
        .map_err(|e| Error::Tls(e.to_string()))?;
    Ok(ControlSession::new(tokio_rustls::TlsStream::Client(tls)))
}

fn make_server_config(identity: Arc<DeviceIdentity>, mode: TlsMode) -> Result<Arc<ServerConfig>> {
    let verifier: Arc<dyn ClientCertVerifier> = match mode {
        TlsMode::Pairing => Arc::new(AllowAnyClient),
        TlsMode::Trusted { expect_fingerprint } => Arc::new(PinClient {
            expect: expect_fingerprint,
        }),
    };
    let cert = identity.certificate_der();
    let key = identity.private_key_der();
    let mut cfg = ServerConfig::builder()
        .with_client_cert_verifier(verifier)
        .with_single_cert(vec![cert], key)
        .map_err(|e| Error::Tls(e.to_string()))?;
    cfg.alpn_protocols = vec![b"lanbridge/0".to_vec()];
    Ok(Arc::new(cfg))
}

fn make_client_config(identity: Arc<DeviceIdentity>, mode: TlsMode) -> Result<Arc<ClientConfig>> {
    let verifier: Arc<dyn ServerCertVerifier> = match mode {
        TlsMode::Pairing => Arc::new(AllowAnyServer),
        TlsMode::Trusted { expect_fingerprint } => Arc::new(PinServer {
            expect: expect_fingerprint,
        }),
    };
    let cert = identity.certificate_der();
    let key = identity.private_key_der();
    let mut cfg = ClientConfig::builder()
        .dangerous()
        .with_custom_certificate_verifier(verifier)
        .with_client_auth_cert(vec![cert], key)
        .map_err(|e| Error::Tls(e.to_string()))?;
    cfg.alpn_protocols = vec![b"lanbridge/0".to_vec()];
    Ok(Arc::new(cfg))
}

fn schemes() -> Vec<SignatureScheme> {
    rustls::crypto::ring::default_provider()
        .signature_verification_algorithms
        .supported_schemes()
}

#[derive(Debug)]
struct AllowAnyServer;

impl ServerCertVerifier for AllowAnyServer {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp: &[u8],
        _now: UnixTime,
    ) -> std::result::Result<ServerCertVerified, RustlsError> {
        if end_entity.as_ref().is_empty() {
            return Err(RustlsError::General("empty server cert".into()));
        }
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, RustlsError> {
        rustls::crypto::verify_tls12_signature(
            message,
            cert,
            dss,
            &rustls::crypto::ring::default_provider().signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, RustlsError> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &rustls::crypto::ring::default_provider().signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        schemes()
    }
}

#[derive(Debug)]
struct PinServer {
    expect: Fingerprint,
}

impl ServerCertVerifier for PinServer {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp: &[u8],
        _now: UnixTime,
    ) -> std::result::Result<ServerCertVerified, RustlsError> {
        let fp = fingerprint_sha256(end_entity.as_ref());
        if fp != self.expect {
            return Err(RustlsError::General("server fingerprint mismatch".into()));
        }
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, RustlsError> {
        rustls::crypto::verify_tls12_signature(
            message,
            cert,
            dss,
            &rustls::crypto::ring::default_provider().signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, RustlsError> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &rustls::crypto::ring::default_provider().signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        schemes()
    }
}

#[derive(Debug)]
struct AllowAnyClient;

impl ClientCertVerifier for AllowAnyClient {
    fn root_hint_subjects(&self) -> &[DistinguishedName] {
        &[]
    }

    fn verify_client_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _now: UnixTime,
    ) -> std::result::Result<ClientCertVerified, RustlsError> {
        if end_entity.as_ref().is_empty() {
            return Err(RustlsError::General("empty client cert".into()));
        }
        Ok(ClientCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, RustlsError> {
        rustls::crypto::verify_tls12_signature(
            message,
            cert,
            dss,
            &rustls::crypto::ring::default_provider().signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, RustlsError> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &rustls::crypto::ring::default_provider().signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        schemes()
    }
}

#[derive(Debug)]
struct PinClient {
    expect: Fingerprint,
}

impl ClientCertVerifier for PinClient {
    fn root_hint_subjects(&self) -> &[DistinguishedName] {
        &[]
    }

    fn verify_client_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _now: UnixTime,
    ) -> std::result::Result<ClientCertVerified, RustlsError> {
        let fp = fingerprint_sha256(end_entity.as_ref());
        if fp != self.expect {
            return Err(RustlsError::General("client fingerprint mismatch".into()));
        }
        Ok(ClientCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, RustlsError> {
        rustls::crypto::verify_tls12_signature(
            message,
            cert,
            dss,
            &rustls::crypto::ring::default_provider().signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, RustlsError> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &rustls::crypto::ring::default_provider().signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        schemes()
    }
}
