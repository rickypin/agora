//! pi adapter 的 hook 链路（agora-c3i.1）：扩展投来的事件经投递箱 → receiver → external / headless
//! 行与状态机。载荷形态是 2026-10-07 本机 pi 1.0.4 实测的（`pi --extension` 真跑，事件序见
//! `src/adapter/pi.rs` 的模块头）；testdata/pi/1.0.4/hooks 的七个场景 + headless 已于 2026-10-07 真录
//! （方法见 testdata/README.md 的 pi 一节），所以这里按 fixture 形态合成载荷的写法只是另一种参照。
//!
//! 两条路都要钉住：`mode = tui` 的交互会话是 `external`（有进程号、可探活、turn_done 弹通知）；
//! `mode = print` 的一次性会话是 `headless`（收进折叠区、不通知、满 24 h 删）。判据只看载荷结构。

mod common;

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};

use agora::hook::{Delivery, Envelope, Inbox, Receiver};
use agora::session::Origin;
use agora::status::{Source, Status};

use common::{Fx, HOST};

fn delivery(agent_session: &str, payload: Value, pi_pid: Option<u32>) -> Delivery {
    static SEQUENCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64;
    let mut agent_env = BTreeMap::new();
    if let Some(pid) = pi_pid {
        agent_env.insert("PI_PID".to_owned(), pid.to_string());
    }
    Delivery {
        envelope: Envelope {
            host: "pi".into(),
            agora_session_id: None,
            agora_epoch: None,
            agent_session_id: agent_session.into(),
            agent_env,
            runtime_env: BTreeMap::new(),
            ppid: 1,
            received_at: String::new(),
            received_unix_ms: now_ms + SEQUENCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
        },
        payload,
    }
}

fn session_start(agent_session: &str, mode: &str) -> Value {
    json!({
        "hook_event_name": "session_start",
        "session_id": agent_session,
        "session_file": "/home/u/.pi/agent/sessions/x.jsonl",
        "cwd": "/work/agora",
        "mode": mode,
        "reason": "startup",
    })
}

fn with_hooks() -> (Fx, Arc<Receiver>, tempfile::TempDir) {
    let mut fx = Fx::new();
    let home = tempfile::tempdir().unwrap();
    let receiver = Arc::new(Receiver::new(home.path(), fx.sessions.clone()));
    receiver.attach_events(fx.state.events.clone(), fx.state.node.clone());
    fx.state.hooks = Some(receiver.clone());
    (fx, receiver, home)
}

fn ingest(receiver: &Receiver, home: &std::path::Path, d: &Delivery) -> Option<String> {
    let path = Inbox::new(home).write(d).unwrap();
    receiver.ingest(&path).unwrap().map(|r| r.session_key)
}

#[tokio::test]
async fn an_interactive_pi_session_registers_external_and_goes_turn_done() {
    let (fx, receiver, home) = with_hooks();
    // 存活线索：一个真进程当 pi 本体，`PI_PID` 报它。
    let mut child = std::process::Command::new("sleep")
        .arg("30")
        .spawn()
        .unwrap();
    let pid = child.id();

    let id = ingest(
        &receiver,
        home.path(),
        &delivery("pi-1", session_start("pi-1", "tui"), Some(pid)),
    )
    .unwrap();
    let rec = fx.sessions.record(&id).unwrap();
    assert_eq!(rec.origin, Origin::External, "tui 会话按 external 登记");
    assert_eq!(rec.agent_type, "pi");
    assert_eq!(rec.runtime_ref, None);
    assert_eq!(rec.agent_session_id.as_deref(), Some("pi-1"));
    assert_eq!(rec.working_directory.as_deref(), Some("/work/agora"));
    assert_eq!(rec.display_name, "agora");
    let view = fx.sessions.get(&id).unwrap();
    assert!(view.alive, "PI_PID 活着 → alive");
    assert_eq!(view.pid, Some(pid), "行上报的就是 PI_PID");

    // 一条 prompt 进状态机。
    ingest(
        &receiver,
        home.path(),
        &delivery(
            "pi-1",
            json!({ "hook_event_name": "before_agent_start", "session_id": "pi-1", "cwd": "/work/agora", "mode": "tui", "prompt": "把 config 迁到 yaml" }),
            Some(pid),
        ),
    );
    let view = fx.sessions.get(&id).unwrap();
    assert_eq!(view.assessment.status, Status::Running);
    assert_eq!(view.assessment.source, Source::Hook);
    assert_eq!(view.prompt.as_deref(), Some("把 config 迁到 yaml"));

    // 工具轮：`↳` 记正在用的工具。
    ingest(
        &receiver,
        home.path(),
        &delivery(
            "pi-1",
            json!({ "hook_event_name": "tool_execution_start", "session_id": "pi-1", "cwd": "/work/agora", "mode": "tui", "tool_name": "bash" }),
            Some(pid),
        ),
    );
    assert_eq!(
        fx.sessions.get(&id).unwrap().progress.as_deref(),
        Some("bash")
    );

    // agent_settled = 这一轮做完了：TURN_DONE，detail 是最后一条回复。
    ingest(
        &receiver,
        home.path(),
        &delivery(
            "pi-1",
            json!({ "hook_event_name": "agent_settled", "session_id": "pi-1", "cwd": "/work/agora", "mode": "tui", "last_assistant_message": "好了" }),
            Some(pid),
        ),
    );
    let view = fx.sessions.get(&id).unwrap();
    assert_eq!(view.assessment.status, Status::TurnDone);
    assert_eq!(view.detail.as_deref(), Some("好了"));

    // 退出：session_shutdown(reason=quit) → FINISHED（hook），写 ended_at。
    ingest(
        &receiver,
        home.path(),
        &delivery(
            "pi-1",
            json!({ "hook_event_name": "session_shutdown", "session_id": "pi-1", "cwd": "/work/agora", "mode": "tui", "reason": "quit" }),
            Some(pid),
        ),
    );
    let view = fx.sessions.get(&id).unwrap();
    assert_eq!(view.assessment.status, Status::Finished);
    assert!(view.record.ended_at.is_some());
    child.kill().unwrap();
    child.wait().unwrap();
}

#[tokio::test]
async fn a_registration_that_says_idle_lands_on_idle_not_starting() {
    // /reload 之后只来了 session_start：扩展在登记里报 `ctx.isIdle()`（真值表 x14，agora-wmrq）。
    // 没有这一条，行会停在 STARTING——"刚起"对一个跑了 N 小时的空闲会话是假话（手机按在跑分组）。
    let (fx, receiver, home) = with_hooks();
    let mut p = session_start("pi-6", "tui");
    p["reason"] = json!("reload");
    p["idle"] = json!(true);
    let id = ingest(
        &receiver,
        home.path(),
        &delivery("pi-6", p, Some(std::process::id())),
    )
    .unwrap();
    let view = fx.sessions.get(&id).unwrap();
    assert_eq!(view.assessment.status, Status::Idle);
    assert_eq!(view.assessment.source, Source::Hook);
    assert_eq!(view.prompt, None, "只登记、没讲话：没有 ❯");
    assert_eq!(view.detail, None, "也没有 ↳");
    assert!(view.record.ended_at.is_none());
}

#[tokio::test]
async fn a_print_mode_pi_session_registers_headless_and_stays_out_of_the_inbox() {
    let (fx, receiver, home) = with_hooks();
    // 注册竞速：真 daemon 上每条事件的 hook 各自 spawn，`before_agent_start` 可能先到
    // （2026-10-07 实测：pi -p 因此落成过 external）。判据是载荷里的 mode，不绑事件名。
    let id = ingest(
        &receiver,
        home.path(),
        &delivery(
            "pi-2",
            json!({ "hook_event_name": "before_agent_start", "session_id": "pi-2", "cwd": "/work/agora", "mode": "print", "prompt": "hi" }),
            None,
        ),
    )
    .unwrap();
    let rec = fx.sessions.record(&id).unwrap();
    assert_eq!(
        rec.origin,
        Origin::Headless,
        "pi -p 是无头一次性：收进折叠区、不通知、满 24 h 删"
    );
    assert_eq!(rec.agent_type, "pi");
    // `rpc` 是长期被外部驱动的会话，照常 external。
    let id3 = ingest(
        &receiver,
        home.path(),
        &delivery("pi-3", session_start("pi-3", "rpc"), None),
    )
    .unwrap();
    assert_eq!(fx.sessions.record(&id3).unwrap().origin, Origin::External);
}

#[tokio::test]
async fn session_switches_and_reloads_do_not_end_the_row() {
    // pi 的 session_shutdown 在 /reload、切会话（new / resume / fork）时也发：那些之后同一个 pi
    // 进程还在跑，结束是 supersede / 进程探活的事——把它当 SessionEnd 会让热重载顺手把行做掉。
    let (fx, receiver, home) = with_hooks();
    let mut child = std::process::Command::new("sleep")
        .arg("30")
        .spawn()
        .unwrap();
    let pid = child.id();
    let id = ingest(
        &receiver,
        home.path(),
        &delivery("pi-4", session_start("pi-4", "tui"), Some(pid)),
    )
    .unwrap();
    ingest(
        &receiver,
        home.path(),
        &delivery(
            "pi-4",
            json!({ "hook_event_name": "before_agent_start", "session_id": "pi-4", "cwd": "/work/agora", "mode": "tui", "prompt": "hi" }),
            Some(pid),
        ),
    );
    for reason in ["reload", "new", "resume", "fork"] {
        ingest(
            &receiver,
            home.path(),
            &delivery(
                "pi-4",
                json!({ "hook_event_name": "session_shutdown", "session_id": "pi-4", "cwd": "/work/agora", "mode": "tui", "reason": reason }),
                Some(pid),
            ),
        );
        let view = fx.sessions.get(&id).unwrap();
        assert_eq!(
            view.assessment.status,
            Status::Running,
            "{reason} 不是这一行的结束"
        );
        assert!(view.record.ended_at.is_none(), "{reason}");
    }
    child.kill().unwrap();
    child.wait().unwrap();
}

/// 线上形态：`GET /api/sessions` 的 `agent_type` 与 origin 封套（desktop 与手机共用这份导出）。
#[tokio::test]
async fn the_wire_row_names_pi_and_its_origin() {
    let (fx, receiver, home) = with_hooks();
    let _ = ingest(
        &receiver,
        home.path(),
        &delivery("pi-5", session_start("pi-5", "tui"), None),
    )
    .unwrap();
    let rows = fx.sessions.list().unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].record.agent_type, "pi");
    assert_eq!(rows[0].record.origin, Origin::External);
    // HOST 只让 common 的 fake 网关常量保持被引用（与其它 hook 测试同一习惯）。
    assert_eq!(HOST, "127.0.0.1:7680");
    let _ = Duration::from_secs(1);
}

/// 喂给 node 的驱动：加载渲染后的真扩展，冒充 pi 的 `on` / `sendUserMessage`，让它对着一个真
/// 队列目录跑一遍 `takeOne`。扩展是 TypeScript，只有 node（type stripping，CI 是 24）能直接执行
/// ——这个守卫不做"等价逻辑"的复刻，跑的就是生产代码本身。
const HARNESS: &str = r##"
import { pathToFileURL } from "node:url";
import * as fs from "node:fs";
import * as path from "node:path";

const [ext, home, out] = process.argv.slice(2);
const mod = await import(pathToFileURL(ext).href);
const handlers = new Map();
const captured = [];
mod.default({
  on(event, fn) { handlers.set(event, fn); },
  sendUserMessage(text, opts) { captured.push({ text, opts }); },
});

const session = "guard-session-id";
const dir = path.join(home, "input", Buffer.from(session, "utf8").toString("hex"));
fs.mkdirSync(dir, { recursive: true });
// 队列文件的原文形态：daemon 已经 trim 过尾换行（src/api/sessions.rs 的 queued_data），
// 扩展必须逐字节透传，一个字符也不许改写。
fs.writeFileSync(path.join(dir, "0001-1-0.json"), "/quit");
fs.writeFileSync(path.join(dir, "0002-1-1.json"), "/tmp/foo 看一下");

// session_start 里自己就会 scan() 一次，取件是同步的，不用等 1 s 轮询。
handlers.get("session_start")({ reason: "startup" }, {
  cwd: "/tmp",
  mode: "tui",
  sessionManager: { getSessionId: () => session, getCwd: () => "/tmp" },
});
fs.writeFileSync(out, JSON.stringify({ captured, files: fs.readdirSync(dir).sort() }));
process.exit(0);
"##;

/// 注入文本以 `/` 开头时**原样**交给宿主（agora-mukn；ADR-002 D11「边界」）。
///
/// 结论来自实测（2026-10-08，pi 1.0.4，隔离 daemon + 真 TUI，经 `POST /input` 注入）：pi 的
/// `sendUserMessage` 不执行内置斜杠命令，`/quit` 不会杀会话、`/reload` / `/new` 也不生效；
/// 正例 `/tmp/foo 看一下` 照常当消息送达。这条守卫钉的是扩展这一段不许自作主张——既不许把
/// `/quit` 拦下 / 加确认 / 转义，也不许拦 `/` 开头的合法消息；将来谁在这里加过滤，先红的是它。
#[test]
fn slash_prefixed_queue_text_reaches_the_host_verbatim() {
    use agora::adapter::{pi::PI, AgentHooks as _};

    // 扩展是 TypeScript，只有 node 能执行它（type stripping）。没有 node 就跳过——与真实
    // agent 冒烟遇到 `VersionProbe::Missing` 是同一个口径（CI 的 web 步骤本来就装 node 24）。
    match std::process::Command::new("node").arg("--version").output() {
        Ok(o) if o.status.success() => {}
        _ => {
            eprintln!("node 不在 PATH，跳过斜杠透传守卫（CI 与 web 门禁本来就装 node 24）");
            return;
        }
    }

    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("home");
    std::fs::create_dir_all(&home).unwrap();
    let ext = PI.file_install(&home).expect("pi 走文件式安装").content;
    let ts = dir.path().join("agora.ts");
    std::fs::write(&ts, ext).unwrap();
    let harness = dir.path().join("harness.mjs");
    std::fs::write(&harness, HARNESS).unwrap();
    let out = dir.path().join("out.json");

    let status = std::process::Command::new("node")
        .arg(&harness)
        .arg(&ts)
        .arg(&home)
        .arg(&out)
        .status()
        .expect("node 起不来");
    assert!(status.success(), "harness 退出非零");

    let got: Value = serde_json::from_slice(&std::fs::read(&out).unwrap()).unwrap();
    let captured = got["captured"].as_array().unwrap();
    let texts: Vec<&str> = captured
        .iter()
        .map(|c| c["text"].as_str().unwrap())
        .collect();
    assert_eq!(
        texts,
        ["/quit", "/tmp/foo 看一下"],
        "扩展必须把原文逐字节交给 pi.sendUserMessage（/quit 不拦、/ 开头的正例照常）"
    );
    for c in captured {
        assert_eq!(c["opts"]["deliverAs"], "followUp", "投递方式不许变");
    }
    let files: Vec<&str> = got["files"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f.as_str().unwrap())
        .collect();
    assert_eq!(
        files,
        ["0001-1-0.done", "0002-1-1.done"],
        "两件都被认领并 ack"
    );
}
