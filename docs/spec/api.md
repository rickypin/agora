# API 与 WebSocket

原则在 MISSION §7.3；本文是端点与消息形态，hook 投递按 ADR-002、认证按 ADR-003 定稿。

## REST

```
GET    /api/sessions               # { sessions: [...], unregistered: [...] }：已登记会话 + 运行时里未登记的（Unknown Agent，可采纳，§5.5）
POST   /api/sessions               # { display_name, agent_type, working_directory, worktree?, task_ref?, command?, cols?, rows? } → 201；command 缺省链：agents.<agent_type>.command → Adapter 的 default_command → agent_type 本身
GET    /api/sessions/:id
PATCH  /api/sessions/:id           # { display_name }：改名即落锁（§4.5）；其它 Session Settings 字段随前端落地
POST   /api/sessions/:id/input     # 不经终端的 respond：文本 / 选项；WAITING 与 TURN_DONE 的主路径（M1b）
POST   /api/sessions/:id/restart   # body 可选 { confirmed: bool }；会杀且未确认 → 409 needs_confirmation；响应多一个 restart 字段（见下）
POST   /api/sessions/:id/kill      # 同上；确认跟着"杀"走（MISSION §8）：FINISHED / FAILED / 会话已不在 → 直接执行
POST   /api/sessions/:id/cleanup   # 回收已退出会话保留的运行时会话与输出（ADR-001 D4 清理）；进程还活着 → 错误类型 StillAlive
DELETE /api/sessions/:id           # 只删 metadata；已退出的顺手清理
POST   /api/sessions/adopt         # { runtime_ref, display_name?, project?, agent_type? }：采纳可采纳运行时里的未注册会话（§5.5）→ 201；已登记 → 409 already_registered
GET    /api/projects               # project_roots 扫描结果，按最近使用排序（§6.4）
GET    /api/projects/worktrees     # ?path=<repo>：该仓库现有 worktree（§6.4；新建 worktree 归 M3 A44）；path 不是已知项目 → 400 bad_request
GET    /api/agents                 # New Agent 对话框的 Agent 下拉：[{ name, command }]，来自 Adapter 启动侧 + agents.<name>.command 覆盖（§5.2）
GET    /api/tasks/ready            # ?path=<repo>：有 bd 的仓库的 bd ready，只读（M3 A43）
GET    /api/nodes                  # 本机 + 已配置 peer 的状态：online / last_seen
GET    /api/system                 # { api_version, version, node }
GET    /api/health                 # 未认证只返回 { "status": "ok" }；带 principal 是下文的完整形态
POST   /api/auth/pair              # { token } → Set-Cookie agora_session + { device }；唯一的未认证写端点
POST   /api/auth/pair/new          # 已认证：铸造一条配对链接 → { url }（origin 取自 Host；Dashboard "配对新设备"的 UI 在 V2-1）
POST   /api/auth/logout            # 吊销当前设备并清 cookie → 204
GET    /api/auth/devices           # 已配对设备列表（含已吊销的，revoked_at 非空）
DELETE /api/auth/devices/:id       # 吊销一台设备 → 204；即时生效
```

错误应答统一为 `{ "error": "<type>", "message": "..." }`，`type` 是 snake_case，调用方按它分支、不做字符串匹配（§2.3 规则 10）：`unauthenticated`（401）、`bearer_requires_tls`（401）、`pair_invalid`（401，未知 / 已用 / 过期不区分）、`cross_origin`（403）、`pair_pending_limit`（429）、`device_not_found`（404）；会话端点：`not_found`（404）、`node_unknown`（404，id 的节点前缀不是本节点）、`needs_confirmation`（409）、`still_alive`（409）、`no_runtime`（409，external 会话没有运行时句柄）、`already_registered`（409）、`read_only`（409，采纳 socket 上的会话拒绝写操作）、`bad_request`（400）、`runtime`（502）、`git`（502，`/api/projects/worktrees` 的 git 调用失败）、`database`（500）。`NoPendingDecision` 随 M1b 落地为 `no_pending_decision`。

`GET /api/projects` 每项是 `{ path, name, last_used_at }`，按最近使用排序（未用过的排在后面、按名字）；列表是 `project_roots` 的扫描结果与库里 `projects` 表的并集，目录已不存在的行在读取时删除。`last_used_at` 只在 `POST /api/sessions` 时更新——"最近使用"指的是起过会话。`GET /api/projects/worktrees` 每项是 `{ path, branch, head, main, locked }`，`branch` 去掉 `refs/heads/` 前缀、detached HEAD 为 null，第一项是主 worktree。

每条会话的形态是 `sessions` 行的全部字段 + 运行时实时事实（`name`、`alive`、`exit`、`pid`、`managed`）+ 状态判定（`status`、`source`、`confidence`、`reason`；四层来源仲裁见 ADR-002 D1，`confidence` 0–1 只供排障，UI 不显示）+ `detail`（hook 给的问题文本 / 正在用的工具 / 最后一条回复，就地回答与两行预览用）+ `respond_via`（`hook`：WAITING(decision) 可经 `input` 的 decision 回答；`terminal`：agent 的 hook 不能替用户批准，只能打开终端）+ Dashboard 字段（MISSION §6.3；ADR-002 D8）：`prompt`（`❯`，最近 `prompt.submitted` 的首行）、`progress`（`↳`，`activity` 的当前工具或 `turn.ended` 的最后一条回复的首行）——都来自 hook，没有 hook 的会话两者为 null 而 `preview` 是 pane 末尾最后一个非空行（已 strip ANSI；有 hook 的会话不读 pane，`preview` 为 null）；`status_since`（当前状态的起点，unix 秒，"waiting 3m"与同分排序用）；`task`（`task_ref` 像 beads id 且在会话工作目录里 `bd show --json` 查得到时的 `{ id, title, priority, status }`，否则 null；异步补齐，到了发 `session_updated`；agora 对 beads 零写入，不变量 12）。`task_ref` 为空的会话在首条 `prompt.submitted` 时被补成该 prompt 的首行（≤ 120 字），对话框里填的不动；`id` 是全局 id `<node>:<id>`，本机 id 在 `local_id`，另带 `node`。`unregistered` 的每项是 `{ runtime_ref, name, title, alive, managed, working_directory, agent_hint, node }`，`agent_hint` 是从 pane 进程的后代里认出来的 adapter 名（认不出为 null），只是 Adopt 表单的默认值：`adopt` 请求里用户填的 `agent_type` 优先，没填才用 hint，都没有就是 `unknown`（MISSION §5.4）。未登记列表不走事件流，只随 `GET /api/sessions` 来；前端在 `session_removed` 与采纳之后重拉一次。

agora 没起过的会话经 hook 自己出现（§5.4，A16 / A22 合流）：无 `AGORA_SESSION_ID` 的事件按 `(host, agent_session_id)` 找已登记会话，找不到就登记——信封里的运行时环境（`TMUX` / `TMUX_PANE`）能在 agora 自己的 socket 或 `adopt_sockets` 上定位到 pane 时以 `origin = adopted` 登记（有终端与全部 respond，`agent_type` 就是 hook 的宿主）；定位不到的以 `origin = external` 登记（`runtime_ref` NULL，`display_name` 取工作目录名，状态 / 两行 / 通知与经 hook 的 allow / deny 照常，`text` → 409 `no_runtime`）。external 会话的 `alive` 看最近一条 hook 报来的 agent 进程号（Claude 的 `CLAUDE_PID`）`kill(pid, 0)` 的结果，进程没了报 FINISHED（没有退出码可分 FAILED），daemon 重启到下一条 hook 之前是 UNKNOWN；agent 没自报会话 id 的事件不登记。

## 认证（ADR-003）

- 每个请求先解析出一个 principal：`Human { device }`（cookie `agora_session`）或 `Peer { name }`（`Authorization: Bearer apt_<name>_…`）；两者互斥，Bearer 优先解析。未认证白名单只有 SPA 静态资源、`GET /api/health` 的公开子集、`POST /api/auth/pair`；其余一律 401 `unauthenticated`。**没有 loopback 例外**。
- 配对链接 `<origin>/#pair=<token>` 由 `agora open` / `agora url` / `agora pair`（经 unix socket）或已认证的 `POST /api/auth/pair/new` 铸造；256 位、单次、5 分钟。前端读 fragment 后 `POST /api/auth/pair`，再清掉 fragment。
- cookie `agora_session` 带 `Max-Age`，取值是 `auth.session_idle`（缺省 30 天）；服务端每次刷新 `last_seen_at`（每小时至多一次）时随响应重发一遍该 cookie，让浏览器侧的窗口跟着服务端一起滑动（ADR-003 D2）。
- Bearer 只在 TLS 监听器上被接受，明文监听器回 401 `bearer_requires_tls`。
- cookie 认证的非 GET 请求必须带同源 `Origin`（或 `Sec-Fetch-Site: same-origin`），两者都没有 → 403 `cross_origin`（curl 调写端点要自己带 Origin）；WS 升级校验 `Origin` 与 `Host` 同源；Bearer 跳过。
- Kill / Restart 带 `confirmed`；所属节点判断需要确认而未确认 → 错误类型 `NeedsConfirmation`；转发节点原样转发 `confirmed`。

会话 id 一律 `<node>:<id>`；`GET /api/sessions` 同列本机与 peer 会话，每条带 `node` 字段，对 peer 会话的写操作与终端流经一跳转发（原则与调用方、API 版本、DELETE ≠ kill 见 MISSION §7.3）。路径里的 `:id` 接受全局 id，也接受裸的本机 id（curl 手敲时少打一段）；节点前缀不是本节点 → `node_unknown`（peer 转发随多节点阶段）。

每条 `/api/` 请求写一行结构化日志：方法、路径、状态、耗时、principal（未认证请求该栏为空）；不记请求体（MISSION §10.2）。

## WebSocket

```
WS /api/sessions/:id/terminal    # 终端流
WS /api/events                   # 全局事件：status change / session created / session removed / notification
```

终端流 Client → Server：`{ "type": "input", "data" }`、`{ "type": "resize", "cols", "rows" }`、`{ "type": "ping" }`

终端流 Server → Client：`{ "type": "output", "data" }`、`{ "type": "status", "status": "attached" }`、`{ "type": "exit", "exit": { "kind": "code", "value": n } | { "kind": "signal", "value": "hup" } }`（与 `GET /api/sessions` 里 `exit` 字段同一形态，ADR-001 的 `Exit`）、`{ "type": "pong" }`。`exit` 只说明这一条 attach 流结束了，不代表会话或 agent 退出。升级 URL 可带 `?cols=&rows=` 作为初始尺寸（缺省 160×48），之后由 `resize` 消息调整；多客户端同看时由运行时仲裁尺寸。keepalive：服务端每 20 s 发 WS Ping，65 s 内没有任何入站帧就断开这一条 attach（会话不受影响）。断开时给 attach 进程 SIGHUP，确认其退出后才释放 PTY（ADR-001 D5）。

事件流 Server → Client 每帧一个 JSON **数组**（服务端 ~50 ms 合并突发，同一会话的连续状态变化只留最后一条）：`{ "type": "session_created", "id", "session" }`、`{ "type": "session_removed", "id" }`、`{ "type": "session_updated", "id", "session" }`（metadata 改了，如改名；整行重发）、`{ "type": "status_changed", "id", "status", "source", "reason", "alive", "detail", "prompt", "progress", "preview", "status_since" }`（`detail` / 两行预览 / pane 预览变了也算状态变化；`task` 的到达走 `session_updated`）、`{ "type": "decision_resolved", "id", "tool_use_id", "via": "dashboard" | "terminal" | "session" | "exit" | "timeout" }`（挂起的决定被解除，ADR-002 D5）、`{ "type": "notification", "id", "title", "body", "status" }`（浏览器通知，MISSION §6.6 / A18：只在 RUNNING → WAITING / TURN_DONE / FINISHED / FAILED 四种转换上各发一条，紧跟该会话的 `status_changed` 之后；`title` 是 §6.6 表的文案 `<Agent> / <name> @ <node> needs input | finished its turn | finished | failed`，`body` 是 `detail` 首行，`status` 是转换后的状态，前端据此决定点击落到哪——WAITING / TURN_DONE 落到 Dashboard 就地回答区；`notifications.enabled = false` 时一条不发；RUNNING → IDLE / UNKNOWN、STARTING → FAILED 不通知）、`{ "type": "resync" }`（服务端丢过该客户端的事件，必须重拉全量）。Client → Server 只有 `{ "type": "ping" }` → `{ "type": "pong" }`。进程状态的变化由 daemon 按 `status.detector_interval` 轮询 Session Manager 求差发出；API 自己做的增删立即发。两个 WS 升级都先过 principal，再校验 `Origin` 与 `Host` 同源（403 `cross_origin`）。

MVP 用 JSON / Text WebSocket 足够；binary terminal frames 放到 V2。

客户端消费 `/api/events` 的纪律：就地 patch、合并突发（~300 ms 重同步）、内容相等不重渲染；不得每事件全量刷新，不得回退为轮询；断流重连后拉全量快照对齐（与 peer 链路同一"快照 + 增量"模式，MISSION §3.5）。

## respond 的两种语义（MISSION §7.3）

- `POST /api/sessions/:id/restart` 的命令由 Adapter 按 `(agent 版本, agent_session_id)` 生成 resume 参数（ADR-002 D7）：版本来自 `<program> --version` 的探测（缓存：可用记到 daemon 重启，不可用 60 s 后重探）；库里的 `command` 不动，只这一代的启动命令换成 resume 形态，上一代的 resume / pin flag 先拆掉不叠加。响应在会话形态之外多一个 `restart`：`{ "resumed": true, "agent_session_id" }` 或 `{ "resumed": false, "reason" }`（没自报过 id、版本表外不猜参数、没有 Adapter、命令不在 PATH——退化为原命令且说明原因，绝不静默、绝不 `--continue` / `--last`）。
- `POST /api/sessions` 在 Adapter 支持且版本可解析时给命令加钉 id 的参数（Claude `--session-id <uuid>`），并把它写进 `agent_session_id`；agent 经 hook 自报的 id 每次命中都覆盖它（识别顺序：自报 > 钉死 > 用户挑，D7）。用户命令里已经写了 resume / pin 的 flag 就不钉。
- `POST /api/sessions/:id/input` 接受 `{ "kind": "decision", "decision": "allow" | "deny", "message"?, "tool_use_id"? }` 或 `{ "kind": "text", "data": "..." }`。选择题（AskUserQuestion 类）V1 不经 API：Dashboard 只显示问题与"打开终端"（ADR-002 D5）。
- `decision` 只对有挂起 hook 决定的会话有效（否则 409 `no_pending_decision`）：agora 的 PermissionRequest hook 挂起等待，答复经 hook 返回给 agent，不注入键击；`tool_use_id` 缺省答最早登记的那个（Claude 2.1.260 的 PermissionRequest 实测不带 tool_use_id，键是 `tool_name`）；成功返回 `{ "tool_use_id" }`，行立即退出 WAITING，并发 `decision_resolved` via=dashboard。`respond_via = terminal` 的 agent（Grok）只提供"打开终端"。挂起上限与超时见下文（ADR-002 D5）。
- `text` 经 PTY 写入（运行时的 send-keys：字面文本一次、尾部换行作回车键单独一次——Claude 的 TUI 把两者一起到达当成粘贴不提交）；无 hook 的 agent、自由问答、TURN_DONE 的下一条指令走这条；external 会话没有运行时句柄 → 409 `no_runtime`。

## hook 投递（ADR-002 D3）

- **没有 HTTP 端点**。agent 的 hook 命令是 `<AGORA_HOME>/bin/agora hook --host <agent> --home <AGORA_HOME>`（安装写入的完整形态见 ADR-002 D4）：payload 落到 `<AGORA_HOME>/hooks/inbox/<host>/<agent_session_id>/<ts>-<seq>.json`（先 `.part` 再 rename），再经 `<AGORA_HOME>/agora.sock`（unix socket，仅属主可访问 + 对端 uid 校验，ADR-003 D6）唤醒 daemon；daemon 不在时文件留着，启动时按文件名顺序重放（MISSION §3.4）；应用过的文件移到 `<AGORA_HOME>/hooks/done/` 保留 24 h 供排障；信封里 `AGORA_EPOCH` 小于会话当前 epoch 的事件丢弃（Restart 之前那代进程发的）；`hooks/` 下任何一级目录属主不对或 group / other 有位，daemon 拒绝读并记日志（agent 照跑）。
- 安装：`agora hooks install|uninstall <agent> [--dry-run]` 写用户自己的 agent 配置（Claude `~/.claude/settings.json` 的 `hooks`），装前把 diff 打到 stderr，`--dry-run` 只看不写；条目以 `<AGORA_HOME>/bin/agora hook` 为自己的标记，重复装不重复、卸载只删自己的、别人的条目与其它键原样；`install` 顺手把 `<AGORA_HOME>/bin/agora` 指向当前二进制。每个事件的 timeout 由 Adapter 的 `install_spec` 给（PermissionRequest 3600 s、SessionEnd 1 s、其余 20 s）。信封里的 agent 环境变量只收 `CLAUDE_*` / `CODEX_*` / `GROK_*`，名字含 TOKEN / SECRET 之类的不落盘。
- 需要答复的事件（Claude Code `PermissionRequest`）由 hook 进程在 socket 上挂起等 daemon 回 allow / deny / none；none、超时、socket 断都是 fail-open（exit 0 不输出，TUI 的提示仍在）。挂起上限每会话 8、节点 256，超时 55 min。
- `/api/events` 的 `decision_resolved`（挂起被 Dashboard / 终端答复、本轮结束、进程退出或超时解除）形态见上文。

## Health（MISSION §10.3）

未认证只返回 `{ "status": "ok" }`；下面的完整形态需要 principal。

```
GET /api/health
→ { "status": "ok",
    "runtime": { "status": "ok" | "degraded", "reason": null, "path_source": "shell" | "daemon" },   // ADR-001 D7；status/reason 每次请求现算，运行时恢复后自动转回 ok
    "database": true,
    "tls": "external", "push": { "apple": true, "fcm": false },
    "peers": { "mac": { "online": false, "last_seen": "2026-09-02T23:10:00Z" } } }
```
