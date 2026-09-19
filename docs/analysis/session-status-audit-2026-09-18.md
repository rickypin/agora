# 会话状态盘点与梳理（2026-09-18）

起因：Mac 的 Dashboard 截图里 RUNNING 段挤着七八行 `?`（unknown 35m）、两行 `…`（starting 2d / 15h），主区打开一行是 `no sessions` 退出码 1。随后 Mac 侧（node=local，peer=zuan）在 22:32Z 做了一次只读盘点：79 行（local 55、zuan 24），49 行 FINISHED，而两台机器真活着的 agent 进程只有十几个。用户的要求不是清理，而是把「agora 呈现给使用者的会话状态」厘清：现状盘点、代码梳理、逐条核实、真值表、epic 拆分、修复顺序、需要裁决的产品问题。

本文是分析报告，不是 spec；结论落到 beads（epic **agora-5gg**，§8）。zuan 上的部分是我直接核对的（API、hook 检查点、`/proc` 三方交叉）；Mac 那台 daemon 不给 ssh、TLS 端口也不通，Mac 部分以 Mac 侧盘点提供的证据为准、按代码逐条核实，标注了「待 Mac 上核实」的地方需要跑 §10 的命令。

**版本前提（Mac 盘点 D2）**：Mac 二进制编于 09-10，zuan 是 09-18 的 `7b94c2c`。`git log --since=2026-09-09 -- src/hook src/status src/session src/adapter src/runtime src/peer` 只有三条 09-10 的提交（agora-rip「hook 没接上」文案、agora-t36 归档清理节流、agora-7ad fake-agent 环境），都不碰本文涉及的判定；本文用到的每条逻辑的引入日期都早于 09-10（`runtime session missing` 09-03、replay 09-04、PromptInjected 与 STARTING 衰减 09-05、Codex Desktop 进程号规则 09-06、`external_silent_after` 09-08）。**下面所有现象两个版本一样，没有一条只属于旧二进制。**

## 0. 第一性原理：一行状态要回答什么

MISSION §4.3 的定义方式是对的：**状态从"人要做什么"定义，不从"观察者看见什么"定义**。据此一行状态必须同时回答三个问题，缺一个就是含混：

1. **agent 进程还在不在**（事实，不是推断；不知道就说不知道）。
2. **它此刻要我做什么**：等我答（WAITING）/ 等我看结果给下一步（TURN_DONE）/ 什么都不要（RUNNING）/ 已经没了（FINISHED / FAILED）。
3. **如果 agora 说不清，为什么说不清、我该做什么**（UNKNOWN 必须带可执行的原因，而且必须是暂态：要么 agora 很快能弄清，要么给人一个出口）。

用这三问去量现状：UNKNOWN 与 STARTING 都成了「装不下就往里扔」的筐，筐里的行既不暂态、也没有出口，还都被摆在一个叫 RUNNING 的标题下；`alive` 一个布尔想回答第一问却回答不了「不知道」；FINISHED 的 external 行没有结束时刻，第二问之后的生命周期就无从定义。

## 1. 现状盘点

### 1.1 zuan（本机 daemon，systemd，12:19 UTC 起未重启）

24 行，`unregistered` 空，运行时 ok，4 个 tmux 会话都在。按 `GET /api/sessions` + `~/.agora/hooks/state/` 检查点 + `/proc` 交叉核对（22:30 UTC）：

| 行 | agent / origin | 状态 (source) | 持续 | 进程事实 | 判定 |
|---|---|---|---|---|---|
| 6d7010 capmaster@zuan | grok / agora | TURN_DONE (hook) | 255 h | pane pid 496502 grok 活着 | 正确。但 10 天前的 turn_done 排在 NEEDS ATTENTION 最顶（§3.6） |
| 799dc4 capmaster@claude | claude / agora | TURN_DONE (hook) | 254 h | pane pid 597908 claude 活着 | 同上 |
| 105cc2 capmaster@perf | grok / agora | TURN_DONE (hook) | 208 h | pane pid 706920 grok 活着 | 同上 |
| b4027c code | shell / agora | IDLE (activity) | 10 h | bash 活着 | 正确 |
| **ef0e50 trion** | claude / external | **STARTING (hook)** | **180 h** | pid 2642307 活着（pts/13，7.5 天） | **错**：只收到过 SessionStart，按 §4.3 最多停 10 s 就该衰减成 TURN_DONE `awaiting first prompt`（§3.1） |
| **a3a2a0 handoff** | claude / external | **STARTING (hook)** | **10 h** | pid 2879418 活着（pts/8） | 同上 |
| 5f7c50 agora（本会话） | claude / external | RUNNING (hook) | 现在 | pid 3590709 | 正确。但 13:01 起到 22:26 被 `--resume` 之前它也钉了 9 h STARTING——Mac 截图里那行 `agora @ zuan starting 35m` 就是它 |
| 16ce28 capmaster | grok / external | TURN_DONE (hook) | 112 h | pid 2640044 grok 活着 | 正确 |
| 5de343 PktMask | grok / external | TURN_DONE (hook) | 61 h | pid 634753 grok 活着 | 正确 |
| 662a60 handoff | claude / external | TURN_DONE (hook) | 5.5 h | pid 104960 活着 | 正确 |
| 9d2ecd handoff | claude / external | FINISHED (hook) | 7.9 h | **同一 pid 104960 活着** | 正确（被 662a60 取代：同进程换了对话），但 API 上 `alive: true` + `finished` 并存（§3.5 / B1） |
| **e3cd17 agora** | claude / external | FINISHED (hook) | 4 min | pid 3590709（本会话的进程） | **幽灵行**：`claude --resume` 先以新 id 发 SessionStart(startup)，12 s 后发 SessionEnd(reason=resume) 切到旧 id。一行没有 prompt、没有输出的 finished（§3.4） |
| 800897 / 95e8c2 / 229bb2 / 54a8d2 / 2c5c5f / 4bd69d | claude / external | FINISHED (hook) | 9–14 h | 进程都没了 | 正确，24 h TTL 后自动删；但 `ended_at` 全空（B5） |
| e8c022 agora-49j / b552a8 agora-qfs | grok / external | FINISHED (hook) | 13 h | 没了 | 正确（handoff 工人，C2） |
| 9d9c2b repo / 96a595 grok-cwd / b02837 codex-cwd / b81d74 codex-os-cwd | 各家 / external | FINISHED (hook) | 11–12 h | 没了 | 正确，但它们是 handoff 的 e2e / release-check 探针（`~/.handoff/e2e/…`、`~/.handoff/release-checks/…`），带着用户级 hook 把测试进程登记进了生产 daemon（C2 / §3.9） |

小结：zuan 24 行里 **3 行 STARTING 是错的**（占了「不需要人」段里除 shell 外的全部），1 行是幽灵，4 行是测试探针；其余状态本身正确，但 3 行 10 天前的 TURN_DONE 长期霸占 NEEDS ATTENTION 顶部。

### 1.2 Mac（node=local；截图 + zuan 日志 + Mac 侧盘点）

- zuan 日志：`peer:mac` 于 **21:46:12 UTC** 重新走了 `/api/system` → `/api/events` → `/api/sessions` 全量，之后流一直开着。Mac 侧确认 daemon 21:43:20Z listening、21:46:12Z 重放完成（14026 件，172 s），此前从 09-15T08:50Z 停了 3.5 天（D1）。所有行显示 35m / 0.7h 的来源就是这次重启（B4 ②）。
- 主区那行 shell：`no sessions` 退出码 1 = tmux attach 打在空 server 上；Mac 启动日志 `reconcile alive=0 dead=0 missing=2`。这两行（`b226b5` 十天前被 Kill、`63a22a`）库里有 `killed_at` / `ended_at`，API 却是 UNKNOWN `runtime session missing`（A2）。
- 带 `external` chip 的 `?` 行：`72e5c2` / `f9c572` / `32caf0` / `443d8e` 是 UNKNOWN `hooks silent; no process handle`，而更"新"的 `0e26ad` / `14d791` / `eb129d` / `b939bb` 沉默 55–63 h 仍 TURN_DONE（A4）——同一批 Codex Desktop 行两种结果，差别只在有没有被重放。
- `trion starting 2d`（`db5d48`，pid 14060 活着）、`dbs-operator starting 15h`（`4aa42b`，grok pid 46479）：与 zuan 的 ef0e50 / a3a2a0 同一类（A1）。
- 79 行里 49 行 FINISHED、10 行 finished + alive:true、7 行 turn_done + alive:false、46 行 external 的 `created_at` 落在重放那两分钟（B1 / B2 / B4 ①）。

## 2. 状态是怎么算出来的（代码梳理）

每行一台状态机（`src/status/machine.rs`），两个入口：hook 事件立即 `apply`，每 tick `observe` 吃进程事实 + 文本 + 活动。`src/session/manager.rs:975` 的 `view()` 先按 `(origin, runtime)` 算进程层：

```
有 runtime_ref、列表里找得到     → process_layer：alive → STARTING/RUNNING；退出 → FINISHED/FAILED（conf 1.0）
有 runtime_ref、列表里找不到     → UNKNOWN "runtime session missing"（Source::None，Liveness::Dead）
运行时降级                       → UNKNOWN "runtime unavailable: …"（Source::None）
external、检查点有 pid、pid 没了 → FINISHED "external process gone"（conf 0.8，Liveness::Dead）
external、pid 活着               → UNKNOWN "…process alive, hook only"（Source::None，Liveness::Alive）
external、没有 pid               → UNKNOWN "…no runtime, hook only"（Source::None，Liveness::Unknown）
```

然后 `Machine::observe`（`machine.rs:613`）**第一步**：`liveness == Dead || process.source == None` 就进「进程层压倒一切」分支并 **return**——external 行三种情况 `process.source` 都是 `None`，所以 external 行**永远走不到** `observe_hooked`。而 `observe_hooked` 里有两条对它们至关重要的规则：STARTING 的 10 s 衰减（`machine.rs:782`）和 hook 沉默 → UNKNOWN。第一步分支里只补了一条 `external_silent_after`（2 h，且仅 `Liveness::Unknown`，时钟用 `last_hook_at` = daemon 收到时刻）。

衰减规则的守卫（`tests/state_machine.rs::hook_starting_decays_to_turn_done_awaiting_first_prompt`、`tests/hook_recovery.rs::starting_decays_through_session_manager_and_after_checkpoint_restore`）全都喂了 `rt(true, None)`，即有运行时的行；无句柄 external 行没有守卫。

`alive`（`manager.rs:1107`）= `rt.alive || liveness == Alive`：语义是「最后看到的那个进程号还在」。`pid`（:1109）只取 pane 的。`ended_at` 只在 `mark_ended_at`（:1482）里写：运行时报退出、Kill、reconcile 三条路，external 行一条都走不到（:1357 的注释承认）。external 行的 `created_at` 是登记那一刻的 `strftime('now')`（:826），`ExternalSession`（:114）不带时间。

hook 投递（`src/main.rs`）：160 bind socket → 252 同步等 `replay()` → 335 `sock.serve`。`replay()`（`src/hook/receiver.rs:186`）只对 `inbox.pending()` 的一次快照逐个 ingest；`run_sweeper` → `sweep()` 不扫 inbox；`src/local/mod.rs:175` 说明 bind 与 accept 之间的 hook 排在内核 backlog 里——排上了的照常，没排上的（客户端被宿主按 timeout 杀、backlog 满、connect 失败）就没人再读。

前端（`web/src/attention.ts`）：分数 FAILED 100 > WAITING 90 > TURN_DONE 85 > FINISHED 80 > UNKNOWN 40 > IDLE 30 > STARTING 20 > RUNNING 10；`sectionOf` 只切三段：≥ 80 进 NEEDS ATTENTION，收起来的 FINISHED 进折叠区，**其余一律进标题为 RUNNING 的段**。同分按等待时长升序。

## 3. 问题清单（zuan 视角首轮；Mac 盘点的逐条核实见 §4）

### 3.1 external 行的 STARTING 永不衰减【(a) bug，违反 MISSION §4.3 / ADR-002 D1】→ agora-rkl

- 现象：zuan 3 行（180 h / 10 h / 9 h）、Mac 2 行（2 d / 15 h）显示 `… starting`。使用者只能理解为"卡在启动"，其实是 agent 起好了停在提示符、没人给第一条指令。
- 根因：见 §2。`observe` 第一步对 external 行提前返回，衰减分支不可达。
- 判断：这行要人做的事是"给它指令"（= TURN_DONE `awaiting first prompt`），与 agora 起的会话完全一样，没有理由按 origin 分叉。修法是把衰减搬到 `observe` 第一步的 external 分支里（`Liveness::Alive | Unknown` 都适用），并补 external 版守卫。

### 3.2 运行时会话没了 = 永远 UNKNOWN【(b) 设计打架】→ agora-u5p（并 A2）

- 现象：Mac 上 tmux server 没了，所有 agora / adopted 行 `? unknown` + `runtime session missing`，点开终端是 `no sessions` + 一个永远连不上的「重新连接」。
- 根因：`list_socket`（`src/runtime/tmux/mod.rs:303`）在 server 不在时返回空列表，不算降级；`process_layer(None)` 给 UNKNOWN；`reconcile`（`manager.rs:1423`）对 missing 行写了 `ended_at`（approximate），:191 的注释明写这类行是 UNKNOWN——**是有意设计**，与 §4.3「UNKNOWN = 看不清」打架，所以判 (b) 而不是 (a)。
- 判断：tmux 会话没了 ⇒ pane 进程已收 SIGHUP ⇒ agent 确定不在，这是事实。应报 **FINISHED（process，conf 0.8，`runtime session gone`）**，通知一次；终端面板给"运行时会话已不在：Restart 重建 / 删除"（Restart 对 missing 行退化为同名 create，`manager.rs:1259` 已支持）。真正"读不到"的 `ServerUnavailable` 降级路径（UNKNOWN + 横幅）不变。

### 3.3 无进程号的 external 行：2 小时后 UNKNOWN，然后永远【(b) 设计缺口】→ agora-e08（阻塞于裁决 Q1）

- `Liveness::Unknown` 的行沉默 2 h 落 `hooks silent; no process handle`，之后只有下一条 hook 事件能让它离开，而进程多半已不在；`external_finished_ttl` 只清 FINISHED。UNKNOWN 必须是暂态：给它出口（按 TTL 走 DELETE 同一路径）。

### 3.4 `claude --resume` 留下幽灵 finished 行【(a) bug，小】→ agora-29n

- `machine.rs:573` 的注释假设 resume 之后同一进程再发 SessionStart 会落回同一行——对有 pane 的行成立，对无句柄 external 行不成立（身份是 agent id，新 id 另起一行）。`clear` 已处理（`clear_ends_external_row`），`resume` 漏了。收到第一条 PromptSubmitted 之前就 SessionEnd(resume | clear) 的 external 行是身份交接，不是会话，应删行。

### 3.5 `alive` 语义【(b)】→ 裁决 Q4（agora-5gg.4）

- 见 §4 B1 / B2。

### 3.6 NEEDS ATTENTION 被 10 天前的 TURN_DONE 霸顶【产品取舍】→ 裁决 Q5（agora-5gg.10）

- §6.3「同分再按等待时长」是为 WAITING 的公平性设计，套到 TURN_DONE 上语义反了：10 天没人理的结果人已经用脚投票"不急"，刚做完的那条才是人在等的。备选 A（段内降序）/ B（「看过」扩到 TURN_DONE）/ C（淡显）；建议 A + B。

### 3.7 RUNNING 段名不副实【呈现】→ agora-5gg.11

- 四段：NEEDS ATTENTION / UNCLEAR（UNKNOWN，带原因与出口）/ WORKING / FINISHED。分数表不动只改 `sectionOf`。

### 3.8 重启后所有时长归零【呈现，小】→ agora-5gg.12（并 B4 ②）

### 3.9 测试探针进了生产 daemon【环境卫生，归 handoff】

- handoff 的 e2e / release-check 起真 agent、带着用户级 hook。归 handoff：测试里用隔离的 hook 配置。agora 侧不替别人擦地。

## 4. Mac 盘点逐条核实

判定：**(a)** 代码违反现行规格的 bug；**(b)** 规格没定义清 / 定义互相打架；**(c)** 盘点看错了。

| # | 判定 | 核实结论 | 代码 / 规格 | 复现 | beads |
|---|---|---|---|---|---|
| **A1** STARTING 永久钉住 | **(a)** | 与 §3.1 同一根因；四行都是无句柄 external，衰减分支不可达。两台二进制都含衰减逻辑（09-05），差别不在版本 | `machine.rs:619`（提前返回）vs `:782`（衰减）；MISSION §4.3、ADR-002 D1 最后一条 | 隔离 daemon + fake-agent 扮 claude、不经 tmux 起（无 `TMUX_PANE`）、只发 SessionStart；或 `Machine` 单测喂 `Observation{ process: Assessment::unknown(..), liveness: Alive, runtime: None }` 20 s | agora-rkl |
| **A2** 已结束的 agora 行显示 UNKNOWN | **(b)** | reconcile 对 missing 行写 `ended_at(approximate)`，同一函数的注释写明状态是 UNKNOWN——有意的；`view()` 也从不看 `killed_at` / `ended_at`。与 §4.3 打架 | `manager.rs:191`、`:1423`、`:975`；`src/status/mod.rs:132`；`tmux/mod.rs:303` | 隔离 daemon 起 shell 会话 → `tmux -L <socket> kill-server` → 下一 tick 仍 UNKNOWN；有 `killed_at` 的同样 | agora-u5p |
| **A3** 重放窗口内的事件滞留 inbox | **(a)** | `replay()` 只扫一次 `pending()` 快照；socket 在 accept 之前只靠内核 backlog（`local/mod.rs:175` 承认）；`sweep()` 从不扫 inbox。任何一个没排上 backlog 的投递件（宿主按 timeout 杀了 hook 进程、backlog 满、connect 失败）就要等下次重启。违反 MISSION §5.1「不得丢失」、ADR-002 §什么会让它变危险 | `main.rs:160/252/335`、`receiver.rs:186`、`:823` | `Receiver::replay()` 之后、`wake` 之前往 inbox 写一份 PermissionRequest 投递件（等价于 backlog 没排上），跑 sweep 一个周期：不会被消费。具体是哪一种没排上要看 Mac 那 6 个文件的宿主超时（Claude 对 Pre/PostToolUse 的 hook timeout）与 daemon.log | agora-5gg.1 |
| **A4** 无句柄 Codex 行沉默两天仍 TURN_DONE | **(b)** | 沉默时钟是 `last_hook_at`，`apply_at` 记的是 **daemon 收到**时刻（`:465`；注释 452–455 说这是有意的：沉默阈值是 daemon 的等待）而 `status_since` 记事件时刻。3.5 天停机后重放的行 `last_hook_at` = 重放时刻，要再等 2 h；从检查点恢复、没被重放的 4 行 `last_hook_at` 是旧值，立即 UNKNOWN。同一批两种结果，盘点没看错。有 pane 的 D1 沉默规则按 daemon 等待算是对的（屏幕证据要 daemon 活着才采得到），无句柄规则回答的是「agent 大概率早退了」，该用事件时刻 | `machine.rs:452-465`、`:628-642` | `Machine` 单测：`apply_at(TurnEnded, now, at = now - 3 h)` 后 `observe(liveness Unknown)`——现在要再等 2 h | agora-5gg.2 |
| **B1** finished + alive:true 10 行 | **(b)** | `alive = rt.alive \|\| liveness == Alive`（`manager.rs:1107`）= 「最后看到的 pid 还在」。superseded / resume 旧行 pid 当然还在。Codex `683eb6` 记下了 app-server 的 pid：`agent_pid` 只在信封 `agent_env` 含 `CODEX_INTERNAL_ORIGINATOR_OVERRIDE` 时返回 None（`codex.rs:202`），那份投递件的 env 里没有这个变量——**待 Mac 上核实**（看 `done/codex/<id>/` 里信封的 `agent_env`；Desktop 的某些线程可能不设它，那就是判据本身不够）。reason 两种说法：`SessionEnded(reason)` 一律映射成 `session ended (hook)`、宿主 reason 丢弃（`:573`），Grok 走 `Superseded`（`:81`） | `manager.rs:1107`、`codex.rs:190-207`、`machine.rs:573/581` | 单测：同一 pid 两个对话 → 旧行 FINISHED 且 `alive` 仍 true | 裁决 agora-5gg.4；词表 agora-5gg.6 |
| **B2** turn_done + alive:false 7 行 | **(b)** | `Liveness::Unknown` 被压成 `false`（api.md 第 39 段「进程号不可信的 alive 为 false」是有意的），与 §4.3「TURN_DONE = 进程仍在」字面冲突。布尔表达不了三值 | 同上 | — | 裁决 agora-5gg.4 |
| **B3** external 行 pid 恒 null | **(a)** 小 | `pid: rt.and_then(..)`（`:1109`）只取 pane；检查点里的 `agent_process.pid` 不出 API | `manager.rs:1109` | 任一无句柄 external 行 `GET /api/sessions/:id` | agora-5gg.5 |
| **B4 ①** created_at = 重放登记时刻 | **(b)** | `register_external` 用 `strftime('now')`（`:826`），`ExternalSession` 没有时间字段；§4.2 没定义 external 行的 created_at。A42 已让 `status_since` 用事件时刻，created_at 该用同一原则（登记那条信封的 `received_unix_ms`） | `manager.rs:114`、`:826` | 停 daemon → fake-agent 发 SessionStart → 起 daemon：`created_at` 比 `status_since` 晚 | agora-5gg.3 |
| **B4 ②** peer 行 status_since 本机重启归零 | **(b)** | `stamp()`（`peer/view.rs:441`）以 `prev` 为准，本机重启 `PeerViews` 为空即 `now`。ADR-004 只说「不信 peer 时钟」，spec 的「重连不重置」指 peer 断线重连；本机重启没定义。UI 把下界画成精确值 | `peer/view.rs:441-458` | 两个隔离 daemon 互为 peer，重启 A：B 的行时长归零 | agora-5gg.12 |
| **B5** external 行从不写 ended_at | **(b)** | `mark_ended_at` 只在运行时退出 / Kill / reconcile 写；`:1357` 注释承认对 external 行是死路，`expire_external_finished` 拿 `status_since` 顶。§4.2 把 `ended_at` 定义成「进程退出时刻」，external 行没有进程退出但有对话结束 | `manager.rs:1357-1395`、`:1482` | 任一 external FINISHED 行 `ended_at` 为 null | agora-5gg.3 |
| **C1** Codex Desktop 子对话登记成会话 | **(b)** | 登记规则（`receiver.rs:373`）只看「有 agent_session_id + 事件不只 SessionEnd」，不分人发起还是宿主内部。判据不能靠 prompt 文本（§2.3 规则 10）；Codex Desktop 子代理的载荷有没有结构信号**待 Mac 上录一份真投递件**对照键集合 | `receiver.rs:373-402` | — | 裁决 agora-5gg.7 |
| **C2** 无头一次性会话混在一起 | **(b)** | 同上。Claude 有结构信号：无头 SessionStart 载荷没有交互模式才有的 `model` / `scratchpad_dir` 键（`testdata/claude/2.1.270/hooks/headless.jsonl` 头注，09-14 实录）；Codex `exec` 有 `testdata/codex/0.153.4/hooks/headless.jsonl` 可比键集合 | 同上 | 回放 headless fixture：登记成普通行 | 裁决 agora-5gg.7 |
| **C3** 一进程多行、同名成堆 | **(b)** | ADR-002 D7：身份 = (host, agent_session_id)，同进程换对话 → 旧行 superseded + 新行（`supersede_external_rows`）；`display_name` 取工作目录名（`:817`），同目录反复开就是一堆同名。数据模型没错，呈现没有「当前那个」的概念 | `manager.rs:784-835`、`:864` | — | 裁决 agora-5gg.8 |
| **C4** 宿主包装文本进 prompt | **(a)** 对 Claude 已修、**(b)** 对 Codex / Grok | `INJECTED_PROMPT_TAGS`（`hooks.rs:172`）只认 `<tag` 前缀，三家共用 `prompt_event`；Codex 的 markdown 标题式包装与 Grok 的 `<\|eos\|>` 尾巴都不在内。要先从 Mac 的 done 归档抠真投递件录 fixture | `adapter/hooks.rs:172-212`、`codex.rs:219`、`grok.rs:243` | 回放 fixture → `PromptSubmitted` 带包装 | agora-5gg.13 |
| **C5** 孤儿检查点 | **(c)** 无害，但值得顺手清 | `restore_hook_checkpoints` 只按 `all_records` 迭代（`:480`），孤儿不会被加载、不会复活；`delete_metadata`（`:1312`）会删检查点，失败只 warn。id 是随机 6 hex，撞上也有 epoch / version 校验 | `manager.rs:480-510`、`:1312`；`hook_state.rs:50` | 往 `state/` 塞一个无行的文件，重启后 API 无该行 | agora-5gg.14 |
| **D1** daemon 停 3.5 天没人知道 | 运维 | Mac 的 daemon 不是 launchd 单元（`bd memories mac-agora-daemon-manual-restart`）。状态可信度的前提是 daemon 在 | — | — | agora-5gg.15 |
| **D2** 两个版本 | **(c)** | 见文首「版本前提」：状态逻辑 09-09 之后没改，所有现象两边一样 | — | — | — |

## 5. 根因归并与修复顺序

七个根因，覆盖 §3 与 §4 的全部条目：

| 根因 | 覆盖 | 性质 |
|---|---|---|
| **R1** `observe` 第一步对 external 行提前返回，hook 层的时间规则（STARTING 衰减、沉默）对它们不可达；无句柄沉默用了 daemon 时钟 | A1、§3.1、A4 | (a) + (b) |
| **R2** 「运行时会话不在」被当成 UNKNOWN 而不是结束事实 | A2、§3.2 | (b) |
| **R3** 投递箱只在启动时扫一次，之后全靠 socket 唤醒 | A3 | (a) |
| **R4** `alive` 是「最后看到的 pid 还在」的布尔，三值压成二值；`pid` 不出 API | B1、B2、B3、§3.5 | (b) |
| **R5** external 行没有自己的时间线：`created_at` 取登记时刻、`ended_at` 从不写、`reason` 是自由文本且丢掉宿主 reason | B4 ①、B5、B1 词表、§3.3（无出口） | (b) |
| **R6** 「一行 = 一个 agent 会话」的边界没定义：宿主子对话、无头一次性、同进程换对话、包装 prompt | C1–C4、§3.4 | (b)，需裁决 |
| **R7** 呈现层：分段名、TURN_DONE 排序、peer 行时长下界 | §3.6–3.8、B4 ② | 产品取舍 |

建议顺序（每步一个小切片、一次 rebase 解决得了）：

1. **agora-5gg.1（A3，P1）**：hook 事件丢失是状态正确性的地基，与其它条目无耦合，先修。
2. **agora-rkl（A1）+ agora-5gg.2（A4）**：同一处代码（`observe` 第一步的 external 分支），一起改；补 external 版守卫。
3. **agora-u5p（A2 / §3.2）**：runtime gone → FINISHED；终端面板文案。
4. **裁决 Q4（alive 三态）→ agora-5gg.5（pid）、agora-5gg.6（词表）**：API 形态改一次、`api_version` bump 一次，别分两回。
5. **agora-5gg.3（ended_at / created_at）**：有了时间线，Q1 的生命周期才有依据。
6. **裁决 Q1 → agora-e08**；**裁决 Q2 → agora-29n**；**裁决 Q3 → agora-5gg.13**。
7. **裁决 Q5 → agora-5gg.11（分段）、agora-5gg.12（peer 时长）**：纯前端。
8. **agora-5gg.17（MISSION / ADR 修订）与 agora-5gg.16（真值表守卫）**收尾：真值表是 epic 的验收。

## 6. 状态真值表（origin × status × process × source）

`process` 是 Q4 提案里的三值（alive / gone / unknown）；现行 `alive` 布尔的对应关系：alive = `process == alive`。「✓」合法且写明对使用者的含义；「◐」合法但只允许短暂（≤ 一个 tick / 宽限）；「✗」必须不可能，各需一条反向守卫（agora-5gg.16）。

### 6.1 origin = agora / adopted（有运行时句柄）

| status | process | source | 判定 | 对使用者的含义 / 出口 |
|---|---|---|---|---|
| STARTING | alive | process（spawn < 2 s）/ hook（SessionStart ≤ 10 s） | ✓ | 刚起，等一下 |
| STARTING | alive | hook，> 10 s | ✗ | 应已衰减为 TURN_DONE（现行守卫已有） |
| RUNNING | alive | process / hook | ✓ | 在干活，不用管 |
| WAITING | alive | hook（有 hook 的 agent）/ text（无 hook） | ✓ | 答它 |
| WAITING | alive | text，agent 有 hook | ✗ | D1：文本不得抬 WAITING（现行守卫 agora-uez） |
| TURN_DONE | alive | hook | ✓ | 看结果、给下一条 |
| TURN_DONE | alive | text / activity | ✗ | D1 |
| IDLE | alive | activity（无 hook） | ✓ | 安静了一阵，没人说为什么 |
| IDLE | alive | activity，agent 有 hook | ✗ | D1：活动层不产 IDLE |
| FINISHED | gone | process（exit 0 / killed_by_user）| ✓ | 退了，看结果或清理 |
| FINISHED | gone | process（**runtime_gone**，R2 新增） | ✓ | 运行时会话没了：Restart 重建或删除 |
| FINISHED | alive | hook（SessionEnd 先于进程退出被观测） | ◐ | 下一 tick 进程层覆盖 |
| FAILED | gone | process（exit ≠ 0 / signal） | ✓ | 出错了，看终端 |
| FAILED / FINISHED | alive，持续 | 任何 | ✗ | 「进程退出压倒一切」 |
| UNKNOWN | unknown | process（`runtime unavailable`，运行时降级） | ✓ | agora 失明，横幅说明；恢复即自愈 |
| UNKNOWN | alive | text（`hooks silent; screen: …` / `permission prompt gone`） | ✓ | hook 没声音而屏幕像在等人：打开终端或查 hook（`hooks_unheard` 提示） |
| UNKNOWN | gone | 任何（现行 `runtime session missing`） | ✗ | R2 之后不再出现 |
| UNKNOWN | 任何 | hook | ✗ | 有 pane 的行 hook 层不产 UNKNOWN |

### 6.2 origin = external（无运行时句柄；process 来自检查点里的 agent 进程号）

| status | process | source | 判定 | 对使用者的含义 / 出口 |
|---|---|---|---|---|
| STARTING | alive / unknown | hook ≤ 10 s | ✓ | 刚起 |
| STARTING | 任何 | hook > 10 s | ✗ | R1（agora-rkl）；现场 5 行 |
| RUNNING / WAITING / TURN_DONE | alive | hook | ✓ | 同 6.1；WAITING 经挂起 hook 可答 |
| RUNNING / WAITING / TURN_DONE | unknown | hook | ✓ | 同上，但 agora 不知道进程在不在（Codex Desktop）；沉默 ≥ 2 h（按**事件时刻**，R1）→ UNKNOWN |
| RUNNING / WAITING / TURN_DONE | gone | 任何 | ✗ | 进程没了就是 FINISHED `process_gone` |
| IDLE | 任何 | 任何 | ✗ | external 行没有活动来源 |
| FINISHED | gone | process（`process_gone`） | ✓ | 一个事件都没来、进程消失：崩溃 / 关窗口；通知一次 |
| FINISHED | gone / alive | hook（`host_session_end{clear\|resume\|logout\|exit\|other}` / `superseded`） | ✓ | 人自己结束的或换了对话：不通知。**`process` 按 Q4 建议一律报 gone**（对话结束不再谈进程）；现行 `alive: true` 是 R4 |
| FAILED | 任何 | 任何 | ✗ | 没有退出码，分不出 FAILED |
| UNKNOWN | unknown | hook（`hooks silent; no process handle`） | ◐ | 暂态：下一条 hook 恢复，或 ≥ TTL 自动删（agora-e08，Q1） |
| UNKNOWN | alive / gone | 任何 | ✗ | 有进程事实就不该 UNKNOWN |
| UNKNOWN | 任何 | none（`no observation yet` / `external session: … hook only`） | ◐ | 只允许在检查点恢复 / 重放完成前的瞬间；持续出现 = 检查点丢了，属 bug |

### 6.3 peer 行

同 6.1 / 6.2，另加：`stale = true` 时整行淡显、`last_seen` 必在；`status_since` 是本节点时钟的**下界**（B4 ②，agora-5gg.12）。

### 6.4 现场对照

| 现场 | 落在哪一格 |
|---|---|
| zuan ef0e50 / a3a2a0、Mac db5d48 / 4aa42b | 6.2 STARTING > 10 s ✗ |
| Mac b226b5 / 63a22a | 6.1 UNKNOWN gone ✗ |
| Mac 0e26ad 等 4 行 TURN_DONE 55–63 h | 6.2 TURN_DONE unknown ✓ 但沉默时钟错（R1） |
| Mac 72e5c2 等 4 行 UNKNOWN | 6.2 UNKNOWN unknown ◐ 却无出口 |
| finished + alive:true 10 行 | 6.2 FINISHED hook，process 应报 gone（R4） |
| turn_done + alive:false 7 行 | 6.2 TURN_DONE unknown ✓，布尔表达不了（R4） |

## 7. MISSION / ADR 修订草案（属 (b) 的条目；随 agora-5gg.17 落地，裁决项定了再改）

**MISSION §4.2 数据模型**

- `ended_at`：改为「**该行结束的时刻**」——运行时报的退出时刻（准）；hook SessionEnd 的事件时刻；superseded 时新对话首条事件的时刻；探活发现进程没了的那个 tick（`ended_at_approximate`）。external 行必须写。
- `created_at`：改为「**agora 第一次得知这一行存在的事件时刻**」——agora 起的是 create 时刻，hook 登记的是登记那条信封的 `received_unix_ms`，不是重放时刻。
- 实时事实新增 `process: alive | gone | unknown`（Q4 选 A 时替代 `alive`；选 B 时并存）；FINISHED / FAILED 行 `process` 恒 `gone`。
- 新增 `end_cause` / `unknown_cause` 枚举（见 ADR-002 D1 修订），`reason` 降为人读的一句话。

**MISSION §4.3 状态定义**

- STARTING 行：「hook 层的 STARTING 最多停 10 s」**不分 origin**：external 行同样衰减为 TURN_DONE `awaiting first prompt`。
- TURN_DONE 行：「进程仍在」改为「进程仍在**或 agora 不知道进程在不在**（`process = unknown`）」。
- UNKNOWN 行：改成封闭清单，每一种带出口——`runtime_unavailable`（运行时降级；恢复即自愈）、`hooks_silent_screen`（hook 沉默且屏幕像在等人；打开终端 / 查 hook）、`prompt_gone`（挂起的提示消失；下一条 hook）、`hooks_silent_no_handle`（无进程号且沉默 ≥ `external_silent_after`，按事件时刻；下一条 hook 或 TTL 淘汰）、`no_observation`（只允许瞬时）。**运行时会话不在不是 UNKNOWN**，是 FINISHED `runtime_gone`。

**MISSION §6.3**：「同分再按等待时长」限定为 WAITING / FAILED；TURN_DONE 段内按 Q5 的裁决。

**ADR-002 D1 修订段（带日期与 agora-5gg）**

- 进程层新增结论：有 `runtime_ref` 而运行时列表里没有该会话（server 不在或会话被杀）→ FINISHED（process，conf 0.8，`end_cause = runtime_gone`）；`ServerUnavailable` 等降级仍是 UNKNOWN `runtime_unavailable`。
- hook 层的时间规则（STARTING 宽限、沉默）对无句柄 external 行同样生效；无句柄行的 `external_silent_after` 以最近一条 hook 事件**自己的时刻**（`at`）计，不以 daemon 收到时刻计。
- `end_cause` 封闭枚举：`exit_code | signal | killed_by_user | host_session_end(clear|resume|logout|exit|other) | superseded | process_gone | runtime_gone`；通知规则按 `end_cause` 分支，不按 reason 字符串。

**ADR-002 D7 修订段**：无句柄 external 行在收到第一条 `prompt.submitted` 之前就 SessionEnd(resume | clear) 的，是身份交接不是会话，不留行。宿主内部子对话与无头一次性会话按 Q3 的裁决定义 `origin`（若选 B：新增 `origin = headless`）。

**ADR-004 补一句**：peer 行的等待时长是本节点时钟的下界；本机重启后按 Q（agora-5gg.12）的做法恢复。

**ADR-002 D3**：daemon 起来之后必须再扫一次投递箱，且 sweep 周期性兜底扫 inbox（幂等，按文件名去重）。

## 8. epic 拆分（beads）

epic **agora-5gg**（P1）。子任务（`bd show` 里各有 `--acceptance` 与依赖）：

| id | 类型 | P | 条目 | 阻塞于 |
|---|---|---|---|---|
| agora-5gg.1 | bug | 1 | A3 重放窗口内事件滞留 inbox | — |
| agora-rkl | bug | 2 | A1 / §3.1 external STARTING 不衰减 | — |
| agora-5gg.2 | bug | 2 | A4 无句柄沉默时钟按事件时刻 | — |
| agora-u5p | bug | 2 | A2 / §3.2 runtime gone → FINISHED | — |
| agora-5gg.3 | bug | 2 | B4 ① / B5 external 行 created_at / ended_at | — |
| agora-5gg.4 | 决策 | 2 | Q4 alive 三态 | 人 |
| agora-5gg.5 | task | 3 | B3 external 行 pid | 5gg.4 |
| agora-5gg.6 | task | 2 | B1 reason 词表封闭 | 5gg.4 |
| agora-5gg.7 | 决策 | 2 | Q3 子对话 / 无头会话 | 人 |
| agora-5gg.8 | 决策 | 2 | Q2 身份 = 对话还是进程 | 人 |
| agora-5gg.9 | 决策 | 2 | Q1 external 生命周期 | 人 |
| agora-5gg.10 | 决策 | 2 | Q5 TURN_DONE 排序 | 人 |
| agora-e08 | bug | 2 | §3.3 无句柄 UNKNOWN 的出口 | 5gg.9 |
| agora-29n | bug | 3 | §3.4 resume 幽灵行 | 5gg.8 |
| agora-5gg.11 | task | 3 | §3.7 四段分栏 | 5gg.10 |
| agora-5gg.12 | task | 3 | B4 ② peer 行时长下界 | — |
| agora-5gg.13 | task | 3 | C4 Codex / Grok 包装文本 | 5gg.7 |
| agora-5gg.14 | chore | 4 | C5 孤儿检查点 | — |
| agora-5gg.15 | task | 3 | D1 macOS launchd 单元 | — |
| agora-5gg.16 | task | 2 | §6 真值表守卫 | 5gg.4、u5p、5gg.3、rkl |
| agora-5gg.17 | task | 2 | §7 MISSION / ADR 修订落地 | 四个决策 + 5gg.10 |

## 9. 需要裁决

| # | 问题 | 选项 | 建议 |
|---|---|---|---|
| Q1 | external 行留多久、谁来收 | A 一条规则：离开「要人」状态（FINISHED 或无句柄 UNKNOWN）≥ ttl 即删（缺省 24 h）；B 分档（superseded 1 h / ended 24 h / UNKNOWN 6 h）；C 只折叠不删 | **A**：简单，与 §4.6「external 行只有两行记录」一致 |
| Q2 | 身份是对话还是进程 | A 维持对话即身份，UI 把同进程 / 同目录的 superseded 旧行折进当前行的历史；B 身份改进程、换对话 = 换 epoch（改 D7，Restart 的 resume 依据跟着换）；C 现状 + 缩短 superseded TTL | **A**：D7 的理由仍成立（Restart 要 resume 具体对话），只改呈现 |
| Q3 | 宿主子对话、无头一次性会话 | A 不登记；B 登记为 `origin = headless`、默认折叠、不通知、24 h 删；C 照常 | **B**：它们确实在跑、偶尔会挂权限，但不该与人的会话争 NEEDS ATTENTION。判据只用结构信号（Claude 无头缺 `model` / `scratchpad_dir` 键已实录；Codex 需先录 fixture） |
| Q4 | `alive` 三态、FINISHED 行带不带 | A 改名 `process: alive\|gone\|unknown`，FINISHED 一律 gone；B 保留布尔另加 `liveness`；C 只把 FINISHED 行 alive 置 false | **A**，api_version bump minor、旧字段保留一版过渡 |
| Q5 | TURN_DONE 排序 | A 段内最新完成在上；B 「看过」扩到 TURN_DONE；C 只淡显 | **A + B** |

## 10. Mac 上要跑的核实命令（只读）

```bash
URL=$(~/.agora/bin/agora url | grep -o 'http[^ ]*' | tail -1); TOK=${URL##*#pair=}
curl -s -c /tmp/cj -H 'Origin: http://127.0.0.1:7680' -H 'Content-Type: application/json' \
  -X POST http://127.0.0.1:7680/api/auth/pair -d "{\"token\":\"$TOK\"}" >/dev/null
curl -s -b /tmp/cj http://127.0.0.1:7680/api/sessions | python3 -c '
import json,sys
for s in json.load(sys.stdin)["sessions"]:
    if s["node"]!="zuan": print(s["local_id"],s["status"],s["origin"],s["runtime_ref"],s["pid"],s["alive"],s["reason"])'
# B1：683eb6 那份信封有没有 CODEX_INTERNAL_ORIGINATOR_OVERRIDE
jq '.envelope.agent_env | keys' ~/.agora/hooks/done/codex/<683eb6 的 agent_session_id>/*.json | head
# A3：那 6 个滞留文件的事件名与宿主 timeout
jq -r '.payload.hook_event_name' ~/.agora/hooks/inbox/claude/6a72ef31-*/*.json
# C1 / C2：Codex Desktop 子代理与 exec 的 SessionStart 载荷键集合（录成 fixture 用）
jq -c '.payload | keys' ~/.agora/hooks/done/codex/*/*.json | sort | uniq -c | sort -rn | head
```

跑完用 `~/.agora/bin/agora auth devices` 找到 `curl` 那台设备并 `auth revoke`。
