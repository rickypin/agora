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
| 实现 | 进程内 fake `InProcessTransport` 与生产实现 `HttpsTransport`（同文件）。生产实现：`peers[]` 一项 → TCP + TLS（只比对端 SPKI 指纹，ADR-003 D4；`src/tls/client.rs`）+ `Authorization: Bearer <token_file 内容>`（D3，每次请求时读、不缓存）；每次调用一条连接、用完即弃，没有连接池。URL 不是 `https://`、指纹不是 `sha256:<64 hex>`、token_file 读不了 / 权限过宽 / 内容不像 token 都是 `TransportError::Config(PeerConfigError)`——显示为「配置错误」而不是离线，且发生在拨号之前，网上没有任何字节 | fake 直接拿同一进程里另一个节点实例的 axum `Router`：HTTP 走 `oneshot`，WS 在 `tokio::io::duplex` 上由 hyper 服务单条连接做真握手；不开 socket、不碰 TLS。`set_offline` 模拟断线（每次尝试立即 `Unreachable`，已建好的 WS 各自死各自的）。 |

fake 让对端看到 `Peer { name }` 靠请求扩展 `InProcessPeer`（`src/api/auth.rs`）：`Principal` 提取器在 Bearer 之前先认它。它不是 ADR-003 D1 禁止的例外路由——请求扩展不是 HTTP 头，线上任何字节都变不成它，只带 peer 名连 `Human` 都造不出来；守卫 `tests/peer_fake.rs::in_process_peer_is_only_known_to_the_extractor_and_the_fake_transport` 把它钉在定义、认、写它的三个文件里。Peer principal 不做浏览器的同源校验（ADR-003 D7 "Bearer 跳过"），`/api/events` 与终端的 WS 升级也是。

单进程多节点的骨架就是这三样（MISSION §2.3 规则 9）：`tests/peer_fake.rs::two_in_process_daemons_can_call_each_other_as_peer` 让 A 以 Peer principal 调到 B 的 `GET /api/sessions`，agora-7ku.5 / agora-7ku.7 的 fake 多节点测试都以此为底。

接缝之上的客户端（agora-7ku.5）分两个文件：`src/peer/client.rs` 的 `PeerClient` 是每 peer 一个的 tokio 任务（`main.rs` 在装好 `registry` 后 `spawn_all`），只做"比版本 → 建流 → 拉全量 → 收增量 → 断了退避"这一个循环，`TransportError` 经 `From` 按类型映成 `PeerError`（`WsRejected(401)` → `unauthorized`、`Config` → `misconfigured`、其余 → `unreachable`），keepalive 与终端流同数字（20 s ping / 65 s 判死）。`misconfigured`（ADR-003 D3；agora-41e）不进 `Backoff`：不 `next_delay`、失败计数归零（配置错误不是网络失败，文件修好后若真连不上该从 1 s 起步），改为等固定的 `BackoffPolicy::misconfigured_recheck`（生产 10 s，不抖）再走一遍 `connected`——`HttpsTransport` 的三项配置检查在拨号之前，文件没修好就不上网，修好了自然进正常流程、`seen` 清错误；`retry_now` / `reconnect` 同样能打断这个等待。仓库没有配置热加载，这个定时重读就是"改文件后 reload 才恢复"的落地形态。日志分档：从别的状态**转入**配置错误 warn 一条（正文是 `TransportError` 的 Display，token_file 那类就是 `auth::peer_token::TokenFileError` 的原话，含 `chmod 600 <path>`），之后每次重读仍失败只 debug；持有方 token_file 的判定只有 `load_token_file` 一份，`HttpsTransport` 经 `PeerConfigError::TokenFile(TokenFileError)` 透传它；`src/peer/view.rs` 的 `PeerViews` 是并入视图（一跳过滤、本机时钟改写 `status_since`、stale 位、与全量差分出事件），挂在 `AppState.peer_views`，`GET /api/sessions` 对 Human 把它的行接在本机行后面。两者之间用 `Wake`（`tokio::sync::Notify` + 一个 force 位）通话：`PeerViews::retry_now(name)` 只缩短一次退避等待（agora-7ku.6 的入口）、`reconnect(name)` 在线时也强制断开重连（测试模拟断线用，不算失败不记 `last_error`）；视图任何变化经 `subscribe()` 的 `watch` 通道发信号。字段形态与守卫见 `docs/spec/api.md`「peer 视图」。

## peer 重连退避与状态模型（agora-7ku.12；A29 的策略半边）

MISSION §3.5 只定行为契约（断线保留最后视图并标记、恢复后自动重连、不变量 8），参数属实现形状，落地在这里：

| 参数 | 值 | 为什么 | 代码 |
|---|---|---|---|
| 退避 | 指数，1 s 起步，每次翻倍 | 刚断的多半是瞬断，先快试 | `src/peer/backoff.rs` `BackoffPolicy::PEER` |
| 封顶 | 30 s | Mac 睡眠是常态场景（MISSION §0.2），醒来后要秒级恢复；上限越高醒来越慢 | 同上 |
| jitter | 在 [d/2, d] 里**往下**抖，jitter ∈ [0, 1) 由调用方注入 | 只往下抖，带 jitter 也不会超上限；jitter = 0 就是 1, 2, 4, …, 30 的整齐序列，测试好断言 | `BackoffPolicy::delay(failures, jitter)`；生产用 `random_jitter()` |
| 放弃 | **永不** | 不变量 8 排除"重试 N 次后标 dead"；`Backoff` 作为 `Iterator` 永远给 `Some`，计数饱和不回绕 | `Backoff` |
| 每次尝试超时 | 5 s | devcenter 教训：`TcpStream::connect` 无超时把 plan 拖满 45 s 并泄漏 120 s 阻塞线程（`docs/analysis/devcenter/appendix-b-multihost.md`） | `CONNECT_TIMEOUT`，传输层（agora-7ku.11）按它设 |
| 立即重试 | 用户点开 stale peer 的会话时插一次 | 人已经在等了，不该再等退避 | 点开 stale 会话 = `PeerViews::retry_now(name)` 缩短客户端**这一次**退避等待（`src/peer/view.rs` 的 `Wake`：`Notify::notify_one`，在线时无事、离线时一次点开只多一次尝试，不是轮询）。接线在节点侧、不在浏览器（agora-7ku.6）：`src/api/forward.rs` 的 `hop` 解析到 `Hop::Peer` 且 principal 是 Human、该 peer 视图 `is_stale`——终端 WS 与六个写操作都从这里过；`src/api/sessions.rs` 的 `get` 对 Human 读 peer 视图时同一句。守卫 `tests/peer_stale.rs::opening_stale_peer_triggers_immediate_retry`（退避给 60 s，不点开就一直 stale，点开 2 s 内恢复） |

全是纯函数：不读时钟、不掷骰子，真正的等待交给调用方的 `tokio::time::sleep`；单测不需要假时钟，直接断言返回值（`src/peer/backoff.rs` 的 5 个单测）。集成版在 `tests/peer_stale.rs::reconnect_backoff_is_capped_and_never_gives_up`：策略给 1 ms 起 4 ms 顶，离线 300 ms 里数进程内 fake 的连接尝试 ≥ 20 次（不封顶的指数退避只到得了 9 次）且每次采样 `retrying` 都为 true，随后恢复在线自愈。V1 参数写死，不造配置面；要暴露再挂 `peers[]` 或全局 `status` 段。

**每 peer 状态模型**（`src/peer/state.rs`；JSON 形态与字段含义见 `docs/spec/api.md` Health 节）：`PeerState { online, last_seen, retrying, last_error }`，两条转移——`seen(now)` 一次成功交互（在线、刷新 last_seen、清错误、停退避）、`failed(err)` 一次失败（离线、记类型、进入退避，**last_seen 不动**；`Misconfigured` 时 `retrying = false`——没有网络重试可排，客户端在定时重读配置）。`retrying` 的语义因此是"下一次**网络**重试已排定"，不是"节点还会再试"：配置错误的 peer 节点也会再试（重读文件），只是不叫 retrying。`last_seen` 由本节点时钟打（ADR-004），是 stale 行的"上次见到"——断线时 `PeerClient` 把它原样写到该 peer 每条 stale 行的 `last_seen` 键上（`PeerViews::mark_stale(peer, last_seen)`，agora-7ku.6），重连拉到全量即清；侧栏行与 Header 显示的是同一个数字。`PeerError` 只有五个类型：`IncompatibleVersion` / `FingerprintMismatch` / `Unauthorized` / `Unreachable` / `Misconfigured`（agora-41e；ADR-003 D3——`token_file` 权限 / 属主 / 内容、`url`、指纹字面上就用不了，`TransportError::Config` 一对一映过来）——按类型不按文本（MISSION §2.3 规则 10），Header 与 stale 行只据此选文案。`PeerStates` 是节点名 → 状态的表，挂在 `AppState`：`main.rs` 启动时把 `peers[]` 的名字登记进去（还没连上的 peer 从第一秒起就在 health 里），peer 客户端（agora-7ku.5）在成败时写，`/api/health` 只读快照、不去连任何 peer。Header 的渲染见 `docs/spec/ux.md`「Header 节点状态」。

## 一跳转发（agora-7ku.7）

MISSION §3.5 "写操作与终端流按会话所属节点路由：本机直达，peer 经一跳转发"在代码里只有一个入口：`src/api/forward.rs`。六个写 handler（`src/api/sessions.rs` 的 kill / restart / cleanup / input / patch / delete）与终端升级（`src/api/terminal.rs`）在开头各调一次它，之后的代码与单节点时一字不差——转发不是 handler 里的分支，是 handler 之前的一道闸。

```
浏览器 ──cookie──▶ A: /api/sessions/b:42/kill {confirmed:false}
                     │ forward::route(state.registry, principal, "b:42")
                     │   Local  → 本地 handler（裸 id 也走这里）
                     │   Peer(t)→ t.request(POST /api/sessions/b:42/kill, 同 body)
                     │   Unknown→ 404 node_unknown
                     ▼
                  B: Principal::Peer{a} ─▶ 同一个 kill handler ─▶ 409 needs_confirmation
                     │ （B 若再看到 c:<id>：principal 是 Peer → node_unknown，不转 C）
                     ▼
浏览器 ◀── 409 needs_confirmation（B 的原话，A 只转）
```

三条结构性保证，各有守卫（`tests/forward.rs`）：

| 保证 | 怎么做到的 | 守卫 |
|---|---|---|
| 确认在所属节点判断，转发节点不能代替、不能绕过（ADR-003 D8） | 转发节点没有 `confirmed` 的读写代码，body 原样序列化过去；所属节点上跑的就是本地那个 `require_confirmation`，看到的 principal 是 `Peer`，与 `Human` 同一条路 | `kill_confirmation_enforced_at_owner` |
| 一跳、不成环（ADR-004） | `forward::hop` 在 `Route::Peer` 分支上先看 principal：`Peer` → `node_unknown`。不是"记录跳数"，是"peer 的请求根本进不了转发分支" | `second_hop_is_refused_at_the_middle_node`、`unknown_node_is_rejected` |
| 终端流端到端（MISSION §3.2 的链路多一段 WS，不多一个解释者） | 先 `connect_ws` 到所属节点再升级浏览器；桥只搬帧——文本帧、Ping / Pong、Close 原样过，`exit` 转完主动关两边；本节点不 attach、不开 PTY、不做 keepalive | `terminal_ws_forwarded_bidirectionally` |

传输层失败按 `TransportError` 的类型映射成 `peer_unreachable` / `peer_fingerprint_mismatch` / `peer_config` / `peer_rejected`（表见 `docs/spec/api.md`「一跳转发」），指纹不匹配独立成类（ADR-003 D4）。转发不写 `PeerStates`（`/api/health` 的 peers 段）：那是 peer 客户端（agora-7ku.5）的事件流连接说了算的，一次转发的成败不该让 Header 上的 peer 状态闪。

## hook 接收与外部会话的登记 / 结束（agora-dvh.12；agora-vfi）

MISSION §5.5 说 hook 是手动起的 agent 被管起来的唯一途径；代码里这条链路是 `src/hook/receiver.rs` 的 `ingest_inner`，投递件（形态见 `docs/spec/api.md` hooks 节）到这里先 `hooks.parse` 一次成 `AgoraEvent`，然后找会话、应用、解挂起、进 done、记账本：

| 步骤 | 规则 | 为什么 |
|---|---|---|
| 找会话 | 信封带 `AGORA_SESSION_ID` 的直达；没有的走 `locate_external`：按 `(host, agent_session_id)` 查库，找到就复用 | agora 起的会话与外部会话的 allow / deny 走同一条路 |
| 登记 | 库里没有时，**先看事件**：`parse` 出来为空、或只有 `SessionEnded` 的**不登记**（debug 日志"没见过的会话的 SessionEnd，不登记"），返回 None；其余按信封里的运行时环境定位 pane → 定位到可采纳 socket 的以 `adopted` 登记（有终端），否则 `external`（无句柄）。agent 没自报 id（`unknown`）的照旧不登记。三家宿主同一规则 | 只送来 SessionEnd 的会话是 agora 没见过的会话在结束——hook 装好之前起的会话退出、Codex Desktop 结束一个线程；登记只会造一行永远没有后续事件的僵尸（2026-09-05 侧栏那行 devcenter，agora-vfi） |
| 不登记之后 | 流程不变：id 为 None 就不 apply，`release_for` 仍按 `<host>:<agent_session_id>` 解挂起，文件进 done，账本记一条 `received` | 排障看得见它来过 |
| 活性 | external 行的存活看 `AgentHooks::agent_pid` 给的进程号 `kill(pid, 0)`：Claude 用信封里的 `CLAUDE_PID`，Codex / Grok 用 hook 的 ppid（安装命令 `exec` 进 agora）。**Codex Desktop 的 ppid 不可信**——信封 `agent_env` 带 `CODEX_INTERNAL_ORIGINATOR_OVERRIDE`（真投递件的值是 `Codex Desktop`；CLI 不设它）时 `agent_pid` 返回 None → `Liveness::Unknown`，行只跟 hook 走 | Desktop 线程的父进程是所有线程共用的常驻 `codex app-server`，探活永远为真；宁可不知道活不活，不要拿一个永远活着的进程当会话活着 |
| 挂起解除 | `ingest_inner` 尾部按 `release_for`：同工具的 PostToolUse 等 → `resolve(…, "terminal")`，Stop / SessionEnd → `resolve_session(…, "session")`；`sweep`（每 5 s）另管三种：超时 → `timeout`、会话不在了 → `exit`、**状态机里已经没有这个键的 hold**（登记 ≥ 2 s、同一 epoch）→ `terminal`。最后一种的来源是状态机的"提示消失"规则（`src/status/machine.rs` 的 `observe_hooked`：有挂起、hook 给的 WAITING、文本层见过提示、之后连续 `text_ticks` 个 tick 见不到 → 清挂起、UNKNOWN(text)，钉住到下一条 hook 事件写出新状态；PostToolUse 的解除事件把它抬回 RUNNING，Notification(idle_prompt) 把它落到 TURN_DONE(hook, `idle`)——Esc 之后 Claude 停在提示符约 60 s 就发它，2026-09-06 agora-01g；沉默规则的 UNKNOWN 同样，两条共用 `unknown_from_screen` 谓词；挂起还在的 WAITING 收到 idle_prompt 仍不动）与 PromptSubmitted 清挂起。SessionManager 只在有挂起时对 hooked 会话每 tick capture 屏幕，hook 键与状态机 `pending` 的键是同一个 `decision_key` | 终端里放行后按 Esc，Claude 2.1.261 一个事件都不发（`testdata/claude/2.1.261/hooks/interrupted.jsonl`），hook 会挂到 55 min 超时、Dashboard 的 Allow / Deny 一直在、行钉在 needs input；挂起期间 capture 是 ADR-002 D1 沉默规则的唯一例外，文本层在此只能降不能抬；落 UNKNOWN 不落 RUNNING，因为中断与工具正在跑在屏幕上分不出来（agora-9cd） |

状态机（`src/status/machine.rs`）对 `SessionEnded(reason)`：清挂起；`reason = clear`（Claude 的 /clear：进程活着，同一秒紧接着新 id 的 `SessionStart(source=clear)`）**不改状态**；其余 reason（Claude resume / logout / prompt_input_exit / other、Codex other、Grok shutdown、没有 reason）→ **FINISHED**（source hook、conf 0.8、reason `session ended (hook)`）。进程退出的事实照旧以 1.0 覆盖（observe 第 1 步），所以 agora 起的会话不受影响；没有进程事实的 external 会话则靠这一条结束，下一 tick 就是 ✓。FINISHED(hook) 不是终态：之后的 `SessionStart` 回 STARTING（Codex TUI 的 /new、Claude 的 /resume），只有进程层的 FINISHED / FAILED 才压倒一切。fixture 的 `expect` 随之改（`testdata/*/*/hooks/*.jsonl` 里 SessionEnd 之后的行是 `finished` / `hook`）。

守卫：`tests/hooks_external.rs::session_end_for_an_unseen_session_registers_nothing`、`::external_session_ends_on_session_end_hook`；`tests/state_machine.rs::external_session_ended_by_hook_is_finished_not_unknown`、`::session_end_reason_clear_keeps_the_session_alive`、`::session_start_after_session_end_restarts`；`src/adapter/codex.rs` 单测 `codex_desktop_parent_is_not_the_agent_pid`；挂起解除那一行：`tests/state_machine_pending.rs::pending_permission_is_released_when_the_prompt_leaves_the_screen`、`::pending_permission_stays_while_the_prompt_is_on_screen`、`::pending_permission_never_seen_on_screen_is_left_alone`、`::dashboard_or_terminal_resolution_still_wins`、`::idle_notification_after_the_prompt_is_gone_lands_on_turn_done`、`::idle_notification_while_the_prompt_is_still_pending_is_ignored`、`tests/state_machine.rs::idle_notification_after_silent_hooks_unknown_lands_on_turn_done`、`tests/api_input.rs::an_interrupted_permission_is_released_via_terminal_when_the_prompt_disappears`。
