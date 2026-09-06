//! TLS 监听器这一侧：可热换证书的 acceptor（ADR-003 D4 external 的"文件变化即热加载"、
//! `agora tls rotate-key` 之后不必重启）。
//!
//! 热换靠 rustls 的 `ResolvesServerCert`：`ServerConfig` 建好后不再动，每次握手走到 resolve 时读
//! 当前的 `CertifiedKey`；换证书就是换那个 `Arc`。不重建 `ServerConfig`，是因为 `TlsAcceptor`
//! 已经被 accept 循环持有——重建就得把循环也换掉。

use std::fmt;
use std::sync::{Arc, RwLock};

use rustls::server::{ClientHello, ResolvesServerCert};
use rustls::sign::CertifiedKey;
use rustls::ServerConfig;
use tokio::net::TcpStream;
use tokio_rustls::TlsAcceptor;

use super::fingerprint::Fingerprint;
use super::{Identity, TlsError};

/// 当前证书 + 指纹 + notAfter；三者一起换，读到的永远是同一张证书的。
struct Current {
    key: Arc<CertifiedKey>,
    fingerprint: Fingerprint,
    not_after: i64,
}

struct Swappable(RwLock<Current>);

impl Swappable {
    fn read(&self) -> std::sync::RwLockReadGuard<'_, Current> {
        self.0.read().unwrap_or_else(|p| p.into_inner())
    }
}

impl fmt::Debug for Swappable {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Swappable({})", self.read().fingerprint)
    }
}

impl ResolvesServerCert for Swappable {
    fn resolve(&self, _client_hello: ClientHello<'_>) -> Option<Arc<CertifiedKey>> {
        Some(self.read().key.clone())
    }
}

/// 一个 TLS 监听器的 acceptor；`Clone` 共享同一份证书，任何一份上 `swap` 全体可见。
#[derive(Clone)]
pub struct Acceptor {
    inner: TlsAcceptor,
    cert: Arc<Swappable>,
}

impl Acceptor {
    pub fn new(identity: &Identity) -> Result<Acceptor, TlsError> {
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let current = Current {
            key: Arc::new(identity.certified_key(&provider)?),
            fingerprint: identity.fingerprint(),
            not_after: identity.validity()?.1,
        };
        let cert = Arc::new(Swappable(RwLock::new(current)));
        let mut cfg = ServerConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()?
            .with_no_client_auth()
            .with_cert_resolver(cert.clone());
        // 只做 HTTP/1.1：accept 循环用 hyper 的 http1 服务连接（WS 升级要它），不让客户端协商到 h2。
        cfg.alpn_protocols = vec![b"http/1.1".to_vec()];
        Ok(Acceptor {
            inner: TlsAcceptor::from(Arc::new(cfg)),
            cert,
        })
    }

    /// 换证书；返回换下来的指纹，调用方据此判断 SPKI 变没变（变了 → peer 需更新指纹）。
    /// 先把新证书完整装好再换，失败时旧证书一点没动。
    pub fn swap(&self, identity: &Identity) -> Result<Fingerprint, TlsError> {
        let provider = rustls::crypto::ring::default_provider();
        let next = Current {
            key: Arc::new(identity.certified_key(&provider)?),
            fingerprint: identity.fingerprint(),
            not_after: identity.validity()?.1,
        };
        let mut guard = self.cert.0.write().unwrap_or_else(|p| p.into_inner());
        let old = guard.fingerprint;
        *guard = next;
        Ok(old)
    }

    pub fn fingerprint(&self) -> Fingerprint {
        self.cert.read().fingerprint
    }

    /// 当前证书的 notAfter（unix 秒）；external 续期按它排。
    pub fn not_after(&self) -> i64 {
        self.cert.read().not_after
    }

    pub async fn accept(
        &self,
        tcp: TcpStream,
    ) -> std::io::Result<tokio_rustls::server::TlsStream<TcpStream>> {
        self.inner.accept(tcp).await
    }
}

impl fmt::Debug for Acceptor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Acceptor({})", self.fingerprint())
    }
}
