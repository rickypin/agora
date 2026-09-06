//! peer 客户端这一侧：只认 SPKI 指纹的证书校验器（ADR-003 D4）。
//!
//! 信任就是配置里那一个指纹，像 ssh 的 host key：不看 CA、不看主机名、不看有效期、没有 TOFU。
//! 握手签名照常验（`verify_tls12_signature` / `verify_tls13_signature`）——那是"对端真的持有这把
//! 私钥"的证明，指纹只说"这把公钥是我认的那把"，两者缺一不可。

use std::fmt;
use std::sync::Arc;

use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::WebPkiSupportedAlgorithms;
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{CertificateError, ClientConfig, DigitallySignedStruct, OtherError, SignatureScheme};
use tokio::net::TcpStream;
use tokio_rustls::TlsConnector;

use super::fingerprint::Fingerprint;
use super::TlsError;

/// 指纹不匹配。它作为 `rustls::Error::Other` 穿过 tokio-rustls 的 `io::Error` 回到传输层，
/// [`Mismatch::from_io`] 把它按类型挖出来——传输层据此报 `FingerprintMismatch` 而不是"不可达"
/// （MISSION §2.3 规则 10：不做字符串匹配）。
#[derive(Debug, Clone, thiserror::Error, PartialEq, Eq)]
#[error("证书指纹不匹配：配置 {expected}，对端 {actual}")]
pub struct Mismatch {
    pub expected: Fingerprint,
    pub actual: Fingerprint,
}

impl Mismatch {
    /// 从 TLS 建连的 `io::Error` 里找出指纹不匹配；不是这一类就 None（交给"不可达"）。
    pub fn from_io(err: &std::io::Error) -> Option<Mismatch> {
        let inner = err.get_ref()?;
        match inner.downcast_ref::<rustls::Error>()? {
            rustls::Error::Other(OtherError(other)) => other.downcast_ref::<Mismatch>().cloned(),
            _ => None,
        }
    }
}

struct PinnedVerifier {
    expected: Fingerprint,
    algs: WebPkiSupportedAlgorithms,
}

impl fmt::Debug for PinnedVerifier {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "PinnedVerifier({})", self.expected)
    }
}

impl ServerCertVerifier for PinnedVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        let actual = Fingerprint::of_cert(end_entity)
            .map_err(|_| rustls::Error::InvalidCertificate(CertificateError::BadEncoding))?;
        if actual == self.expected {
            Ok(ServerCertVerified::assertion())
        } else {
            Err(rustls::Error::Other(OtherError(Arc::new(Mismatch {
                expected: self.expected,
                actual,
            }))))
        }
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(message, cert, dss, &self.algs)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(message, cert, dss, &self.algs)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.algs.supported_schemes()
    }
}

/// 钉住 `pin` 的连接器。没有根证书库——根本不看链。
pub fn connector(pin: Fingerprint) -> Result<TlsConnector, TlsError> {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let algs = provider.signature_verification_algorithms;
    let mut cfg = ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()?
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(PinnedVerifier {
            expected: pin,
            algs,
        }))
        .with_no_client_auth();
    cfg.alpn_protocols = vec![b"http/1.1".to_vec()];
    Ok(TlsConnector::from(Arc::new(cfg)))
}

/// 拨到 `host:port` 并完成握手。SNI 用 host——对端不按名字选证书、我们也不按名字校验，
/// 只是 TLS 要有一个 ServerName；IP 字面量也行。调用方负责超时。
pub async fn connect(
    connector: &TlsConnector,
    host: &str,
    port: u16,
) -> std::io::Result<tokio_rustls::client::TlsStream<TcpStream>> {
    let tcp = TcpStream::connect((host, port)).await?;
    tcp.set_nodelay(true)?;
    let name = ServerName::try_from(host.to_owned())
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidInput, e))?;
    connector.connect(name, tcp).await
}
