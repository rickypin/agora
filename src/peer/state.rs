//! 每个 peer 的连接状态模型（MISSION §3.5 / §10.3；不变量 8）。
//!
//! 这里只有类型与状态转移，没有 I/O：peer 客户端（agora-7ku.5）在每次连接成败时调转移，
//! `/api/health` 的 peers 段原样序列化它，Header 据此渲染"在线 / 异常原因 / 上次见到"。
//! 错误**按类型不按文本**（MISSION §2.3 规则 10）——前端与测试只看 `last_error` 的枚举值，
//! 谁也不解析消息字符串；人看的细节走日志。
//!
//! 不变量 8 的两半：peer 离线后 `last_seen` 保留（stale 不是消失），`retrying` 永远会再变回
//! true（退避可封顶、不可终止，参数见 `super::backoff`）。唯一不 `retrying` 的离线是
//! [`PeerError::Misconfigured`]（ADR-003 D3）：配置本身就不对，网络重试改不了文件，客户端改为
//! 定时重读配置（`BackoffPolicy::misconfigured_recheck`），修好即恢复。

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize, Serializer};

/// 最后一次连接失败的**类型**。前四类是 M2a 剧本点名要在 Header 上区分的（agora-7ku 设计字段
/// 第 2、3 步）：指纹不匹配与版本不兼容都必须显示成自己的原因而不是"离线"；第五类
/// `Misconfigured` 是 ADR-003 D3 点名的「配置错误」（agora-41e）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PeerError {
    /// 对方 `api_version` 与本机不兼容（MISSION §7.3；A33）：不读它的数据，显示"版本不兼容"。
    IncompatibleVersion,
    /// 对方证书 SPKI 指纹与 `peers[].cert_fingerprint` 不符（ADR-003 D4）：显示"指纹不匹配"。
    FingerprintMismatch,
    /// 对方回 401：机器 token 未签发、已吊销或用错（ADR-003 D3）。
    Unauthorized,
    /// 连不上：拒绝、每次尝试 5 s 超时、DNS 失败……全归这一类，细节在日志。
    Unreachable,
    /// 本节点这一行 `peers[]` 字面上就用不了（ADR-003 D3；agora-41e）：`token_file` 权限过宽 /
    /// 不属于自己 / 读不到 / 内容不是 token、`url` 不是 https、指纹不合法。发生在拨号之前，网上
    /// 没有任何字节；**不进退避**（`retrying: false`）——重试改不了文件，daemon 每
    /// `misconfigured_recheck`（生产 10 s）重读一次配置文件，改好即恢复，不需要重启。
    Misconfigured,
}

/// 一个 peer 在本节点眼里的状态。JSON 形态见 docs/spec/api.md Health 节。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PeerState {
    /// 当前有活着的连接。
    pub online: bool,
    /// 上次成功交互的时刻，unix 秒，**本节点时钟**打的（ADR-004：不信 peer 报的时间）；
    /// 从没连上过为 None。序列化成 `YYYY-MM-DDTHH:MM:SSZ`，与会话时间戳同形。
    #[serde(serialize_with = "ser_utc")]
    pub last_seen: Option<i64>,
    /// 下一次**网络**重试已排定（退避中）。离线的 peer 除了"客户端还没起来"这一刻与
    /// `Misconfigured`（重试改不了配置，客户端只是定时重读文件）之外，都应该是 true。
    pub retrying: bool,
    /// 最后一次失败的类型；在线时 None。
    pub last_error: Option<PeerError>,
}

impl PeerState {
    /// 配置里有、客户端还没试过：离线、没见过、没错误、也还没在重试。
    pub const fn new() -> Self {
        PeerState {
            online: false,
            last_seen: None,
            retrying: false,
            last_error: None,
        }
    }

    /// 一次成功交互（连上、收到事件、请求成功）：在线、刷新 last_seen、清错误、停止退避。
    /// 幂等——在线期间每次成功都调，last_seen 才跟得上。
    pub fn seen(&mut self, now_secs: i64) {
        self.online = true;
        self.last_seen = Some(now_secs);
        self.retrying = false;
        self.last_error = None;
    }

    /// 一次失败：离线、记类型、进入退避重试。`last_seen` **不动**——那是 stale 行的"上次见到"。
    /// `Misconfigured` 例外：`retrying = false`——没有网络重试可排，客户端在定时重读配置文件；
    /// Header 的 title 因此不带"重试中"，人看到的是"去改文件"而不是"等一等"。
    pub fn failed(&mut self, err: PeerError) {
        self.online = false;
        self.retrying = err != PeerError::Misconfigured;
        self.last_error = Some(err);
    }

    /// 离线但见过：视图里保留最后一眼并标"上次见到"（MISSION §3.5）。
    pub fn is_stale(&self) -> bool {
        !self.online && self.last_seen.is_some()
    }
}

impl Default for PeerState {
    fn default() -> Self {
        Self::new()
    }
}

/// 传输层的失败 → 状态模型的类型（agora-7ku.5 接线；MISSION §2.3 规则 10：按类型不按文本）。
/// `Timeout` / `Unreachable` / `Protocol` / `Tls` 都是"连不上"；`FingerprintMismatch` 保持独立
/// （ADR-003 D4，绝不并进离线）；`WsRejected(401)` 是未授权、其余状态码当不可达；`Config(_)`
/// （url / 指纹 / token_file 字面上就用不了）是「配置错误」（ADR-003 D3；agora-41e）。日志不在
/// 这里打——转入 / 仍在配置错误的 warn / debug 分档由 `PeerClient` 看着前一个状态决定。
impl From<crate::peer::transport::TransportError> for PeerError {
    fn from(e: crate::peer::transport::TransportError) -> Self {
        use crate::peer::transport::TransportError as T;
        match e {
            T::FingerprintMismatch { .. } => PeerError::FingerprintMismatch,
            T::WsRejected(status) if status == axum::http::StatusCode::UNAUTHORIZED => {
                PeerError::Unauthorized
            }
            T::Config(_) => PeerError::Misconfigured,
            T::Timeout(_) | T::Unreachable(_) | T::WsRejected(_) | T::Protocol(_) | T::Tls(_) => {
                PeerError::Unreachable
            }
        }
    }
}

/// 全部 peer 的状态表：节点名 → 状态。`AppState` 持有一份，peer 客户端写、`/api/health` 读。
/// 配置里的每个 peer 启动时先 `register`，还没连上的 peer 也出现在 health 里（离线、没见过），
/// 而不是等第一次连上才"冒出来"——Header 从第一秒起就能告诉人有几台节点。
#[derive(Debug, Clone, Default)]
pub struct PeerStates(Arc<Mutex<BTreeMap<String, PeerState>>>);

impl PeerStates {
    pub fn new() -> Self {
        Self::default()
    }

    /// 登记一个配置里的 peer；已有的不动（幂等）。
    pub fn register(&self, name: &str) {
        lock(&self.0).entry(name.to_owned()).or_default();
    }

    /// 见 `PeerState::seen`；没登记过的顺手登记。
    pub fn seen(&self, name: &str, now_secs: i64) {
        lock(&self.0)
            .entry(name.to_owned())
            .or_default()
            .seen(now_secs);
    }

    /// 见 `PeerState::failed`；没登记过的顺手登记。
    pub fn failed(&self, name: &str, err: PeerError) {
        lock(&self.0)
            .entry(name.to_owned())
            .or_default()
            .failed(err);
    }

    pub fn get(&self, name: &str) -> Option<PeerState> {
        lock(&self.0).get(name).cloned()
    }

    /// 拷一份给 health 序列化（BTreeMap：JSON 键序稳定，测试与人眼都好比）。
    pub fn snapshot(&self) -> BTreeMap<String, PeerState> {
        lock(&self.0).clone()
    }
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

fn ser_utc<S: Serializer>(secs: &Option<i64>, s: S) -> Result<S::Ok, S::Error> {
    match secs {
        Some(v) => s.serialize_str(&crate::clock::format_utc_secs(*v)),
        None => s.serialize_none(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const T0: i64 = 1_788_393_600; // 2026-09-03T00:00:00Z

    #[test]
    fn fresh_peer_is_offline_never_seen_and_not_yet_retrying() {
        let p = PeerState::new();
        assert!(!p.online && p.last_seen.is_none() && !p.retrying && p.last_error.is_none());
        assert!(!p.is_stale(), "没见过的 peer 不算 stale，只是还没连上");
        assert_eq!(
            serde_json::to_value(&p).unwrap(),
            json!({ "online": false, "last_seen": null, "retrying": false, "last_error": null })
        );
    }

    #[test]
    fn failure_after_contact_keeps_last_seen_and_enters_retry() {
        // 不变量 8：离线保留最后视图与时间，绝不静默消失。
        let mut p = PeerState::new();
        p.seen(T0);
        assert!(p.online && !p.retrying && p.last_error.is_none());
        p.failed(PeerError::Unreachable);
        assert!(!p.online && p.retrying);
        assert_eq!(p.last_seen, Some(T0));
        assert_eq!(p.last_error, Some(PeerError::Unreachable));
        assert!(p.is_stale());
        assert_eq!(
            serde_json::to_value(&p).unwrap(),
            json!({
                "online": false,
                "last_seen": "2026-09-03T00:00:00Z",
                "retrying": true,
                "last_error": "unreachable",
            })
        );
    }

    #[test]
    fn misconfigured_is_offline_without_retry_and_keeps_last_seen() {
        // ADR-003 D3 / agora-41e：配置错误不进退避——retrying 是 false（重试改不了文件），
        // 但 last_seen 照样保留（stale 行的"上次见到"），修好后 seen 走正常恢复。
        let mut p = PeerState::new();
        p.seen(T0);
        p.failed(PeerError::Misconfigured);
        assert!(!p.online && !p.retrying, "{p:?}");
        assert_eq!(p.last_error, Some(PeerError::Misconfigured));
        assert_eq!(p.last_seen, Some(T0));
        assert!(p.is_stale());
        assert_eq!(
            serde_json::to_value(&p).unwrap(),
            json!({
                "online": false,
                "last_seen": "2026-09-03T00:00:00Z",
                "retrying": false,
                "last_error": "misconfigured",
            })
        );
        // 其它四类照旧 retrying。
        for e in [
            PeerError::IncompatibleVersion,
            PeerError::FingerprintMismatch,
            PeerError::Unauthorized,
            PeerError::Unreachable,
        ] {
            let mut q = PeerState::new();
            q.failed(e);
            assert!(q.retrying, "{e:?} 应进退避");
        }
        p.seen(T0 + 10);
        assert!(p.online && !p.retrying && p.last_error.is_none());
    }

    #[test]
    fn recovery_clears_error_and_stops_retrying() {
        let mut p = PeerState::new();
        p.failed(PeerError::FingerprintMismatch);
        p.seen(T0 + 5);
        assert!(p.online && !p.retrying && p.last_error.is_none());
        assert_eq!(p.last_seen, Some(T0 + 5));
    }

    #[test]
    fn registry_lists_configured_peers_before_first_contact_and_keeps_key_order() {
        let table = PeerStates::new();
        table.register("zuan");
        table.register("mac");
        table.seen("zuan", T0);
        table.register("zuan"); // 幂等：不把已连上的打回初始
        table.failed("mac", PeerError::Unauthorized);
        let snap = table.snapshot();
        assert_eq!(snap.keys().collect::<Vec<_>>(), ["mac", "zuan"]);
        assert!(snap["zuan"].online && snap["zuan"].last_seen == Some(T0));
        assert_eq!(snap["mac"].last_error, Some(PeerError::Unauthorized));
        assert!(table.get("nope").is_none());
    }

    #[test]
    fn transport_errors_map_to_peer_error_by_type() {
        use crate::peer::transport::{PeerConfigError, TransportError as T};
        use axum::http::StatusCode;
        use std::time::Duration;
        let io = || std::io::Error::other("x");
        assert_eq!(
            PeerError::from(T::Timeout(Duration::from_secs(5))),
            PeerError::Unreachable
        );
        assert_eq!(
            PeerError::from(T::Unreachable(io())),
            PeerError::Unreachable
        );
        assert_eq!(
            PeerError::from(T::FingerprintMismatch {
                expected: "a".into(),
                actual: "b".into()
            }),
            PeerError::FingerprintMismatch
        );
        assert_eq!(
            PeerError::from(T::WsRejected(StatusCode::UNAUTHORIZED)),
            PeerError::Unauthorized
        );
        assert_eq!(
            PeerError::from(T::WsRejected(StatusCode::BAD_GATEWAY)),
            PeerError::Unreachable
        );
        assert_eq!(
            PeerError::from(T::Protocol("p".into())),
            PeerError::Unreachable
        );
        // ADR-003 D3：配置错误是自己的类型，绝不并进不可达（agora-41e）。
        assert_eq!(
            PeerError::from(T::Config(PeerConfigError::Url("http://x".into()))),
            PeerError::Misconfigured
        );
        assert_eq!(
            PeerError::from(T::Config(PeerConfigError::TokenFile(
                crate::auth::peer_token::TokenFileError::TooOpen {
                    path: "/x".into(),
                    mode: 0o644
                }
            ))),
            PeerError::Misconfigured
        );
    }

    #[test]
    fn errors_are_typed_and_serialize_snake_case() {
        // 规则 10：前端只认这五个值，不解析文本。改名就是破坏 API；加值随 api_version minor。
        for (e, s) in [
            (PeerError::IncompatibleVersion, "incompatible_version"),
            (PeerError::FingerprintMismatch, "fingerprint_mismatch"),
            (PeerError::Unauthorized, "unauthorized"),
            (PeerError::Unreachable, "unreachable"),
            (PeerError::Misconfigured, "misconfigured"),
        ] {
            assert_eq!(serde_json::to_value(e).unwrap(), json!(s));
            assert_eq!(serde_json::from_value::<PeerError>(json!(s)).unwrap(), e);
        }
    }
}
