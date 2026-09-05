# ADR-002: Agent 状态来源分层

- 状态：**Accepted**（2026-09-02；用户验收 D1–D10，其中投递方式、Grok 权限决定只能在终端答、hook 沉默规则三点经单独确认）
- 日期：2026-09-02
- beads：`agora-90t.3`（输入清单、Grok 与 Claude Code 的 hook 实测记录都在该 issue 注记里）
- 依赖：MISSION §1.2（respond 不经终端）、§2.2 不变量 10、§3.3、§3.4（hook 事件不得丢失）、§4.3–§4.4（状态与状态机）、§5（全部）、§6.3（两行预览与就地 respond）、§7.3（respond 两种语义、hook 投递端点）；ADR-001（`LaunchSpec.env`、`Exit` 形态、capture 参数）；ADR-003 消费本文 D3（hook 投递不走 HTTP 端点）。

## Context

agora 区别于 Web Terminal 的能力只有一件：知道哪个 agent 在等人（§5）。devcenter 把这件事建在 `capture-pane` 文本 + 正则上——对 Claude Code 可用但脆弱（全 tail `contains`、无防抖、无真正状态机、Claude 一次 UI 改版即失效），对 Codex / pi 未实现（报告 §3.5 第 1 条、附录 A §2b、§5）。它自己的 SessionStart hook drop-box 已证明结构化事件源可行，但只用了这一个 hook。

MISSION v0.10 已把"hook-first 是 V1 架构"定为前提（§5.1），依据是三个一等 agent 都有文档化的 hook 接口。本文写作前把这个前提再钉实一遍（附录 A）：

- **Claude Code 2.1.258**：33 个 hook 事件（文档）；本机实测 SessionStart / UserPromptSubmit / PreToolUse / PostToolUse / **PermissionRequest** / Notification(`permission_prompt`) / Stop / SessionEnd 的 payload，**Stop 带 `last_assistant_message`**，PermissionRequest hook **挂起期间 TUI 同时显示权限提示**，hook 返回 `allow` 即放行。
- **Grok 1.0.13**：事件集比 Claude Code 还全，payload 双写 camel / snake，Stop 带 `lastAssistantMessage`，`GROK_SESSION_ID` ≡ `--resume` 的 id；但 PreToolUse 的 `allow` **不能替用户批准**（文档原话："an allow means only not blocked"），且默认兼容加载 `~/.claude/settings.json` 里的 hooks。
- **Codex 0.152.1**（本机实际版本；`docs/spec/instance.md` 原记的 0.145.0 是过期的更新检查记录）：hooks 引擎 stable 且默认开启（`codex features list`），12 个事件含 **`PermissionRequest`（可代答）** 与 `Stop`（带 `last_assistant_message`）；**没有 Notification 事件**；hook 必须在 TUI 里 `/hooks` 审阅、按内容哈希信任；legacy `notify` 只有 `agent-turn-complete`、JSON 作最后一个 argv、源码注释写明计划移除。MISSION §5.1 "三个一等 agent 全有 hook" 的前提成立，且 devcenter 时代"Codex hook 不可注入"的判断已过时。

解空间的硬约束：有 hook 的 agent 的 WAITING / TURN_DONE 只认 hook（§5.1）；hook 事件在 daemon 不在时不得丢失（§3.4）；决定经挂起的 hook 同步返回、不注入键击（§1.2、§5.6）；程序不得靠给人看的文本做判断（规则 10），文本层是兜底不是判断依据；宁可 UNKNOWN 不要误报 WAITING（§5.3）。

## 决策问题

状态（以及两行预览、任务摘要、对话身份这些 L2 标签层原料）从哪些来源获取；优先级与冲突裁决；hook 事件怎么投递、怎么装、怎么挂起等答复；无 hook 时怎么兜底；Adapter 边界与测试形态。

## 备选项

| 来源 | 信号 | 可靠性 | 覆盖 | devcenter 的经验 |
|---|---|---|---|---|
| **agent hook / 结构化事件** | 会话起止与 id、prompt 提交、工具调用、权限请求、一轮结束（含最后一条回复）、空闲、API 错误 | 高：agent 自己说的，带 session id 与时间戳 | Claude Code、Grok 全覆盖；Codex 见 D2；generic shell 无 | 只用了 SessionStart（`--settings` 注入、`cat && mv` 原子写）；§6.2 建议扩到 Notification / Stop / PreToolUse |
| **进程状态** | 存活、退出码 / 信号、pid | 高但语义粗：只知道死活 | 通用；运行时内的会话有退出码，外部会话只有存活 | tmux `remain-on-exit` + `pane_dead_status`（ADR-001 实测） |
| **transcript / 会话文件** | Claude jsonl（`last-prompt`、assistant text、`ai-title`）、Codex rollout、Grok sessions | 中：私有格式随版本漂移 | L2 原料，不是状态 | `recap` 两行倒序扫 jsonl；搬迁功能强绑定格式后一路踩坑（报告 §3.4 搬迁行） |
| **屏幕文本** | capture-pane 末尾窗口 + 模式 | 低：UI 改版即失效，scrollback 污染 | 通用兜底 | 全 tail `contains` 的反例见附录 A §2b；generic 的"只看末尾两行"是对的 |
| **活动启发式** | 输出有无变化 + 时长 | 低，只能说"安静了" | 通用 | `idle_after` 阈值；resize / attach 造成的重绘要排除 |

## Decision

### D1 四层来源与裁决规则

```
1. hook 事件        有 hook 的 agent 的 WAITING / TURN_DONE 唯一来源；conf 0.95–1.0
2. 进程状态         FINISHED / FAILED 唯一来源；conf 1.0；压倒一切
3. 屏幕文本         无 hook 的 agent 才能产生 WAITING（conf ≤ 0.8）；有 hook 的 agent 只做预览
4. 活动启发式       IDLE 的唯一来源（conf 0.6）；有 hook 的 agent 走"hook 沉默"规则
```

裁决规则（都有测试，见守卫）：

- **进程退出压倒一切**：`Exit::Code(0)` → FINISHED，`Code(n≠0)` → FAILED，`Signal(x)` → FAILED；**agora 自己 `terminate` 的会话**按信号退出、或以壳的 128+signo 码退出（129/130/143，agent 自己捕获 SIGTERM 后以 143 退出，2026-09-04 Claude 2.1.260 实测，agora-3ib）报 FINISHED，reason `killed by user`（Kill 是人的决定，不该拿 attention 100）；其它非零码即使按过 Kill 也是 FAILED。退出后任何 hook 事件只更新 metadata（如 SessionEnd 的 reason），不改状态。
- **有 hook 的 agent**：文本层永远不能把状态抬到 WAITING / TURN_DONE；活动层不能产生 IDLE。它们只提供 pane preview。
- **hook 沉默规则**（有 hook 的 agent 的兜底）：SessionManager 在达到沉默阈值后才 capture 屏幕并交给 Adapter；hook 健康时不 capture，有 hook 的会话不展示 pane preview。文本结果只能导致 UNKNOWN，绝不能抬到 WAITING / TURN_DONE（agora-uez：守卫须从 SessionManager 入口覆盖，单测直接喂 Machine 文本会漏掉接线错误）。进程活着、`hook_silence_after`（默认 10 min）内没有任何 hook 事件、且文本层看见了空闲提示符或权限提示 → **UNKNOWN**，reason `hooks silent`。这是"宁可 UNKNOWN"的落点：hook 没装好、被 `disableAllHooks` 关掉、或 agent 升级改了事件名时，Dashboard 显示"看不清"而不是永远 RUNNING，也不会误报 WAITING。补充（2026-09-05，agora-dvh.15）：沉默规则要等 10 min 且屏幕像在等人，对"hook 从来没接上"太迟钝——Codex 未在 `/hooks` 信任、`disableAllHooks`、二进制路径失效都表现为一条事件都没有。所以另有一条更早的提示：声明了 hook 的会话在本代起始 10 s 宽限之后有过真实终端输出（不算 TUI 启动那波，因为 Codex 的 SessionStart 到第一条 prompt 才 fire）、再过 `hooks.unheard_after`（默认 90 s）仍无任何事件 → `SessionView.hooks_unheard` 带一句指向 `/hooks` 或重装的话（不改状态，状态仍按进程层）；第一条事件到达即撤。守卫 `tests/state_machine.rs::hooks_never_heard_after_terminal_activity_is_flagged_but_not_at_startup`。
- **epoch 与顺序**：每个会话有 `epoch`（ADR-001 `create` 为 1，每次 `respawn` +1，经 `LaunchSpec.env` 的 `AGORA_EPOCH` 交给 agent，hook 原样带回）；epoch 小于当前的事件丢弃；同 epoch 内按投递箱文件名（时间戳 + 序号）顺序应用。
- **驻留时间**：hook 事件立即生效；文本层的 WAITING 需要连续 2 个 tick 证据一致；IDLE 需要 `idle_after`（60 s）无输出；低层来源不得覆盖 30 s 内由高层写入的状态。resize / attach / detach 引起的重绘不算活动（devcenter 的 `(width,height)` 教训，外加 attach 计数变化）。
- **每个状态值带 `source`（hook / process / text / heuristic）与 `confidence`、`reason`**；UI 不显示 confidence，但 API 返回，日志记录。

### D2 事件最小集合与各 agent 映射

agora 内部事件（§5.6 的集合，补两个）：`session.started` / `session.id` / `prompt.submitted` / `activity` / `input.needed` / `decision.needed` / `turn.ended` / `turn.failed` / `session.ended`，新增 `idle`（agent 自报的空闲）与 `decision.resolved`（挂起的决定被终端或 hook 解决）。

| agora 事件 → 状态 | Claude Code（文档 + 2.1.258 实测） | Grok（1.0.13 实测） | Codex（0.152.1 文档 + 二进制核对，未实测；附录 A） | generic shell |
|---|---|---|---|---|
| session.started → STARTING | `SessionStart`（`source`: startup / resume / clear / compact / fork） | `SessionStart` | `SessionStart`（`source`: startup / resume / clear / compact） | 进程创建 |
| session.id（每次命中覆盖 `agent_session_id`） | `SessionStart.session_id`；`/clear` `/resume` 再触发带新 id（devcenter 实测）；`transcript_path` 同来 | `GROK_SESSION_ID` ≡ payload `sessionId` ≡ `--resume` 参数；`transcriptPath` | `session_id` ≡ rollout 文件名尾部 uuid ≡ `codex resume <id>` 参数；`transcript_path` 指向 rollout | — |
| prompt.submitted → RUNNING；首条 → `task_ref` 摘要 | `UserPromptSubmit.prompt` | `UserPromptSubmit.prompt` | `UserPromptSubmit.prompt` | — |
| activity → RUNNING 行"正在做什么" | `PreToolUse` / `PostToolUse`（`tool_name`、`tool_input`）；pane title（OSC 2，实测随任务变："✳ probe-b.txt file creation"） | 同左（`toolName`） | `PreToolUse` / `PostToolUse`（`tool_name`: Bash / apply_patch / `mcp__<server>__<tool>`…、`turn_id`） | pane 输出变化 |
| decision.needed → WAITING(decision) | **`PermissionRequest`**（挂起，返回 `decision.behavior` allow / deny）；`Notification(permission_prompt)` 作为确认（实测在挂起期间也 fire） | `Notification(permission_prompt)`（只在权限 UI 真等人时 fire）；**不挂起**：PreToolUse 的 allow 不能替用户批准，`respond_via = terminal` | **`PermissionRequest`**（挂起，`decision.behavior` allow / deny；文档："no matching hook decides → normal approval flow"）；没有 Notification 作确认；`approval_policy = never` 时永不 fire（本机现状） | 文本模式（D6） |
| input.needed → WAITING(question) | `PreToolUse` 匹配 `AskUserQuestion` → 直到同 `tool_use_id` 的 `PostToolUse`；`Elicitation` | 同类工具（adapter 表） | **无**：`requestUserInput` 只在 app-server 协议里，hook 看不到 → 只显示 pane 预览 | 文本模式（D6） |
| turn.ended → TURN_DONE；`↳` 行 = 最后一条回复 | `Stop`（`last_assistant_message`、`stop_hook_active`） | `Stop` **且 `reason == end_turn`**（session 结束与续跑也 fire，文档明说要过滤）；`lastAssistantMessage` | `Stop`（`last_assistant_message`、`stop_hook_active`）；legacy `notify` 的 `agent-turn-complete` 同一时刻也发，**不用**（计划移除） | — |
| turn.failed → TURN_DONE，reason 带错误类型 | `StopFailure`（matcher: rate_limit / authentication_failed / … 11 种） | `StopFailure`（6 种）、`StopCancelled`（user_interrupt / permission_rejected / …） | `Interrupt`（0.150+）→ 中断；**无 StopFailure**：API 错误只能从 Stop 缺席 + hook 沉默规则兜底 | — |
| idle → TURN_DONE 的确认 / 补漏 | `Notification(idle_prompt)`（~60 s） | `Notification(idle_prompt)`（约 1 min，文档建议以它为准） | **无**：TURN_DONE 只靠 `Stop` | 活动启发式 → IDLE |
| session.ended → 等进程退出定 FINISHED / FAILED | `SessionEnd`（`reason`: clear / resume / logout / prompt_input_exit / other） | `SessionEnd` | `SessionEnd` | 进程退出 |

**没有的能力就写没有**：Adapter 声明 `decision_via_hook`（Claude true、Grok false、Codex true——前提是 hook 已在 `/hooks` 信任且 `approval_policy ≠ never`）；false 时 WAITING(decision) 行的按钮是"打开终端"，不是 allow / deny。这不是妥协——注入 `y` / `1` 是 MISSION §1.2 明令禁止的。

### D3 投递：投递箱文件 + unix socket 唤醒；hook 命令是 agora 自己

hook 命令统一为 `agora hook --host <claude|grok|codex>`（安装时写成 `if [ -x <稳定路径> ]; then exec <稳定路径> hook --host claude --home <AGORA_HOME>; fi`，形态见 D4；卸载或升级期间二进制不在也不会让 agent 报错——orca 在本机就是这么装的）。它做三件事：

1. **落盘**：把 stdin 的 payload 连同信封（host、`AGORA_SESSION_ID` / `AGORA_EPOCH` 若有、`CLAUDE_PID` / `GROK_*`、`TMUX` / `TMUX_PANE`、hook 进程的 ppid、本地时间）写到 `<state_dir>/hooks/inbox/<host>/<agent_session_id>/<ts>-<seq>.json`（`<state_dir>` = `AGORA_HOME`，默认 `~/.agora`，ADR-003 D6；hook 进程不依赖环境里有 `AGORA_HOME`——Terminal.app 里起的外部会话没有它——安装写入的命令显式带 `--home`，D4），先写 `.part` 再 rename——daemon 不在也不丢（§3.4）。
2. **唤醒**：连 `<state_dir>/agora.sock`，送文件路径；socket 不在（daemon 没起）→ 直接 exit 0。daemon 只读文件，不信 socket 上的内容——单一真相。
3. **挂起**（仅 `decision_via_hook` 的事件）：连上 socket 后等 daemon 回 allow / deny / none；none、超时、socket 断（daemon 崩）→ exit 0 不输出——**fail-open**，TUI 的提示还在，人照样能在终端答。

daemon 侧：启动时先恢复 `hooks/state/` 的每会话 hook 观测检查点，再按文件名顺序重放 inbox（§3.4）。检查点格式带 `version = 1`，只含 hook 状态、摘要、待回答工具键、epoch、原观测时间与最后已处理文件名，不含进程存活、退出码或挂起连接，也不进入 SQLite。每次应用 hook 后以 0600 临时文件写入、sync、原子替换检查点，成功后才将投递移到 `done/`（保留 24 h 排障）；重复或更旧的同 epoch 文件不再次应用，旧 epoch 检查点不恢复。运行时退出 / 缺失的事实始终覆盖恢复状态。首次升级尚无检查点的会话从现存 `done/` 按序补建；归档已删除的历史无法凭空恢复，下一条 hook 会建立检查点。检查点不受 24 h 归档清理影响。2026-09-05 review 反例（agora-9dj）：只重放 inbox 会让已消费 Stop 的 TURN_DONE 在重启后永远退回 RUNNING。**不再有 `POST /api/hooks/:agent`**：unix socket 仅属主可访问 + 对端 uid 校验（ADR-003 D6）就是"这台机器上的这个用户"，多用户主机上比 loopback 端口 + 一次性 token 更准确（§0.1、ADR-003 少一个要守的端点）。Windows 时代换 named pipe，同属第二运行时（ADR-001 D9）。

为什么不选 devcenter 的 `--settings` 注入：它只覆盖 agora 起的会话，看不见 Terminal.app 里起的（A16）；而 §5.1 已定 hook 装进用户配置。为什么不选 HTTP hook（Claude / Grok 都支持 `type: http`）：daemon 不在就丢事件，正是 §3.4 禁止的。

### D4 安装：装进用户配置，幂等、可卸载、装前 diff、宿主自认

- `agora hooks install <agent>`：Claude Code 写 `~/.claude/settings.json`（user 作用域）的 `hooks`，Grok 写 `~/.grok/hooks/agora.json`，Codex 写 `~/.codex/hooks.json`（**Codex 的非托管 hook 必须由用户在 TUI 里 `/hooks` 审阅、按内容哈希信任，改动即失效**——所以 command 里的 agora 路径必须是升级不变的稳定路径，否则每次升级都要重新信任；agora 不用 `--dangerously-bypass-hook-trust`）；装前显示 diff，只增不删别人的条目（hooks 是拼接不是替换，文档确认）；条目里 command 含 agora 二进制路径，是识别自己条目的标记——重复安装不重复，卸载只删自己的。Claude Code 与 Grok 都热加载配置文件（文档：file watcher / Hooks tab reload），装完不用重启 agent。
- **命令形态**：`if [ -x <AGORA_HOME>/bin/agora ]; then exec <AGORA_HOME>/bin/agora hook --host <agent> --home <AGORA_HOME>; fi`。`exec`（2026-09-04 补）：让 agora 顶替 `sh -c` 那层，hook 进程的 ppid 就是 agent 本体——Grok 的环境里没有进程号变量，外部 Grok 会话的存活只能靠 ppid（实测不带 `exec` 时 ppid 是 sh）。二进制走 `<AGORA_HOME>/bin/agora` 这个稳定路径（安装与升级维护的符号链接，A26 / A39，ADR-003 D6），Codex 的内容哈希信任才不会随升级失效；对 Claude / Grok 则是升级不静默断 hook——二进制位置一变，`if [ -x … ]` 守卫会静默跳过，会话只会经 hook 沉默规则退成 UNKNOWN 而没人知道原因。`--home` 显式给出，因为外部会话的环境里没有 `AGORA_HOME`。三家命令形态一致，宿主自认（下文）与"识别自己的条目"只需一种写法。
- 每个事件的 `timeout` 显式写死：PermissionRequest 3600 s（挂起上限，D5；Codex 60 s，见 D5）；SessionEnd 1 s（Claude 给全部 SessionEnd hook 共 1.5 s 预算，所以 SessionEnd 只落盘不唤醒等待）；其余 20 s。不依赖各家默认值（Claude 600 s、Grok 5 s、UserPromptSubmit 30 s，三处都不一样）。
- **agora 起的会话不再重复注入**（§5.1）：身份靠 `LaunchSpec.env` 的 `AGORA_SESSION_ID` / `AGORA_EPOCH`，hook 进程继承环境后带回（实测 hook 能看到 agent 进程的全部环境）。
- **Grok 兼容加载的串扰**：Grok 默认也执行 `~/.claude/settings.json` 里的 hooks，agora 装给 Claude 的条目会被 Grok 以 `--host claude` 跑一遍。对策：`agora hook` 发现 `GROK_SESSION_ID` 存在而 `--host` 不是 grok → exit 0；反之亦然。不靠 payload 键名风格猜宿主。
- 外部会话（没有 `AGORA_*` 环境）：信封里的 `TMUX` / `TMUX_PANE` 直接指向 pane → 若该 socket 在 ADR-001 的 `adopt_sockets` 里，会话可采纳且有终端（A16 与 A22 合流）；没有 tmux 的（Terminal.app 裸跑）→ `origin = external`：没有终端与文本输入，状态、两行、通知照常，D5 的挂起与 allow / deny 也照常（挂起不依赖终端）；存活靠 `CLAUDE_PID` / hook ppid 的 `kill(pid, 0)`，没有退出码。

### D5 respond：挂起、上限、超时、与终端并存

- **挂起的决定**以 `(session, tool_use_id)` 为键；Dashboard 另用每次挂起随机生成的 `request_id` 绑定实例（不复用工具名），会话返回 `pending_decision = { request_id, summary, epoch }`，显示与提交用同一对象。旧 ID、已超时 ID、旧 epoch ID 一律拒绝，不能回退到另一请求；挂起解除后对象切换到剩余请求或 null。daemon 重启只恢复待回答状态，不恢复挂起连接或批准按钮。2026-09-05 review 反例（agora-9cf）：最后一条 Write 覆盖 detail，但不带 ID 的 Allow 实际批准最早的 Bash。
- **并行上限**：一个会话可有多个（Claude 并行工具调用会同时 fire 多个 PermissionRequest，文档有 `PostToolBatch` 为证），每会话上限 8，节点上限 256；超限的 hook 立即 exit 0（不输出）。
- **与终端并存**：实测 Claude Code 在 hook 挂起期间照常显示权限提示——终端里答了，TUI 走自己的路；Dashboard 答了，hook 返回决定。所以不需要"有人 attach 就放弃挂起"的逻辑。挂起在下列任一发生时自动解除（`decision.resolved`）：同 `tool_use_id` 的 PostToolUse / PostToolUseFailure / PermissionDenied 到达（终端答了）、Stop / SessionEnd 到达、进程退出、超时。
- **超时**：默认 55 min（安装的 hook timeout 3600 s 减余量；Claude 文档没写超时后是 allow 还是 block，所以 agora 永远在 agent 超时前自己退出）。宿主可更短（`AgentHooks::hold_timeout`）：Codex 0.152.1 实测挂起期间 TUI 不显示审批提示（附录 A），挂起是独占而非并存，上限 20 s，超时 fail-open 把提示交回终端；API 的 `respond_within_secs` 把它告诉 UI。
- **Dashboard 上的三个动作**：allow、deny（可带一句 message，走 `decision.message`）、"在终端回答"（解除挂起并打开终端）。`updatedInput`、`permission_suggestions`（如"本会话改为 acceptEdits"）V1 不暴露。
- **WAITING(question)**：`AskUserQuestion` 类工具的选项渲染在 TUI 里，从 Dashboard 选项等于注入键击 → V1 只显示问题文本（来自 `tool_input`）与"打开终端"；自由问答与下一条指令走 `POST /api/sessions/:id/input` 的 text（PTY）。
- **respond 路由**：decision 只对有挂起的会话有效，其余返回错误类型 `NoPendingDecision`；text 对任何有终端的会话有效。

### D6 文本兜底与活动启发式（只服务 generic shell 与采纳的未知会话）

- 只看 capture 末尾 **8 个非空行**（不是全 tail；devcenter 的反例：scrollback 里 `cat` 出来的源码把会话钉在 WAITING）；ADR-001 的 capture 是 200 行 `-J`，多出来的只喂预览。
- WAITING 模式（沿用 devcenter v1 启发式，大小写不敏感，行尾锚定）：`[y/N]`、`(y/n)`、`Do you want to proceed?`、`Would you like`、`Approve`、`Allow`、`Continue?`、`Press Enter`、以及以 `?` 结尾且随后无输出 ≥ 2 tick；conf 0.8（裸 `?` 问句 0.7）。模式行可以不在最后一行，但它下面只能是选项行（`❯ 1. Yes` 之类）——下面已有普通输出或新提示符说明那句早答过了（agora-dvh.8 落地时补，反例 `testdata/generic/pane/question_in_history.txt`）。密码提示（`password:`、`[sudo] password for x:`、`Enter passphrase …:`）同样是 WAITING，reason 标 `secret`，提示行内容不进 reason、UI 不回显。
- RUNNING：两次 capture 内容哈希不同（排除尺寸变化与 attach 计数变化）；IDLE：`idle_after` 60 s 无变化且无 WAITING 模式；conf 0.6。
- 有 hook 的 agent 走这条路径的唯一出口是 D1 的"hook 沉默 → UNKNOWN"。

### D7 对话身份

- `session.id` 每次命中覆盖 `agent_session_id`（§5.6）：Claude `/clear` `/resume` 会再 fire SessionStart 带新 id（devcenter 实测），Grok 同理；Codex 的 `/clear` 与 resume 也 fire SessionStart（`source` = clear / resume），交互式下是否可靠见 `agora-dvh.2` 实测。
- Restart 的 resume 参数由 Adapter 按 `(agent_version, agent_session_id)` 生成（Claude `--resume <id>`、Grok `--resume <id>`、Codex `resume <id>` 子命令）；**绝不 `--continue` / `--last`**（A32）。
- 识别顺序：agent 自报 > 启动时钉死的 id（Claude `--session-id <uuid>`，devcenter 做法；只在自报缺席时用）> 用户从候选里挑；**绝不按 mtime 猜**。
- **agent 可用性三值**（可用 / 不可解析 / 缺失）：只解析 `--version` 这一个文档化输出（实测：`2.1.258 (Claude Code)`、`codex-cli 0.152.1`、`grok 1.0.13 (…)`），**不解析 `--help`**——flag 有无由 Adapter 的版本表回答，版本表没有的版本报"不可解析"，这就是规则 10 与三值的边界。

### D8 两行预览与任务摘要的来源

- `❯` 行 = 最近一次 `prompt.submitted` 的首行（hook）；`↳` 行 = `turn.ended` 的最后一条回复（hook，Claude / Grok 都带）或 `activity` 的当前工具。**两行都不读 transcript**；读不到 hook 的会话（generic、external 无 hook）保持一行 pane preview（§6.3 已允许）。
- `task_ref` 摘要 = 首条 `prompt.submitted` 的首行（无 bd 时）。
- transcript 文件（`transcript_path`）V1 只存路径不解析；Claude jsonl 的 `ai-title` / `last-prompt` 记录是将来 L2 的候选，纳入时按版本钉 fixture。

### D9 Adapter 接口：三个 trait

```
AgentIdentity {                      # 对话身份与启动
    name(), match_process(tree), default_command(),
    version(output_of_version_flag) -> Version | Unparsable,
    resume_args(version, agent_session_id) -> [arg] | Unsupported,
    pin_args(version, new_id) -> [arg] | Unsupported
}
AgentHooks {                         # 有 hook 的 agent 才实现
    install_spec() -> [HookInstall{file, event, matcher, timeout}],
    decision_via_hook() -> bool,
    parse(raw: Envelope + payload) -> [AgoraEvent]   # 一条原始事件可映射成多条
}
AgentFallback {                      # 所有 agent 都有默认实现
    detect(tail_lines: [str], activity: ActivitySample) -> DetectionResult{state, confidence, reason}
}
```

核心状态机只吃 `AgoraEvent` 与 `DetectionResult`，不知道任何 agent 的 payload 形状（规则 5）。

### D10 fixture 驱动测试的形态

- `testdata/<agent>/<version>/hooks/<scenario>.jsonl`：从真实 agent 录下的 hook 事件序列（`agora hook --record` 开关；内容脱敏，保留键与枚举值），场景至少：一轮完成、权限挂起后终端答、权限挂起后 Dashboard 答、`/clear`、API 错误、被中断、并行工具。回放器把序列喂给 Adapter + 状态机，断言状态序列与 `source` / `confidence`。
- `testdata/<agent>/<version>/pane/*.txt`：屏幕文本 fixture，只给文本层与预览。
- fake-agent（ADR-001：agora 二进制的子命令）**走真实 `agora hook` 路径**发事件，集成测试里跑在真实 tmux 上，覆盖投递箱落盘、daemon 重启重放、挂起 / 答复 / 超时。
- 版本漂移守卫：CI 里对本机安装的真实 agent 只跑一个冒烟（SessionStart + Stop 的键集合与 fixture 一致），不一致 → 测试红，提示录新 fixture。

## Non-Goals

- 不解析 transcript 做状态或预览（V1 只存路径）；不做对话索引、摘要、token / 成本（§11）。
- 不替 agent 做 TUI 选择题（AskUserQuestion 的选项）；不注入任何键击当作 respond。
- 不暴露 `updatedInput`、权限模式切换等"替用户改 agent 行为"的能力。
- 不定义 peer 之间的事件传播（ADR-004 已定：只导出本机会话，事件随 `/api/events` 走）。
- 不做 hook 的 HTTP 投递、不做 `POST /api/hooks/:agent`。

## 什么会让它变危险

- **文本层把有 hook 的 agent 抬到 WAITING** → 高频误报，正是 §5.3 最怕的。守卫：状态机对 `has_hooks` 会话拒绝 text 来源的 WAITING / TURN_DONE → `tests/state_machine.rs::text_cannot_raise_hooked_session`。
- **hook 事件在 daemon 不在时丢失** → daemon 重启的几秒里 agent 停下提问，从此永远 RUNNING。守卫：集成测试杀 daemon、fake-agent 发事件、起 daemon、断言重放后状态正确 → `tests/hooks_inbox.rs::events_survive_daemon_restart`。
- **旧 epoch 的事件污染新进程** → Restart 后前一轮的 Stop 把新会话标成 TURN_DONE。守卫：`tests/state_machine.rs::stale_epoch_dropped`。
- **hook 挂起把 agent 卡死** → daemon 崩了 hook 还在等。守卫：hook 进程在 socket 断开或超时时 exit 0；测试杀 daemon 断言 hook 在 1 s 内退出 → `tests/hook_cmd.rs::hold_releases_on_daemon_death`；以及安装的 timeout 永远大于 agora 的挂起上限 → `tests/hooks_install.rs::timeout_exceeds_hold`。
- **Grok 兼容加载让同一事件送两次或送错宿主** → 状态抖动、错误的 session id。守卫：`tests/hook_cmd.rs::host_mismatch_exits_silently`（`GROK_SESSION_ID` + `--host claude` → 无输出无文件）。
- **hook 没装 / 被关 / 事件名改了而无人知道** → 永远 RUNNING。守卫：hook 沉默规则 → `tests/state_machine.rs::silent_hooks_become_unknown`；真实 agent 冒烟测试比对键集合。
- **Adapter 解析 `--help` 或错误文本** → 规则 10 失守。守卫：源码扫描 Adapter 目录没有 `--help` 字面量 → `tests/arch_boundary.rs`。
- **核心层知道某个 agent 的 payload 键** → 规则 5 失守。守卫：`session/` `status/` 目录不得引用 `hook_event_name` / `hookEventName` 等键名 → `tests/arch_boundary.rs`。
- **投递箱被其他用户写入或读取** → 伪造事件 / 泄漏 prompt。守卫：`<state_dir>/hooks` 与 socket 0700 / 0600，启动时校验权限否则拒绝读 → `tests/hooks_inbox.rs::rejects_wrong_permissions`。
- **挂起无上限** → 恶意或失控 agent 开成百上千 hook 进程。守卫：每会话 8、节点 256，超限 exit 0 → `tests/hook_cmd.rs::hold_cap`。

## Consequences

**正面**：WAITING / TURN_DONE 的来源是 agent 自己，UI 改版不影响；Dashboard 的 allow / deny 与终端答复天然并存；两行预览与任务摘要不碰私有格式；外部会话凭 `TMUX_PANE` 可直接采纳带终端；daemon 重启零丢事件。

**负面 / 接受的代价**：每个一等 agent 一张版本表要维护；Grok 的权限决定只能在终端答（agent 的限制，不是 agora 的）；AskUserQuestion 类问题 V1 只能在终端答；投递箱目录要清理；用户的 `~/.claude/settings.json` 多了几行（可卸载、装前有 diff）。

**回写**（2026-09-02 已做）：MISSION v0.12——§3.3 投递方式、§5.1 hook 沉默规则、§5.6 Grok 只提供"打开终端"、§7.3 / §8 删 hook 端点与凭据；`docs/spec/api.md` 删 `POST /api/hooks/:agent`、补 `decision.resolved` 与 `NoPendingDecision`；`docs/spec/config.md` 加 `hooks:` 段；ADR-003 预填稿两处 hook 端点表述；`docs/spec/instance.md` 三个 agent 版本更新。MISSION v0.14（同日，文档交叉验证）：§6.3 两行来源改为 hook 事件（原文误写 transcript）、§4.3 WAITING 来源措辞、§4.2 / §5.5 三种 `origin` 与 `epoch`、§5.6 input.needed 只留提问、§1.2 / §7.3 选择题的限定。

**跟进 issue**（均 discovered-from `agora-90t.3`）：`agora-dvh.1` `agora hooks install` 的 diff / 卸载 / 宿主自认 / 热加载验证（M1b）；`agora-3la.1` fixture 录制工具与回放器；`agora-3la.2` 真实 agent hook 冒烟测试接入 CI；`agora-dvh.2` Codex adapter 实测——交互式 TUI 触发、PermissionRequest 挂起与并存、`/hooks` 信任流程、`CODEX_*` 环境变量（M1b）。

## 参考

- `docs/analysis/devcenter/README.md` §3.4 状态检测行、§3.5 第 1–3 条、§6.2 Adapter 与 drop-box 行、§6.4 第 1–2 条
- `docs/analysis/devcenter/appendix-a-backend.md` §2b（规则与反例）、§5（文本路线判断）、§4 借鉴第 7 条
- Claude Code 文档：hooks reference / hooks guide / settings reference（`code.claude.com/docs/en/hooks.md` 等，2026-09-02 抓取）
- Grok 自带文档 `~/.grok/docs/user-guide/10-hooks.md`（1.0.13）
- 本机 orca 的 hook 安装形态（`~/.claude/settings.json`、`~/.grok/hooks/orca-status.json`）：`if [ -x … ]` 守卫 + 显式 timeout + curl loopback（agora 不沿用 curl：daemon 不在即丢）

## 附录 A：实测记录（2026-09-02）

### Claude Code 2.1.258（macOS，tmux 3.7c 内交互式 + `-p` 无头）

| # | 实测 | 结果 |
|---|---|---|
| 1 | `-p` 无头 + `--settings` 内联 8 个 hook | SessionStart(`source=startup`)、UserPromptSubmit(`prompt`)、PreToolUse / PostToolUse(`tool_name`、`tool_input`、`tool_use_id`、`tool_response`、`duration_ms`)、**Stop(`stop_hook_active`、`last_assistant_message`、`background_tasks`、`session_crons`)**、SessionEnd(`reason=other`) 全 fire；公共字段 `session_id`、`transcript_path`、`cwd`、`prompt_id`、`permission_mode`、`effort` |
| 2 | `-p` 无头请求 Write | **PermissionRequest / Notification / PermissionDenied 都不 fire**：无头模式直接拒绝，不走审批。hook 实验必须交互式 |
| 3 | 交互式（tmux 内）请求 Write，hook 只记录 | `PermissionRequest`（`tool_name=Write`、`tool_input`、`permission_suggestions=[{setMode acceptEdits session}]`）6 s 内 fire；随后 `Notification(permission_prompt, message="Claude needs your permission")`；TUI 显示 "Do you want to create probe-a.txt? 1. Yes 2. Yes, and switch… 3. No"；pane title 变为 "✳ probe-a.txt creation" |
| 4 | 交互式，hook `sleep 12` 后返回 `{"decision":{"behavior":"allow"}}` | **挂起期间 TUI 同时显示权限提示**，Notification 照常 fire；hook 返回后文件创建、无人按键；Stop 带 `last_assistant_message="done"` |
| 5 | hook 进程的环境 | 可见 `CLAUDE_CODE_SESSION_ID`、`CLAUDE_PID`、`CLAUDE_PROJECT_DIR`、`CLAUDECODE`、`TMUX`、`TMUX_PANE` 等——继承 agent 进程全部环境（D4 的身份传递与 pane 定位依据） |
| 6 | 新目录首次交互式启动 | 信任对话框默认选 "No, exit"，Enter 即退出；`send-keys` 文本后紧接 Enter 会被当粘贴吞掉，需间隔再送 Enter（fake-agent 与集成测试的坑） |
| 7 | 文档抓取 | 33 个事件；command hook 默认 timeout 600 s（UserPromptSubmit 30 s、SessionEnd 共 1.5 s）；同事件 hooks 并行、多文件拼接；决定取最严 deny > defer > ask > allow；配置文件热加载；**超时后 allow 还是 block 文档未写**（D5 因此自己先退出） |

### Grok 1.0.13（`agora-90t.3` 注记，2026-09-02 无头 `-p` + 自带文档）

事件全集与时序、payload 双写、`Stop` 的 `reason` 过滤、`lastAssistantMessage`、`GROK_SESSION_ID` ≡ resume id、fail-open、5 s / 600 s 默认超时、兼容加载 `~/.claude/settings.json` 与 `~/.cursor/hooks.json`——见该注记；文档补充：PreToolUse 的 `allow` 不能替用户批准（"only not blocked"），`permission_prompt` 只在权限 UI 真等人时 fire，`idle_prompt` 约 1 min 后 fire。

#### Grok 1.0.13 交互式补测（2026-09-04，tmux 内 `--permission-mode default`，探针 hook 落盘 stdin；agora-dvh.6）

- **事件名是小写蛇形**：payload 的 `hookEventName` 是 `session_start` / `user_prompt_submit` / `pre_tool_use` / `post_tool_use` / `permission_denied` / `notification` / `stop` / `stop_cancelled` / `session_end`，不是配置文件里的 `SessionStart`；每个键双写 camel + snake（`sessionId` / `session_id`）。按 CamelCase 匹配的映射表在 Grok 上一条都认不出——adapter 归一后再比。
- `session_start.source = new`（不是 startup）；`/clear` 直接 fire 新 id 的 `session_start(source=new)`，旧会话**不发** `session_end`。
- 权限：default 模式下 `echo > file`、`curl` 都被判安全直接放行，`rm -rf` 才弹权限 UI；`notification(permission_prompt)` 只有 `level` / `message`("Tool permission requested") / `notificationType`，**不带工具身份**。终端选 reject → `permission_denied`（带 toolName / toolUseId）+ `stop_cancelled(reason=permission_rejected, cancelledBy, reasonDetails)`；约 60 s 后 `notification(idle_prompt, message="Waiting for your next prompt")`。
- Esc 中断 → `stop_cancelled(reason=user_interrupt, cancelTrigger, lastAssistantMessage)`，没有 `post_tool_use_failure`。
- 会话结束：`session_end(reason=shutdown)` 之后还有一次 `stop(reason=shutdown)`（无 promptId）。
- hook 进程环境：`GROK_SESSION_ID` / `GROK_HOOK_EVENT` / `GROK_HOOK_NAME` / `GROK_WORKSPACE_ROOT`，**没有进程号**；`CLAUDE_PID` 等是从起 grok 的 Claude Code 会话继承的，不是 grok 的。命令写成 `if [ -x … ]; then <bin> …; fi` 时 hook 的 ppid 是 `sh`，其父才是 grok → 安装命令改为 `exec`（D4）。
- `toolUseId` 形如 `call-<uuid>-0`；`post_tool_use.toolResult.output` 是字节数组。fixture 见 `testdata/grok/1.0.13/hooks/`。

### Codex 0.152.1（2026-09-02，研究 agent：本机二进制 strings + `codex features list` + 官方文档 learn.chatgpt.com/docs/hooks + GitHub release notes；未做交互式实测）

| 项 | 核实结果 | 来源 |
|---|---|---|
| hooks 存在与默认 | `codex features list` → `hooks stable true`；文档 "Hooks are enabled by default"，`[features] hooks = false` 关闭 | 本机 + 文档 |
| 事件 | SessionStart(`source`) / SessionEnd / UserPromptSubmit(`prompt`) / PreToolUse / PostToolUse / PermissionRequest / PreCompact / PostCompact / SubagentStart / SubagentStop / Stop / Interrupt（0.150.0 起）；二进制 strings 里 12 个名字连续出现 | 文档 + strings |
| stdin | `session_id`、`transcript_path`（rollout 路径）、`cwd`、`hook_event_name`、`model`、`permission_mode`；turn 级 `turn_id`；工具类 `tool_name` / `tool_use_id` / `tool_input` / `tool_response`；Stop / SubagentStop 加 `stop_hook_active`、`last_assistant_message` | 文档，字段名在 strings 命中 |
| 输出 | exit 2 + stderr = 阻断；`hookSpecificOutput.permissionDecision` allow / deny / ask、`decision:{behavior, message}`；PermissionRequest 返回 `{"decision":{"behavior":"allow"}}` 即批准，无 hook 决定则走正常审批；`async: true` 的 hook 不能阻断 | 文档 |
| 配置与信任 | `~/.codex/hooks.json` 或 `config.toml [hooks]`；项目层需信任 `.codex/`；**非托管 hook 须在 TUI `/hooks` 审阅、按内容哈希信任，改动即失效**；`--dangerously-bypass-hook-trust` 可绕过（agora 不用） | 文档 + `codex --help` |
| 版本 | 0.114.0（2026-03-11）实验性引擎只有 SessionStart / Stop；0.145.0 "permission hooks resolve strict auto-review"；0.150.0 加 Interrupt。PreToolUse / PermissionRequest 首发版本与"默认开启"的确切版本**未钉住** | GitHub release notes |
| notify | 顶层 `notify = [argv…]`，JSON 作最后一个 argv，只有 `agent-turn-complete`（`thread-id`、`turn-id`、`cwd`、`input-messages`、`last-assistant-message`）；不为审批 fire；源码注释 "legacy … for backward compatibility"，计划移除 | 文档 + 源码 |
| 审批 | `approval_policy` untrusted / on-request / never（+ granular）；本机 `never` + `danger-full-access`，即本机现状下 PermissionRequest 永不 fire；`tui.notifications` 只是终端 OSC 9 / BEL，程序不可挂接 | 文档 + 本机 config |
| 会话文件 | `~/.codex/sessions/YYYY/MM/DD/rollout-<ts>-<uuid>.jsonl`，首行 `session_meta.payload.id` = uuid；`codex resume <id>`（`--last` 存在但 A32 禁用）；审批请求**不落盘**（1135 个 rollout 零命中）→ 文件不能当状态源 | 本机 + 文档 |
| 环境变量 | `CODEX_THREAD_ID` / `CODEX_SESSION_ID` 在 strings 出现，**是否注入 hook 子进程未核实**；身份传递先靠 payload 的 `session_id` 与 `AGORA_*` | strings |
| 推断（未验证） | hooks 引擎在 core crate、app-server 有 `hook/started` 通知，因此 TUI 交互式会话应当触发 hooks；PermissionRequest 挂起期间 TUI 是否同时显示审批提示（Claude 实测是）未验证 | — |

未核实项全部进 Codex adapter 实测 issue（M1b）：交互式 TUI 下事件触发、`approval_policy = on-request` 时 PermissionRequest 的挂起与并存、`/hooks` 信任流程对安装器的要求、`CODEX_*` 环境变量、`TMUX_PANE` 继承。

#### Codex 0.152.1 交互式补测（2026-09-05，tmux 内 TUI `-c approval_policy=on-request -c sandbox_mode=workspace-write` + `codex exec` 无头，探针 hook 落盘 stdin 与环境；agora-dvh.2）

| 项 | 实测结果 | 对 agora 的含义 |
|---|---|---|
| ① TUI 触发 | 交互式会话触发 SessionStart(`source` startup / clear) / UserPromptSubmit / PreToolUse / PostToolUse / PermissionRequest / Stop / Interrupt / SessionEnd；事件名 **CamelCase**、键 **snake_case**（`hook_event_name`、`tool_use_id`），与 Grok 相反；每条带 `model`、`permission_mode`，turn 级带 `turn_id` | `hooks::GenericHooks` 的拼法对 Codex 是对的 |
| ② 挂起与并存 | on-request 下沙箱外写文件（`touch ~/x`）fire PermissionRequest，**不带 `tool_use_id`**（PreToolUse 带 `exec-<uuid>`），只能按 `tool_name` 键；挂起期间 TUI 只显示 `• Running PermissionRequest hook`，**不显示审批提示**——终端里的人答不了；hook 返回 `{"hookSpecificOutput":{"decision":{"behavior":"allow"}}}` 直接放行；hook 无输出退出后 TUI 才弹出三选项审批（y / p 不再问 / esc），终端按 y → PostToolUse | **与 Claude 的"并存"不同：挂起 = 独占**。Codex 的挂起上限必须短（秒级、可配），否则终端用户只看到一行 "Running hook" 干等；dvh.7 定上限并在 UI 说明 |
| ③ `/hooks` 信任 | 未信任的非托管 hook **静默跳过**，TUI 与 `codex exec` 都无任何提示；`/hooks` 面板列出每条 hook，`t` 一键信任全部、`enter` 逐条审阅；信任存 `~/.codex/config.toml` 的 `[hooks.state."<file>:<event>:<i>:<j>"] trusted_hash = "sha256:…"`，**每条 hook 单独一个哈希**；哈希覆盖 hook 条目本身（只改 Stop 的 `timeout` 20→21，只有 Stop 失去信任，其余照常 fire），**不覆盖命令指向的脚本内容**（改脚本不失效）；`--dangerously-bypass-hook-trust` 跳过信任检查 | 安装器必须写稳定不变的条目（命令字符串、timeout 都不能随版本漂），装后必须提示用户进 TUI `/hooks` 按 `t`——否则一条事件都收不到且无从察觉；agora 侧要有"装了却从未收到 SessionStart"的提醒 |
| ④ `CODEX_*` | hook 子进程环境里**没有** `CODEX_THREAD_ID` / `CODEX_SESSION_ID`（也没有 agent 自己的进程号变量）；ppid 是 codex 本体 | 身份靠 payload `session_id`；进程号靠 `exec` 后的 ppid（同 Grok） |
| ⑤ `TMUX_PANE` | `TMUX` / `TMUX_PANE` 原样继承 | pane 归属可用 |
| ⑥ TURN_DONE | 没有 Notification / idle_prompt / StopFailure；每轮以 Stop(`last_assistant_message`) 收尾，Esc 中断以 Interrupt(`turn_id`) 收尾且没有 PostToolUse；被中断的命令转成后台终端继续跑 | Stop + Interrupt 够用；API 错误未触发到，靠沉默规则 |
| 其他 | SessionStart **在第一条 prompt 提交时**才 fire（与 UserPromptSubmit 相隔几十毫秒；TUI 启动、`/clear`、resume 之后都要等用户先提问），所以自报 id 在首问之前缺席、Restart 只能退化；`/clear` 只发新 id 的 SessionStart(`source=clear`)，旧会话当时不发 SessionEnd，退出时两个 session 各发一次 SessionEnd(`reason=other`)；SessionEnd / Interrupt 的 hook timeout 上限 3 s（超过自动 clamp 并 stderr 警告）；插件 `codex@openai-codex` 1.0.6 自带 hooks.json 也走同一信任表；沙箱直接拦网络（`curl` 无输出、不问）| SessionEnd 里不能干慢活；testdata/codex/0.152.1/hooks 七场景中五个真录、`api_error` / `parallel_tools` 按键集合合成 |

## 附录 B：事故记录

（上线后追加）
