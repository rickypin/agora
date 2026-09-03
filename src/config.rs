//! `AGORA_HOME/config.yaml` 的加载与校验（docs/spec/config.md；MISSION §9）。
//!
//! 文件可以不存在：全部段都有默认值，缺文件等价于空文件。存在时**拒绝未知键**——
//! 默默忽略一个拼错的 `server.listen` 比启动失败危险得多。校验只做"配置字面上就错"的
//! 那类（明文监听器非 loopback、两个监听器同端口、时长语法）；需要跑起来才知道的
//! （端口被占、运行时版本）归各自的模块。
//!
//! 运行时段这里只认 `runtime.kind`，具体运行时的子段原样交给 `main.rs` 里选中的实现去
//! 解析——核心层看不见任何一个具体运行时的名字（ADR-001 D2，`tests/arch_boundary.rs`）。
//! 同理 `agents` 是任意名字到覆盖项的映射，核心层不知道具体 agent（ADR-002 D2）。

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Deserialize;

use crate::api::{plaintext_listen, ListenError};

pub const CONFIG_FILE: &str = "config.yaml";

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("读取 {path} 失败: {source}")]
    Read {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("解析 {path} 失败: {source}")]
    Parse {
        path: String,
        #[source]
        source: serde_yaml_ng::Error,
    },
    #[error("server.listen: {0}")]
    Listen(#[from] ListenError),
    #[error("server.tls_listen 无法解析: {0}")]
    TlsListenParse(String),
    #[error("server.tls_listen 与 server.listen 端口相同（{0}）：两个监听器必须不同端口")]
    SamePort(u16),
    #[error("{field}: 时长 {value:?} 不合法，形态如 5s / 2m / 24h / 30d")]
    Duration { field: &'static str, value: String },
    #[error("node.id 不能为空，且只能含字母、数字、`-`、`_`（得到 {0:?}）")]
    NodeId(String),
}

/// 文件形态（未校验）。字段名与 docs/spec/config.md 一一对应。
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Config {
    pub server: ServerSection,
    pub node: NodeSection,
    pub peers: Vec<PeerSection>,
    pub runtime: RuntimeSection,
    pub terminal: TerminalSection,
    pub status: StatusSection,
    pub hooks: HooksSection,
    pub notifications: NotificationsSection,
    pub tls: TlsSection,
    pub auth: AuthSection,
    pub project_roots: Vec<PathBuf>,
    pub worktree_root: String,
    /// agent 名 → 覆盖项；名字是自由字符串。
    pub agents: BTreeMap<String, AgentOverride>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct ServerSection {
    pub listen: String,
    pub tls_listen: Option<String>,
    pub public_url: Option<String>,
}

impl Default for ServerSection {
    fn default() -> Self {
        ServerSection {
            listen: "127.0.0.1:7680".into(),
            tls_listen: None,
            public_url: None,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct NodeSection {
    /// 全局会话 id `<node>:<id>` 的前缀。安装脚本落地前默认 `local`（2026-09-03）。
    pub id: String,
}

impl Default for NodeSection {
    fn default() -> Self {
        NodeSection { id: "local".into() }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PeerSection {
    pub name: String,
    pub url: String,
    pub token_file: PathBuf,
    pub cert_fingerprint: String,
}

/// `kind` 选实现；其余键按实现名分组原样保留，由 `main.rs` 交给对应实现解析。
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct RuntimeSection {
    pub kind: String,
    #[serde(flatten)]
    pub sections: BTreeMap<String, serde_yaml_ng::Value>,
}

impl Default for RuntimeSection {
    fn default() -> Self {
        RuntimeSection {
            // 与 docs/spec/config.md 的默认一致；名字本身不在这里出现（ADR-001 D2）。
            kind: DEFAULT_RUNTIME_KIND.into(),
            sections: BTreeMap::new(),
        }
    }
}

/// V1 唯一实现的名字，由 `main.rs` 注入，避免核心层写死。
static DEFAULT_RUNTIME_KIND: &str = "default";

impl RuntimeSection {
    /// 取某个实现的子段；没写就是空 map，让实现用自己的默认值。
    pub fn section(&self, kind: &str) -> serde_yaml_ng::Value {
        self.sections
            .get(kind)
            .cloned()
            .unwrap_or(serde_yaml_ng::Value::Mapping(Default::default()))
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct TerminalSection {
    pub scrollback: u32,
}

impl Default for TerminalSection {
    fn default() -> Self {
        TerminalSection { scrollback: 10_000 }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct StatusSection {
    pub idle_after: String,
    pub detector_interval: String,
}

impl Default for StatusSection {
    fn default() -> Self {
        StatusSection {
            idle_after: "60s".into(),
            detector_interval: "2s".into(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct HooksSection {
    pub silence_after: String,
    pub hold_timeout: String,
    pub hold_per_session: u32,
    pub hold_per_node: u32,
    pub inbox_retention: String,
}

impl Default for HooksSection {
    fn default() -> Self {
        HooksSection {
            silence_after: "10m".into(),
            hold_timeout: "55m".into(),
            hold_per_session: 8,
            hold_per_node: 256,
            inbox_retention: "24h".into(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct NotificationsSection {
    pub enabled: bool,
}

impl Default for NotificationsSection {
    fn default() -> Self {
        NotificationsSection { enabled: true }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct TlsSection {
    pub mode: String,
    pub external: TlsExternal,
}

impl Default for TlsSection {
    fn default() -> Self {
        TlsSection {
            mode: "self-signed".into(),
            external: TlsExternal::default(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct TlsExternal {
    pub cert_file: Option<PathBuf>,
    pub key_file: Option<PathBuf>,
    pub renew_command: Option<Vec<String>>,
    pub renew_before: String,
}

impl Default for TlsExternal {
    fn default() -> Self {
        TlsExternal {
            cert_file: None,
            key_file: None,
            renew_command: None,
            renew_before: "720h".into(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct AuthSection {
    pub pair_ttl: String,
    pub pair_pending_max: usize,
    pub session_idle: String,
    pub session_max: String,
}

impl Default for AuthSection {
    fn default() -> Self {
        AuthSection {
            pair_ttl: "5m".into(),
            pair_pending_max: 4,
            session_idle: "30d".into(),
            session_max: "365d".into(),
        }
    }
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(deny_unknown_fields, default)]
pub struct AgentOverride {
    /// 可移植的裸命令名，不写绝对路径（ADR-001 D7）。
    pub command: Option<String>,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            server: ServerSection::default(),
            node: NodeSection::default(),
            peers: Vec::new(),
            runtime: RuntimeSection::default(),
            terminal: TerminalSection::default(),
            status: StatusSection::default(),
            hooks: HooksSection::default(),
            notifications: NotificationsSection::default(),
            tls: TlsSection::default(),
            auth: AuthSection::default(),
            project_roots: Vec::new(),
            worktree_root: "../{repo}-wt".into(),
            agents: BTreeMap::new(),
        }
    }
}

/// 校验过的、daemon 直接拿来用的形态。只放已经有消费者的字段；其余留在 [`Config`] 里。
#[derive(Debug, Clone)]
pub struct Settings {
    pub listen: SocketAddr,
    pub tls_listen: Option<SocketAddr>,
    pub node_id: String,
    pub detector_interval: Duration,
    pub idle_after: Duration,
    pub auth: crate::auth::AuthConfig,
    pub raw: Config,
}

impl Config {
    /// 缺文件 = 默认配置；有文件就必须整份合法。
    pub fn load(home: &Path, default_runtime_kind: &str) -> Result<Settings, ConfigError> {
        let path = home.join(CONFIG_FILE);
        let cfg = match std::fs::read_to_string(&path) {
            Ok(text) => Self::parse(&text).map_err(|source| ConfigError::Parse {
                path: path.display().to_string(),
                source,
            })?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Config::default(),
            Err(source) => {
                return Err(ConfigError::Read {
                    path: path.display().to_string(),
                    source,
                })
            }
        };
        cfg.validate(default_runtime_kind)
    }

    pub fn parse(text: &str) -> Result<Config, serde_yaml_ng::Error> {
        // 空文件解析成 null 而不是空 map，serde 对 null 走不到 default。
        if text.trim().is_empty() {
            return Ok(Config::default());
        }
        serde_yaml_ng::from_str(text)
    }

    pub fn validate(mut self, default_runtime_kind: &str) -> Result<Settings, ConfigError> {
        if self.runtime.kind == DEFAULT_RUNTIME_KIND {
            self.runtime.kind = default_runtime_kind.to_owned();
        }
        let listen = plaintext_listen(&self.server.listen)?;
        let tls_listen = match &self.server.tls_listen {
            None => None,
            Some(s) => {
                let addr: SocketAddr = s
                    .parse()
                    .map_err(|_| ConfigError::TlsListenParse(s.clone()))?;
                if addr.port() == listen.port() {
                    return Err(ConfigError::SamePort(addr.port()));
                }
                Some(addr)
            }
        };
        let id = self.node.id.trim();
        if id.is_empty()
            || !id
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        {
            return Err(ConfigError::NodeId(self.node.id.clone()));
        }
        let auth = crate::auth::AuthConfig {
            pair_ttl: parse_duration("auth.pair_ttl", &self.auth.pair_ttl)?,
            pair_pending_max: self.auth.pair_pending_max,
            session_idle: parse_duration("auth.session_idle", &self.auth.session_idle)?,
            session_max: parse_duration("auth.session_max", &self.auth.session_max)?,
            cookie_secure: false,
        };
        let detector_interval =
            parse_duration("status.detector_interval", &self.status.detector_interval)?;
        let idle_after = parse_duration("status.idle_after", &self.status.idle_after)?;
        // 其余时长字段现在没有消费者，但语法先卡住，免得日后消费时才在运行中炸。
        for (field, value) in [
            ("hooks.silence_after", &self.hooks.silence_after),
            ("hooks.hold_timeout", &self.hooks.hold_timeout),
            ("hooks.inbox_retention", &self.hooks.inbox_retention),
            ("tls.external.renew_before", &self.tls.external.renew_before),
        ] {
            parse_duration(field, value)?;
        }
        Ok(Settings {
            listen,
            tls_listen,
            node_id: id.to_owned(),
            detector_interval,
            idle_after,
            auth,
            raw: self,
        })
    }
}

/// `5s` / `2m` / `24h` / `30d`；整数 + 单位，无空格。
pub fn parse_duration(field: &'static str, value: &str) -> Result<Duration, ConfigError> {
    let bad = || ConfigError::Duration {
        field,
        value: value.to_owned(),
    };
    let v = value.trim();
    let split = v.find(|c: char| !c.is_ascii_digit()).ok_or_else(bad)?;
    let (num, unit) = v.split_at(split);
    let n: u64 = num.parse().map_err(|_| bad())?;
    let mult = match unit {
        "s" => 1,
        "m" => 60,
        "h" => 3600,
        "d" => 86_400,
        _ => return Err(bad()),
    };
    Ok(Duration::from_secs(n * mult))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durations_parse_units_and_reject_garbage() {
        assert_eq!(parse_duration("x", "5s").unwrap(), Duration::from_secs(5));
        assert_eq!(
            parse_duration("x", "30d").unwrap(),
            Duration::from_secs(30 * 86_400)
        );
        assert!(parse_duration("x", "5").is_err());
        assert!(parse_duration("x", "5 s").is_err());
        assert!(parse_duration("x", "1w").is_err());
    }

    #[test]
    fn empty_and_missing_sections_fall_back_to_defaults() {
        let s = Config::parse("").unwrap().validate("rt").unwrap();
        assert_eq!(s.listen.to_string(), "127.0.0.1:7680");
        assert_eq!(s.node_id, "local");
        assert_eq!(s.raw.runtime.kind, "rt");
        let s = Config::parse("node:\n  id: mac\n")
            .unwrap()
            .validate("rt")
            .unwrap();
        assert_eq!(s.node_id, "mac");
        assert_eq!(s.auth.pair_pending_max, 4);
    }

    #[test]
    fn unknown_keys_are_rejected() {
        assert!(Config::parse("server:\n  lisen: 1\n").is_err());
        assert!(Config::parse("srever: {}\n").is_err());
    }

    #[test]
    fn runtime_subsection_is_kept_opaque_for_the_implementation() {
        let c = Config::parse("runtime:\n  kind: rt\n  rt:\n    socket: x\n").unwrap();
        let sec = c.runtime.section("rt");
        assert_eq!(sec["socket"].as_str(), Some("x"));
        assert!(c.runtime.section("other").as_mapping().unwrap().is_empty());
    }

    #[test]
    fn tls_listener_must_differ_in_port() {
        let c = Config::parse("server:\n  tls_listen: \"0.0.0.0:7680\"\n").unwrap();
        assert!(matches!(c.validate("rt"), Err(ConfigError::SamePort(7680))));
    }
}
