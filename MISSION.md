# agora — MISSION

**Multi-Agent Workspace：跨节点、任意设备的 agent 控制台**

| | |
|---|---|
| 版本 | v0.12（2026-09-02）；变更历史见 beads `agora-90t.1` 注记 |
| 项目类型 | Self-hosted Web Application |
| 目标平台 | 节点：macOS / Ubuntu Linux / Windows 主机（Windows V1 延期）；客户端：macOS 笔记本、iOS、Android 设备上的现代浏览器（V1 只验收 macOS 笔记本） |
| 主要用户 | 单用户 |
| 核心场景 | 在任意客户端设备上通过浏览器管理任意数量节点上运行的多个 CLI coding agent session（当前实例见 §0.2） |

**文档分层**：本文只写决定——问题、边界、不变量、Non-Goal、DoD、施工约束、验收。"为什么"在 `docs/adr/`；实现形状（端点、配置、schema、线框、键位）在 `docs/spec/`，随代码改；路线图、任务与变更历史在 beads。【ADR-00N】= 交给该 ADR 决定。**编号冻结**：本文的章节号与验收编号（A 编号）一经被引用即不重排、不复用；新增内容挂现有节尾或拿新号。

---

## 0. 范围边界与当前实例

名词：**节点** = 一个 agora daemon 实例（通常一台机器一个）；**客户端** = 浏览器里的 agora Web；**会话** = 一个 agent 进程及其运行时载体；**任务** = 会话在做的那件事。

### 0.1 范围边界（远视，MISSION 级）

边界回答"agora 必须能容纳什么"，不是"V1 必须实现什么"；V1 的实现范围由 §12 与 beads epic 定义。当前实例（§0.2）是边界内的一个点，不是边界本身。

| 维度 | 边界 | V1 实现 |
|---|---|---|
| 节点类型 | 三类主机：**macOS**、**Ubuntu Linux**、**Windows**（用于只支持 Windows 的项目） | macOS + Linux；Windows 延期（§11），但运行时抽象**不得排除它**【ADR-001】 |
| 节点数量 | 任意；新增节点不改变架构（节点互为 peer，§3.5） | 2 |
| 客户端类型 | 三类设备：**macOS 笔记本**、**iOS 设备**、**Android 设备**；都是同一个 Web 客户端，功能集相同（不变量 9），差别只在显示与交互的优化 | macOS 笔记本；iOS / Android 为 V2 首批（§11） |
| agent 类型 | 任何 CLI coding agent，经 Adapter 接入（§5.2）；有 hook 的走 hook，没有的走文本兜底（§5.1） | 一等：Claude Code、Codex、Grok Build；pi 备选一等 |
| 人 | 单人；多用户是 Non-Goal，认证按 principal 留口（ADR-003）。同一主机可有多个用户各跑各的实例，互为陌生人（`node.id` 只需在自己的 peer 网内唯一） | 单人 |
| 网络 | 无关：只要求客户端能到达节点，认证与 TLS 由 agora 自己保证（§8）；**网络可达 ≠ 授权** | 当前用 tailnet，只是部署选择 |
| 任务来源 | 可选：有 beads 的仓库用 issue id，没有的用首条 prompt 摘要 | 本仓库有 bd，多数仓库没有 |

### 0.2 当前实例

**MacBook Air**（macOS 节点 + 日常客户端所在）+ **zuan**（Ubuntu 常开节点，承担无人值守的工作；它的资源不是瓶颈，人的注意力才是）。并发 ≤ 30 个会话（本机可达 10、zuan 其余），跨上百个仓库，多 git worktree 并行是常态。Mac 睡眠时其上的 agent 暂停、醒来继续，不算失败；需要无人值守的工作放 zuan。实测清单（硬件、网络、已装 agent）见 `docs/spec/instance.md`——它是各 ADR 的输入，不是上限。

**一天的形态**（任何一步都可以在任何设备上做，下面只是最常见的分工）：

1. 在 Mac 上打开本机的 agora（它把 zuan 作为 peer 并入）；首页跨节点合并、按"需要我"排序：谁在等回答、谁失败了、谁做完了。
2. 逐个处理：切到会话回答 / 看结果 / 重启失败的会话。
3. 起新会话：选仓库与 worktree、选 agent、选节点（本机或 zuan）、填任务（issue id 或一句话）。
4. 离开：合盖或出门，zuan 上的 agent 继续。（V2 首批：手机上打开 zuan，看"谁在等我"，回答或审批。）
5. 回来：Mac 醒来、本地 agent 继续，从第 1 步接着做。

---

## 1. 产品定位与北极星

### 1.1 定位

几台机器上同时运行多个 Coding Agent，以及 shell / build / test 进程。本地用 terminal multiplexer 可以管得不错，但一旦离开 Mac（只有手机）、或要同时看几台机器，现有方案——SSH + tmux、Remote Desktop、VS Code Remote、一堆终端窗口——要么 UX 差，要么太重。

agora 是一个 **Browser-native、极轻量、Persistent、Agent-aware 的远程 agent 控制台**：打开一个 URL，看到所有节点上的全部 agent session，像浏览器标签页一样快速切换。核心体验：**One client → All nodes → All agents**；对手机它就是 "One URL"（§3.5）。

### 1.2 北极星

整个项目持续围绕一个问题做取舍：

> "我现在有几十个 Coding Agent 在几台机器上运行，我需要关注哪几个？"

而不是"如何把所有开发工具搬进浏览器"。如果一个功能不能明显改善以下五个动作之一，就不进入 MVP：

```
launch → observe → switch → respond → resume
```

**respond 必须可以不经终端完成**：终端是传输层，不是唯一交互层。权限与选择经 hook 返回决定（不注入键击——该敲 `y` 还是 `1` 是 agent 私有的 TUI 行为），自由问答与下一条指令经 PTY 送文本；在 Dashboard 上都是一次点触（§6.3、§7.3）。**respond 含看结果**：TURN_DONE / FINISHED 时先看产出（§6.3）再给下一条——这不是新动词，五个动作不扩。

### 1.3 "管理"是哪一层

| 层次 | 管理什么 | agora 的态度 |
|---|---|---|
| L1 终端 / 进程级 | session 活着吗、在跑还是在等人、切过去回答它 | **完整覆盖**：持久化运行时、状态检测、远程 attach |
| L2 任务 / 语义级 | 每个 agent 在做什么任务、目标、进度、产出、成本、对话历史 | **做到标签层**：每个会话关联到"它在做的那件事"（有 beads 的仓库用 issue id，没有的用首条 prompt 摘要），首页按任务显示、attention 同分按任务优先级（§6.3）；标签层还带任务的验收标准（读自 beads）与会话的**只读产出**（改动文件列表、diff，§6.3）。成本、进度、对话摘要不做（§11） |
| L3 协作 / 编排级 | 多 agent 分工、依赖、消息、DAG、并行审查 | **明确 defer**（§11）；没有任何 agent 能创建 session |

### 1.4 Non-Goals

agora **不是 IDE**。不实现：

- Code Editor
- Git GUI（只读的改动列表与 diff 是观察，不是 Git GUI；stage / commit / push / branch 任何写操作才是。唯一例外：起会话时的 `git worktree add`——只增不改的工作区准备，§6.4）
- Debugger
- File Explorer
- AI Chat UI
- Project Management（agora 只读 beads、只存会话 ↔ 任务的关联，不变量 12）
- Agent Workflow Engine
- Remote Desktop（含端口转发、远程浏览器像素流）
- 跨 host 搬迁 session——需要换机器时，在正确的节点用同一个 `task_ref` 重起；agent 经 hook 自报对话 id 的识别机制单独保留（§5.6），它是同机 Restart 正确 resume 的前提，与搬迁无关
- 只读 viewer 的 URL 抓取路径——出站抓取换不来价值，只带来 SSRF 面（渲染 session 指名文件的 Artifact Viewer 留 §11）
- 原生客户端（iOS / Android / macOS / Windows）——只有一个 Web / PWA
- 独立的 hub 组件、节点间隧道——汇聚是节点的一种配置（peers），不是一种组件（§3.5，ADR-004）
- 网络连通方案——VPN / tailnet / 中继 / 反向代理交给部署环境。**TLS 与证书不在此列**：PWA 与推送强制 HTTPS，证书是 agora 必须自动化的事（§8）

它只解决一个问题：**管理大量 Persistent CLI Agent Session。**

也**不是终端产品**：终端只做 attach 的保真（键位映射、剪贴板、resize），不做终端模拟器的产品功能（分屏、多标签、主题、配置文件）。

### 1.5 Definition of Done

MVP 的完成定义不是"可以在浏览器里打开终端"，而是：

> 用户可以在任意设备上通过一个轻量的浏览器 UI 管理多台机器上的大量长期运行 Coding Agent；无需 Remote Desktop，不需要频繁 SSH / tmux 操作，并可以一眼看到哪些 agent 正在工作、哪些需要输入、哪些已经完成。

最终体验应接近：**现代终端管理器 + persistent remote tmux + agent attention management**，而不是 another browser IDE。

**一个 issue "做完" =**

1. 已合入主干，CI 绿，commit 引用 issue id
2. 若改变了决策，MISSION / ADR 已回写——过期的真相源不是真相源
3. 能在所属 epic 的演示路径里跑给人看
4. 由人关闭

---

## 2. 核心原则与架构不变量

### 2.1 Thin 原则

系统尽量薄。避免：Kubernetes、PostgreSQL、Redis、Docker（作为部署前提）、Electron、Next.js server、Remote Desktop stack。

理想部署形式：**单个 binary + 内嵌静态前端 + SQLite**，每个节点一份；一条命令能把一台空机变成可用节点。

### 2.2 Architectural Invariants

以下不变量开发全程不得违反，**每条都必须有测试钉死**（规则 8）：

| # | Invariant |
|---|---|
| 1 | Browser crash ≠ Agent crash |
| 2 | WebSocket disconnect ≠ Agent crash |
| 3 | agora daemon crash ≠ Agent crash |
| 4 | Close Tab ≠ Kill Session |
| 5 | One broken Agent must not affect another Agent |
| 6 | UI state is disposable. Agent state is persistent. |
| 7 | 运行时是 Agent persistence 的 Source of Truth；SQLite 只保存 metadata / preferences / mapping，不是 process existence 的 Source of Truth【ADR-001】 |
| 8 | One broken node must not affect another node；peer 离线在视图里表现为 stale（保留最后视图与时间），绝不静默消失 |
| 9 | 客户端零特权：一切功能经节点 API 完成，任何设备、任何客户端形态的功能集相同 |
| 10 | 状态来源分层：hook / 结构化事件 > 进程状态 > 屏幕文本；每个状态值带来源与置信度（§5，ADR-002） |
| 11 | 节点互不信任：每个节点独立认证 principal（人：TOTP；节点作为 peer 客户端：机器 token + 证书指纹）；网络可达（局域网 / tailnet / 公网）≠ 授权（§8，ADR-003） |
| 12 | 任务真相在仓库里（beads / git）：agora 只存 session ↔ 任务的关联，永不复制任务的内容、状态、依赖 |

原则：**浏览器可以随时关闭，但 agent 必须继续运行。浏览器不是 agent 的 process owner。**

### 2.3 施工规则（Coding Agent 约束）

本项目由 Coding Agent 施工，必须遵守：

| Rule | 内容 |
|---|---|
| 1 | 不要擅自替换持久化运行时架构（ADR-001 选定后锁死） |
| 2 | 不要引入 §2.1 避免清单里的东西，除非存在经过 ADR 记录的必要原因 |
| 3 | 每个 epic 完成后必须能独立运行（演示路径，§1.5） |
| 4 | 不能为了 mock 简化而破坏真实 PTY / 运行时集成 |
| 5 | 任何 agent-specific behavior 必须通过 Adapter 实现 |
| 6 | 所有 destructive action 必须是 explicit API / action |
| 7 | 不得让 HTTP / WebSocket connection ownership 与 agent process ownership 耦合 |
| 8 | 守卫必须有测试钉死：每条不变量、安全围栏、认证逐条关掉验证测试变红 |
| 9 | fake-agent + fixture + 单进程多节点的测试骨架先于真实 agent 集成，并接入 CI |
| 10 | 程序不得靠给人看的文本做判断（错误消息、日志行、CLI 输出）：错误按类型分类，不做字符串匹配。数据库迁移带版本号——N 个节点各自原地升级（§7.3 API 版本） |

架构决策记录在 `docs/adr/`（约定见 `docs/adr/README.md`）。规划分层见文首"文档分层"；任务纪律（claim、commit 引用 issue id、discovered-from、同步授权等）见 `AGENTS.md`，它是唯一出处。

---

## 3. 系统架构

### 3.1 组件与 Ownership

每个节点一个 daemon：**Session Manager + State Detector + Terminal Gateway**，其下是持久化运行时【ADR-001】与其中的 agent 进程，旁边是只存 metadata 的 SQLite。客户端是浏览器里的同一个 Web（Dashboard / Tabs / xterm.js / 通知），经 HTTPS / WebSocket 连接某一个节点。总体架构图见 `docs/spec/architecture.md`。

Ownership 边界（MVP 最重要的架构边界）：

```
运行时     owns Agent lifecycle
agora 节点 owns discovery + metadata + observation + remote interaction
           + 对 peer 会话的转发视图（不拥有 peer 的会话）
客户端     owns presentation only
```

### 3.2 Terminal 链路

```
xterm.js ←→ WebSocket ←→ Terminal Gateway ←→ PTY ←→ 运行时 attach ←→ Agent
```

Resize：xterm.js FitAddon 产生 cols / rows，经 WebSocket 通知后端，后端调用 PTY resize API。

### 3.3 State Observer 独立于 Browser Connection

**不要通过 WebSocket client output 来判断 agent 状态**，否则没有浏览器连着时无法检测 agent。Observer 有两个输入，都不依赖浏览器连接：① **agent 的 hook 事件**——有 hook 的 agent 的主来源（§5.1），经投递箱文件 + unix socket 送达，没有 HTTP 端点（ADR-002 D3）；② **运行时轮询**——进程树 + 屏幕文本，用于无 hook 的 agent、退出码与兜底，参数是 ADR-001 的默认值。

### 3.4 Server Restart 语义

agora daemon 重启不得影响任何 agent：agent 生命周期挂在运行时之下，不挂在 agora 之下。重启后的恢复流程：

```
discover 运行时中的 session → read SQLite metadata → reconcile → rebuild runtime state → 重放投递箱里的 hook 事件
```

**hook 事件在 daemon 不在时不得丢失**：投递箱落盘、重启后重放（ADR-002）。这是 §5.1 "有 hook 的 agent 的 WAITING 只来自 hook"能成立的前提。

### 3.5 多节点：peer 模型【ADR-004】

- **节点 = daemon + 本机会话**。节点可配置若干 **peer**（地址 + 机器 token + 证书指纹，§9.1）；默认一个也没有。
- **仓库不归 agora 管**：每个节点各自 clone，一致性交给 git remote（beads 同理，经 Dolt remote）；同一仓库的多节点副本是用户的部署选择，agora 不 clone、不同步、只呈现（不变量 12；换节点走 §1.4 的 `task_ref` 重起）。
- **节点是 peer 的 API 客户端**：用的就是浏览器用的那套 API（§7.3）——连上时拉全量，之后收事件流，按需转发终端流与指令，把 peer 的会话并入自己的视图，每行标明节点。可用 fake 节点测试（规则 9）。
- **只导出本机会话，一跳**：节点对外暴露的永远只有自己的会话，不转发从 peer 收来的；这同时防环。
- **peer 断线保留最后视图并标记**（"上次见到 23:10"），绝不静默消失（不变量 8）；恢复后自动重连。最后视图 V1 只在内存里，持久化历史留 V2。
- **浏览器一次只连一个节点**：把浏览器指向哪个节点，哪个节点就是汇聚点——由配置决定，不由架构决定。
- **谁需要浏览器可信证书**：只有被浏览器直接打开的节点。V1 浏览器只打开 `127.0.0.1`（本身是安全上下文），因此 V1 不需要任何浏览器可信证书，也不需要 TOTP；peer 之间自签证书 + 指纹钉住即可（§8）。
- **peer 访问默认关闭**：节点上没有签发过任何机器 token 就没有 peer 能连进来；多节点是显式配置出来的状态，不是默认状态（§8）。
- 写操作与终端流按会话所属节点路由：本机直达，peer 经一跳转发；对用户不可见（确认逻辑归 §8）。
- peer 视图里的时间（上次见到、等待时长）由本节点的时钟打，不信 peer 报的时间。

**V1 唯一形态**：Mac 配 zuan 为 peer（一行配置），Mac 自身保持只听 loopback。全部形态见 `docs/spec/architecture.md`。留给 V2（§11）：peer 历史、反向拨号、多跳转发。

---

## 4. Session 模型与状态机

### 4.1 Agent Session

每一个 agent session 对应一个 persistent 运行时会话【ADR-001】；浏览器断开不影响运行时中的 agent。agora 只配置自己创建的会话（运行时名、退出后保留输出供读退出码、关掉多余的状态栏）；被 adopt 的会话保持用户原有配置。

### 4.2 数据模型

概念字段（存储形态见 `docs/spec/config.md`）：

```
Session {
    id                 # 本机内唯一；对外 <node>:<id>
    display_name       # 与 name_locked 配对（§4.5）
    agent_type         # claude | codex | grok | pi | shell | custom
    working_directory
    worktree           # git worktree 路径；可空
    task_ref           # beads issue id 或首条 prompt 摘要；可空
    command
    runtime_ref        # 运行时句柄【ADR-001】
    agent_session_id   # agent 自报的当前对话 id（§5.6）；Restart 的 resume 依据
    adopted            # 是否为采纳的外部会话（§5.5）
    created_at
    ended_at           # 进程退出时刻；等待时长与 attention 用
}
```

`status / exit_code / last_activity_at` 是运行时与 observer 的实时结果，**不落库**（不变量 7）；SQLite 只存上面的 metadata。

节点身份不进本机模型：每个节点只存自己的 session。对外（浏览器与 peer）会话以 `<node>:<id>` 标识，`node` 是节点 id（§9.1）；peer 的会话并入视图时保留来源节点，绝不改写。

### 4.3 状态定义

| 状态 | UI | 含义 |
|---|---|---|
| STARTING | | process created，尚无有效活动 |
| RUNNING | ● | 持续产生 output |
| WAITING | ⚠ | agent 在**一轮之中**停下来等人：提问、权限确认（hook：Notification / PreToolUse；兜底：文本匹配） |
| TURN_DONE | ◆ | agent **一轮做完**、等下一条指令（hook：Stop；进程仍在）。人的动作是看结果 / 给下一步，与 WAITING 的"回答问题"不同 |
| IDLE | ○ | 长时间无 output 但 process 仍存在，且没有 hook 信息说明原因——只出现在兜底路径（状态从"人要做什么"定义，不从"观察者看见什么"定义） |
| FINISHED | ✓ | process exit 且 exit code = 0 |
| FAILED | ✕ | process exit 且 exit code ≠ 0 |
| UNKNOWN | ? | 外部 session / detector failure / 不支持的 agent / 进程信息不一致 |

### 4.4 状态机

必须显式实现状态机，而不是让 UI 自己猜。

```
                 process created
                      │
                      ▼
                  STARTING
                      │
            first meaningful activity
                      │
                      ▼
                   RUNNING
            /          │          \
           /           │           \
  prompt / permission  Stop hook   silent timeout（无 hook 信息）
  （hook，兜底：文本）  │              │
          │            ▼              ▼
       WAITING     TURN_DONE         IDLE
          │            │              │
     user input   next prompt    output resumes
          └────────────┼──────────────┘
                       ▼
                    RUNNING
                       │
                 process exits
                   /       \
                  /         \
             exit=0         exit!=0
                │              │
                ▼              ▼
            FINISHED         FAILED
```

### 4.5 Session 命名

系统生成运行时名 `ag-<short-id>`，而不是直接用 Display Name（避免空格 / unicode / 重名 / 改名复杂度）：

```
Display Name:  sglog parser refactor
runtime:       ag-a81c28
```

Display name 可任意修改，运行时身份不变。一行显示的名字有两个来源：用户存的 `display_name`，和 agent 自己设的 pane title（OSC 2）。**没改过名时 title 赢**——它更新鲜；**改过名之后 `display_name` 永远赢**，这就是 `name_locked`。因此"改名"是两件事：存一个名字，*并且*把 agent 的 title 挡在外面；**包括改成和原来一模一样的字符串**也要落锁。客户端不得因"名字没变"而跳过请求。

### 4.6 生命周期语义

必须严格区分：

| 操作 | 语义 |
|---|---|
| **Detach** | 停止浏览，agent 继续运行 |
| **Close Tab** | 与 Detach 等价 |
| **Archive** | 从默认 Dashboard 隐藏，保留运行时会话（可放 V2） |
| **Kill** | 杀掉 agent 进程，**必须显式确认**；运行时会话连同其输出保留，按下面的清理策略回收（ADR-001 D4：scrollback 还在、Restart 仍是同一会话） |
| **Delete Metadata** | 移除 agora metadata，但不杀运行时会话 |
| **Restart** | kill 现有进程 + 在**同一个运行时会话**内重建，并 resume 原对话——依据是 agent 自报的当前对话 id（§5.6），**绝不用 `--continue` / `--last`**（那会静默恢复成另一段对话）。只在真的会杀掉正在运行的 agent 时才必须显式确认（§8） |

**已退出会话的清理**：FINISHED / FAILED / 被 Kill 的运行时会话在用户看过之后清理（Dashboard 里确认或 Delete Metadata 时一并清掉保留的输出）。这不是 kill——进程已经不在——所以不需要确认，但不得在用户看到结果之前发生；V1 不做定时回收。策略细节见 ADR-001 D4。

浏览器 Tab 表示当前浏览器打开的 agent，而不是 agent 生命周期。切换 / 关闭 Tab 不允许：restart session、recreate 运行时会话、丢失 agent 状态、终止 PTY 拥有的进程。

---

## 5. Agent State Detection

这是本项目区别于普通 Web Terminal 的核心能力。普通 Terminal Manager 只知道 terminal exists；agora 应知道**哪一个 agent 正在运行，哪一个 agent 需要人的注意**——用户无需逐个打开终端检查。

### 5.1 分层 State Source（V1）

三个一等 agent（Claude Code、Codex、Grok Build）的 hook / notify 都是文档化的稳定接口（Grok 2026-09-02 实测证实，细节见 ADR-002）；文本兜底只服务 generic shell 与采纳的未知会话。因此分层来源是 V1 的架构，不是 V2 的愿望（论证见 ADR-002）：

```
Agent State Source（高 → 低）
1. Native Hook / 结构化事件    最高置信度；有 hook 的 agent 的 WAITING / TURN_DONE 只认它
2. Process State               进程存活、退出码 → FINISHED / FAILED
3. Terminal Pattern            无 hook 的 agent 才用于 WAITING；有 hook 的 agent 只用于 preview
4. Activity Heuristic          IDLE 的唯一来源
```

规则：

- 有 hook 的 agent：WAITING 与 TURN_DONE **只**来自 hook；文本匹配不得把它们抬到 WAITING（误报的代价见 §5.3）。
- 无 hook 的 agent（generic shell、采纳的未知会话）：文本启发式 → WAITING（模式清单见 ADR-002 D6）；有 hook 的 agent 长时间无事件而屏幕像在等人 → UNKNOWN，不猜 WAITING（ADR-002 D1）；持续 output → RUNNING；长时间无 output 但进程在 → IDLE。
- 所有 agent：process exit 且 exit code = 0 / ≠ 0 → FINISHED / FAILED。
- 状态机带驻留时间，避免 hook 与轮询交错造成抖动。
- 每个状态值带 `source`（hook / process / text / heuristic）与 `confidence`（§5.3）。

hook 还带来第二个能力：它看得见**这台机器上该 agent 的所有会话**，不管是不是在 agora 的运行时里起的——这是 §5.5 采纳手动会话的唯一途径。因此 hook 装在**用户自己的** agent 配置里，经用户一次确认安装（幂等、可卸载、装前显示 diff）；agora 起的会话不再重复注入，避免同一事件送两次。

**hook 事件在 daemon 不在时不得丢失**（投递箱落盘、重启后重放，§3.4）：否则 daemon 重启的几秒里 agent 停下来提问，事件丢失，而上面的规则又禁止文本把它抬成 WAITING——它会永远显示 RUNNING。

### 5.2 Adapter 架构

agent-specific logic 不允许写死在核心层（规则 5）。每个 Adapter 回答五个问题：装哪些 hook、hook 事件怎么映射到状态（§5.6）、没有 hook 时怎么从进程与文本兜底、怎么在进程树里认出自己、默认命令是什么（接口签名见 ADR-002）。

实现：Claude Code、Codex、Grok、Generic Shell；pi 为备选一等；未来允许 Cursor 与自定义 agent。核心 Session Manager 不应知道任何 agent 的具体输出格式。

### 5.3 Detection Confidence

状态检测不可能永远准确，内部返回：

```json
{ "state": "WAITING", "confidence": 0.93, "reason": "permission prompt detected" }
```

UI MVP 不一定显示 confidence，但必须保留该字段便于 debug。Detector 必须满足：**宁可 UNKNOWN，也不要高频错误地报警 WAITING。**

### 5.4 Agent Auto Detection

取得 PTY 的 PID，遍历 process tree，自动识别 claude / codex / grok / shell。**Process detection 只能作为 hint，用户手动配置的 agent type 优先级更高。**

### 5.5 Existing Session Discovery 与 Adoption

agora 不应只能管理自己创建的 session。启动后扫描运行时中现存 session；未注册的显示为 Unknown Agent，允许 **Adopt**：配置 Display Name / Project / Agent Type。

有 hook 的 agent 在**任何地方**起的会话（Terminal.app、IDE 终端、别的 tmux）都会经 session.started 事件出现在列表里，标为 `external`：在运行时里的才提供终端与 respond；其余只读（状态、两行、通知）。这是手动起的 agent 被管起来的唯一途径。

### 5.6 Hook 事件的最小集合

每个 Adapter 至少要把 agent 的 hook 映射到下列事件；只用文档化接口，不读 private API。各 agent 的具体映射与覆盖度见 ADR-002。

| 事件 | 产生的状态 / 用途 |
|---|---|
| session.started / ended | STARTING / 配合进程状态判定退出 |
| turn.ended | TURN_DONE |
| input.needed（提问 / 权限） | WAITING |
| decision.needed（权限 / 选择） | WAITING；答复经挂起的 hook 同步返回 allow / deny，不注入键击（§7.3）；agent 的 hook 不能替用户批准时（Grok），Dashboard 只提供"打开终端"；挂起上限与超时见 ADR-002 D5 |
| activity（可选） | RUNNING 行的"正在做什么" |
| prompt.submitted | 首条 prompt → 无 bd 时的 `task_ref` 摘要 |
| session.id（agent 自报当前对话 id） | 落 `agent_session_id`（§4.2），**每次命中都覆盖**；Restart 的 resume 依据。识别按新鲜度排序：agent 自报 > 启动时钉死的 id > 用户从候选里挑，绝不按 mtime 猜 |

### 5.7 Terminal History 与 Tail Buffer

MVP 依赖运行时 scrollback，不自行构建 terminal transcript database。State Detector 只保留一个有限 tail buffer，仅用于 state detection / recent activity / UI preview；**不得将无限 terminal output 全部保存在内存**。上限数值是 ADR-001 的默认值。

---

## 6. 用户体验

### 6.1 主界面

侧栏是 agent 列表（状态符号 + 任务 + agent + 节点 + 一行活动），主区是当前会话的终端，header 显示本机与每个 peer 的状态。线框见 `docs/spec/ux.md`。

### 6.2 MVP Screens（只需要 4 个）

- **Screen A — Dashboard**：agents / status / needs attention；**WAITING 行可就地回答、TURN_DONE 行可就地下一条指令**，不必进 Screen B
- **Screen B — Terminal Workspace**：Sidebar + Tabs + Terminal
- **Screen C — New Agent Dialog**：创建 agent
- **Screen D — Session Settings**：修改 Display Name / Project，以及危险操作

不要增加 Settings Center 等无必要页面。peer 的增删是节点配置（§9.1），不是客户端设置；浏览器只记住最近打开的节点地址。

### 6.3 Attention Dashboard

首页不是 Terminal，而是 Agent Attention Dashboard（线框见 `docs/spec/ux.md`）。内部计算 Attention Score：

```
FAILED 100   WAITING 90   TURN_DONE 85   FINISHED 80   UNKNOWN 40   IDLE 30   STARTING 20   RUNNING 10
```

原则：**凡是卡在人身上的（FAILED / WAITING / TURN_DONE / FINISHED）都高于不需要人的（RUNNING / STARTING）**——跑着的 agent 什么都不需要，做完的 agent 正在等你；UNKNOWN 排中间（看不清，值得瞟一眼）。同分先按任务优先级（bd 的 P0–P4；无 bd 视为 P2），再按等待时长、状态变化与未读通知微调。用户打开页面最先看到**需要自己处理的 agent**，而不是最近创建的。

每一行的第一列是**任务**而不是进程名：issue id + 标题 > 首条 prompt 摘要 > display name。行下面再带两行，回答"它现在到哪了"：`❯` 是用户最后输入的那一条；`↳` 是 agent 正在做什么或它最后说了什么，两者互为兜底。这两行读自 **agent 自己写在磁盘上的 transcript**，不是 pane；读不出来的 agent 保持一行 pane preview。所有客户端形态用同一条规则渲染同样的两行。

**看结果**：TURN_DONE / FINISHED 的行展开后，除了两行，还显示该 worktree 的改动文件列表（`git status --short`），旁边是任务的验收标准（读自 beads，不复制）供对照；"看 diff"在会话所在 worktree 开一个只读终端跑 `git --no-pager diff`——复用现有终端，不做 diff 组件（§11）。git 写操作是 Git GUI（唯一例外见 §1.4）。

**就地 respond**：WAITING 行展开后直接显示问题（hook 的文本，兜底为最后几行 pane）与一个输入框 / 选项按钮；TURN_DONE 行展开后是"下一条指令"输入框。提交即 `POST /api/sessions/:id/input`（§7.3）。这是手机上的主路径；终端是逃生口。

### 6.4 创建 Session 与 Quick Launch

`+ New Agent` 对话框：Node（本机 + 在线的 peer，选 peer 则经一跳转发执行）/ Project / Worktree / Agent / Task（有 bd 的仓库从 issue 列表选，否则一句话，留空取首条 prompt）/ Name / Command（线框见 `docs/spec/ux.md`）。

Project 列表**不靠手写配置**：从 `project_roots`（§9.1）扫描 git 仓库并按最近使用排序——上百个仓库的手写列表会立刻过期。目标：**从打开 Dialog 到 agent 启动，常用项目最多 2–3 次操作。**

**Worktree 跟着任务生灭，agora 只管"生"**：对话框可新建 worktree（`git worktree add -b <name> <path> <base>`）——基础分支默认主 worktree 当前 checked-out 的分支（兜底 default branch），名字默认 issue id（无 bd 用 Name），路径按 `worktree_root` 约定（§9.1）。这是 §1.4 Git GUI 的唯一例外（只增不改的工作区准备）；任务验收后的合并与销毁归人（Git GUI / 终端），agora 不做。

**从就绪任务起会话**：对话框可以反过来从 `bd ready` 出发——选一个就绪任务，自动填仓库 / worktree / 名字，并把任务 id 写进首条 prompt。**agora 不写 beads**：claim 是 agent 开工的纪律（AGENTS.md），由 agent 自己执行；agora 对 beads 零写入（不变量 12），有守卫测试。

### 6.5 Keyboard First

桌面上管理几十个 agent 的效率靠键盘：命令面板（fuzzy search sessions / projects / nodes / actions）、侧栏过滤、上下一个 agent、按序号跳转。两条硬约束：浏览器全局快捷键**不得吞掉终端内的 Ctrl 组合键**；Cmd/Ctrl + 数字被浏览器保留，序号跳转用 Alt/Option。手机端没有键盘，快捷键与命令面板只在桌面生效。键位表见 `docs/spec/ux.md`。

### 6.6 Notifications

| 状态转换 | 通知 |
|---|---|
| RUNNING → WAITING | "Claude / frontend @ zuan needs input" |
| RUNNING → TURN_DONE | "Claude / agora @ mac finished its turn" |
| RUNNING → FINISHED | "Codex / tests @ mac finished" |
| RUNNING → FAILED | "Codex / migration @ mac failed" |

点击 notification 直接导航至对应会话（WAITING → 就地回答，不是终端）。V1 桌面用浏览器通知（A18）。

手机阶段（§11）：通知要在浏览器关闭时也能到达，因此 PWA 需要 Web Push（iOS 需加到主屏）。**推送由承载 PWA 的那个节点发出**（它经 peer 链路看得见全部事件），推送订阅只注册在那一个节点；节点到不了推送端点（如 FCM）时降级为"PWA 打开期间实时更新"，并在 health（§10.3）显示原因；不做经其它节点的推送中转。

### 6.7 Terminal Preview

Sidebar 显示最近一行或简化 activity。必须：strip ANSI escape sequences、truncate、不显示大段内容、不做 LLM summary。**MVP 不调用任何模型。**

### 6.8 Multi-device 与 Reconnection

允许多个客户端同时登录；每个客户端一次只连一个节点（§3.5），看到的是该节点及其 peer 的会话。MVP 允许多个 client attach；V2 增加 Controller / Viewer 角色与 Take Control，避免两台设备同时向 CLI 输入。

网络切换（WiFi → 5G）时 agent 不受影响：WebSocket 断开只关闭 PTY attachment，运行时中的 agent 继续；恢复后 new WebSocket → new PTY → attach。

### 6.9 Responsive

**所有设备功能集相同**（不变量 9）；手机 / iPad 优先优化 Dashboard、就地回答与确认（§6.3）；桌面优先优化终端与创建。Desktop breakpoint 显示 sidebar；窄屏用 sidebar drawer。V1 只做桌面断点（§0.1）；窄屏与手机优化随手机阶段，届时手机终端可用但不做完整 ergonomics（§11）。

### 6.10 视觉方向

关键词：**fast、dense、keyboard-first、low visual noise、dark-mode-first**；不演化为传统 enterprise dashboard。参考与组件库见 `docs/spec/ux.md`。

---

## 7. 技术栈（原则）【ADR-001 一并确认】

### 7.1 Backend

要求：PTY support 成熟、WebSocket 简单、process management 简单、并发模型适合大量 terminal stream、单 binary、部署简洁、内存占用小、**能在 macOS / Linux / Windows 三个平台出单 binary**（§0.1）。语言、运行时与依赖清单在 ADR-001 一并决定；依赖保持最少，不得为普通 CRUD 引入大型 backend framework 或重型 ORM。

### 7.2 Frontend

浏览器前端 + xterm.js 终端 + PWA（manifest + service worker）。PWA 在手机阶段是**必选，不是增强**——iOS 上没有它就没有推送（§6.6、§8）；V1 桌面上可选。框架选择在 ADR-001。

### 7.3 Backend API（原则）

- **每个节点同一套 API，两种调用方**：浏览器（人的登录 cookie；loopback 免登录）与 peer 节点（`Authorization: Bearer <机器 token>`，§8）。没有 peer 专用端点。
- **会话 id 一律 `<node>:<id>`**；本机会话与并入的 peer 会话同列，每条带 `node`；对 peer 会话的写操作与终端流由本节点经一跳转发（§3.5）。
- **respond 有两种语义**：结构化决定（权限 / 选择——经挂起的 hook 返回，有 hook 的 agent 走这条）与原始文本（经 PTY——自由问答、下一条指令、无 hook 的 agent）。
- **DELETE metadata 与 kill process 是两个端点、两个语义**；不能因为删除 metadata 而误杀运行时会话。
- **API 版本**：`GET /api/system` 返回 `api_version`；调用方版本不一致时必须识别并降级或提示，不得静默错读。
- **hook 投递不经 HTTP**：agent 的 hook 命令把事件写进投递箱文件、经 unix socket（0600）唤醒 daemon；没有 hook 端点，也就没有要守的凭据（ADR-002 D3）。

端点与消息清单见 `docs/spec/api.md`。

### 7.4 WebSocket Protocol

终端流与全局事件流各一条 WS；MVP 用 JSON / Text 足够，binary terminal frames 放到 V2。消息形态见 `docs/spec/api.md`。

### 7.5 仓库结构

代码即真相，不在此维护目录树。硬约束一条：agent 特定代码只能住在 adapter 目录（规则 5）。

---

## 8. 安全模型【ADR-003】

该应用本质上拥有 **Remote Shell Access**，安全等级等价于 SSH。

- 默认 `listen = 127.0.0.1:7680`；**禁止默认 `0.0.0.0` 并暴露至公网**。要让另一台设备访问，必须显式配置监听地址并启用认证。
- **非 loopback 监听而未配置认证 → 拒绝启动**："认证"指任一 principal 的凭据已配置（TOTP 或机器 token）。"改了 listen 忘了配凭据"不能是一个可运行的状态（理由见 ADR-003）。此项有守卫测试。
- **TLS 是 agora 的事，不是部署的事**：PWA、service worker、Web Push 只在安全上下文工作。V1 只需 peer 链路的自签证书 + 指纹钉住（§3.5）；被浏览器直接打开的非 loopback 节点（手机阶段）必须能以浏览器可信的 HTTPS 提供服务，至少一条路径自动化——三条候选与默认选择在 ADR-003。
- 所有命令不得通过 shell string interpolation 拼接未经验证的用户输入。

### 人的凭据：TOTP【ADR-003】

- 只绑 loopback 时可不配置（loopback 即安全边界）；一旦要让**人**从非 loopback 访问，必须先完成 TOTP 注册。V1 没有这种访问（浏览器只开 loopback，zuan 只接受机器 token），TOTP 随手机阶段落地。**多用户主机上 loopback 不是安全边界**（§0.1）——loopback 也认证或改 0600 unix socket，由 ADR-003 定。
- 注册后，**人的**请求（含 loopback 的浏览器与 WS 握手）必须携带登录 cookie；hook 投递不经 HTTP（§7.3）。
- 算法参数、cookie 属性、登录限流阶梯、可信代理、响应头、WS 跨站防护：ADR-003。原则一句：**一旦不只绑 loopback，认证就是唯一控制点，登录限流的强度直接等于整个系统的强度。**

### 多节点信任边界

- **两种 principal**：人用 TOTP 登录；节点作为 peer 客户端用**机器 token**——在被访问的节点上按 peer 单独签发、可吊销、只存哈希——并**钉住对方证书指纹**。peer 之间不依赖浏览器信任链，自签证书即可。
- **peer 访问默认关闭**：没有签发过机器 token 的节点不接受任何 Bearer 调用；签发 token 时该节点必须已在非 loopback 监听且已配置 TLS，否则拒绝（与"非 loopback 无认证拒绝启动"同一条守卫，有测试）。
- 网络可达（局域网 / tailnet / 公网）≠ 授权（不变量 11）。
- **危险操作的确认逻辑在会话所在节点执行**：转发节点只转发请求，不代替判断，也不能绕过。
- 持有方的 `token_file` 是明文：`0600`、不进 git、不进日志。签发与轮换、被攻破的代价与止损：ADR-003。

### 危险操作确认

Kill / Restart（会杀时）需要显式确认（确认框文案见 `docs/spec/ux.md`）。**确认跟着"杀"走，而不是跟着按钮走**：判断依据是 agent 状态，不是运行时会话是否 alive——会杀 → 弹框且第一句话就是那个 kill；不会杀（FINISHED / FAILED / 会话已不在）→ 直接执行，且菜单项自己说明恢复的是哪个对话。一个永远都会出现的确认框，就是在真正需要拦住人的时候被一路点过去的确认框。

Close Browser Tab 只代表 detach UI，绝不代表 kill process。

---

## 9. 配置与存储

### 9.1 配置

- 每个节点一份；`node.id` 是全局会话 id 的前缀（安装时生成，改名需迁移）；`peers` 默认空——多节点是显式配置出来的状态（§3.5）。
- 项目列表不靠手写：`project_roots` 扫描 git 仓库并按最近使用排序；`worktree_root` 是新建 worktree 的存放约定，默认 `../<repo>-wt/<name>`（§6.4）。
- 机器 token 由被访问的节点签发，本节点只存哈希；持有方存明文 `token_file`（§8）。浏览器只记住最近打开的节点地址（不变量 6）。

### 9.2 存储

- SQLite 只存 metadata / preferences / mapping（不变量 7，字段见 §4.2）；迁移带版本号（规则 10）。
- MVP 不保存 operational telemetry；peer 的最后视图只在内存（重启后等待重连），持久化历史留 V2（§11）。

YAML 与 schema 见 `docs/spec/config.md`。

---

## 10. 性能目标与运维

### 10.1 性能目标

至少支撑：**50 persistent sessions、20 simultaneously RUNNING agents、5 simultaneously attached terminals**，在普通工作站上无明显压力；目标按**每节点**定义，不因当前实例的规模放宽。节点并入 N 个 peer 的开销随 N 线性、不得超线性；经一跳转发的终端延迟增量在 LAN / tailnet 下不可感知（目标 < 10 ms）。

| 指标 | 目标 |
|---|---|
| Dashboard 首屏 | < 1 s（LAN 桌面）/ < 3 s（蜂窝，PWA 冷启动到列表；手机阶段） |
| Session switch perceived | < 150 ms |
| Keystroke latency (LAN / VPN) | primarily network-bound |
| Idle server memory | < 150 MB target |
| 状态变化 → 可见 | hook 路径不引入任何轮询等待（< 1 s，由网络决定）；文本兜底 = 自适应轮询间隔 |

状态扫描不得产生明显 CPU 消耗；轮询按状态自适应，间隔是 ADR-001 的默认值。

### 10.2 Logging

结构化日志（timestamp / level / component / session_id / event）。**禁止默认记录完整 terminal content**；尤其不要把 API keys / source code / credentials / prompts 写入普通日志。

### 10.3 Health Check

`GET /api/health` 报告：runtime / database 可用性、TLS 模式、推送端点可达性、每个 peer 的在线状态与 last_seen（JSON 见 `docs/spec/api.md`）。Header 对本机与每个 peer 显示状态（在线 / 异常原因 / 上次见到）；推送端点不可达时显示原因。

---

## 11. MVP 范围外（Explicitly Deferred to V2）

以下内容除非 MVP 已经非常稳定，否则不得实现：

```
Agent orchestration          Agent-to-Agent messaging     Task DAG
Prompt management            Agent spawning Agent         Git integration
Diff visualization           Token usage                  Cost tracking
Conversation indexing        LLM summary                  Semantic search
RBAC                         Teams                        Cloud service
```

（Git integration 与 Diff visualization 的**只读观察形态**——改动列表、canned `git diff` 终端——已进基线（§6.3、§12）；留在这里的是 git 写操作、diff 组件、Token / Cost、LLM summary。）

**Artifact Viewer（文件路径变体）**：渲染 session 自己指名的那**一个**文件 / 页面。界线：≠ File Explorer——不列目录、不树形导航、不编辑、不提供自由路径输入；URL 抓取路径是 Non-Goal（§1.4）。

**Multi-host 的 V1 之外**：**peer 历史**（常开节点持久化其它节点的快照与事件历史）；**反向拨号**（工作节点主动拨出到常开节点、本机零监听——只在 peer 从外网不可达时才需要）；**多跳转发**。

**Windows 节点**：在范围边界内（§0.1），V1 延期；ADR-001 的运行时抽象必须为它留位，安装脚本与 PTY 层的 Windows 实现在此兑现。它实质上是**第二个运行时实现**；容器 / 云沙箱 / SDK 运行时同归此处。

**手机客户端（iOS / Android，V2 首批）**：PWA 安装、Web Push（推送端点不可达时降级 + health 显示，§6.6）、TOTP 人机认证、承载 PWA 的节点的浏览器可信证书（§8）、窄屏布局（§6.9）。验收（编号全局唯一，自 §12 移入）：

- [ ] **A13** 可以从 Mac detach，在手机上 reconnect（反之亦然）
- [ ] **A19** iPhone PWA 收到推送；Android 在节点可达 FCM 时收到推送、不可达时 health 显示原因
- [ ] **A28** 从手机经 zuan 回答 Mac 上的 WAITING、attach 到 Mac 上的会话终端并交互（一跳转发）
- [ ] **A35** 节点以 HTTPS 提供服务，手机可安装 PWA（ADR-003 证书路径至少一条走通）
- [ ] **A37** iPhone 与 Android 浏览器能看 Dashboard、回答 WAITING、确认危险操作；可安装为 PWA

**TUI 桌面客户端**：同一套节点 API 的第二种客户端形态，桌面专用。V1 只有浏览器——它是三类设备唯一共有的运行环境（§0.1）。

**手机终端 ergonomics**：软键盘 Ctrl / Esc、手势，以及对远程节点的剪贴板与文件上传。V1 手机终端可用不优化（§6.9）。

---

## 12. MVP Acceptance Criteria

MVP 完成必须同时满足；验收按 beads epic 分阶段推进：**M1a 终端底座 → M1b Agent 感知 → M2 peer 与安装运维**；**M3 产出与起会话增强** 在 M1b 之后、与 M2 并行；手机是 V2 首批（§11）。阶段门用 blocks 表达，每个 epic 的 `--acceptance` **引用下列编号**而不复制文本。

- [ ] **A1** 可以通过浏览器查看所有运行时中的 agent session
- [ ] **A2** 可以创建 Claude Code session
- [ ] **A3** 可以创建 Codex session
- [ ] **A4** 可以创建 Grok Build session
- [ ] **A5** 可以创建 Generic Shell session
- [ ] **A6** 浏览器终端支持完整交互
- [ ] **A7** Terminal resize 正确
- [ ] **A8** Browser refresh 后 agent 不退出
- [ ] **A9** Browser close 后 agent 不退出
- [ ] **A10** WebSocket failure 后 agent 不退出
- [ ] **A11** agora daemon restart 后 agent 不退出
- [ ] **A12** daemon restart 后能够重新发现已有 agent
- [ ] **A14** Sidebar 可以显示 RUNNING / WAITING / TURN_DONE / IDLE / FINISHED / FAILED；有 hook 的 agent 的 WAITING / TURN_DONE 来自 hook，并带来源与置信度
- [ ] **A15** 不打开终端即可回答 WAITING、给 TURN_DONE 下一条指令
- [ ] **A16** 手动在 Terminal.app 里起的 Claude Code 会话出现在列表里（hook 采纳，external）
- [ ] **A17** WAITING agent 会进入 Attention 区域
- [ ] **A18** WAITING / TURN_DONE / FINISHED / FAILED 可以触发 browser notification
- [ ] **A20** Close Tab 不会 kill agent
- [ ] **A21** Kill 必须经过 explicit confirmation
- [ ] **A22** 可以 adopt 外部创建的运行时会话
- [ ] **A23** 每个会话显示"在做的任务"（issue id + 标题，或首条 prompt 摘要）；attention 同分按任务优先级排序
- [ ] **A24** 不依赖 Redis / Postgres / Kubernetes
- [ ] **A25** 可以作为单机 daemon 运行
- [ ] **A26** 节点安装脚本支持 macOS 与 Linux，含随登录自启（launchd / systemd）——否则重启后"打开一个 URL"就是 404（Windows 延期，§11）
- [ ] **A27** zuan 上一条命令装好节点并为 Mac 签发机器 token；Mac 配 zuan 为 peer 后，打开 `127.0.0.1` 看到两台节点的会话并标明节点；Mac 自身仍只听 loopback
- [ ] **A29** peer 离线时其会话显示 stale（含上次见到时间）而非消失，本机会话不受影响；恢复后自动重连
- [ ] **A30** 每个节点独立认证；一台能连通节点但未授权的设备、一个未持有机器 token 的节点，都被拒绝
- [ ] **A31** 未签发机器 token 的节点拒绝一切 Bearer 调用；签发 token 的前置条件不满足时拒绝签发，且有守卫测试
- [ ] **A32** Restart 用 agent 自报的对话 id resume，不用 `--continue`；在 pane 里 `/clear` 后 Restart 恢复的是新对话
- [ ] **A33** 浏览器或 peer 客户端遇到 API 版本不一致时提示或降级，不静默错读
- [ ] **A34** 节点在非 loopback 监听且未配置认证时拒绝启动，且有守卫测试
- [ ] **A36** 不变量 1–5、7、8、10、11 各有 fake-agent 集成测试；逐条关掉守卫，对应测试变红
- [ ] **A38** Mac 打开 `127.0.0.1`，经 peer 一跳回答 zuan 上的 WAITING、attach 到 zuan 上的会话终端并交互
- [ ] **A39** 一条命令升级节点，升级期间 agent 不死、会话与 metadata 完整——不变量 3 与规则 10 最真实的测试
- [ ] **A40** 会话行展开显示任务的验收标准（读自 beads，agora 不复制）
- [ ] **A41** TURN_DONE / FINISHED 的会话可看改动文件列表，并一键在其 worktree 开只读终端看 `git diff`；看结果链路不做任何 git 写操作
- [ ] **A42** `ended_at` 落库；attention 的等待时长按事件时间计算
- [ ] **A43** 从 `bd ready` 选任务起会话：预填仓库 / worktree / 名字与首条 prompt；agora 对 beads 零写入，有守卫测试
- [ ] **A44** 对话框可新建 worktree：base 默认主 worktree 当前 checked-out 分支、路径按 `worktree_root` 约定（§6.4）；agora 不做合并与销毁

编号全局唯一、不复用：A13 / A19 / A28 / A35 / A37 在 §11 手机条目。

开发阶段划分、里程碑与测试策略见 `ROADMAP.md`（由 beads 生成）。
