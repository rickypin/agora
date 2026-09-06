# API 与 WebSocket

原则在 MISSION §7.3；本文是端点与消息形态，hook 投递按 ADR-002、认证按 ADR-003 定稿。

## REST

```
GET    /api/sessions               # { sessions: [...], unregistered: [...] }：已登记会话 + 运行时里未登记的（Unknown Agent，可采纳，§5.5）；Human 调用还并入各 peer 的会话行（带 node / stale），Peer 调用只给本机行——一跳防环（文末「peer 视图」）
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
GET    /api/projects/worktrees     # ?path=<repo>：该仓库现有 worktree（§6.4）；path 不是已知项目 → 400 bad_request
POST   /api/projects/worktrees     # { path, name, base? }：git worktree add -b <name> <worktree_root>/<name> <base> → 201，形态同 GET 的一项（§6.4 只管"生"，A44）；冲突 409 worktree_exists / branch_exists / path_exists，名字不合法 400 bad_request，git 失败 502 git
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

错误应答统一为 `{ "error": "<type>", "message": "..." }`，`type` 是 snake_case，调用方按它分支、不做字符串匹配（§2.3 规则 10）：`unauthenticated`（401）、`bearer_requires_tls`（401）、`pair_invalid`（401，未知 / 已用 / 过期不区分）、`cross_origin`（403）、`peer_forbidden`（403，Peer 调 `/api/auth/*` 里 logout 以外的端点）、`pair_pending_limit`（429）、`device_not_found`（404）；会话端点：`not_found`（404）、`node_unknown`（404，id 的节点前缀既不是本机也不是已配置的 peer；或请求来自 peer 而前缀不是本机——一跳）、`needs_confirmation`（409）、`still_alive`（409）、`no_runtime`（409，external 会话没有运行时句柄）、`no_command`（409，采纳的会话没记下启动命令，Restart 不知道重跑什么；前端对这类行禁用 Restart）、`already_registered`（409）、`read_only`（409，采纳 socket 上的会话拒绝写操作）、`bad_request`（400）、`runtime`（502）、`git`（502，`/api/projects/worktrees` 的 git 调用失败）、`database`（500）；一跳转发（本节点自己产生的，细节见「一跳转发」）：`peer_unreachable`（502）、`peer_fingerprint_mismatch`（502）、`peer_config`（502）、`peer_rejected`（所属节点拒绝终端 WS 升级时的原状态码）；新建 worktree：`worktree_exists`（409，同名 worktree 已登记）、`branch_exists`（409）、`path_exists`（409，目录已存在但不是 worktree）。`NoPendingDecision` 随 M1b 落地为 `no_pending_decision`。

`GET /api/projects` 每项是 `{ path, name, last_used_at }`，按最近使用排序（未用过的排在后面、按名字）；列表是 `project_roots` 的扫描结果与库里 `projects` 表的并集，目录已不存在的行在读取时删除。`last_used_at` 只在 `POST /api/sessions` 时更新——"最近使用"指的是起过会话。`GET /api/projects/worktrees` 每项是 `{ path, branch, head, main, locked }`，`branch` 去掉 `refs/heads/` 前缀、detached HEAD 为 null，第一项是主 worktree。`POST /api/projects/worktrees { path, name, base? }` 新建一个（MISSION §6.4「agora 只管"生"」，§1.4 Git GUI 边界的唯一例外；A44，agora-h1k.1）：`path` 与 GET 同一"已知项目"校验；`name` 既是目录名也是分支名，空、含 `/` 或 `..` 或空白、以 `-` / `.` 开头等 → 400 `bad_request`，在起 git 之前判；目标路径按 `worktree_root`（`docs/spec/config.md`）相对**主 worktree** 展开（从 linked worktree 发起也落到同一处）；`base` 缺省链：主 worktree 当前 checked-out 的分支 → detached 时 `git symbolic-ref refs/remotes/origin/HEAD` 指向的远端分支（`origin/main` 形态，直接作起点）→ 再兜底 `main`。冲突在起 git 之前按类型判、各 409：同名 worktree 已登记 `worktree_exists`、分支已存在 `branch_exists`、目录已存在 `path_exists`；git 自己失败（如 base 不存在）→ 502 `git`。成功 201，响应体就是 GET 会列出的那一项（`path` 是 git 登记的规范路径，`main: false`）。agora 不做合并与销毁：src/ 里的 git 子命令只允许 status / diff / rev-parse / symbolic-ref / worktree list / worktree add，守卫 `tests/arch_boundary.rs::git_subprocesses_are_read_only_or_worktree_add`；端点守卫 `tests/worktree_create.rs`。

每条会话的形态是 `sessions` 行的全部字段（其中 `ended_at` 是进程退出时刻，来源是**运行时报的**退出时刻——tmux 3.3+ 的 `pane_dead_time`；daemon 停机期间退出的会话，重启后 reconcile 补的也是它，不是 daemon 起来的时刻；运行时会话已经不在、或 Kill 那一刻运行时还没收集到退出时刻，才记 daemon 的当下并把 `ended_at_approximate` 置 true，之后运行时报了准确值就覆盖并清掉该标记；Restart 两者一起清空。A42，agora-h1k.4；守卫 `tests/session_manager.rs::ended_at_from_runtime_exit_time_after_daemon_restart`）+ 运行时实时事实（`name`、`alive`、`exit`、`pid`、`managed`）+ 状态判定（`status`、`source`、`confidence`、`reason`；四层来源仲裁见 ADR-002 D1，`confidence` 0–1 只供排障，UI 不显示）+ `detail`（hook 给的问题文本 / 正在用的工具 / 最后一条回复，就地回答与两行预览用）+ `pending_decision`（当前可答的挂起实例 `{ request_id, summary, epoch, host }`，`host` 是挂着的 hook 的宿主名（信封 `--host`），无活连接为 null；按钮展示 summary 并提交同一个 request_id；多个请求时取最新仍挂起的实例，解除后切换剩余实例）+ `respond_via`（`hook`：WAITING(decision) 可经 `input` 的 decision 回答；`terminal`：agent 的 hook 不能替用户批准，只能打开终端。有挂起时按 `pending_decision.host` 那个宿主的能力判定，与会话声明的 `agent_type` 无关——custom 类型跑着包了 claude 的脚本、采纳时类型猜错，hook 挂着就能答；没有挂起才按声明类型）+ `respond_within_secs`（`respond_via = hook` 时该宿主的挂起上限，秒；Claude 3300、Codex 20——Codex 挂起期间 TUI 不显示审批提示，超时 fail-open 交回终端；`terminal` 时 null）+ Dashboard 字段（MISSION §6.3；ADR-002 D8）：`prompt`（`❯`，最近 `prompt.submitted` 的首行）、`progress`（`↳`，`activity` 的当前工具或 `turn.ended` 的最后一条回复的首行）——都来自 hook，没有 hook 的会话两者为 null 而 `preview` 是 pane 末尾最后一个非空行（已 strip ANSI；有 hook 的会话只在沉默阈值后读 pane 用于 UNKNOWN 兜底，`preview` 为 null）；`status_since`（当前状态的起点，unix 秒，"waiting 3m"与同分排序用。来源是**事件自己的时刻**：hook 层的状态用投递箱文件名里的 ts——hook 进程写下事件那一刻——而不是 daemon 收到的时刻，所以 daemon 停机期间攒下的 WAITING / TURN_DONE 重放后"waiting 3m"从三分钟前算起，检查点恢复后也不变；进程层 / 文本层的状态用观测到变化的那个 tick；Dashboard 自己合成的事件（allow / deny 后的 `decision resolved`）是收到的时刻。A42，agora-h1k.4；守卫 `tests/hooks_replay.rs::status_since_uses_event_time_on_replay`）；`hooks_unheard`（装了 hook 的 agent 从没送来过一条事件、而终端在启动 10 s 宽限后已经活动了 `hooks.unheard_after` 以上时的一句提示，Codex 指向 TUI `/hooks` 信任、其余指向重装；进程退出或第一条事件到达即回 null；agora-dvh.15）；`task`（`task_ref` 像 beads id 且在会话工作目录里 `bd show --json` 查得到时的 `{ id, title, priority, status, acceptance }`，否则 null；`acceptance` 是 beads 的 acceptance_criteria 全文（多行原样），没写或只有空白为 null——侧栏行展开区据此显示 / 不占位（A40，agora-h1k.3，位置见 `docs/spec/ux.md`）；异步补齐，到了发 `session_updated`，按 `TaskIndex` 的 TTL（5 min）重查，在 beads 里改了验收标准几分钟内跟着变；agora 对 beads 零写入，`task` 的每个字段都只在内存缓存里、一个字不进 SQLite，库里只有 `task_ref`——不变量 12，守卫 `tests/task_info.rs::acceptance_is_read_not_stored`）。`task_ref` 为空的会话在首条 `prompt.submitted` 时被补成该 prompt 的首行（≤ 120 字），对话框里填的不动。宿主自己注入的 prompt（Claude Code 把后台任务完成通知 `<task-notification>`、`<system-reminder>`、斜杠命令回显 `<command-name>` 等也当 UserPromptSubmit 发出，2.1.261 真录 `testdata/claude/2.1.261/hooks/task_notification.jsonl`）是 `prompt.injected`：一轮照常开始，但既不补 `task_ref` 也不进 `prompt`、`progress` 不动；`task_ref` 已经是这类系统消息的库存行，下一条真人 prompt 允许覆盖一次（agora-3s5）；`id` 是全局 id `<node>:<id>`，本机 id 在 `local_id`，另带 `node`。`unregistered` 的每项是 `{ runtime_ref, name, title, alive, managed, working_directory, agent_hint, node }`，`agent_hint` 是从 pane 进程的后代里认出来的 adapter 名（认不出为 null），只是 Adopt 表单的默认值：`adopt` 请求里用户填的 `agent_type` 优先，没填才用 hint，都没有就是 `unknown`（MISSION §5.4）。未登记列表不走事件流，只随 `GET /api/sessions` 来；前端在 `session_removed` 与采纳之后重拉一次。

agora 没起过的会话经 hook 自己出现（§5.4，A16 / A22 合流）：无 `AGORA_SESSION_ID` 的事件按 `(host, agent_session_id)` 找已登记会话，找不到就登记——信封里的运行时环境（`TMUX` / `TMUX_PANE`）能在 agora 自己的 socket 或 `adopt_sockets` 上定位到 pane 时以 `origin = adopted` 登记（有终端与全部 respond，`agent_type` 就是 hook 的宿主）；定位不到的以 `origin = external` 登记（`runtime_ref` NULL，`display_name` 取工作目录名，状态 / 两行 / 通知与经 hook 的 allow / deny 照常，`text` → 409 `no_runtime`）。external 会话的 `alive` 看最近一条 hook 报来的 agent 进程号（Claude 的 `CLAUDE_PID`）`kill(pid, 0)` 的结果，进程没了报 FINISHED（没有退出码可分 FAILED），daemon 重启到下一条 hook 之前是 UNKNOWN；agent 没自报会话 id 的事件不登记。

## 认证（ADR-003）

- 每个请求先解析出一个 principal：`Human { device }`（cookie `agora_session`）或 `Peer { name }`（`Authorization: Bearer apt_<name>_…`）；两者互斥，Bearer 优先解析。未认证白名单只有 SPA 静态资源、`GET /api/health` 的公开子集、`POST /api/auth/pair`；其余一律 401 `unauthenticated`。**没有 loopback 例外**。
- 配对链接 `<origin>/#pair=<token>` 由 `agora open` / `agora url` / `agora pair`（经 unix socket）或已认证的 `POST /api/auth/pair/new` 铸造；256 位、单次、5 分钟。前端读 fragment 后 `POST /api/auth/pair`，再清掉 fragment。
- cookie `agora_session` 带 `Max-Age`，取值是 `auth.session_idle`（缺省 30 天）；服务端每次刷新 `last_seen_at`（每小时至多一次）时随响应重发一遍该 cookie，让浏览器侧的窗口跟着服务端一起滑动（ADR-003 D2）。
- Bearer 只在 TLS 监听器上被接受，明文监听器回 401 `bearer_requires_tls`——只要带了 `Authorization` 头就拒，连 scheme 都不看。实现是结构而不是检查：TLS 监听器给自己的 router 盖 `api::TlsListener` 请求扩展（`router(state).layer(Extension(TlsListener))`），明文监听器用裸 router 永远盖不上；扩展不是 HTTP 头，线上任何字节都变不成它（与进程内 fake 的 `InProcessPeer` 同一机制）。
- Bearer 的校验（ADR-003 D3；`src/auth/peer_token.rs`）：scheme 须为 `Bearer`（大小写不敏感）→ token 形态 `apt_<name>_<43 字符>` → 按 `<name>` 查 `peer_tokens` 一行 → 整串 SHA-256 常量时间比对 → 未吊销 → `Peer { name }`。任何一步失败对外都是 401 `unauthenticated`，不区分"没签过 / 不匹配 / 已吊销"，原因只进日志（已吊销的 token 再出现记 warn）。没有签发过任何 token 的节点因此拒绝一切 Bearer（A31）；吊销即时——每次请求查库，不缓存。`last_used_at` 每小时至多写一次。peer 不是浏览器：不做 cookie 续期，也不做下一条的 CSRF 同源校验。守卫 `tests/peer_token.rs::no_token_issued_rejects_all_bearer`、`::bearer_rejected_on_plaintext_listener`、`::revoked_token_rejected_immediately`、`::plaintext_never_stored`、`::rotate_invalidates_old_token`。签发 / 吊销 / 轮换的 CLI 与 `token_file` 见 `docs/spec/config.md`「机器 token 文件」。
- cookie 认证的非 GET 请求必须带同源 `Origin`（或 `Sec-Fetch-Site: same-origin`），两者都没有 → 403 `cross_origin`（curl 调写端点要自己带 Origin）；WS 升级校验 `Origin` 与 `Host` 同源；Bearer 跳过。
- `/api/auth/*` 除 `logout` 外（`POST pair/new`、`GET devices`、`DELETE devices/:id`）只接受 `Human`：`Peer` → 403 `peer_forbidden`，且不产生任何副作用（ADR-003 D1 例外句，依据 D3 "peer 没有委托链"——配对链接是持久化的提权，吊销 peer token 后配出来的设备还在）。这是"任一 principal 全权"的唯一例外；`/api/sessions/*`（含 `input`）对 Peer 保持全权，一跳转发要靠它们。守卫 `tests/auth.rs::peer_cannot_mint_pair_link_or_touch_devices`。
- Kill / Restart 带 `confirmed`；所属节点判断需要确认而未确认 → 错误类型 `NeedsConfirmation`；转发节点原样转发 `confirmed`。

会话 id 一律 `<node>:<id>`；`GET /api/sessions` 同列本机与 peer 会话，每条带 `node` 字段，对 peer 会话的写操作与终端流经一跳转发（原则与调用方、API 版本、DELETE ≠ kill 见 MISSION §7.3）。路径里的 `:id` 接受全局 id，也接受裸的本机 id（curl 手敲时少打一段）；节点前缀是已配置的 peer → 读从并入视图来（「peer 视图」）、写操作与终端流经一跳转发（「一跳转发」）；既不是本机也不是 peer → `node_unknown`。

每条 `/api/` 请求写一行结构化日志：方法、路径、状态、耗时、principal（未认证请求该栏为空）；不记请求体（MISSION §10.2）。

## api_version 兼容规则（MISSION §7.3）

```
GET /api/system
→ { "api_version": { "major": 1, "minor": 0 },   // API 形态的版本；与二进制版本无关
    "version": "0.1.0",                          // 二进制（crate）版本，只给人看，程序不据此判断
    "node": "mac" }
```

每个节点各自原地升级，N 个节点的 `api_version` 会不一致（MISSION §2.3 规则 10）；节点的 API 有两种调用方——浏览器页面与 peer 节点——两者都在读任何业务数据之前先比版本，不兼容就提示或降级，**绝不静默错读**。

**递增规则**（改形态与改版本号在同一个 commit；形态变更记在本文对应端点的小节）：

- **major +1**（minor 归 0）：任何会让老调用方错读的改动——删字段、改字段类型或语义、改端点路径 / 方法 / 状态码、改错误类型名、改 WS 消息形态、改会话 id 形态。
- **minor +1**：只增不删——新字段、新端点、新错误类型、新 WS 消息类型、新的可选请求参数。老调用方忽略不认识的字段与消息类型照常工作；新调用方对老节点缺失的字段按"没有"处理，不按错误。
- 首个版本 `1.0`；`api_version` 从这条规则落地起就是 `{ major, minor }` 对象，此前二进制回的裸整数 `1` 不再被任何调用方接受。
- `1.1`（2026-09-06，第一波第二批合入时集成者统一 bump 一次）：只增字段——session 形态加 `ended_at_approximate`（agora-h1k.4）、task 形态加 `acceptance`（agora-h1k.3）、health 加 `peers` 段（agora-7ku.12）；`/api/system` 本身不变。多条分支同时改字段时不各自 bump，合入一批后由集成者统一加一次 minor，避免撞车。
- `1.2`（2026-09-06，第三批合入时集成者统一 bump 一次）：只增——peer 会话行加 `stale`（agora-7ku.5，本机行没有此键）；新端点 `POST /api/projects/worktrees` 与错误类型 `worktree_exists` / `branch_exists` / `path_exists`（agora-h1k.1）；错误类型 `peer_forbidden`（agora-0df）与一跳转发的 `peer_unreachable` / `peer_fingerprint_mismatch` / `peer_config` / `peer_rejected`（agora-7ku.7）；`/api/system` 本身不变。

**兼容判定**（`agora::api::version`，`check(ours, theirs)`；结论是枚举，按类型分类、不做字符串匹配）：

- major 相同 → **兼容**：`Compatibility::Same` / `PeerNewer` / `PeerOlder` 三档只区分 minor 的高低，用于日志与提示——对方 minor 更大，多出来的字段本方看不懂、忽略；对方 minor 更小，本方要的字段可能缺、按缺省处理。**不得因为 minor 不同拒绝对话**，否则滚动升级期间整个集群互相看不见。
- major 不同 → **不兼容** `Incompatible::MajorMismatch { ours, theirs }`：不读对方任何业务数据。
- `api_version` 缺失、不是 `{ major, minor }` 两个非负整数（含旧二进制的裸整数） → **不兼容** `Incompatible::Unreadable`：对方不是本协议的节点或早于本规则，同样不读。`negotiate(ours, body)` 从 `GET /api/system` 的原始 JSON 一步得到上面两类结论。

**调用方的义务**：

- 浏览器（`web/src/health.ts`）：页面带着构建时的 `API_VERSION`（与 `agora::api::version::API_VERSION` 同步改，单测钉住两边一致），在 `/api/events` 每次连上（启动的首连、断流后的重连）时读一次 `/api/system`——升级节点必然重启 daemon、WS 必然断一次，所以换代总能在重连时被看见，不必轮询。不兼容 → 主区顶部横幅「节点 API 版本 X，页面按 Y 构建，请刷新页面 / 升级节点」（复用 agora-bgr 的 runtime-degraded 横幅位置与样式，不做第二套），侧栏、Tabs、终端一概不渲染；事件流照常保持连接，下一次重连版本对上了页面自己恢复。拉不到 `/api/system`（网络 / 5xx / 401）沿用上一次结论，不在"不兼容"与"正常"之间闪。
- peer 客户端（agora-7ku.5）：连 peer 的第一步就是 `GET /api/system` 过 `negotiate`；不兼容 → 该 peer 标 `incompatible_version`（每 peer 状态模型见 agora-7ku.12），不拉 `/api/sessions`、不并入视图、不转发写操作，按退避照常重试——对方升级到同 major 后自愈，不需要人干预。
- 两边都只按 major 决定"读不读"，minor 只影响提示。

## WebSocket

```
WS /api/sessions/:id/terminal    # 终端流
WS /api/events                   # 全局事件：status change / session created / session removed / notification
```

终端流 Client → Server：`{ "type": "input", "data" }`、`{ "type": "resize", "cols", "rows" }`、`{ "type": "ping" }`

终端流 Server → Client：`{ "type": "output", "data" }`、`{ "type": "status", "status": "attached" }`、`{ "type": "exit", "exit": { "kind": "code", "value": n } | { "kind": "signal", "value": "hup" } }`（与 `GET /api/sessions` 里 `exit` 字段同一形态，ADR-001 的 `Exit`）、`{ "type": "pong" }`。`exit` 只说明这一条 attach 流结束了，不代表会话或 agent 退出。升级 URL 可带 `?cols=&rows=` 作为初始尺寸（缺省 160×48），之后由 `resize` 消息调整；多客户端同看时由运行时仲裁尺寸。keepalive：服务端每 20 s 发 WS Ping，65 s 内没有任何入站帧就断开这一条 attach（会话不受影响）。断开时给 attach 进程 SIGHUP，确认其退出后才释放 PTY（ADR-001 D5）。

事件流 Server → Client 每帧一个 JSON **数组**（服务端 ~50 ms 合并突发，同一会话的连续状态变化只留最后一条）：`{ "type": "session_created", "id", "session" }`、`{ "type": "session_removed", "id" }`、`{ "type": "session_updated", "id", "session" }`（metadata 或 pending_decision 改了；整行重发）、`{ "type": "status_changed", "id", "status", "source", "reason", "alive", "detail", "prompt", "progress", "preview", "status_since", "hooks_unheard" }`（`detail` / 两行预览 / pane 预览 / `hooks_unheard` 变了也算状态变化；`task` 的到达走 `session_updated`）、`{ "type": "decision_resolved", "id", "tool_use_id", "via": "dashboard" | "terminal" | "session" | "exit" | "timeout" }`（挂起的决定被解除，ADR-002 D5）、`{ "type": "notification", "id", "title", "body", "status" }`（浏览器通知，MISSION §6.6 / A18：只在 RUNNING（或 IDLE）→ WAITING / TURN_DONE / FINISHED / FAILED 四种转换上各发一条，紧跟该会话的 `status_changed` 或 `session_updated` 之后；`title` 是 §6.6 表的文案 `<Agent> / <name> @ <node> needs input | finished its turn | finished | failed`，`body` 是 `detail` 首行，`status` 是转换后的状态，前端据此决定点击落到哪——WAITING / TURN_DONE 落到 Dashboard 就地回答区；`notifications.enabled = false` 时一条不发；RUNNING → IDLE / UNKNOWN、STARTING → FAILED、TURN_DONE → FINISHED、用户自己 Kill 的不通知）、`{ "type": "resync" }`（服务端丢过该客户端的事件，必须重拉全量）。Client → Server 只有 `{ "type": "ping" }` → `{ "type": "pong" }`。进程状态的变化由 daemon 按 `status.detector_interval` 轮询 Session Manager 求差发出；API 自己做的增删立即发。两个 WS 升级都先过 principal，再校验 `Origin` 与 `Host` 同源（403 `cross_origin`）。

MVP 用 JSON / Text WebSocket 足够；binary terminal frames 放到 V2。

客户端消费 `/api/events` 的纪律：就地 patch、合并突发（~300 ms 重同步）、内容相等不重渲染；不得每事件全量刷新，不得回退为轮询；断流重连后拉全量快照对齐（与 peer 链路同一"快照 + 增量"模式，MISSION §3.5）。

## respond 的两种语义（MISSION §7.3）

- `POST /api/sessions/:id/restart` 的命令由 Adapter 按 `(agent 版本, agent_session_id)` 生成 resume 参数（ADR-002 D7）：版本来自 `<program> --version` 的探测（缓存：可用记到 daemon 重启，不可用 60 s 后重探）；库里的 `command` 不动，只这一代的启动命令换成 resume 形态，上一代的 resume / pin flag 先拆掉不叠加。响应在会话形态之外多一个 `restart`：`{ "resumed": true, "agent_session_id" }` 或 `{ "resumed": false, "reason" }`（没自报过 id、版本表外不猜参数、没有 Adapter、命令不在 PATH——退化为原命令且说明原因，绝不静默、绝不 `--continue` / `--last`；`reason` 只有首行、≤ 120 字符、截断加 `…`——探版本时子进程吐的整段 stderr 留在 daemon 日志里，不进 UI，agora-k9r）。
- `POST /api/sessions` 在 Adapter 支持且版本可解析时给命令加钉 id 的参数（Claude `--session-id <uuid>`），并把它写进 `agent_session_id`；agent 经 hook 自报的 id 每次命中都覆盖它（识别顺序：自报 > 钉死 > 用户挑，D7）。用户命令里已经写了 resume / pin 的 flag 就不钉。
- `POST /api/sessions/:id/input` 接受 `{ "kind": "decision", "decision": "allow" | "deny", "message"?, "request_id"?, "tool_use_id"? }` 或 `{ "kind": "text", "data": "..." }`。选择题（AskUserQuestion 类）V1 不经 API：Dashboard 只显示问题与"打开终端"（ADR-002 D5）。
- `decision` 只对有挂起 hook 决定的会话有效（否则 409 `no_pending_decision`）：agora 的 PermissionRequest hook 挂起等待，答复经 hook 返回给 agent，不注入键击；Dashboard 必须带展示的 `pending_decision.request_id`：旧 ID、已解除 ID 或旧 epoch ID 返回 409，不能批准其它请求；工具名复用时新挂起也有新 ID。检查点写入失败返回 500 `hook_state`，不发送批准，恢复存储后可用同一个 request_id 重试。兼容命令行调用：未带 request_id 时仍可按 tool_use_id 选择，`tool_use_id` 缺省答最早登记的那个（Claude 2.1.260 的 PermissionRequest 实测不带 tool_use_id，键是 `tool_name`）；成功返回 `{ "tool_use_id" }`，该请求立即解除，全部待回答工具都解除后行才退出 WAITING，并发 `decision_resolved` via=dashboard。`respond_via = terminal` 的 agent（Grok）只提供"打开终端"。挂起上限与超时见下文（ADR-002 D5）；Codex 的挂起只有 `respond_within_secs`（20 s）。
- `text` 经 PTY 写入（运行时的 send-keys：字面文本一次、尾部换行作回车键单独一次——Claude 的 TUI 把两者一起到达当成粘贴不提交）；无 hook 的 agent、自由问答、TURN_DONE 的下一条指令走这条；external 会话没有运行时句柄 → 409 `no_runtime`。

## hook 投递（ADR-002 D3）

- **没有 HTTP 端点**。agent 的 hook 命令是 `<AGORA_HOME>/bin/agora hook --host <agent> --home <AGORA_HOME>`（安装写入的完整形态见 ADR-002 D4）：payload 落到 `<AGORA_HOME>/hooks/inbox/<host>/<agent_session_id>/<ts>-<seq>.json`（先 `.part` 再 rename），再经 `<AGORA_HOME>/agora.sock`（unix socket，仅属主可访问 + 对端 uid 校验，ADR-003 D6）唤醒 daemon；daemon 不在时文件留着，启动时按文件名顺序重放（MISSION §3.4）；应用后先将 hook 观测原子写入 `<AGORA_HOME>/hooks/state/` 的版本化每会话检查点（0700 目录、0600 文件），成功才移到 `<AGORA_HOME>/hooks/done/` 保留 24 h 排障；启动先恢复检查点再消费更新的 inbox，旧版无检查点时从保留的 done 补建。检查点含 epoch 与已处理文件名以去重，不含 alive / exit 或挂起连接，不影响运行时作为存活真相源；恢复 WAITING 后 pending_decision 为 null，不能再向已断开的 hook 发送批准；信封里 `AGORA_EPOCH` 小于会话当前 epoch 的事件丢弃（Restart 之前那代进程发的）；`hooks/` 下任何一级目录属主不对或 group / other 有位，daemon 拒绝读并记日志（agent 照跑）。
- 单实例：daemon 启动最先绑 TCP 监听器、再绑 `agora.sock`（绑前先 connect 探活，活的不 unlink），两个都成功才碰运行时 / 库 / reconcile / 投递箱重放，"daemon 就绪"在两个 listening 之后；同一 `AGORA_HOME` 的第二个实例在任一步失败都以非 0 退出并在 stderr 说"已有实例在跑"，不动活实例的任何东西。退出只删自己绑的那个 socket 文件（绑定时记 dev+inode，删前比对），`process::exit` 留下的残留文件下次启动当陈旧文件重绑（agora-apr）。
- 安装：`agora hooks install|uninstall <agent> [--dry-run]` 写用户自己的 agent 配置（Claude `~/.claude/settings.json` 的 `hooks`、Codex `~/.codex/hooks.json`、Grok `~/.grok/hooks/agora.json`），装前把 diff 打到 stderr，`--dry-run` 只看不写；条目以 `<AGORA_HOME>/bin/agora hook` 为自己的标记，重复装不重复、卸载只删自己的、别人的条目与其它键原样；`install` 顺手把 `<AGORA_HOME>/bin/agora` 指向当前二进制。每个事件的 timeout 由 Adapter 的 `install_spec` 给（Claude：PermissionRequest 3600 s、SessionEnd 1 s、其余 20 s；Codex：PermissionRequest 60 s、SessionEnd / Interrupt 3 s）。装完由 Adapter 的 `install_hint` 再说一句：Codex 未在 TUI `/hooks` 信任的 hook 会被静默跳过，条目内容就是信任哈希的输入，所以重复装是 noop、升级不用重做。信封里的 agent 环境变量只收 `CLAUDE_*` / `CODEX_*` / `GROK_*`，名字含 TOKEN / SECRET 之类的不落盘。
- 录制与冒烟（ADR-002 D10；agora-3la.1 / agora-3la.2）：`agora hook … --record <file>`（或环境变量 `AGORA_HOOK_RECORD`）在投递之前把脱敏后的 payload 追加到 fixture 文件（行格式、脱敏规则与录法见 `testdata/README.md`），录制失败只写 stderr、不影响投递；`cargo test --test hook_smoke -- --ignored` 对本机装有的每个一等 agent 跑一次 `-p` 无头最小交互（临时 HOME 隔离用户配置、只借登录凭据，事件投到测试用的临时 AGORA_HOME），经真实 hook 路径收到的 SessionStart 与 Stop 顶层键集合须与 `testdata/<agent>/<version>/hooks/headless.jsonl` 一致，版本不在表里或键集合漂移即红并提示 `AGORA_SMOKE_RECORD=1` 录新 fixture；没装的 agent 跳过，CI 每次都跑。
- 需要答复的事件（Claude Code `PermissionRequest`）由 hook 进程在 socket 上挂起等 daemon 回 allow / deny / none；none、超时、socket 断都是 fail-open（exit 0 不输出，TUI 的提示仍在）。挂起上限每会话 8、节点 256，超时按宿主（`AgentHooks::hold_timeout`）：Claude 55 min，Codex 20 s（挂起期间 Codex 的 TUI 不显示审批提示，只能短）。
- `/api/events` 的 `decision_resolved`（挂起被 Dashboard / 终端答复、本轮结束、进程退出或超时解除）形态见上文。

## Health（MISSION §10.3）

未认证只返回 `{ "status": "ok" }`；下面的完整形态需要 principal。

```
GET /api/health
→ { "status": "ok",
    "runtime": { "status": "ok" | "degraded", "reason": null, "path_source": "shell" | "daemon" },   // ADR-001 D7；status/reason 每次请求现算，运行时恢复后自动转回 ok
    "database": true,
    "tls": "external", "push": { "apple": true, "fcm": false },
    "peers": { "mac": { "online": false, "last_seen": "2026-09-02T23:10:00Z", "retrying": true, "last_error": "fingerprint_mismatch" } } }
```

`peers`（agora-7ku.12；模型在 `src/peer/state.rs`）：键是 `peers[].name`，配置了的 peer 从启动起就在（还没连上：`online: false`、`last_seen: null`、`retrying: false`、`last_error: null`）。`last_seen` 是上次成功交互的时刻，**本节点时钟**打的 UTC 文本（ADR-004：不信 peer 报的时间），离线后保留——这就是 stale 行的"上次见到"（不变量 8）。`retrying` 表示下一次重试已排定（退避 1 s 起步、30 s 封顶、永不放弃，参数见 `docs/spec/architecture.md`）。`last_error` 是最后一次失败的**类型**，只有四个值：`incompatible_version`（对方 `api_version` 不兼容，A33）、`fingerprint_mismatch`（证书 SPKI 指纹不符）、`unauthorized`（对方 401）、`unreachable`（拒绝 / 5 s 超时 / DNS 失败）；在线时为 `null`。前端只按这四个值渲染原因文案，不解析任何消息文本（MISSION §2.3 规则 10）。守卫 `tests/health.rs::health_peers_section_reports_each_peer_state_by_type`。

前端对 `runtime` 段的消费（agora-bgr）：配对之后 Workspace 带 cookie 拉完整形态（`web/src/health.ts` 的 `HealthWatcher`），`runtime.status = degraded` 时在主区顶部给一条横幅——`reason` 原文 + "会话状态暂不可知，进程没有被杀"——恢复后自动消失。这是前端唯一的一处轮询：degraded 是服务端每次请求现算的结论、没有事件推它，健康时 60 s 一次、degraded 期间 10 s 一次，拉不到就沿用上一次的结论不闪。未认证的门页只用公开子集判在线 / 不可达，公开子集没有 `runtime` 段、也不算 degraded（ADR-003 D1：`tests/health.rs` 守卫公开子集只有 `status` 一个键）。

## peer 视图（MISSION §3.5；agora-7ku.5）

节点是 peer 的 API 客户端，用的就是上面这套 API、守上面「消费纪律」那几条（`src/peer/client.rs`，每个配置了的 peer 一个任务）：

1. 连上先 `GET /api/system` 过 `negotiate`（「api_version 兼容规则」一节）：不兼容 → 该 peer 标 `incompatible_version`，不拉、不并入，退避照常重试；对方 `node` 与 `peers[].name` 不一致只 warn（它的行会被下面的一跳规则全部丢掉——这是配置写错了名字）。
2. 兼容 → 先建 `WS /api/events`、再 `GET /api/sessions` 全量（先流后快照，与浏览器一样），之后逐帧应用；帧里任一条 `resync` 就整帧应用完再重拉一次全量。keepalive 与终端流同一套数字：每 20 s 发 `{"type":"ping"}`，65 s 没有入站帧当断开。
3. 断线 → 行**保留**、标 stale 并写上"上次见到"，`/api/health` 的 peers 段记类型，1 s 起 30 s 顶永不放弃地重连（`docs/spec/architecture.md`「peer 重连退避」）；人点开 stale 的会话就插一次重试（下面「stale」那条）。

**并入规则**（`src/peer/view.rs` 的 `PeerViews`，挂在 `AppState.peer_views`；每 peer 一份 `{ rows: BTreeMap<全局 id, 行>, stale, last_seen }`）：

- **一跳**：只收 `node == peers[].name` 且 `id` 以 `<name>:` 开头的行，其它一律丢弃并 warn。本节点回答 Peer principal 的 `GET /api/sessions` 时也只给本机行（`unregistered` 也只有本机的）——两边各守一半，A 经 B 看不到 C，环上也不会滚雪球。守卫 `tests/peer_view.rs::only_local_sessions_are_exported_one_hop`。
- **时间是本节点时钟**（MISSION §3.5；ADR-004）：peer 行的 `status_since` 改写成**本节点第一次看见该行处于当前状态**的时刻，peer 报的 `(status, status_since)` 只当"状态没变"的 token——重连后同一状态不会重置成 0 分钟；`last_seen` 与它用同一只表。`created_at` / `ended_at` 之类的 metadata 保留 peer 的原值。守卫 `tests/peer_view.rs::peer_timestamps_use_local_clock`（注入固定时钟）。
- **`stale` 字段只出现在 peer 行，`last_seen` 只出现在 stale 的 peer 行**（agora-7ku.6）：本机行两个键都没有；peer 在线 `stale: false`、没有 `last_seen`；断线后每行 `stale: true` 且 `last_seen` 是该 peer 的"上次见到"——与 `/api/health` peers 段的 `last_seen` **同一个值**（`PeerState::last_seen`，本节点时钟打的 `YYYY-MM-DDTHH:MM:SSZ`），不是另一只表；重连拉到全量即两个都清（`last_seen` 键消失）。peer 报的行里就算夹带 `last_seen` 也剥掉。两次翻转都发 `session_updated`（断线时带 `stale: true` + `last_seen`，恢复时 `stale: false`、无 `last_seen`），浏览器不用轮询。**点开即重试**：Human 对 stale peer 的会话做任何事——`GET /api/sessions/:id`（`sessions::get`）、建终端 WS 或六个写操作（都经 `forward::hop`）——节点就调一次 `PeerViews::retry_now(peer)` 缩短客户端这一次退避等待；在线时无事，一次点开只多一次尝试，不是轮询；不加端点、浏览器不用多发请求。守卫 `tests/peer_view.rs::disconnect_keeps_rows_marked_stale`；`tests/peer_stale.rs::offline_peer_is_stale_with_last_seen_not_removed`、`::opening_stale_peer_triggers_immediate_retry`、`::recovery_resyncs_full_snapshot`、`::reconnect_backoff_is_capped_and_never_gives_up`、`::local_sessions_unaffected_by_broken_peer`（不变量 8 的 fake 版，A36）。
- **事件原样出本机总线**：peer 的 `session_created` / `session_updated` / `session_removed` / `decision_resolved` / `notification` 经视图改写后发进本机 `/api/events`（`status_changed` 以整行 `session_updated` 出去——行上的时间已改写），浏览器只连本机一条流。全量快照与本地视图差分成 created / updated / removed。守卫 `tests/peer_view.rs::peer_sessions_are_merged_with_node_label`、`incompatible_peer_is_flagged_not_merged`。
- `GET /api/sessions/:id` 的 `<node>` 是已配置 peer → Human 从并入视图读（404 `not_found` 表示视图里没有），Peer principal 问 peer 的会话仍是 `node_unknown`（一跳）。对 peer 会话的写操作与终端流是另一条链路（一跳转发，agora-7ku.7）。

前端：侧栏行的 `@ <node>` 只给 `node ≠ 本机` 的行，本机 id 取 `/api/system` 的 `node`（`web/src/health.ts` 的 `VersionWatcher` 同一次拉取带出，不另起轮询），Header 本机那一枚也叫这个名字（`docs/spec/ux.md`）。stale 行直接读行上的 `last_seen` 显示「○ 上次见到 HH:MM」并整行淡显（`web/src/SessionRow.tsx`，`docs/spec/ux.md`「行上的 `@ <node>`」段）；点开 stale 行前端不做任何事，重试由节点在 `hop` / `get` 里插。`api_version`：`stale`（agora-7ku.5）随 1.2 已 bump；`last_seen`（agora-7ku.6，只增）由本批集成者统一 bump minor，本任务不动它。

## 一跳转发（ADR-003 D8、ADR-004；agora-7ku.7）

写操作与终端流按会话 id 的节点前缀路由（MISSION §3.5 / §7.3），实现在 `src/api/forward.rs`；`POST /api/sessions/:id/{kill,restart,cleanup,input}`、`PATCH` / `DELETE /api/sessions/:id` 与 `WS /api/sessions/:id/terminal` 七个端点各在入口查一次 `state.registry.route(<node>)`：

| 前缀 | 去向 |
|---|---|
| 本机 `node.id`，或裸 id（无前缀） | 本地 handler，行为与单节点时相同 |
| 已配置的 peer | 同方法、同路径、同 body 经该 peer 的 `PeerTransport` 送到所属节点，响应**原样**回（状态码、`content-type`、body）；所属节点的每一种判定（409 `needs_confirmation` / `no_pending_decision` / `still_alive`、404 `not_found`、400 `bad_request`、200 + `restart` 字段……）都是它说的，本节点不改写、不补充 |
| 既不是本机也不是 peer | 404 `node_unknown`，不发任何请求 |

**一跳**（ADR-004）：请求本身的 principal 是 `Peer` 时，非本机前缀一律 404 `node_unknown`——B 收到 A 对 `c:<id>` 的请求就到此为止，哪怕 B 自己配了 C；A→B→C 在结构上不存在，环也就不存在。peer 对本机会话的操作照常（这条规则拦的是"再转"，不是 peer 本身）。守卫 `tests/forward.rs::second_hop_is_refused_at_the_middle_node`、`::unknown_node_is_rejected`。

**确认在所属节点**（ADR-003 D8；MISSION §8）：`confirmed` 随 body 原样过去——不带就是不带、`false` 就是 `false`，转发节点没有任何一条代码路径能把它置 `true`；所属节点按**自己**会话的 agent 状态判断，409 `needs_confirmation` 原样回到浏览器，浏览器弹框后带 `confirmed: true` 重发，再转一次。所属节点看到的调用方是 `Peer { name }`，peer 不享有免确认。守卫 `tests/forward.rs::kill_confirmation_enforced_at_owner`。

**请求上带什么**：只有 `content-type: application/json`（有 body 时）与 body。浏览器的 cookie / `Origin` / `Host` / `Authorization` / `Content-Length` 一个都不转——Bearer 由 `HttpsTransport` 自己加（ADR-003 D3），主机与 TLS 归 transport（D4）。转发的 body 是本节点解析后再序列化的形态（写端点的 body 都是本节点自己定义的小结构，页面又是本节点发的，不会带本节点不认识的字段）。

**终端流**：先向所属节点建 `WS /api/sessions/:id/terminal?<原查询串>`，成功才升级浏览器这条；之后两边帧**原样**互转——`output` / `input` / `resize` / `status` / `exit` / `ping` / `pong` 的 JSON 帧与 WS 层的 Ping / Pong / Close 都不解释、不合并，所属节点的 20 s Ping 与 65 s idle 经本节点到达浏览器，活性判断仍是端到端的。任一侧断（关闭、错误、流结束）就关另一侧；`exit` 帧转过去后本节点主动关两边（所属节点在 exit 之后还要等 attach 退出最多 3 s 才关，浏览器不必陪它等）。所属节点在升级前的拒绝（会话不存在、没有运行时、认证失败）以 HTTP 状态回到浏览器，不建 WS。守卫 `tests/forward.rs::terminal_ws_forwarded_bidirectionally`。

**转发失败的错误类型**（本节点自己产生的；所属节点的非 2xx 不在此列，它们原样回）：

| type | 状态 | 何时 |
|---|---|---|
| `node_unknown` | 404 | 前缀既不是本机也不是 peer；或请求来自 peer 且前缀不是本机（一跳） |
| `peer_unreachable` | 502 | 连不上 / 5 s 内没有应答 / 线上协议错误（`TransportError::Unreachable` / `Timeout` / `Protocol`） |
| `peer_fingerprint_mismatch` | 502 | 对端证书 SPKI 指纹与 `peers[].cert_fingerprint` 不符（ADR-003 D4：独立类型，绝不并进"不可达"） |
| `peer_config` | 502 | 本节点这一行 `peers[]` 字面上就用不了（URL 不是 https、指纹格式不对、token_file 读不了 / 权限过宽）或 TLS 客户端建不起来 |
| `peer_rejected` | 所属节点的状态码 | 只在终端 WS：所属节点以非 101 拒绝了升级；WS 握手失败没有 body，只有状态码可传 |

守卫 `tests/forward.rs::unreachable_peer_is_reported_by_type`。前端按 `type` 分支、不解析 `message`（MISSION §2.3 规则 10）。
