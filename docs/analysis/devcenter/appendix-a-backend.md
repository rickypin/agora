# 附录 A：后端核心代码级评审

> 对象：`~/code/devcenter` v0.5.1（commit `1540ac4`，2026-08-28）。
> 范围：`src/{main,cli,config,storage,tmux,procs,which,session,status,observer,gateway,protocol,ws_proxy,auth,net,server}.rs`、`src/agent/*`、`scripts/fake-agent`、`testdata/`、核心集成测试。
> 本附录是分领域评审的原始结论，综合评价见 [README.md](README.md)。行号以上述 commit 为准。

## 1. 模块地图

| 模块 | 职责 | 关键类型 | 依赖 |
|---|---|---|---|
| `src/main.rs` | 启动编排：parse CLI → 初始化 tracing → load config → open Storage → 构造 `AppState` → spawn observer / federation / tunnel → axum serve | — | cli, config, storage, tmux, server |
| `src/config.rs` | YAML 配置，`deny_unknown_fields`，内置 agent 表（含 yolo flag、`PaneKind`） | `Config`, `AgentConfig`, `PaneKind` | hosts |
| `src/storage.rs` | rusqlite 封装，`Mutex<Connection>`；sessions / session_tabs / session_tags / preferences / auth_tokens | `Storage`, `SessionRecord`, `TabRecord` | 无 |
| `src/tmux.rs` | tmux CLI 薄封装，全部 argv 数组、`-L socket` 隔离 | `Tmux`, `PaneInfo` | 无 |
| `src/procs.rs` | 一次 `ps -axo` 快照、process tree、env 读取 | `Proc` | 无 |
| `src/which.rs` | 按 tmux server 的 PATH 解析二进制、`--help` 探针、uuid v4 | — | 无 |
| `src/session.rs` | **核心**：create/adopt/list/kill/delete/relaunch、conversation id 三级来源、命令 instrument | `SessionManager`, `SessionView`, `NewSessionSpec` | tmux, storage, agent, procs, which, browser |
| `src/agent/mod.rs` | `AgentAdapter` trait + 注册表 + 通用工具（strip_ansi、preview、relaunch_command、transcript 搜索） | `AgentAdapter`, `AgentState`, `DetectionResult`, `Recap` | 各 adapter |
| `src/agent/{claude,codex,pi,generic,fake,browser}.rs` | 各 agent 的检测规则、transcript 布局、resume 语义 | 各 `*Adapter` | mod |
| `src/status.rs` | 决策顺序引擎（dead pane → starting → adapter → activity） | `StatusEngine`, `Observation` | agent |
| `src/observer.rs` | 独立线程周期 tick；`StatusStore`（`RwLock<HashMap>`）；`Event` broadcast | `StatusStore`, `SessionStatus`, `Event` | session, status, agent |
| `src/gateway.rs` | WS ↔ PTY(`tmux attach -d`) 桥 | `Utf8Stream`, `Keepalive` | tmux, protocol |
| `src/protocol.rs` | JSON 帧协议 | `ClientMessage`, `ServerMessage` | 无 |
| `src/ws_proxy.rs` | hub 侧 WS 透传到 worknode | — | 无 |
| `src/auth.rs` | TOTP(RFC6238)、token hash、`LoginLimiter` | `LoginLimiter` | 无 |
| `src/net.rs` | URL 解析 + SSRF 判定（viewer 用） | `Target`, `AddressVerdict` | 无 |
| `src/server.rs` | axum 路由、中间件、全部 handler、**跨主机 move 编排**（~3000 行） | `AppState`, `ApiError` | 几乎所有模块 |

调用方向是干净的单向：`server → session → {tmux, storage, agent}`；`observer → session → tmux`；`gateway → tmux`。agent 子模块不依赖 session/storage。唯一"反向"是 `session.rs` 调 `crate::browser::*`（ADR-012 引入）。

## 2. 关键机制的实际实现

### 2a. Session 生命周期

**命名与创建**：`generate_id` 用纳秒×常量混合出 24-bit hex（`session.rs:720`），碰撞检查同时查 SQLite 与 tmux；tmux 名 = `prefix + id`（默认 `dc-xxxxxx`）。`Tmux::new_session`（`tmux.rs:116`）把 `new-session -d -s name [-e LANG=...] [-c cwd] cmd ; set-option remain-on-exit on ; set-option status off` 合并成**一次** tmux 调用——注释解释了原因：命令秒退时第二次调用会找不到 session。`-e LANG` 是为 launchd/systemd 起的无 locale 守护进程补 UTF-8（`tmux.rs:293-320`）。

`create()`（`session.rs:773`）顺序：校验 name/tags → 解析命令 → `apply_yolo` → `pin_session_id`（对 claude 探 `--help` 后追加 `--session-id <uuid>`）→ 智能破折号检查 → 解析 parent → 校验 cwd 绝对且存在 → **先 tmux 后 SQLite**，SQLite 失败则回滚杀 tmux（`session.rs:911`）。运行的命令是 `instrument()` 后的（追加 `--settings '{hooks…}'` 让 Claude 把 session_id 写到 report 文件），但**存库的是未 instrument 的命令**（`session.rs:440-470` 注释解释了不存的原因：路径不可移植）。

**discover / reconcile**：MISSION §3.5 描述的"discover → read SQLite → reconcile → rebuild"并没有一个显式启动步骤；实际是 `list()`（`session.rs:1035`）每次调用做一次合并：SQLite 全表 ⨝ `tmux list-sessions` 集合，注册行标 `alive = tmux 有该 session`，未注册的 tmux session 附带 `agent_hint`（`hint_for` → `pane_pid` + ps 树，`session.rs:987`）。运行时状态（`StatusStore`）完全靠 observer 从零重建。注意 `alive` 的语义是"tmux session 存在"，由于 remain-on-exit，进程已退出的 FINISHED 会话 `alive` 仍为 true——`server.rs:1895-1920` 的 `PaneTail` 注释承认了这个陷阱。

**adopt**（`session.rs:930`）：仅校验 tmux 存在且未注册，记 `command: None, adopted: true`，cwd 从 `#{pane_current_path}` 取。

**kill vs delete**：`kill`（`:1115`）= `tmux kill-session`，保留行；`delete_metadata`（`:1170`）= 删行 + 删 report 文件 + 删 browser profile，不碰 tmux；唯一拒绝是"还在跑的 browser"。`kill_unregistered`（`:1146`）按 tmux 名杀无行会话，拒绝已注册名。

**restart**：`server.rs:4152` `restart_session` = 先检查 move_state 不在 past_no_return → `kill`（"is not running" 字符串匹配容忍）→ `relaunch`（`session.rs:564`）。relaunch 复用**同一 id 与 tmux 名**，拒绝 adopted（无命令）、拒绝仍在运行；命令由 `relaunch_command`（`agent/mod.rs:583`）改写：拔掉 mint flag 与所有旧 resume flag，有 transcript 则 `--resume <id>`，否则复用 `--session-id <id>`。

**name_locked / pane title**：`update_session` 的 SQL 直接 `name_locked = name_locked OR (?2 IS NOT NULL)`（`storage.rs`），observer 每 tick 把 `adapter.session_title(pane.title)` 写入 status（`observer.rs:263`）；由前端决定 title 与 display_name 谁赢。Claude 的 title 清洗只是剥掉 `✳✻✽✶·*` 和 "Claude Code"（`claude.rs:148`）。

### 2b. Agent 状态检测

**trait 真实签名**（`agent/mod.rs:77`）：
```rust
fn detect(&self, tail: &str, seconds_since_activity: f64) -> Option<DetectionResult>;
```
外加 15 个带默认实现的方法（`session_title`/`preview`/`recap`/`session_id_flag`/`session_id_env_var`/`resume_args`/`context_root`/`transcript_matches`/`transcript_id`/`sidecar_dir`/`context_dir_for`/`arrival_gate`/`report_id_args`/`id_from_report`/`transcript_from_report`）。与 MISSION §5.2 的 `Detect(process, tail)`/`MatchProcess` 不同：**没有 process 参数**，process-tree 探测独立在 `procs.rs`，且 trait 的三分之二实际是 ADR-011 "对话身份"而非状态检测。

**状态机在哪**：`status.rs:35 evaluate` 是全部决策顺序：pane_dead → exit code 定 FINISHED/FAILED/UNKNOWN；`seconds_since_start<2 且 tail 空` → STARTING；adapter.detect；否则 `seconds_since_activity < idle_after` → RUNNING(0.5) 否则 IDLE(0.6)。**不是 MISSION §4.4 那种带转移约束的状态机**——任意 tick 的结果直接覆盖 `entry.status.state`（`observer.rs:259`），状态可从 FINISHED 跳回任何值。**没有任何防抖/驻留时间**；"防抖"仅体现在结构上：generic 只看最后 2 个非空行（`generic.rs:23`）、fake 只看最后 1 行、`idle_after` 阈值。

**Claude 规则**（`claude.rs:96-140`）：
1. `tail.contains("Do you want to proceed?") || contains("❯ 1. Yes") || contains("│ ❯ 1.")` → WAITING 0.95
2. `tail.contains("esc to interrupt")` → RUNNING 0.95
3. 最后 4 个非空行有空 `│ >` 输入框，或 `tail.contains("? for shortcuts")` → IDLE 0.85
4. generic 提示词 → WAITING 0.8

注意 1、2、3 的后半段是**全 tail 200 行 contains**，模块头注释（`claude.rs:3-4`）的辩护是"全屏 TUI，已回答的 prompt 会消失"。但 `capture-pane -S -200` 包含 scrollback，Claude 不用 alternate screen；任何被推入 scrollback 的工具输出（例如 agent 自己 `cat` 了这个仓库的 `claude.rs`，或 grep 出 "esc to interrupt"）都会让 session 被钉在 RUNNING/WAITING 直到滚出 200 行。generic/fake 的"只看末尾"约束在最强信号上被放弃了。

**Codex/pi**：`detect` 直接转发 generic（`codex.rs:24`, `pi.rs:18`），模块头注释坦白 RUNNING/WAITING fixture 因账号限流未采集（`codex.rs:5-9`）。也就是说 codex 的 WAITING 检测依赖 `PROMPT_MARKERS` 里 "approve"/"allow this" 等小写子串是否碰巧出现在最后两行。

**confidence**：全仓库唯一消费点是 `observer.rs:260` 存入 status；无任何阈值逻辑，纯调试字段，与 ADR-004 Consequences 一致。

**活动判定**：tail 的 `DefaultHasher` 变化即活动（`observer.rs:220`），但 `(width,height)` 变化时不计（`:217-226`）。**代码与注释不一致**：`observer.rs:80-83` 与 `:204-216` 大段解释"attached client 数也必须计入，否则 flake 只是降低频率"，而 `layout` 类型是 `(u16,u16)`，`pane.attached` 在 observer 中从未被读——`PaneInfo.attached` 是死字段。

**transcript 读取**：`refresh_recap`（`observer.rs:306`）每 30s 重找路径（drop-box 即代码中的 report 文件：`transcript_path` 优先 → `transcript_near` 单次 read_dir），按 `(len, mtime)` 变化才重读，先 64 KiB 尾窗、缺项再 1 MiB（`tail_window` 丢首个不完整行，`agent/mod.rs:675`）。Claude 的 `recap`（`claude.rs:361`）倒序扫 jsonl，取 `last-prompt` 记录与最近 assistant text block，跳过 `isSidechain`/`isMeta`/`isCompactSummary`。session id 三级来源：report 文件 > 库中 pinned > 进程树 env（`session.rs:650`），report 通过 `--settings` 注入 `SessionStart` hook `cat >x.part && mv -f`（`claude.rs:301-320`），这是整个项目最"侵入但可控"的技巧，注释记录了对 `--settings` merge 语义的实测。

**process-tree**：`procs::snapshot` 一次 `ps -axo pid,ppid,comm`，`tree()` 用 Vec 做 BFS（O(N·深度) 的 `contains`），`agent_hint` 按 basename 匹配配置里的 agent 名；仅用于未注册 session 的 hint（`session.rs:1065-1072`）和按需的 env 探测（macOS 走 `ps eww`，`procs.rs:78`）。

### 2c. Terminal gateway

`gateway.rs:156 run`：先 best-effort `set-clipboard on` / `mouse on`（全局 tmux 选项！会改用户自己的 tmux 行为，注释承认）→ `openpty(80x24)` → spawn `tmux -u [-L s] attach-session -d -t =name`（`-d` 踢掉其他 client，`tmux.rs:64` 解释"一个 window 只有一个尺寸"）→ 阻塞读线程 → `mpsc(64)` → 主 `select!`（`biased`，退出信号优先）。

- **resize**：`master.resize` 直接调（`:296`），无节流。
- **多 client**：不支持共视，最新 attach 抢占，被踢方收到 `Detached{taken_over|detached}`（`:236`），判据是 `has_session` + `client_count`。
- **断线**：WS 断 → `killer.kill()` SIGHUP → 等 3s 确认子进程退出后才 drop writer，否则 `mem::forget`（`:327-331`）。原因是 portable-pty drop 时写 `\n^D`，若 attach client 还活着会把 ^D 送进 pane 杀掉 agent——这是一个真实踩过的坑。
- **keepalive**：20s ping，65s 无任何帧即断（`:269-277`）。
- **背压/缓冲**：有界 channel 64×≤8 KiB，读线程 `blocking_send` 满时阻塞，PTY 反压到 tmux；WS 发送串行 await。无帧合并；每个 PTY read 一个 JSON 文本帧，UTF-8 用 `Utf8Stream` 跨块拼接（`:33`）。MISSION §7.4 的二进制帧从未落地。

### 2d. `/api/events`

`server.rs:746 events_ws`：订阅 `broadcast(256)`，只有 `Event::Status` 一种事件；`Lagged` 直接 `continue`（丢事件不告知）。事件源：observer 本地状态变化（`observer.rs:248`，首次观察不发）+ federation 对远端 `/api/sessions` 轮询做 `last_states` diff（`federation.rs:415-440`）。**创建/杀死/改名都没有事件**。前端 `App.tsx:210` 仍 3s 轮询 `/api/sessions`，注释"Phase 2 replaces this with /api/events"未兑现——所以是**拉为主、推为通知**的混合模型，`list()` 会被 observer(2s) 与每个客户端(3s) 各自独立触发。

### 2e. 存储

schema 见 `storage.rs:80-125`；migration 是 `CREATE TABLE IF NOT EXISTS` + 一组 `ALTER TABLE ADD COLUMN`，靠捕获 "duplicate column name" 字符串幂等（`:150-190`），无 `schema_version`，不支持非增量变更。`PRAGMA foreign_keys` 故意不开、不写 REFERENCES（注释解释是为防将来有人开 pragma 后触发级联删除违反 ADR-009）。`busy_timeout 5s`（`:131`），**未开 WAL**。tmux/SQLite 一致性完全由 `list()` 的即时 join 保证，DB 从不存"活"状态（Invariant 7 严格遵守）。

### 2f. 认证/安全

- TOTP：HMAC-SHA1/30s/6 位、±1 步窗口、`last_counter` 持久化且要求严格递增防重放（`auth.rs:66-83`）。
- token：32 字节随机 hex，库存 SHA-256，TTL 30 天；`logout` 删**全部** token（单用户语义）。
- cookie：`HttpOnly; SameSite=Lax; Path=/`，仅当经受信代理 `X-Forwarded-Proto: https` 才加 `Secure`（`server.rs:597-604`）。
- Origin：`origin_allowed`（`:467`）只校验 authority == Host，且**只用于两个 WS 路由**；REST POST 无 CSRF token，依赖 SameSite=Lax。
- 限流：`LoginLimiter` 每源 5 次失败进入 60→300→1800→7200→21600s 阶梯，全局 50 次失败关公网入口但豁免 loopback/RFC1918/CGNAT（`auth.rs:96-230`）；`client_ip` 只在对端 ∈ `trusted_proxies` 时信任 XFF 最后一跳（`server.rs:415`）。
- 中间件顺序：`log_failures` ⊃ `security_headers`(CSP/nosniff/DENY/HSTS) ⊃ `auth_middleware`（每请求两次 SQLite 查询）。

## 3. 工程质量评价

**代码组织**：`server.rs` 5036 行不合理，但不是"handler 太多"这么简单：`build_plan`/`run_move`/`one_leg`/`durable_destination`/`undo_move`（约 `:2581-4150`）是一个完整的跨主机搬迁状态机，与 HTTP 无关却住在 HTTP 层；23 处 `if host != LOCAL_HOST { proxy_call(...) }` 复制粘贴（其中 `update_session` 还得手写字段转发，`:1120-1145` 注释自嘲"漏字段不报错只在远端悄悄失效"）。合理拆法是 `server/{auth,sessions,tabs,moves,ws}.rs` + 一个 `Routed<T>` 提取器统一做本地/远端分派。其余模块（session/agent/observer/gateway）职责清晰，注释密度极高且几乎每条都记录了"实测日期与失败案例"，这是本项目最突出的优点。

**错误处理**：全程 `anyhow`，无领域错误类型。后果是 `server.rs:4197`、`:4290-4296` 靠 `msg.contains("is not running")` 判"已死"，注释承认"ugly"。`ApiError` 的状态码由各 handler 自选（`delete_session` 把所有错误当 409，`kill_session` 当 404，`:4713-4716` 明说）。`Storage::with` 用 `expect("storage mutex poisoned")`（`storage.rs:192`）：任何 handler 在持锁时 panic 会让所有后续请求 panic，进程活着但服务死了。

**并发模型**：三种混用——observer 是 `std::thread` + `std::sync::RwLock`；handler 在 tokio 上；federation 在 tokio 上用 `std::sync::RwLock`。主要问题：
1. `manager.list()`/`create()`/`kill()` 在 async handler 中**同步 spawn 子进程**（`tmux`, `ps`, 首次 create 还有 `claude --help`），未用 `spawn_blocking`（对比 `:1405`、`:4634` 等 IO 处用了）。一个 hang 住的 tmux server 会卡死 tokio worker。
2. `Tmux::run` 用 `Command::output()` 无超时（`tmux.rs:88`）。
3. observer 在持 `store.inner.write()` 期间做 `refresh_recap` 文件 IO（`observer.rs:181` 加锁，`:264` 读最多 1 MiB），期间 `/api/sessions` 的 `snapshot()` 读锁阻塞。
4. observer 线程 `JoinHandle` 被丢弃（`server.rs:173-186`），`tick` 内 `expect("status store poisoned")` panic 会让线程静默死亡，无重启无告警——状态检测从此停摆。
5. 每 tick 串行对每个 session 起 2 个进程（capture-pane + display-message），无并行也无自适应间隔。

**可测试性**：注入点做得好——`Tmux::with_socket` 隔离 tmux server、`Storage::open_in_memory`、`AppState::with_home` 允许两个节点跑在一个进程里做 federation 测试、`scripts/fake-agent` 五种模式 + `FakeAgentAdapter`、`Keepalive` 可缩短。缺的是 `Tmux` 不是 trait，无法不启真 tmux 测 session 逻辑；`procs::snapshot` 硬调 `ps`。

**测试覆盖真实度**：集成测试大部分跑真 tmux（13/21 个文件、218/326 个用例；`devcenter-*test-*` socket），端到端覆盖了 attach→断开→存活→重连、多客户端抢占、resize 实际到达 `tput cols`、OSC52 到达 WS、daemon 重启不杀 agent——这些是真价值。但检测 fixture 极薄：Claude 4 个（每个 <1.1 KB），codex 1 个（仅 idle），pi/opencode 零个；`detection_test.rs` 没有反例语料（scrollback 含关键字的 false positive），没有状态序列/抖动测试。`observer_test` 用 200ms 间隔 + 15s 超时轮询，属于时序敏感测试。

**具体坏味道/潜在 bug**：
- §2b 已述的两处：attached 计数的注释/代码不一致（`PaneInfo.attached` 死字段）与全 tail `contains`。
- `tmux.rs:205` `capture-pane` 无 `-J`，折行被切成两行，`browser.rs:44-60` 承认因此放弃 preview；`-J` 可直接解决。
- `session.rs:1050-1052` 每次 `list()` 对每个未注册 session 起一次 `tmux display-message`，且 `list()` 被 observer 和所有客户端各自调用，无缓存。
- `storage.rs` 无 WAL，observer 写 + 多 handler 读 + `busy_timeout 5s` 会让 handler 卡 5s 才报错。
- `strip_ansi`（`agent/mod.rs:775`）不处理 DCS/APC（`ESC P … ESC \`），按二字符转义丢首字节后泄漏 payload；由于 `capture-pane` 默认不带 `-e` 实际影响小。
- `generate_id` 用纳秒混合而非 CSPRNG，多进程同 prefix 同 DB 时依赖 `has_session` 兜底。

## 4. 值得借鉴 / 应避免

**借鉴**：
1. **Kill ≠ Delete、tmux 是唯一活性真相**（`session.rs:1115/1170`, `storage.rs:1-4`）：DB 永不记录"活着"，所有活性由 `list-sessions` 即时 join，daemon 重启零恢复逻辑。
2. **"先创建外部资源、后写元数据、失败回滚"**（`session.rs:900-914`）及 tags 失败不致命的分级。
3. **`new-session ; set-option ; set-option` 合并成一次 tmux 调用**（`tmux.rs:124-141`）解决秒退竞态；`=name:` 精确匹配防前缀误伤。
4. **Utf8Stream 跨块解码**（`gateway.rs:33-80`）+ 6 个边界测试，是 PTY→WS 的必备件。
5. **portable-pty drop 时 VEOF 的处理**（`gateway.rs:314-332`）：确认子进程死后再 drop writer，否则宁可泄漏 fd。
6. **Storage 的运行命令 ≠ 存储命令**（`session.rs:440-470`）：可移植/可重放的命令入库，机器相关的 instrument 每次现算。
7. **通过 `--settings` 注入 SessionStart hook 让 agent 自报 session_id 到"只写自己文件"的 drop-box**（`claude.rs:266-320`）：非侵入、无端点、无权限扩散，且实测了 merge 语义。
8. **三值 `agent_availability`**（`session.rs:702`, `which.rs`）：`$SHELL` 是"不可解析"而非"缺失"；`--help` 探针决定是否追加 flag。
9. **LoginLimiter 的"公网全局锁 + 受信网段豁免"**（`auth.rs:96-180`）：DoS 不会把主人锁在门外。
10. **注释即 ADR**：几乎每个决策都写了"哪天在哪台机器实测、反例是什么"。

**避免/改进**：
1. server.rs 把跨主机 move 状态机和 23 份代理分派塞进 HTTP 层；应拆模块 + 统一路由提取器。
2. 状态检测无真正状态机、无驻留时间；一个 tick 的误判即广播事件。至少要求连续 N 次一致或最小驻留后才发 `Status` 事件。
3. Claude adapter 的全 tail `contains`，应像 generic 一样限定在末尾若干行，或只匹配 Ink 的非 static 区域。
4. 在 async handler 中同步 spawn 子进程且无超时；`Tmux` 应给 `Command` 加超时并在 handler 侧 `spawn_blocking`。
5. `Mutex/RwLock` 的 `expect("poisoned")` 到处都是，加上 observer 线程无看门狗；应 `unwrap_or_else(|e| e.into_inner())` 或改 `parking_lot`，并让 observer 崩溃可重启。
6. `list()` 被多方独立触发、无缓存；应让 observer 产出统一快照，HTTP 只读快照。
7. `/api/events` 只有状态事件、丢帧静默、前端仍轮询——要么做完整事件流（含 create/kill/rename 与 lag 通知），要么删掉只留轮询，别留一半。
8. anyhow 全程 + 字符串匹配错误分类；session 层至少要一个 `SessionError { NotFound, NotRunning, Adopted, … }`。
9. 增量 migration 靠 "duplicate column" 字符串、无版本号；应上 `user_version`。
10. gateway 在 attach 时改**全局** tmux 选项（`mouse`, `set-clipboard`），会改变用户本机 tmux 行为；应在 session 级或通过独立 socket 隔离。

## 5. 对 "capture-pane 文本启发式" 路线的判断

从代码看，这条路线**对 Claude Code 的当前 UI 是可用的但脆弱**，对其他 agent 基本是未实现：

- 真正 agent 特化的规则只有 Claude 的 4 条子串 + fake-agent；codex/pi 直接转 generic，作者在 `codex.rs:5-9`、`pi.rs:3-6` 明说缺 fixture 不敢猜。fixture 总量 4.3 KB，`testdata/claude/*` 每个不到 20 行。
- 规则对 Claude UI 字符串（`│ >`、`❯ 1. Yes`、`? for shortcuts`、`esc to interrupt`、statusline 的 `] │`）强耦合，Claude Code 一次 UI 改版即失效；`CHROME` 列表（`claude.rs:12-22`）已经是 9 条特判，`is_statusline` 三种格式，这是典型的"每个版本加一条"的增长曲线。
- 作者已经意识到 pane 文本的上限，因此把"这个会话在干什么"（recap）和"这是哪个对话"（session id）**都改从磁盘 transcript / hook 报告读取**，只把 RUNNING/WAITING/IDLE 留给 pane。这说明项目实际路线已经是 ADR-004 预告的"Native Hook > Process > Pattern > Activity"分层的前两层——只是状态检测那一层还没升级。
- 没有防抖、全 tail contains、折行未 `-J`、每 tick 每 session 两个进程——这些都是"能跑起来但 50 会话/多 agent 后会显形"的问题，且没有一个被测试覆盖。

结论：对新项目，pane 启发式只应作为**最低优先级的兜底层**并从第一天就带驻留时间/末尾窗口约束；WAITING 这种要打扰人的状态，应优先走 agent 的 hook/事件（Claude 的 hooks、Codex 的 notify）而不是屏幕文本。devcenter 里 `report_id_args` 的 SessionStart hook 注入是可以直接复用的模式——把同样机制扩到 `Notification`/`Stop`/`PreToolUse` hook，就能用一个 drop-box 文件替代大半 Claude 文本规则。
