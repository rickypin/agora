//! `AGORA_HOME/config.yaml` 的加载与校验（docs/spec/config.md；agora-xqa.9）。

use agora::config::{Config, ConfigError};

fn load(text: Option<&str>) -> Result<agora::config::Settings, ConfigError> {
    let dir = tempfile::tempdir().unwrap();
    if let Some(t) = text {
        std::fs::write(dir.path().join("config.yaml"), t).unwrap();
    }
    Config::load(dir.path(), "rt")
}

#[test]
fn missing_file_means_defaults() {
    let s = load(None).unwrap();
    assert_eq!(s.listen.to_string(), "127.0.0.1:7680");
    assert_eq!(s.node_id, "local");
    assert_eq!(s.detector_interval.as_secs(), 2);
    assert_eq!(s.auth.session_idle.as_secs(), 30 * 86_400);
    assert_eq!(
        s.external_finished_ttl.as_secs(),
        86_400,
        "默认 24h（agora-j4w.3）"
    );
}

#[test]
fn external_finished_ttl_zero_turns_expiry_off_and_garbage_is_rejected() {
    // sessions.external_finished_ttl：裸 0 = 关闭；其余按时长语法卡在启动（agora-j4w.3）。
    let s = load(Some("sessions:\n  external_finished_ttl: \"0\"\n")).unwrap();
    assert!(s.external_finished_ttl.is_zero());
    let s = load(Some("sessions:\n  external_finished_ttl: \"1m\"\n")).unwrap();
    assert_eq!(s.external_finished_ttl.as_secs(), 60);
    assert!(matches!(
        load(Some("sessions:\n  external_finished_ttl: \"1 day\"\n")),
        Err(ConfigError::Duration {
            field: "sessions.external_finished_ttl",
            ..
        })
    ));
}

#[test]
fn spec_example_loads_and_runtime_subsection_stays_opaque() {
    // docs/spec/config.md 的整份样例（运行时段换成占位实现名）必须能加载。
    let text = r#"
server:
  listen: "127.0.0.1:7680"
  tls_listen: null
  public_url: null
node:
  id: "mac"
peers: []
runtime:
  kind: rt
  rt:
    socket: "agora"
    adopt_sockets: ["default"]
    prefix: "ag-"
    history_limit: 10000
    exec_timeout: "5s"
    min_version: "3.2"
terminal:
  scrollback: 10000
status:
  idle_after: "60s"
  detector_interval: "2s"
sessions:
  external_finished_ttl: "24h"
hooks:
  silence_after: "10m"
  unheard_after: "90s"
  external_silent_after: "2h"
  hold_timeout: "55m"
  hold_per_session: 8
  hold_per_node: 256
  inbox_retention: "24h"
notifications:
  enabled: true
tls:
  mode: "self-signed"
  external:
    cert_file: null
    key_file: null
    renew_command: null
    renew_before: "720h"
auth:
  pair_ttl: "5m"
  pair_pending_max: 4
  session_idle: "30d"
  session_max: "365d"
project_roots:
  - "/Users/ricky/code"
worktree_root: "../{repo}-wt"
agents:
  one: { command: "one" }
  two: { command: "two-cli" }
"#;
    let s = load(Some(text)).unwrap();
    assert_eq!(s.node_id, "mac");
    assert_eq!(s.raw.runtime.kind, "rt");
    assert_eq!(s.raw.runtime.section("rt")["prefix"].as_str(), Some("ag-"));
    assert_eq!(s.raw.agents["two"].command.as_deref(), Some("two-cli"));
    assert_eq!(s.auth.pair_ttl.as_secs(), 300);
}

#[test]
fn plaintext_listen_must_be_loopback() {
    // ADR-003 D5 在配置层就卡住，不等到 bind。
    let err = load(Some("server:\n  listen: \"0.0.0.0:7680\"\n")).unwrap_err();
    assert!(matches!(
        err,
        ConfigError::Listen(agora::api::ListenError::NotLoopback(_))
    ));
}

#[test]
fn typos_and_bad_durations_are_rejected() {
    assert!(matches!(
        load(Some("server:\n  lisen: \"127.0.0.1:1\"\n")),
        Err(ConfigError::Parse { .. })
    ));
    assert!(matches!(
        load(Some("auth:\n  pair_ttl: \"five minutes\"\n")),
        Err(ConfigError::Duration {
            field: "auth.pair_ttl",
            ..
        })
    ));
    assert!(matches!(
        load(Some("node:\n  id: \"has space\"\n")),
        Err(ConfigError::NodeId(_))
    ));
}
