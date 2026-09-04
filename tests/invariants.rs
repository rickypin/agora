//! 真实 tmux + fake-agent 的端到端不变量（MISSION §2.2；A8 A9 A10 A11 A12 A20）。
//!
//! 不变量 1–5、7 每条一个独立测试，每条注明它靠哪个守卫成立、把那个守卫关掉会怎样
//! （agora-xqa.13）。测试之间不共享节点：一条红了就是那条不变量塌了，不是别人的连坐。
//!
//! 都跑在真 tmux 上、各自的 socket 与 AGORA_HOME 里，agent 是 `agora fake-agent`。

mod common;

use std::time::{Duration, Instant};

use agora::runtime::Exit;
use agora::status::Status;
use axum::http::header;
use futures_util::{SinkExt, StreamExt};
use serde_json::Value;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::Message;

use agora::runtime::{LaunchSpec, Runtime, Size};
use common::node::TmuxNode;

// 轮询 tmux 不能太密：每次 inspect/capture 都要新起一个 tmux client 进程，密集轮询会和
// server 收集子进程退出状态抢 SIGCHLD，pane 会死得"没有退出码"（agora-tc4；实测 2026-09-03
// 3.2a 上密集轮询丢 5/6、200 ms 轮询丢 1/6）。生产的 status.detector_interval 是 2 s。
const POLL: Duration = Duration::from_millis(200);

type Ws =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

async fn terminal(node: &TmuxNode, gid: &str) -> Ws {
    let addr = node.addr.clone().expect("节点已 serve");
    let mut req = format!("ws://{addr}/api/sessions/{gid}/terminal")
        .into_client_request()
        .unwrap();
    req.headers_mut()
        .insert(header::COOKIE, node.cookie().parse().unwrap());
    req.headers_mut()
        .insert(header::ORIGIN, format!("http://{addr}").parse().unwrap());
    tokio_tungstenite::connect_async(req).await.unwrap().0
}

/// 攒输出直到出现 `needle`。
async fn output_until(ws: &mut Ws, needle: &str) -> String {
    let mut got = String::new();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while !got.contains(needle) {
        let left = deadline.saturating_duration_since(tokio::time::Instant::now());
        let msg = tokio::time::timeout(left, ws.next())
            .await
            .unwrap_or_else(|_| panic!("等 {needle:?} 超时，已收到: {got:?}"))
            .expect("流未结束")
            .unwrap();
        if let Message::Text(t) = msg {
            let v: Value = serde_json::from_str(&t).unwrap();
            if v["type"] == "output" {
                got.push_str(v["data"].as_str().unwrap());
            }
        }
    }
    got
}

async fn send_input(ws: &mut Ws, data: &str) {
    let frame = serde_json::json!({ "type": "input", "data": data }).to_string();
    ws.send(Message::Text(frame.into())).await.unwrap();
}

/// A12 的终局断言：进程死了，退出码就必须读得到。
///
/// 曾经这里是"已死且结论自洽"的软断言：tmux 在**经 PTY 交互输入之后**退出时有时根本不收集
/// 退出状态，会话永远停在 UNKNOWN（agora-tc4）。2026-09-03 查明那不是"迟到"而是 tmux < 3.6
/// 链 libutempter 时 SIGCHLD 被吞（上游 issue 4559），runtime 现在会补发 SIGCHLD 把它捞回来
/// （见 `runtime::tmux::SIGCHLD_EATEN_BELOW`）。所以这条断言重新收紧：捞不回来就是那条补救坏了。
fn assert_dead_with_code(node: &TmuxNode, id: &str, expected: i32) -> agora::session::SessionView {
    let mut v = node.wait(id, |v| !v.alive);
    // 退出信息可能比"进程已死"晚一两个 tick；被吞掉的那次还要等一轮补发 SIGCHLD。
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while v.exit.is_none() && std::time::Instant::now() < deadline {
        std::thread::sleep(POLL);
        v = node.sessions.get(id).unwrap();
    }
    assert_eq!(
        v.exit,
        Some(Exit::Code(expected)),
        "进程死了退出码就必须读得到（A12）"
    );
    assert_eq!(v.assessment.status, Status::Failed);
    v
}

#[tokio::test(flavor = "multi_thread")]
async fn two_nodes_in_one_process_are_isolated() {
    let mut a = TmuxNode::new();
    let mut b = TmuxNode::new();
    a.serve().await;
    b.serve().await;
    let sa = a.create_fake("a", "print A-READY; sleep 60000");
    let sb = b.create_fake("b", "print B-READY; read; exit 3");
    a.wait(&sa.record.id, |v| v.alive);
    b.wait(&sb.record.id, |v| v.alive);

    // 各看各的。
    let ids_a: Vec<String> = a
        .sessions
        .list()
        .unwrap()
        .into_iter()
        .map(|v| v.record.id)
        .collect();
    let ids_b: Vec<String> = b
        .sessions
        .list()
        .unwrap()
        .into_iter()
        .map(|v| v.record.id)
        .collect();
    assert_eq!(ids_a, vec![sa.record.id.clone()]);
    assert_eq!(ids_b, vec![sb.record.id.clone()]);
    assert!(
        a.sessions.unregistered().unwrap().is_empty(),
        "A 不该看到 B 的运行时会话"
    );

    // 杀 A 的会话，B 的纹丝不动；B 的退出码经它自己的节点可读。
    a.sessions.kill(&sa.record.id).unwrap();
    a.wait(&sa.record.id, |v| !v.alive);
    assert!(b.pane_alive(&sb), "B 的 agent 不受 A 的 kill 影响");
    let mut ws = terminal(&b, &b.gid(&sb.record.id)).await;
    output_until(&mut ws, "B-READY").await;
    send_input(&mut ws, "bye\r").await;
    output_until(&mut ws, "read:bye").await;
    assert_dead_with_code(&b, &sb.record.id, 3);
}

/// 直接问 tmux 要 attach 客户端的 pid（不经 agora，作为独立证人）。
fn client_pids(node: &TmuxNode) -> Vec<u32> {
    let out = std::process::Command::new("tmux")
        .args(["-L", &node.socket, "list-clients", "-F", "#{client_pid}"])
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| l.trim().parse().ok())
        .collect()
}

/// 不变量 1：浏览器崩了 ≠ agent 崩了。
///
/// 最硬的一种崩：attach 客户端进程被 SIGKILL，连 detach 都没机会跑。
/// 守卫：gateway 只管自己那条 attach，从不碰运行时会话本身（`AttachedPty::drop` 杀的是
/// attach 进程，`bridge` 结束时不 kill session）。把"detach 后顺手 kill 会话"加回去，这条红。
#[tokio::test(flavor = "multi_thread")]
async fn invariant_1_browser_crash_does_not_kill_the_agent() {
    let mut node = TmuxNode::new();
    node.serve().await;
    let s = node.create_fake(
        "agent",
        "ignore-hup; print READY; read; print AFTER; exit 5",
    );
    let id = s.record.id.clone();
    let gid = node.gid(&id);
    node.wait(&id, |v| v.alive);

    let mut ws = terminal(&node, &gid).await;
    output_until(&mut ws, "READY").await;
    let pids = client_pids(&node);
    assert_eq!(pids.len(), 1, "应当只有 gateway 这一条 attach: {pids:?}");

    // 浏览器那一侧整个消失：attach 客户端被打死，WS 也不再有人读。
    unsafe { libc::kill(pids[0] as i32, libc::SIGKILL) };
    drop(ws);
    tokio::time::sleep(Duration::from_millis(500)).await;

    assert!(node.pane_alive(&s), "attach 被打死不能带走 agent");
    assert!(node.sessions.get(&id).unwrap().alive);

    // 而且还能接着用：重连、输入照样送达。
    let mut ws = terminal(&node, &gid).await;
    output_until(&mut ws, "READY").await;
    send_input(&mut ws, "go\r").await;
    output_until(&mut ws, "AFTER").await;
    assert_dead_with_code(&node, &id, 5);
}

/// 不变量 2：WS 断了 ≠ agent 崩了。
///
/// 这条不是"断开时别去杀它"那么简单——断开时**往 pane 里写了什么**同样致命。
/// 守卫：`AttachedPty::detach` 的释放顺序 —— 先 SIGHUP、确认 attach 进程真的退了，才发
/// `Release` 让写线程 drop 掉 portable-pty 的 writer；没确认退出就 `std::mem::forget(w)`。
/// 因为 `UnixMasterWriter::drop` 会往 PTY 写 `\n` + `^D`，只要 attach 还活着，那个 EOF 就
/// 一路转到 pane 里，还在 `read` 的 agent 于是走完剩下的脚本退出（devcenter 的老回归）。
/// 把 `Release` 提到 kill 之前，这条红。
///
/// 所以断开之后不只断言"活着"，还断言"仍卡在 read 上"：EOF 一旦进去，fake-agent 会
/// 立刻打印 AFTER 并退出，pane 就死了。
#[tokio::test(flavor = "multi_thread")]
async fn invariant_2_websocket_disconnect_does_not_kill_the_agent() {
    let mut node = TmuxNode::new();
    node.serve().await;
    let s = node.create_fake(
        "agent",
        "ignore-hup; print READY; read; print AFTER; exit 5",
    );
    let id = s.record.id.clone();
    let gid = node.gid(&id);
    node.wait(&id, |v| v.alive);

    for round in 0..3 {
        let mut ws = terminal(&node, &gid).await;
        output_until(&mut ws, "READY").await;
        drop(ws); // 网络断：不发 close 帧，直接掉线
        tokio::time::sleep(Duration::from_millis(500)).await;
        assert!(node.pane_alive(&s), "第 {round} 次断线后 agent 死了");
        assert!(
            !node.tail(&s).contains("AFTER"),
            "断线不该把 EOF 写进 pane（第 {round} 次）: {}",
            node.tail(&s)
        );
    }

    // 三次断线都没喂过 EOF：read 还在等，这一行才是它收到的第一份输入。
    let mut ws = terminal(&node, &gid).await;
    output_until(&mut ws, "READY").await;
    send_input(&mut ws, "go\r").await;
    output_until(&mut ws, "AFTER").await;
    assert_dead_with_code(&node, &id, 5);
}

/// 不变量 3：daemon 崩了 ≠ agent 崩了，而且重启后结论还读得回来。
///
/// 守卫：会话建在 tmux server 里（agora 进程只是客户端）＋ 建会话时的 `remain-on-exit on`。
/// 去掉 `remain-on-exit`，agent 退出的瞬间 pane 就被回收，重启后的 daemon 只能看到
/// "会话不见了"，退出码永远读不到 —— 这条的后半段红。
#[tokio::test(flavor = "multi_thread")]
async fn invariant_3_daemon_crash_keeps_agent_and_its_exit_code() {
    let mut node = TmuxNode::new();
    node.serve().await;
    let s = node.create_fake(
        "agent",
        "ignore-hup; print READY; read; print AFTER; exit 5",
    );
    let id = s.record.id.clone();
    let gid = node.gid(&id);
    node.wait(&id, |v| v.alive);

    let mut ws = terminal(&node, &gid).await;
    output_until(&mut ws, "READY").await;

    // daemon 死。crash() 只取消监听器：axum 把升级后的连接交给独立任务，它们要等对端断开
    // 才结束（真实进程死亡时 PTY master 随进程关闭，attach 自行退出），所以把 ws 也断掉。
    node.crash();
    drop(ws);
    tokio::time::sleep(Duration::from_millis(500)).await;

    assert!(node.pane_alive(&s), "daemon 死后 agent 不能退出");
    assert!(
        client_pids(&node).is_empty(),
        "daemon 死后 attach 客户端应被收走: {:?}",
        client_pids(&node)
    );

    // daemon 重启：重建对象、reconcile，会话仍在且活着；重新 attach 输入照样送达。
    let m = node.rebuild();
    assert!(m.get(&id).unwrap().alive);
    node.serve().await;
    let mut ws = terminal(&node, &gid).await;
    output_until(&mut ws, "READY").await;
    send_input(&mut ws, "go\r").await;
    output_until(&mut ws, "AFTER").await;

    // 退出发生在"上一个 daemon 已经死过一次"之后，结论仍读得到：remain-on-exit 的功劳。
    let v = assert_dead_with_code(&node, &id, 5);
    assert!(
        node.tail(&v).contains("READY"),
        "跨 daemon 重启的 scrollback 还在: {}",
        node.tail(&v)
    );
}

/// 不变量 4：关标签页 ≠ 杀会话。
///
/// 与不变量 1 的区别是这次是**正常关闭**（发 close 帧走完握手），最容易被写成"顺手清理"。
/// 守卫：gateway 的 detach 只收 attach，不动运行时会话；`SessionManager` 也不因断连删记录。
/// 断开后会话必须还在列表里、还 alive、scrollback 完整、能再 attach。
#[tokio::test(flavor = "multi_thread")]
async fn invariant_4_closing_a_tab_does_not_kill_the_session() {
    let mut node = TmuxNode::new();
    node.serve().await;
    let s = node.create_fake(
        "agent",
        "ignore-hup; print READY; read; print AFTER; exit 5",
    );
    let id = s.record.id.clone();
    let gid = node.gid(&id);
    node.wait(&id, |v| v.alive);

    let mut ws = terminal(&node, &gid).await;
    output_until(&mut ws, "READY").await;
    ws.close(None).await.unwrap(); // 用户点 ×：正常关闭
    drop(ws);
    tokio::time::sleep(Duration::from_millis(500)).await;

    let ids: Vec<String> = node
        .sessions
        .list()
        .unwrap()
        .into_iter()
        .map(|v| v.record.id)
        .collect();
    assert_eq!(ids, vec![id.clone()], "关标签页不该删掉会话记录");
    assert!(node.sessions.get(&id).unwrap().alive);
    assert!(node.pane_alive(&s));
    assert!(
        node.tail(&s).contains("READY"),
        "关标签页不该丢 scrollback: {}",
        node.tail(&s)
    );
    assert!(
        client_pids(&node).is_empty(),
        "关标签页要收走 attach 客户端，只是不许动会话"
    );

    // 再打开一次，还是同一个 agent。
    let mut ws = terminal(&node, &gid).await;
    output_until(&mut ws, "READY").await;
    send_input(&mut ws, "go\r").await;
    output_until(&mut ws, "AFTER").await;
    assert_dead_with_code(&node, &id, 5);
}

/// 不变量 5：一个坏掉的 agent 不能影响另一个。
///
/// 最难缠的"坏"不是退出，是**不返回**：运行时调用挂住。这里把一个节点的运行时二进制换成
/// 永远不返回的脚本，另一个节点是真 tmux。
/// 守卫：`runtime::exec` 的看门狗（`done_rx.recv_timeout(timeout)` 超时就 kill 子进程）＋
/// 所有运行时调用都在 `spawn_blocking` 里跑。把超时去掉（或改成 None/极大值），
/// 坏节点的 list 永不返回，这条测试直接卡死超时红。
#[tokio::test(flavor = "multi_thread")]
async fn invariant_5_one_hung_agent_does_not_affect_another() {
    let good = TmuxNode::new();
    let s = good.create_fake("good", "print READY; sleep 60000");
    let gid = s.record.id.clone();
    good.wait(&gid, |v| v.alive);

    // 一个"永远不返回"的运行时。
    let hung = std::env::temp_dir().join(format!("agt-hung-{}.sh", std::process::id()));
    // `exec`：让脚本本身变成 sleep，别留下一个持着 stdout 的孙子进程——看门狗 kill 的是
    // 直接子进程，孙子还攥着管道的话主线程等不到 EOF（这是脚本的毛病，不是守卫的）。
    std::fs::write(&hung, "#!/bin/sh\nexec sleep 600\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&hung, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let bad = TmuxNode::with_runtime(hung.to_str().unwrap(), Duration::from_secs(1));

    // 坏节点上开一个会话：`create` 一定会真的去 exec 运行时（`list` 见到 socket 不存在会
    // 直接返回空，测不到看门狗），于是这一次调用就挂在假 tmux 上。
    // 结果走 channel 回来：看门狗要是没了，这里是"收不到"而不是"整个测试卡死 600 s"。
    let bad_rt = bad.rt.clone();
    let (tx, rx) = std::sync::mpsc::channel();
    let started = Instant::now();
    std::thread::spawn(move || {
        tx.send(bad_rt.create(&LaunchSpec {
            name: "hung".into(),
            command: "true".into(),
            cwd: std::env::temp_dir(),
            env: vec![],
            size: Size::default(),
        }))
    });

    // 挂着的这段时间里，好节点照常读、照常 attach、照常收输出。
    for _ in 0..3 {
        assert!(good.sessions.get(&gid).unwrap().alive, "好节点被拖累了");
        assert!(good.pane_alive(&s));
        std::thread::sleep(POLL);
    }

    // 坏调用自己在超时里收场，不是永久占着一个线程。假 tmux 要 sleep 600 s，
    // 这里 15 s 收不到结论就说明看门狗没生效（节点的 exec_timeout 是 1 s）。
    let got = rx.recv_timeout(Duration::from_secs(15));
    assert!(
        got.is_ok(),
        "看门狗没生效：挂住的运行时调用 {:?} 还没回来",
        started.elapsed()
    );
    let err = got.unwrap().expect_err("假运行时不该报告建好了会话");
    assert!(
        matches!(err, agora::runtime::RuntimeError::Timeout(_)),
        "挂住的运行时调用应当以超时收场: {err:?}"
    );

    // 好节点仍然完好。
    assert!(good.pane_alive(&s));
    assert!(good.sessions.get(&gid).unwrap().alive);
    let _ = std::fs::remove_file(&hung);
}

/// 不变量 7：运行时是唯一真相，SQLite 只放元数据。
///
/// 把库整个删掉（磁盘坏 / 用户清了 AGORA_HOME）再重启：会话必须还能从运行时被发现。
/// 守卫：`SessionManager::unregistered()` + reconcile 的"运行时里有、库里没有"分支。
/// 让 `unregistered()` 返回空，这条红。
///
/// 后半段钉的是"别把活性写进库"这个更根本的诱惑：schema 里不许出现 alive / exit /
/// pid 这类活性列，否则重启后会拿旧快照冒充真相。
#[tokio::test(flavor = "multi_thread")]
async fn invariant_7_runtime_is_the_source_of_truth_not_the_database() {
    let node = TmuxNode::new();
    let s = node.create_fake("agent", "print READY; sleep 60000");
    let id = s.record.id.clone();
    node.wait(&id, |v| v.alive);
    let rref = s.record.runtime_ref.clone().unwrap();

    // sessions 表只存元数据：活性字段一个都不许有。
    let cols: Vec<String> = {
        let conn = node.db.conn();
        let mut stmt = conn
            .prepare("SELECT name FROM pragma_table_info('sessions')")
            .unwrap();
        let rows = stmt.query_map([], |r| r.get::<_, String>(0)).unwrap();
        rows.map(|r| r.unwrap()).collect()
    };
    for banned in ["alive", "pid", "exit_code", "exit_signal", "status"] {
        assert!(
            !cols.iter().any(|c| c == banned),
            "sessions 表出现活性列 `{banned}`，真相就有了第二份: {cols:?}"
        );
    }

    // 库没了，daemon 重启。
    let m = node.rebuild_after_db_loss();
    assert!(m.list().unwrap().is_empty(), "新库是空的");
    let found = m.unregistered().unwrap();
    assert!(
        found
            .iter()
            .any(|r| r.session.r#ref.0 == rref && r.session.alive),
        "会话必须从运行时重新被发现: {found:?}"
    );
    assert!(node.pane_alive(&s), "删库不能碰到 agent");
}
