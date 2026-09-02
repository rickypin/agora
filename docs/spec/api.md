# API 与 WebSocket

原则在 MISSION §7.3；本文是端点与消息形态，ADR-002 / ADR-003 定稿后重写。

## REST

```
GET    /api/sessions
POST   /api/sessions
GET    /api/sessions/:id
PATCH  /api/sessions/:id
POST   /api/sessions/:id/input     # 不经终端的 respond：文本 / 选项；WAITING 与 TURN_DONE 的主路径
POST   /api/sessions/:id/restart
POST   /api/sessions/:id/kill
DELETE /api/sessions/:id
GET    /api/nodes                  # 本机 + 已配置 peer 的状态：online / last_seen
GET    /api/system                 # 含 api_version
GET    /api/health
```

会话 id 一律 `<node>:<id>`；`GET /api/sessions` 同列本机与 peer 会话，每条带 `node` 字段，对 peer 会话的写操作与终端流经一跳转发（原则与调用方、API 版本、DELETE ≠ kill 见 MISSION §7.3）。

WebSocket：

```
WS /api/sessions/:id/terminal    # 终端流
WS /api/events                   # 全局事件：status change / session created / session removed / notification
```


## WebSocket

Client → Server：`{ "type": "input", "data" }`、`{ "type": "resize", "cols", "rows" }`、`{ "type": "ping" }`

Server → Client：`{ "type": "output", "data" }`、`{ "type": "status", "status" }`、`{ "type": "exit", "exit_code" }`

MVP 用 JSON / Text WebSocket 足够；binary terminal frames 放到 V2。

客户端消费 `/api/events` 的纪律：就地 patch、合并突发（~300 ms 重同步）、内容相等不重渲染；不得每事件全量刷新，不得回退为轮询；断流重连后拉全量快照对齐（与 peer 链路同一"快照 + 增量"模式，MISSION §3.5）。

## respond 的两种语义（MISSION §7.3）

- `POST /api/sessions/:id/input` 接受 `{ "kind": "decision", "decision": "allow" | "deny" | <option> }` 或 `{ "kind": "text", "data": "..." }`。
- `decision` 只对有挂起 hook 决定的会话有效（否则错误类型 `NoPendingDecision`）：agora 的 PermissionRequest hook 挂起等待，答复经 hook 返回给 agent，不注入键击；`decision_via_hook = false` 的 agent（Grok）只提供"打开终端"。挂起上限与超时见下文（ADR-002 D5）。
- `text` 经 PTY 写入；无 hook 的 agent 与自由问答走这条。

## hook 投递（ADR-002 D3）

- **没有 HTTP 端点**。agent 的 hook 命令是 `agora hook --host <agent>`：payload 落到 `<state_dir>/hooks/inbox/<host>/<agent_session_id>/<ts>-<seq>.json`（先 `.part` 再 rename），再经 `<state_dir>/agora.sock`（unix socket，0600）唤醒 daemon；daemon 不在时文件留着，启动时按文件名顺序重放（MISSION §3.4）。
- 需要答复的事件（Claude Code `PermissionRequest`）由 hook 进程在 socket 上挂起等 daemon 回 allow / deny / none；none、超时、socket 断都是 fail-open（exit 0 不输出，TUI 的提示仍在）。挂起上限每会话 8、节点 256，超时 55 min。
- `/api/events` 新增 `decision.resolved`（挂起被终端答复、进程退出或超时解除）。
## Health（MISSION §10.3）

```
GET /api/health
→ { "status": "ok", "runtime": true, "database": true,
    "tls": "external", "push": { "apple": true, "fcm": false },
    "peers": { "mac": { "online": false, "last_seen": "2026-09-02T23:10:00Z" } } }
```
