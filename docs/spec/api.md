# API 与 WebSocket

原则在 MISSION §7.3；本文是端点与消息形态，hook 投递按 ADR-002、认证按 ADR-003 定稿。

## REST

```
GET    /api/sessions
POST   /api/sessions
GET    /api/sessions/:id
PATCH  /api/sessions/:id
POST   /api/sessions/:id/input     # 不经终端的 respond：文本 / 选项；WAITING 与 TURN_DONE 的主路径
POST   /api/sessions/:id/restart
POST   /api/sessions/:id/kill
POST   /api/sessions/:id/cleanup   # 回收已退出会话保留的运行时会话与输出（ADR-001 D4 清理）；进程还活着 → 错误类型 StillAlive
DELETE /api/sessions/:id           # 只删 metadata；已退出的顺手清理
POST   /api/sessions/adopt         # { runtime_ref, display_name?, project?, agent_type? }：采纳可采纳运行时里的未注册会话（§5.5）
GET    /api/projects               # project_roots 扫描结果，按最近使用排序（§6.4）
GET    /api/projects/worktrees     # ?path=<repo>：该仓库现有 worktree（§6.4；新建 worktree 归 M3 A44）
GET    /api/tasks/ready            # ?path=<repo>：有 bd 的仓库的 bd ready，只读（M3 A43）
GET    /api/nodes                  # 本机 + 已配置 peer 的状态：online / last_seen
GET    /api/system                 # 含 api_version
GET    /api/health                 # 未认证只返回 { "status": "ok" }
POST   /api/auth/pair              # { token } → Set-Cookie agora_session + { device }；唯一的未认证写端点
POST   /api/auth/pair/new          # 已认证：铸造一条配对链接 → { url }（origin 取自 Host；Dashboard "配对新设备"的 UI 在 V2-1）
POST   /api/auth/logout            # 吊销当前设备并清 cookie → 204
GET    /api/auth/devices           # 已配对设备列表（含已吊销的，revoked_at 非空）
DELETE /api/auth/devices/:id       # 吊销一台设备 → 204；即时生效
```

错误应答统一为 `{ "error": "<type>", "message": "..." }`，`type` 是 snake_case：`unauthenticated`（401）、`bearer_requires_tls`（401）、`pair_invalid`（401，未知 / 已用 / 过期不区分）、`cross_origin`（403）、`pair_pending_limit`（429）、`device_not_found`（404）。上文的 `StillAlive` / `NeedsConfirmation` / `NoPendingDecision` 落地时同样写成 snake_case。

## 认证（ADR-003）

- 每个请求先解析出一个 principal：`Human { device }`（cookie `agora_session`）或 `Peer { name }`（`Authorization: Bearer apt_<name>_…`）；两者互斥，Bearer 优先解析。未认证白名单只有 SPA 静态资源、`GET /api/health` 的公开子集、`POST /api/auth/pair`；其余一律 401 `unauthenticated`。**没有 loopback 例外**。
- 配对链接 `<origin>/#pair=<token>` 由 `agora open` / `agora url` / `agora pair`（经 unix socket）或已认证的 `POST /api/auth/pair/new` 铸造；256 位、单次、5 分钟。前端读 fragment 后 `POST /api/auth/pair`，再清掉 fragment。
- Bearer 只在 TLS 监听器上被接受，明文监听器回 401 `bearer_requires_tls`。
- cookie 认证的非 GET 请求必须带同源 `Origin`（或 `Sec-Fetch-Site: same-origin`），两者都没有 → 403 `cross_origin`（curl 调写端点要自己带 Origin）；WS 升级校验 `Origin` 与 `Host` 同源；Bearer 跳过。
- Kill / Restart 带 `confirmed`；所属节点判断需要确认而未确认 → 错误类型 `NeedsConfirmation`；转发节点原样转发 `confirmed`。

会话 id 一律 `<node>:<id>`；`GET /api/sessions` 同列本机与 peer 会话，每条带 `node` 字段，对 peer 会话的写操作与终端流经一跳转发（原则与调用方、API 版本、DELETE ≠ kill 见 MISSION §7.3）。

## WebSocket

```
WS /api/sessions/:id/terminal    # 终端流
WS /api/events                   # 全局事件：status change / session created / session removed / notification
```

Client → Server：`{ "type": "input", "data" }`、`{ "type": "resize", "cols", "rows" }`、`{ "type": "ping" }`

Server → Client：`{ "type": "output", "data" }`、`{ "type": "status", "status" }`、`{ "type": "exit", "exit": { "code": n } | { "signal": "term" } }`（ADR-001 的 `Exit` 形态）

MVP 用 JSON / Text WebSocket 足够；binary terminal frames 放到 V2。

客户端消费 `/api/events` 的纪律：就地 patch、合并突发（~300 ms 重同步）、内容相等不重渲染；不得每事件全量刷新，不得回退为轮询；断流重连后拉全量快照对齐（与 peer 链路同一"快照 + 增量"模式，MISSION §3.5）。

## respond 的两种语义（MISSION §7.3）

- `POST /api/sessions/:id/input` 接受 `{ "kind": "decision", "decision": "allow" | "deny", "message"? }` 或 `{ "kind": "text", "data": "..." }`。选择题（AskUserQuestion 类）V1 不经 API：Dashboard 只显示问题与"打开终端"（ADR-002 D5）。
- `decision` 只对有挂起 hook 决定的会话有效（否则错误类型 `NoPendingDecision`）：agora 的 PermissionRequest hook 挂起等待，答复经 hook 返回给 agent，不注入键击；`decision_via_hook = false` 的 agent（Grok）只提供"打开终端"。挂起上限与超时见下文（ADR-002 D5）。
- `text` 经 PTY 写入；无 hook 的 agent 与自由问答走这条。

## hook 投递（ADR-002 D3）

- **没有 HTTP 端点**。agent 的 hook 命令是 `<AGORA_HOME>/bin/agora hook --host <agent> --home <AGORA_HOME>`（安装写入的完整形态见 ADR-002 D4）：payload 落到 `<AGORA_HOME>/hooks/inbox/<host>/<agent_session_id>/<ts>-<seq>.json`（先 `.part` 再 rename），再经 `<AGORA_HOME>/agora.sock`（unix socket，仅属主可访问 + 对端 uid 校验，ADR-003 D6）唤醒 daemon；daemon 不在时文件留着，启动时按文件名顺序重放（MISSION §3.4）。
- 需要答复的事件（Claude Code `PermissionRequest`）由 hook 进程在 socket 上挂起等 daemon 回 allow / deny / none；none、超时、socket 断都是 fail-open（exit 0 不输出，TUI 的提示仍在）。挂起上限每会话 8、节点 256，超时 55 min。
- `/api/events` 新增 `decision.resolved`（挂起被终端答复、进程退出或超时解除）。

## Health（MISSION §10.3）

未认证只返回 `{ "status": "ok" }`；下面的完整形态需要 principal。

```
GET /api/health
→ { "status": "ok",
    "runtime": { "status": "ok" | "degraded", "reason": null, "path_source": "shell" | "daemon" },   // ADR-001 D7
    "database": true,
    "tls": "external", "push": { "apple": true, "fcm": false },
    "peers": { "mac": { "online": false, "last_seen": "2026-09-02T23:10:00Z" } } }
```
