# 架构图与 peer 形态

原则见 MISSION §3；为什么选 peer 模型见 ADR-004。

## 总体架构

```
┌───────────────────────────────────────────────────────────────┐
│  CLIENT（Mac 浏览器 / iPhone / Android / iPad —— 同一个 Web） │
│  ┌─────────────────────────────────────────────────────────┐  │
│  │ Dashboard │ Agent Tabs │ xterm.js │ Notifications       │  │
│  │      （一次只连一个节点，看到它 + 它的 peer）            │  │
│  └───────────────┬──────────────────────────┬──────────────┘  │
└──────────────────┼──────────────────────────┼─────────────────┘
   HTTP（仅 loopback）/ HTTPS + WS         HTTPS / WebSocket
              （任意网络；二选一，不是同时；§8 两个监听器）
                   │                          │
┌──────────────────┼──────────┐   ┌───────────┼──────────────────┐
│  NODE A (macOS)             │   │  NODE B (Linux)   … NODE N   │
│         agora daemon ◄──── peer 链路（同一套 API，机器 token）────► agora daemon │
│   ┌─────────┼─────────┐     │   │   ┌─────────┼─────────┐      │
│ Session   State   Terminal  │   │ Session   State   Terminal   │
│ Manager  Detector Gateway   │   │ Manager  Detector Gateway    │
│   └─────────┼─────────┘     │   │   └─────────┼─────────┘      │
│        运行时【ADR-001】     │   │        运行时【ADR-001】      │
│   ┌──────┼──────┐           │   │   ┌──────┼──────┐            │
│ Claude  Codex  Grok         │   │ Claude  Codex  Grok          │
│ Metadata: SQLite            │   │ Metadata: SQLite             │
└─────────────────────────────┘   └──────────────────────────────┘
   节点互为 peer、一跳、只导出本机会话（MISSION §3.5）；节点数量任意
```

## 当前实例的 peer 形态

| 形态 | 怎么走 |
|---|---|
| Mac 浏览器看自己 + zuan | 开 `127.0.0.1`；Mac 配了 zuan 为 peer。**V1 唯一形态**：只需 Mac 这一行配置，Mac 自己保持只听 loopback |
| Mac 浏览器只看 zuan | 开 zuan（需 zuan 有浏览器可信证书 + 该浏览器已配对，手机阶段之后才可用） |
| 手机看 Mac + zuan | 开 zuan；zuan 配了 Mac 为 peer（V2 首批） |
| 手机只看一个 | 开该节点（V2 首批） |

两台互为 peer 不是新机制，就是两边各写一行配置。Mac 带出门且 zuan 够不着它时，zuan 上显示 Mac 为"上次见到"。

## peer 接缝（agora-7ku.11）

MISSION §3.5 说节点是 peer 的 API 客户端；代码里这条链路切成三层，让并入视图（agora-7ku.5）、一跳转发（agora-7ku.7）与认证 / TLS（agora-7ku.2 / agora-7ku.10）不互相等：

| 层 | 在哪 | 回答的问题 |
|---|---|---|
| `PeerTransport` | `src/peer/transport.rs` | 把一个 HTTP 请求（`request`）或一次 WS 建连（`connect_ws`）送到某个 peer。请求 URI 只写路径与查询串，主机、TLS、Bearer 由实现补。每次尝试带超时（缺省 5 s，`DEFAULT_TIMEOUT`），超时是 transport 的属性而不是调用方的参数——devcenter 无超时 connect 拖满 45 s 的教训（ADR-004 引用的附录 B）。失败按类型分：`Timeout`、`Unreachable`、`FingerprintMismatch`（独立状态，绝不并进"离线"，ADR-003 D4）、`WsRejected(status)`、`Protocol`；HTTP 的非 2xx 不是传输错误，原样交回调用方。 |
| `PeerRegistry` | `src/peer/registry.rs` | 节点名 → transport，启动时装好。本机 `node.id` 不在表里（本机不是自己的 peer，配成 peer 是配置错误）；`route(node)` 三分：`Local` 直达 SessionManager、`Peer(t)` 经 t 一跳转发、`Unknown` 就是 `docs/spec/api.md` 里 `node_unknown` 的依据。 |
| 实现 | 进程内 fake `InProcessTransport` 与生产桩 `HttpsTransport`（同文件）。桩只有 `peers[]` 一项的配置与超时，用起来回 `Unimplemented`（不是超时也不是不可达，别显示成离线）；agora-7ku.10 在原地填成 TLS + SPKI 指纹钉住 + Bearer（ADR-003 D3 / D4） | fake 直接拿同一进程里另一个节点实例的 axum `Router`：HTTP 走 `oneshot`，WS 在 `tokio::io::duplex` 上由 hyper 服务单条连接做真握手；不开 socket、不碰 TLS。`set_offline` 模拟断线（每次尝试立即 `Unreachable`，已建好的 WS 各自死各自的）。 |

fake 让对端看到 `Peer { name }` 靠请求扩展 `InProcessPeer`（`src/api/auth.rs`）：`Principal` 提取器在 Bearer 之前先认它。它不是 ADR-003 D1 禁止的例外路由——请求扩展不是 HTTP 头，线上任何字节都变不成它，只带 peer 名连 `Human` 都造不出来；守卫 `tests/peer_fake.rs::in_process_peer_is_only_known_to_the_extractor_and_the_fake_transport` 把它钉在定义、认、写它的三个文件里。Peer principal 不做浏览器的同源校验（ADR-003 D7 "Bearer 跳过"），`/api/events` 与终端的 WS 升级也是。

单进程多节点的骨架就是这三样（MISSION §2.3 规则 9）：`tests/peer_fake.rs::two_in_process_daemons_can_call_each_other_as_peer` 让 A 以 Peer principal 调到 B 的 `GET /api/sessions`，agora-7ku.5 / agora-7ku.7 的 fake 多节点测试都以此为底。
