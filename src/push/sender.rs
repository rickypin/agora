//! 投递：订阅 → RFC 8291 密文 → Apple 端点。承载节点上的一个常驻任务订阅本机事件总线
//! （本机与 peer 的通知同源），对每条 `Event::Notification` 给所有未吊销设备的订阅发一份。
//!
//! 失败处理（agora-thc.6）：每订阅独立超时与指数退避；`404` / `410` 删订阅（端点已经作废）；
//! 连不上 Apple 记 `health.push.apple = false` 与原因。日志只记端点**主机**与失败类型，
//! 不记正文、不记订阅密钥。

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::http::{header, Request};
use http_body_util::Full;
use hyper::body::Bytes;
use hyper_util::client::legacy::Client;
use hyper_util::rt::TokioExecutor;
use tokio::sync::broadcast::error::RecvError;

use crate::events::Event;
use crate::session::Db;

use super::store::{PushStore, PushSubscription};
use super::vapid::Vapid;
use super::PushHealth;

type HttpClient = Client<
    hyper_rustls::HttpsConnector<hyper_util::client::legacy::connect::HttpConnector>,
    Full<Bytes>,
>;

/// 投递参数。超时与退避都在这里，测试调到毫秒级。
#[derive(Debug, Clone)]
pub struct SenderConfig {
    /// 单次 HTTP 请求（含 TLS 握手）的上限。
    pub request_timeout: Duration,
    /// 退避下限；第 n 次连续失败等 `base * 2^(n-1)`，封顶 [`SenderConfig::backoff_max`]。
    pub backoff_base: Duration,
    pub backoff_max: Duration,
    /// VAPID JWT 的 `sub`（联系方式）：`https://…` 或 `mailto:…`。
    pub subject: String,
}

impl Default for SenderConfig {
    fn default() -> Self {
        SenderConfig {
            request_timeout: Duration::from_secs(10),
            backoff_base: Duration::from_secs(30),
            backoff_max: Duration::from_secs(30 * 60),
            subject: "mailto:agora@localhost".into(),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum DeliveryError {
    /// 连不上推送服务（DNS / TCP / TLS / 超时）：可达性问题，health 记 apple=false。
    #[error("推送端点不可达: {0}")]
    Unreachable(String),
    /// 拿到了应答但不是成功：服务本身可达（health 记 apple=true），这条推送被拒。
    #[error("推送端点拒绝：HTTP {status}")]
    Rejected { status: u16 },
    /// 订阅材料坏了（密钥不是 P-256 点 / 端点没 origin）：这条订阅永远发不出去，删掉。
    #[error("订阅材料非法: {0}")]
    Invalid(String),
}

/// 端点的形状校验：只收 Apple 的推送服务。浏览器给自己的订阅端点，恶意 / 被篡改的页面可以
/// 塞任意 https 地址，那是拿节点当 SSRF 跳板；Apple-only 也和"只做 Apple 端点"的裁决一致
/// （agora-thc.9 / w9ki）。
pub fn validate_endpoint(endpoint: &str) -> Result<(), String> {
    let rest = endpoint
        .strip_prefix("https://")
        .ok_or_else(|| "端点必须是 https://".to_owned())?;
    let host = rest
        .split(['/', '?', '#'])
        .next()
        .unwrap_or("")
        .rsplit_once(':')
        .map(|(h, _)| h)
        .unwrap_or_else(|| rest.split(['/', '?', '#']).next().unwrap_or(""));
    if host == "push.apple.com" || host.ends_with(".push.apple.com") {
        Ok(())
    } else {
        Err(format!(
            "只支持 Apple 推送端点（push.apple.com），收到 {host}"
        ))
    }
}

/// 端点的 origin（VAPID 的 `aud`）：`https://host[:port]`。
fn origin_of(endpoint: &str) -> Option<String> {
    let (scheme, rest) = endpoint.split_once("://")?;
    let authority = rest.split(['/', '?', '#']).next()?;
    (!authority.is_empty()).then(|| format!("{scheme}://{authority}"))
}

/// 端点的主机名，只给日志用（不打印路径里的端点凭据）。
pub fn host_of_endpoint(endpoint: &str) -> &str {
    let rest = endpoint
        .split_once("://")
        .map(|(_, r)| r)
        .unwrap_or(endpoint);
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    authority
        .rsplit_once(':')
        .map(|(h, _)| h)
        .unwrap_or(authority)
}

pub struct PushSender {
    store: PushStore,
    health: PushHealth,
    vapid: Vapid,
    client: HttpClient,
    cfg: SenderConfig,
    /// 每端点连续失败次数与下次允许时刻。
    backoff: Mutex<HashMap<String, (u32, Instant)>>,
}

impl PushSender {
    /// 生产构造：CA 用 webpki-roots（Apple 是真证书）。
    pub fn new(db: Arc<Db>, health: PushHealth, vapid: Vapid, cfg: SenderConfig) -> Self {
        let https = hyper_rustls::HttpsConnectorBuilder::new()
            .with_webpki_roots()
            .https_only()
            .enable_http1()
            .build();
        Self::with_client(PushStore::new(db), health, vapid, cfg, https)
    }

    /// 注入连接器（测试给假端点装私有 CA；连接器本身就是被验证的那一段）。
    pub fn with_client(
        store: PushStore,
        health: PushHealth,
        vapid: Vapid,
        cfg: SenderConfig,
        https: hyper_rustls::HttpsConnector<hyper_util::client::legacy::connect::HttpConnector>,
    ) -> Self {
        let client = Client::builder(TokioExecutor::new()).build(https);
        PushSender {
            store,
            health,
            vapid,
            client,
            cfg,
            backoff: Mutex::new(HashMap::new()),
        }
    }

    pub fn health(&self) -> &PushHealth {
        &self.health
    }

    pub fn store(&self) -> &PushStore {
        &self.store
    }

    /// 常驻任务：消费一个事件总线订阅直到发送端关闭。接收端由调用方先建好再 spawn——
    /// spawn 与 subscribe 之间有空窗，通知是增量，空窗里发出的事件没人补（测试踩过一次）。
    /// `Lagged` 只记一笔：通知是尽力而为，不因为掉队就阻塞事件流与 WS。
    pub async fn run(self: Arc<Self>, mut rx: tokio::sync::broadcast::Receiver<Event>) {
        loop {
            match rx.recv().await {
                Ok(Event::Notification {
                    id: Some(id),
                    title,
                    ..
                }) => self.notify(&title, &id).await,
                Ok(_) => {}
                Err(RecvError::Lagged(n)) => {
                    tracing::warn!(component = "push", lagged = n, "事件积压，跳过若干条推送");
                }
                Err(RecvError::Closed) => break,
            }
        }
    }

    /// 一条通知：给每个可用的订阅发。单条失败不影响其余订阅。
    pub async fn notify(&self, title: &str, session_id: &str) {
        let subs = match self.store.list_active() {
            Ok(subs) => subs,
            Err(err) => {
                tracing::warn!(component = "push", %err, "读订阅失败，本条不推");
                return;
            }
        };
        if subs.is_empty() {
            return;
        }
        let payload =
            serde_json::to_vec(&serde_json::json!({ "title": title, "session": session_id }))
                .expect("推送载荷可序列化");
        for sub in subs {
            if !self.backoff_allows(&sub.endpoint) {
                continue;
            }
            match self.deliver(&sub, &payload).await {
                Ok(()) => self.note_success(&sub.endpoint),
                Err(DeliveryError::Unreachable(reason)) => {
                    self.note_unreachable(&sub.endpoint, &reason);
                }
                Err(DeliveryError::Rejected { status }) => {
                    self.note_rejected(&sub, status);
                }
                Err(DeliveryError::Invalid(reason)) => {
                    tracing::warn!(
                        component = "push",
                        endpoint = %host_of_endpoint(&sub.endpoint),
                        reason,
                        "订阅材料非法，删除"
                    );
                    if let Err(err) = self.store.remove(&sub.endpoint) {
                        tracing::warn!(component = "push", %err, "删除非法订阅失败");
                    }
                }
            }
        }
    }

    /// 向一个订阅投递（含退避记账的底层版本不给外部用，测试直接打 [`Self::deliver`]）。
    pub async fn deliver(
        &self,
        sub: &PushSubscription,
        payload: &[u8],
    ) -> Result<(), DeliveryError> {
        let body = super::crypto::encrypt(&sub.p256dh, &sub.auth, payload)
            .map_err(|e| DeliveryError::Invalid(e.to_string()))?;
        let audience = origin_of(&sub.endpoint).ok_or_else(|| {
            DeliveryError::Invalid(format!(
                "端点没有 origin: {}",
                host_of_endpoint(&sub.endpoint)
            ))
        })?;
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let jwt = self.vapid.jwt(&audience, &self.cfg.subject, now);
        let req = Request::builder()
            .method("POST")
            .uri(&sub.endpoint)
            .header(header::CONTENT_ENCODING, "aes128gcm")
            .header(header::CONTENT_TYPE, "application/octet-stream")
            .header("ttl", "86400")
            .header(
                header::AUTHORIZATION,
                format!("vapid t={jwt}, k={}", self.vapid.public_key()),
            )
            .body(Full::new(Bytes::from(body)))
            .map_err(|e| DeliveryError::Unreachable(format!("请求构造失败: {e}")))?;
        let send = self.client.request(req);
        let resp = match tokio::time::timeout(self.cfg.request_timeout, send).await {
            Err(_) => return Err(DeliveryError::Unreachable("超时".into())),
            Ok(Err(err)) => return Err(DeliveryError::Unreachable(classify_transport(&err))),
            Ok(Ok(resp)) => resp,
        };
        let status = resp.status().as_u16();
        // 拿到应答就是可达：health 的可达性说的是"到得了 Apple"，不是这一条被收了。
        self.health.set_reachable();
        if (200..300).contains(&status) {
            Ok(())
        } else {
            Err(DeliveryError::Rejected { status })
        }
    }

    fn note_success(&self, endpoint: &str) {
        if let Err(err) = self.store.note_success(endpoint) {
            tracing::warn!(component = "push", %err, "记录投递成功失败");
        }
        if let Ok(mut g) = self.backoff.lock() {
            g.remove(endpoint);
        }
    }

    fn note_unreachable(&self, endpoint: &str, reason: &str) {
        self.health.set_unreachable(reason.to_owned());
        self.note_failure(endpoint, "unreachable");
        tracing::warn!(component = "push", endpoint = %host_of_endpoint(endpoint), reason, "推送端点不可达");
    }

    fn note_rejected(&self, sub: &PushSubscription, status: u16) {
        let kind = status.to_string();
        match status {
            // 端点已经作废（卸载 PWA / 清站点数据）：删订阅，别再发。
            404 | 410 => {
                if let Err(err) = self.store.remove(&sub.endpoint) {
                    tracing::warn!(component = "push", %err, "删除失效订阅失败");
                }
                tracing::info!(
                    component = "push",
                    endpoint = %host_of_endpoint(&sub.endpoint),
                    status,
                    "订阅已失效，删除"
                );
            }
            _ => {
                self.health.note_delivery_error(format!("HTTP {status}"));
                self.note_failure(&sub.endpoint, &kind);
                tracing::warn!(
                    component = "push",
                    endpoint = %host_of_endpoint(&sub.endpoint),
                    status,
                    "推送被拒"
                );
            }
        }
    }

    fn note_failure(&self, endpoint: &str, kind: &str) {
        if let Err(err) = self.store.note_failure(endpoint, kind) {
            tracing::warn!(component = "push", %err, "记录投递失败失败");
        }
        if let Ok(mut g) = self.backoff.lock() {
            let entry = g.entry(endpoint.to_owned()).or_insert((0, Instant::now()));
            entry.0 = entry.0.saturating_add(1);
            let delay = self
                .cfg
                .backoff_base
                .saturating_mul(1u32.checked_shl(entry.0.min(16) - 1).unwrap_or(u32::MAX))
                .min(self.cfg.backoff_max);
            entry.1 = Instant::now() + delay;
        }
    }

    fn backoff_allows(&self, endpoint: &str) -> bool {
        self.backoff
            .lock()
            .ok()
            .and_then(|g| g.get(endpoint).map(|(_, at)| *at <= Instant::now()))
            .unwrap_or(true)
    }
}

/// 传输错误按类型分类（MISSION §2.3 规则 10：不拿错误文本做程序判断，这里只是选一句人话）。
fn classify_transport(err: &hyper_util::client::legacy::Error) -> String {
    let text = err.to_string();
    if text.contains("dns") || text.contains("Name or service not known") {
        return "DNS 解析失败".into();
    }
    if text.contains("timed out") || text.contains("timeout") {
        return "连接超时".into();
    }
    if text.contains("certificate") || text.contains("TLS") || text.contains("tls") {
        return "TLS 校验失败".into();
    }
    if err.is_connect() {
        return "连接失败".into();
    }
    "传输失败".into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn endpoint_validation_only_accepts_apple() {
        assert!(validate_endpoint("https://web.push.apple.com/abc").is_ok());
        assert!(validate_endpoint("https://push.apple.com/abc").is_ok());
        assert!(validate_endpoint("http://web.push.apple.com/abc").is_err());
        assert!(validate_endpoint("https://evil.example/abc").is_err());
        assert!(validate_endpoint("https://web.push.apple.com.evil.example/").is_err());
        assert!(validate_endpoint("not a url").is_err());
    }

    #[test]
    fn backoff_grows_per_endpoint_and_resets_on_success() {
        let db = Arc::new(crate::session::Db::open_in_memory().unwrap());
        let sender = PushSender::new(
            db,
            PushHealth::new(),
            Vapid::generate(),
            SenderConfig {
                backoff_base: Duration::from_secs(1),
                backoff_max: Duration::from_secs(8),
                ..Default::default()
            },
        );
        assert!(sender.backoff_allows("https://web.push.apple.com/A"));
        for _ in 0..5 {
            sender.note_failure("https://web.push.apple.com/A", "timeout");
        }
        assert!(
            !sender.backoff_allows("https://web.push.apple.com/A"),
            "连续失败后该端点进冷却"
        );
        assert!(
            sender.backoff_allows("https://web.push.apple.com/B"),
            "退避按端点独立"
        );
        let (count, at) = sender.backoff.lock().unwrap()["https://web.push.apple.com/A"];
        assert_eq!(count, 5);
        assert!(
            at > Instant::now() + Duration::from_secs(5),
            "5 次失败后是 base*2^4，但不超过封顶 8 s：{at:?}"
        );
        sender.note_success("https://web.push.apple.com/A");
        assert!(sender.backoff_allows("https://web.push.apple.com/A"));
    }

    #[test]
    fn origin_extraction_keeps_the_port() {
        assert_eq!(
            origin_of("https://web.push.apple.com/QF/x").as_deref(),
            Some("https://web.push.apple.com")
        );
        assert_eq!(
            origin_of("https://127.0.0.1:8443/push").as_deref(),
            Some("https://127.0.0.1:8443")
        );
        assert_eq!(origin_of("nope"), None);
    }
}
