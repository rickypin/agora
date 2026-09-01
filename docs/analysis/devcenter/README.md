# DevCenter 深度分析与评价

| | |
|---|---|
| 目的 | agora（多 Agent 管理工具）立项前，对同类项目 `~/code/devcenter` 的需求定义、架构设计、实现方案做全面评价，并划定可参考、可借鉴的范围 |
| 对象 | DevCenter v0.5.1，commit `1540ac4`（2026-08-28），作者 Vader Yang |
| 分析日期 | 2026-09-01 |
| 方法 | 通读 MISSION / ROADMAP / 12 篇 ADR / 配置示例与 git 历史；三路代码级评审（附录 A/B/C，含 `path:line` 引用）；本机实际编译与运行非 tmux 依赖的测试子集 |
| 附录 | [A 后端核心](appendix-a-backend.md) · [B 多主机/搬迁/浏览器](appendix-b-multihost.md) · [C 三端客户端与测试](appendix-c-clients-tests.md) |

---

## 0. 结论摘要

**一句话定位**：DevCenter 是一个"远程多 Agent **终端**工作台"——它管理的对象是 *tmux session 里跑着的 CLI coding agent*（Claude Code / Codex / pi / shell），提供 launch → observe → switch → respond → resume 五个动作。它**不是**任务编排、Agent 协作或 Agent 运行时；这些被它明确列为 Non-Goal。

**七条核心结论**：

1. **需求定义是这个项目最强的部分。** 北极星问题（"20 个 agent 在跑，我该看哪几个"）、五动作、Non-Goals、Definition of Done、7 条架构不变量、7 条 Coding-Agent 施工规则、20 条 MVP 验收——每一层都可执行、可验收。agora 应直接继承这种**写法**，而不只是内容。
2. **ADR 制度是最值得学的工程实践。** 12 篇 ADR 中，被否决的 ADR-010 全文保留作为"给下一个提出同样想法的人的回答"；ADR-011 附录记录了 4 批上线后发现的事故与修正。这种"决策 + 反例 + 实测数据"的记录方式，是 Coding Agent 施工模式下防止架构漂移的关键。
3. **核心技术路线"tmux 承载 + capture-pane 文本启发式"对当前 Claude Code 可用但脆弱。** 真正 agent 特化的检测规则只有 Claude 的 4 条子串；codex/pi 直接退化为通用规则；没有防抖、没有真正的状态机。作者自己已经把"这个会话在做什么"和"这是哪段对话"改从磁盘 transcript 和注入的 SessionStart hook 读取——只有状态检测这一层还停在屏幕文本上。
4. **范围在 16 天内从"thin console"扩张到了"舰队工作站"。** Phase 0–4（含多主机 + TOTP）在第一天完成；之后按时间加入 macOS 原生客户端（08-14）、session tabs / 只读 viewer（08-16）、HarmonyOS 客户端（08-17）、跨主机搬迁（08-18–21；devcenter 自称"搬运"；5000+ 行，一个功能）、远程浏览器像素流（08-24，~1500 行）；两个原生客户端合计 ~2 万行。后三者的价值高度绑定作者自家 6 台机器的具体场景。
5. **工程质量两极：** 注释密度极高且每条都带"哪天在哪台机器实测"；~850 个测试，集成测试大部分跑真 tmux。但 `server.rs` 5036 行 god file、hub↔node 零认证、async handler 中同步 spawn 子进程、事件流只做了一半（前端仍 3 秒轮询）、三端领域逻辑手写三遍并已出现漂移。
6. **本机实测**：fresh clone 直接 `cargo test --no-run` 失败（`web/dist/.gitkeep` 从未提交）；补目录后 10.8 秒编译通过；251 个不依赖 tmux 的测试全部通过，其余需 tmux（审查机 2026-09-01 未安装，详见 §4.4）。
7. **对 agora 的借鉴范围**：**理念与纪律层几乎全部继承**；**技术方案层选择性参考**（tmux 承载、Adapter、WS 协议、attention 排序、hook drop-box 技巧）；**舰队特性层明确不借鉴**（ssh 隧道联邦、跨主机搬迁、远程浏览器、三端手写客户端）；并补上 devcenter 刻意留白的 **Agent 语义层**（任务 / 目标 / 产出 / 成本 / 结构化事件）——如果 agora 的"管理"要比"看终端"更深一层，这才是主战场。

---

## 1. 项目概况

### 1.1 基本事实

| 项目 | 值 |
|---|---|
| 仓库 | `~/code/devcenter`，单作者（Vader Yang），146 commits，tag v0.1.0 / v0.5.0 / v0.5.1 |
| 时间跨度 | **2026-08-13 → 2026-08-28，共 16 天**；累计 +80,112 / −2,393 行 |
| 开发方式 | 人定方向 + Coding Agent 施工（MISSION §2.3 明写"本项目大概率由 Codex / Claude 自动开发"）；issue 追踪用 beads（154 个 issue，77 个顶层；commit 1540ac4 时的数据）；Claude 与 Codex 的 hook 都配了 `bd prime` |
| 后端 | Rust 2024 edition，axum + tokio + portable-pty + rusqlite + rust-embed；20 个直接依赖（`[dependencies]`，不含 dev-dependencies），**无 TLS 栈**（刻意，为保纯 Rust 交叉编译） |
| 前端 | React 18 + Vite + xterm.js，仅 4 个运行时依赖，无状态库、无路由、无 UI 库 |
| 原生客户端 | macOS（SwiftUI + SwiftTerm）、HarmonyOS NEXT（ArkTS + 内嵌 xterm.js） |
| 部署形态 | 单 binary + 内嵌前端 + SQLite；hub 通过 `ssh -N -L` 聚合 worknode（作者舰队：m4ba / laojun / wukong / jinjiao / yinjiao / sglog-dev 共 6 台） |
| 许可 | **仓库内没有 LICENSE 文件**（MISSION §7.5 的目录规划里有，实际未创建） |

### 1.2 规模

| 层 | 代码行数 | 测试数量 |
|---|---|---|
| Rust `src/` | 20,733（`server.rs` 5,036；`session.rs` 1,374；`agent/mod.rs` 971；`moving.rs` 921） | 144 内联单测 |
| Rust `tests/` | 11,301 | 326 集成测试（21 个文件；其中 13 个文件 / 218 个用例依赖真 tmux，其余 108 个无 tmux 可跑，见 §4.4） |
| Web `src/` | 9,677（TS/TSX 7,414 + CSS 2,263） | 纯函数测试寄生在 Playwright 里 |
| Web `e2e/` | 3,760 | ~170 Playwright 用例（24 个 spec） |
| macOS | 8,118 Swift（源码约 5,972） | 94 |
| HarmonyOS | 12,027 ArkTS（源码约 10,373，其余为测试） | 118（纯逻辑，headless node 跑） |
| 文档 | MISSION 42 KB + ROADMAP 14 KB + ADR 97 KB | — |

### 1.3 时间线（按提交）

```
08-13  Phase 0 spike → Phase 1 console → Phase 2 agent awareness → Phase 3 UX
       → Phase 4 multi-host (WP1–WP8, 含 TOTP + node deploy)        ← 30 commits，一天
08-14  macOS 原生客户端；登录路径面向公网加固（ADR-008 Amendment）
08-16  Phase 5：session tabs / 只读 artifact viewer / 终端内可点击链接
08-17  HarmonyOS 客户端；ADR-010 端口转发提出并当天否决；viewer 修订 ×3
08-18–21  Phase 6：跨主机搬迁（该区间 36 commits，其中搬迁相关约 25：候选识别、JSONL 改写、hub 中继、墓碑、undo）
08-23  identity（agent 自报 session id）、workspaces、首页 recap 两行
08-24  Phase 7：远程浏览器（ADR-012），三端各加 browser pane
08-27/28  v0.5.0 / v0.5.1，收尾修复
```

**观察**：Phase 0–3（MISSION 定义的整个 MVP）和 Phase 4 在同一天完成，说明 MISSION/ROADMAP 是先于代码写好的完整施工图；之后的 15 个日历日（13 个提交日）全是 MVP 之外的扩张。

---

## 2. 用户问题与需求定义

### 2.1 它要解决什么问题

一台高性能工作站上同时跑着 N 个 CLI coding agent；用户在笔记本 / 平板上远程工作时，SSH + tmux / Remote Desktop / VS Code Remote 要么 UX 差要么太重。DevCenter 的答案是 **"One URL → All Agents"**：一个自托管的 Web 控制台，看到所有 agent session、像浏览器标签一样切换，并且**知道哪个 agent 需要人**。

北极星问题写得非常具体：

> "我现在有 20 个 Coding Agents 在一台 Workstation 上运行，我需要关注哪几个？"

所有功能取舍都以是否改善 `launch → observe → switch → respond → resume` 五个动作为准。

### 2.2 需求定义的质量：优点

这是本项目最值得学的部分，逐项列出它做对了什么：

| 手段 | devcenter 的具体写法 | 为什么好 |
|---|---|---|
| **北极星问题** | 一个具体场景 + 一个数字（20 个） | 每个功能提案都能被问"它改善了五个动作中的哪一个" |
| **Non-Goals 清单** | 不是 IDE：不做编辑器 / Git GUI / Debugger / File Explorer / AI Chat / 项目管理 / Workflow Engine / Remote Desktop | 比 Goals 更能约束 Coding Agent 的自由发挥 |
| **Definition of Done** | "无需 Remote Desktop、不需要频繁 SSH/tmux，一眼看到哪些在工作、哪些需要输入" | 把"完成"定义为用户体验而非功能列表 |
| **架构不变量（7 条）** | Browser crash ≠ Agent crash；WS disconnect ≠ Agent crash；daemon crash ≠ Agent crash；Close Tab ≠ Kill；一个 agent 坏不影响另一个；UI 状态可丢弃、Agent 状态持久；tmux 是活性真相、SQLite 只存 metadata | 全部可测试；1–4、6、7 在测试里钉住了，第 5 条只是"大体成立"（见 §3.2）（`tests/session_manager_test.rs`、`federation_test.rs` 的 detach 不变量） |
| **Coding Agent 施工规则（7 条）** | 不得替换 tmux 架构；不引入 Docker/Redis/Postgres/K8s/Electron/Next.js；每个 Phase 独立可运行；不能用 mock 破坏真实 PTY/tmux；agent 特化必须走 Adapter；破坏性操作必须是显式 API；连接所有权与进程所有权不得耦合 | 这是写给 AI 施工者的"宪法"，是本项目在 Coding Agent 驱动下没有散架的原因 |
| **显式状态机** | STARTING / RUNNING / WAITING / IDLE / FINISHED / FAILED / UNKNOWN，附转移图；并规定"宁可 UNKNOWN 不误报 WAITING" | 状态语义在写代码前就定死 |
| **生命周期动词分离** | Detach / Close Tab / Archive / Kill / Delete Metadata / Restart / Move 七个动词各有精确语义；Kill 与 Delete Metadata 在 API 与 schema 层都必须分离 | 避免了"删掉一条记录顺手杀了进程"这类灾难 |
| **危险操作确认的原则** | "确认跟着杀走，而不是跟着按钮走"：Restart 只在真的会杀掉运行中 agent 时才弹框 | 一个永远出现的确认框就是那个真正需要时被一路点过去的确认框 |
| **性能目标量化** | 50 sessions / 20 RUNNING / 5 attached；dashboard < 1s；切换 < 150ms；idle < 150MB | Phase 3 验收时实测填入（56ms / 25ms / 9.2MB） |
| **MVP 验收 20 条 + Phase gate** | 每条对应 Phase；上一 Phase 不过不进下一 Phase | ROADMAP 与 MISSION 互相引用，无悬空条目 |
| **V2 延期清单** | Agent orchestration / A2A messaging / Task DAG / Prompt management / Git / Diff / Token & Cost / Conversation indexing / LLM summary / Mobile native / RBAC / Teams / Cloud | 把"以后再说"写成清单，比口头说更能挡住 scope creep |

### 2.3 需求定义的局限

- **单用户、单作者、无外部验证。** 主要用户就是作者本人；所有"用户反复要做两件事"的需求陈述（ADR-009）来自作者自己的使用。对 agora 而言，这份需求定义是一个**高质量样本**而不是市场验证。
- **"管理"被定义为终端级观察。** DevCenter 知道一个 agent 是 RUNNING 还是 WAITING，但不知道它在做什么任务、目标是什么、产出了什么、花了多少钱。这是刻意的（MISSION §6.7 "MVP 不调用任何模型"；MISSION §11 全部延期），但也意味着它对"多 Agent 管理"这个词只覆盖了最底层。
- **对 agent 的模型是"一个会打印文字的进程"。** Adapter 只回答"屏幕上有没有提示词"；对 agent 的结构化能力（Claude Code hooks、Codex notify、Agent SDK）只在 MISSION §5.6 留了一句"V2 分层来源"。这个假设在 2026 年已经偏保守——作者自己在 ADR-011 第 0 级识别中也不得不用 hook 注入了。
- **多用户 / 权限完全缺席。** 安全模型等价于 SSH：拿到 cookie 等于拿到所有节点的 shell。作为个人工具合理；作为团队工具不可用。

### 2.4 范围演化与"边界的三次落笔"

Non-Goals 清单在 16 天里被重新画线三次，每次都写了 ADR 正面回答矛盾：

| 次序 | 触发 | 画的线 | ADR |
|---|---|---|---|
| 1 | 想看 agent 写出来的报告 | **Viewer ≠ File Explorer**：只渲染 session 自己指名的那一个文件，没有列目录、没有自由路径输入框 | ADR-009 |
| 2 | 节点上的 Vite dev server 在 viewer 里是空白 | **端口转发 = Remote Desktop 的一半**，否决："局域网内只是便利，唯一带来新能力的远程场景成本最高" | ADR-010（Rejected） |
| 3 | 同一个问题 | **一个浏览器是 Observe；一台机器的桌面是 Remote Desktop**——走像素不走端口 | ADR-012 |

**评价**：作者对边界的自觉程度远超一般项目——每次扩张都有"为什么这不违反 Non-Goals"的论证。但从结果看，产品确实从"thin console"长成了"能搬 session、能开浏览器、有三个原生客户端的舰队工作站"。这提示 agora：**Non-Goals 挡得住功能，挡不住场景**——当作者的日常场景（6 台机器、跨机器接着干活）需要时，边界会被"合理地"重画。agora 立项时应把"目标场景的物理形态"（几台机器、几个人、什么设备）也写进 MISSION，而不只写功能边界。

### 2.5 对 agora 的直接启示

agora 的目标是"多 Agent 管理工具"。devcenter 给出的最重要启示是：**先回答"管理"是什么层次**。至少有三层，它们的架构完全不同：

| 层次 | 管理什么 | devcenter 覆盖 | 关键技术问题 |
|---|---|---|---|
| L1 终端/进程级 | session 活着吗、在跑还是在等人、切过去回答它 | ✅ 完整覆盖 | 持久化运行时（tmux/PTY）、状态检测、远程 attach |
| L2 任务/语义级 | 每个 agent 在做什么任务、目标、进度、产出、成本、对话历史 | ❌ 只有 recap 两行 + session id 身份 | 结构化事件源（hooks/SDK）、transcript 索引、产出物追踪 |
| L3 协作/编排级 | 多 agent 分工、依赖、消息、DAG、并行审查 | ❌ 明确 defer | 编排引擎、A2A 协议、任务队列 |

devcenter 证明了 L1 可以用 tmux + 单 binary 在一天内做出可用的 MVP；也证明了 L1 的天花板——它最后加的三个大特性（搬迁、浏览器、原生端）都是在 L1 内部横向扩张，而不是向上。**agora 应在 MISSION 第一页就写明自己站在哪一层、要不要向上走。**

---

## 3. 架构设计

### 3.1 核心架构

```
tmux       owns Agent lifecycle
DevCenter  owns discovery + metadata + observation + remote interaction
Browser    owns presentation only
```

链路：`xterm.js ↔ WebSocket ↔ Terminal Gateway ↔ PTY ↔ tmux attach ↔ Agent`。状态检测走**独立于浏览器连接**的 State Observer（周期 `tmux capture-pane -S -200`），保证无人在看时也能检测。daemon 重启后通过 `tmux list-sessions ⨝ SQLite` 重建，agent 无感。

这个 ownership 三分法是整个项目的定海神针，后面所有扩张（联邦、搬迁、浏览器）都被强制套进它：worknode 就是普通 daemon；跨主机搬迁 = "换台机器的 Restart"；远程浏览器 = "在 tmux 里起的一个 session"。

### 3.2 七条不变量：文档 vs 代码

| 不变量 | 落地情况 |
|---|---|
| 1–3 Browser / WS / daemon crash ≠ Agent crash | ✅ 结构性保证（agent 挂在 tmux 下），并有集成测试 |
| 4 Close Tab ≠ Kill | ✅ 前端 tab 只是 pane 隐藏 |
| 5 一个 agent 坏不影响另一个 | ⚠️ 大体成立；但 observer 单线程串行 tick，一个 hang 住的 tmux 调用会拖住所有 session 的状态更新（`tmux.rs:88` 无超时） |
| 6 UI 状态可丢弃 | ✅ 前端不持久化打开的 pane；hub 的 move 进度视图明确"允许蒸发" |
| 7 tmux 是活性真相、SQLite 只存 metadata | ✅ 严格遵守——DB 里没有任何"活着"字段，`list()` 每次即时 join |

### 3.3 ADR 体系

12 篇 ADR 分两代：

- **ADR-001~008（第一天写）**：每篇 < 3 KB，Context / Decision / Consequences 三段，是"把 MISSION 里的决定编号"。
- **ADR-009~012（扩张期写）**：13–42 KB，包含备选方案、拒绝理由、真机实测数据（吞吐 114 MB/s vs 3.9 MB/s、11 段对话里 9 段早于 session 行）、上线后的事故与修正（ADR-011 有 4 批附录：tar `--` 分隔符、macOS→Linux 死锁、`starts_with` 分量比较漏洞、沉默≠同意）。

**特别值得学的三个做法**：

1. **被否决的 ADR 全文保留**（ADR-010）："这个想法一定会被重新提出来，这篇是给那个人的回答。"
2. **"Non-Goals 一节比 Decision 一节更重要"**（ADR-010/012 原话）：每个新特性内部再列一次 non-goals，并写"什么会让它变危险"的 review 检查表。
3. **"一个没有测试的守卫，和一个不存在的守卫，看起来完全一样"**（ADR-011）：每条安全围栏都要求有一条测试钉死，并"逐条关掉、看测试变红"验证过。

**不足**：ADR 覆盖了后端的每个决定，却没有为"三端原生客户端"这个最大的成本决定立 ADR（Mac 与 HarmonyOS 各只有一个 README 给理由）；ADR-004 承诺的"分层状态源"至今没有后续 ADR 跟进。

### 3.4 关键技术决策逐项评价

| 决策 | devcenter 的选择 | 评价 | 对 agora 的建议 |
|---|---|---|---|
| **持久化运行时** | tmux 拥有生命周期，daemon 永不直接 spawn agent | ✅ 用最少代码换来 Invariant 1–3、scrollback、多客户端、外部 session 采纳。❌ 硬依赖 tmux（审查机未装即无法运行；Windows 不可能）；gateway 会改用户全局 tmux 选项（`mouse on` / `set-clipboard`） | 若 agora 也需要"浏览器关了 agent 还在"，tmux 仍是最低成本方案，但应抽象为 `Runtime` 接口（tmux / 自管 PTY+supervisor / 容器），不要让 tmux 泄漏到 session 模型之外 |
| **状态检测** | `capture-pane` 文本 + Adapter 正则；confidence 只做调试字段 | ⚠️ 对 Claude 可用，对 codex/pi 未实现；全 tail `contains`（附录 A §2b）；无防抖、无真正状态机；fixture 总量 4.3 KB | 反转优先级：**hook/事件 > 进程状态 > 屏幕文本**。devcenter 自己的 SessionStart hook drop-box（`claude.rs:266-320`）扩展到 Notification / Stop / PreToolUse 即可覆盖 WAITING/FINISHED；屏幕文本只做兜底且带驻留时间 |
| **语言与框架** | Rust + axum + portable-pty + rusqlite；无 ORM、无 TLS 栈 | ✅ idle 9 MB、单 binary、交叉编译干净。❌ async Rust 心智成本在代码里可见（三种并发模型混用、async 里同步 spawn 子进程） | 语言不是关键决策；关键是"单 binary + 内嵌前端 + 零外部服务"这条部署约束值得继承 |
| **存储** | SQLite 只存 metadata；`CREATE IF NOT EXISTS` + `ALTER ADD COLUMN` 增量迁移 | ✅ 边界清晰。❌ 无 schema 版本、无 WAL、靠 "duplicate column" 字符串幂等 | 继承"外部系统是活性真相、DB 只存 metadata"；迁移用 `user_version` |
| **终端协议** | JSON 文本帧 `{input/output/resize/ping}`，二进制帧延期 | ✅ 简单够用。❌ Web 端定义了 ping/pong 却不实现心跳与重连；事件流 `/api/events` 只有状态事件，前端仍 3 秒轮询 | 协议本身可直接参考；事件流要么做完整（含 create/kill/rename + lag 通知），要么只留轮询，不要留一半 |
| **安全边界** | 默认 loopback；TOTP + cookie；按来源阶梯限流 + 受信网段豁免；Origin 校验 WS | ✅ 单用户场景下扎实；限流设计（DoS 锁不住主人）值得学。❌ hub↔node **零认证**（隧道端口即信任）；无 CSRF token 只靠 SameSite；LAN 明文 | 第一天就做节点侧认证（共享密钥 / mTLS）；多用户若在目标内，认证模型从一开始就要是 principal-based |
| **多主机** | hub/spoke over `ssh -N -L`；worknode 即普通 daemon；hub 无状态、只轮询快照；写至多一次；事件 poll-diff | ✅ 零证书、零总线、节点自治、stale 而非消失——单人 ≤10 台的最省事正解。❌ O(节点数) 全量轮询；事件 A→B→A 丢失；所有字节双跳（3.9 MB/s vs 114 MB/s）；hub 单点 | 继承**纪律**（节点自治、hub 只做快照、不可逆步骤最后做），不继承**实现**（ssh 隧道 + pidfile 收割）；节点数 > 十几台或多用户时改为节点主动外连 + 推送 |
| **Session tabs + 只读 viewer** | 子终端 = 带 `parent_id` 的普通 session；viewer 走独立表；`<iframe sandbox>` 无脚本、不引 HTML 净化器；抓取 shell out 到 curl，每跳自校验 | ✅ 安全设计教科书级（附录 B §4、附录 C §3）。⚠️ 产品价值中等——SPA 页面天然空白，最终催生了 ADR-010/012 | "agent 说写到 X 了 → 看 X"这个 Observe 需求是真的；实现可以更轻（直接读文件 + Markdown 渲染），不必背 URL 抓取的 SSRF 面 |
| **跨主机搬迁** | 目录 + 认定的那一份 transcript + 侧车 + 按 id 恢复；一次性 listener + token；tombstone；undo；hub 中继兜底 | ⚠️ 设计论证极其严密（ADR-011 42 KB），但 5000+ 行为一个功能，强绑定各 agent 私有 transcript 格式（Claude 目录 slug、4 个 JSON 键），agent 升级即漂移 | **不借鉴功能**；借鉴其中的通用纪律：计划即数据、kill 放最后、删除永远单独显式、守卫必须有测试 |
| **远程浏览器** | 节点上 headless Chrome 在 tmux 里跑，CDP screencast 像素流，输入事件翻译，方法白名单由测试钉死，默认关闭 | ✅ 安全模型正确（不透传 CDP、不开 `Runtime.evaluate`）。❌ 本质是 Remote Desktop 的第一步，作者自己承认"界线会被反复试探"；实锤 bug：`offset_top` 恒为 0（附录 B §3） | 不借鉴。这是 devcenter 场景（节点内网 dev server）特有的需求 |
| **三端客户端** | Web + macOS 原生 + HarmonyOS 原生，领域逻辑各写一遍，靠 "Mirrors web/src/..." 注释同步 | ❌ ~2 万行；已出现漂移（WAITING 图标、时长格式、排序默认值、Mac 无 attention 排序）；只有终端的 clipboard / link 相关三个文件真正共享 | 不借鉴。一个 Web 端 + PWA / Tauri 壳；若必须原生，领域模型要么共享 TS 核心要么从后端生成 |

### 3.5 结构性局限

1. **状态检测建立在 TUI 文本上**——这是产品差异化的根基，也是最脆弱的一层（详见附录 A §5）。
2. **"Agent 语义层"缺席**：知道 WAITING 不知道在等什么；知道 RUNNING 不知道在做哪个任务；`recap` 两行是这个方向唯一的探索。
3. **拉模型 + 事件半成品**：observer 2 秒、hub 2 秒、每个客户端各自独立触发 `list()`（Web 3 秒、Harmony 10 秒），每次 `list()` 都起子进程；50 sessions × 多客户端时会显形。
4. **`server.rs` 是 god file**：HTTP 路由、认证、23 处本地/远端分派复制粘贴、一整个搬迁状态机都在里面。
5. **hub↔node 零认证**：hub 本机任何进程都能在所有节点以用户身份执行任意命令。
6. **tmux 硬依赖 + macOS/Linux only**。

---

## 4. 实现方案

### 4.1 后端核心（附录 A）

**做得好的**：
- Kill ≠ Delete 在 API、存储、UI 三层都分离；tmux 是唯一活性真相，daemon 重启零恢复逻辑。
- "先创建外部资源、后写 metadata、失败回滚"；`new-session ; set-option ; set-option` 合并为一次 tmux 调用解决秒退竞态。
- PTY→WS 的 `Utf8Stream` 跨块解码；portable-pty drop 时 VEOF 会杀 agent 的坑被正确绕开（宁可泄漏 fd）。
- 存储的命令 ≠ 运行的命令：可移植命令入库，机器相关的 instrument 每次现算。
- 通过 `claude --settings '<inline JSON>'` 注入 SessionStart hook，让 agent 把自己的 session id 写进 daemon 专属 drop-box 文件——非侵入、无端点、无权限扩散、实测过 merge 语义。**这是整个项目最值得直接复用的技巧。**

**问题**：
- 状态机不存在：任意 tick 直接覆盖状态，无驻留时间，一次误判即广播事件。
- Claude adapter 三条最强规则是全 200 行 tail 的 `contains`，scrollback 里出现关键字就会钉死状态。
- async handler 里同步 spawn `tmux` / `ps` / `du` / `df`，无超时；observer 线程 panic 即静默死亡、无看门狗。
- `anyhow` 全程，靠 `msg.contains("is not running")` 分类错误。
- `capture-pane` 不带 `-J`，折行断成两行；`PaneInfo.attached` 是死字段但注释宣称在用。

### 4.2 多主机 / 搬迁 / 浏览器（附录 B）

**做得好的**：节点自治 + stale 快照；健康判定只归 ticker；隧道监督（退避、健康期重置、pidfile 记 argv、精确前缀收割孤儿）；子进程 stderr 必须排空并留尾（两次事故换来的规则）；传输接收端流式校验 tar 头、拒绝压缩流、事后 `verify_staged`；出站抓取的 canonicalize / 每跳判地址 / `--resolve` 钉 DNS 三件套；用 `direct:` transport 把多机集成测试压进单进程。

**问题**：hub↔node 零认证；元数据只在重连时刷新却拿来做磁盘容量决策；`TcpStream::connect` 无超时；`transfers` map 只增不减；`ws_proxy` 自称协议无关却在失败路径写死终端协议帧；浏览器 `offset_top` 恒为 0；ssh 中继路径零自动化测试。

### 4.3 客户端（附录 C）

**做得好的**：纯逻辑抽成 `sessions/{sort,filter,tree,recap,workspace,state}.ts` 并被三端互钉"金数据"；扁平行列表同时驱动渲染与 ⌥N 索引；指针停在行上时冻结排序；HarmonyOS 的 `TerminalSurface` 接缝（传输与模拟器分离）比 Web 自己的终端连接更健壮；OSC 52 只写不读、路径只在显式点击时打开、viewer 无脚本沙箱——三端安全姿态一致。

**问题**：`App.tsx` 1129 行 + 30 个 `useState` + 35 个 props；轮询叠加每事件全量刷新（Mac 端已经修了，Web 没修）；Web/Mac 终端断线不重连；`App.tsx:262-276` 在 stale host 上可能无限请求循环；三份领域模型手写并已漂移。

### 4.4 测试与工程实践

**测试策略**（按 ROADMAP 执行）：Unit（fixture 驱动检测、ANSI、状态机）→ Integration（真 tmux：create / attach / detach / reattach / resize / kill / discover）→ E2E（Playwright，真 daemon × 2 + 真 tmux + 隔离 socket/DB/HOME）。核心验收 "attach A → 断开 → 进程存活 → attach B 恢复" 从 Phase 1 起一直保留。

**本机实测（2026-09-01，macOS，未安装 tmux）**：

| 步骤 | 结果 |
|---|---|
| `cargo test --no-run`（fresh checkout） | ❌ 失败：`rust_embed` 找不到 `web/dist`。`.gitignore` 写了 `!/web/dist/.gitkeep` 但该文件从未提交（`git ls-files web/dist` 为空） |
| `mkdir web/dist` 后重编译 | ✅ 10.8 秒通过，零 warning |
| 不依赖 tmux 的测试（detection / config / storage / recap / cli / logging / transfer / security / lib 单测） | ✅ **251 通过**（22+16+18+16+6+1+15+14+143） |
| 需要 tmux 的测试（auth / report / candidates / workspace 等） | ❌ 全部因 `failed to execute tmux` 失败——环境原因，非缺陷 |
| `deploy::path_probe_tests` | ❌ 1 个失败，同样依赖本机有 tmux |

**测试体系评价**：数量与真实度都高于同类项目。缺口：检测 fixture 极薄（Claude 4 个、codex 1 个、pi/opencode 0 个）且无反例语料；E2E 里 `fake-agent` 零引用，WAITING/FAILED 驱动的 UX 没有浏览器级验证；`perf.spec.ts` 用墙钟断言必 flaky；Mac 的 live 测试无环境变量时静默通过而非 skip。

**工程实践**：
- **提交信息是叙事句**（"a rename to the name already stored is still a rename"、"sentence-shaped"），每条都说"为什么"而不是"改了什么"。
- **注释即 ADR**：几乎每个非显然决定都带"哪天、哪台机器、实测结果、反例"。
- **beads 作为 AI-native issue tracker**，Claude 与 Codex 的 SessionStart hook 都自动 `bd prime`。
- **无 CI**（`.github` 不存在；ROADMAP 提到 CI 但未落地）。
- **无 LICENSE**。

### 4.5 安全

| 面 | 评价 |
|---|---|
| 登录 | TOTP 严格递增计数器防重放；token 只存哈希；按来源阶梯锁定 + 全局兜底 + 受信网段豁免——**DoS 锁不住主人**，值得学 |
| 浏览器侧 | CSP / `X-Frame-Options: DENY` / HSTS 由 daemon 自发；WS 校验 Origin；viewer 无脚本沙箱 |
| 出站 | fetcher 每跳重新过策略、`--resolve` 钉地址、link-local 无开关禁止；tar 解包流式校验 |
| **hub↔node** | **零认证**——最大软肋，作者已知并延期到 "0600 unix socket" |
| 其他 | 无 CSRF token；LAN 明文；`transfer_offer` 的 `bind` 按请求原样采用；`is_own_address` 用 UDP bind 问内核的做法很巧 |

---

## 5. 综合评分

| 维度 | 分 (1–5) | 一句话 |
|---|---|---|
| 需求定义 | **5** | 北极星 / Non-Goals / DoD / 不变量 / 施工规则 / 验收清单齐全且互相引用 |
| 架构设计 | **4** | ownership 三分法与不变量优秀；ADR 制度优秀；状态检测层与事件模型是短板 |
| 实现质量 | **3** | 注释与局部技巧一流；god file、并发模型混用、错误处理靠字符串、事件半成品 |
| 测试 | **4** | ~850 测试、真 tmux、单进程多 daemon；检测 fixture 薄、E2E 不用 fake-agent |
| 安全 | **3** | 面向公网的 hub 侧扎实；hub↔node 零认证是结构性缺口 |
| 范围控制 | **3** | 边界自觉且每次都写 ADR；但 16 天内三次重画线，最终背上搬迁 / 浏览器 / 三端 |
| 可维护性 | **2** | 三份手写客户端已漂移；server.rs 5000 行；agent 私有格式强耦合 |
| 可移植性 | **2** | tmux + macOS/Linux only；fresh clone 编译失败；无 CI |

---

## 6. 借鉴范围判断

### 6.1 直接继承：理念与纪律（建议全部采用）

这些与技术栈无关，是 devcenter 最有价值、也最容易迁移的部分：

1. **MISSION 的写法**：北极星问题 + 核心动作序列 + Non-Goals + DoD + 架构不变量 + 施工规则 + 量化性能目标 + 验收清单 + V2 延期清单。agora 的 MISSION 应逐节对应。
2. **ADR 制度**：Context / Decision / Consequences；被否决的 ADR 保留全文；每个特性内部再列 Non-Goals 和"什么会让它变危险"；上线后的事故追加为附录。
3. **Coding Agent 施工规则**：agora 也会由 Coding Agent 施工，需要同样的"宪法"——不得替换核心架构、不得为 mock 破坏真实集成、每 Phase 独立可运行、agent 特化必须走 Adapter、破坏性操作必须显式。
4. **生命周期动词分离**：Detach / Close / Kill / Delete Metadata / Restart 各自精确语义；Kill ≠ Delete 在 API 与存储层分离。
5. **"外部系统是活性真相，DB 只存 metadata"**：不论运行时是 tmux 还是别的，DB 永不记录"活着"。
6. **"确认跟着杀走"**：只在真的会破坏时才弹确认；破坏性操作是独立路由不是 query 参数。
7. **"守卫必须有测试钉死"**：安全围栏逐条关掉验证测试变红。
8. **Phase gate + 每 Phase 一个可演示的里程碑**（ROADMAP 的 M1 十一步 demo、M2 十个 fake agent）。
9. **fake-agent + fixture 驱动的测试基础设施**先于真实 agent 集成——继承的是 ROADMAP 的原则；devcenter 自己没做到位（§4.4：fixture 极薄、E2E 里 fake-agent 零引用），agora 要补上。
10. **日志不记 terminal 内容 / prompts / 凭据**；结构化事件名（`session.created` / `agent.status_changed`）（MISSION §10.2）。
11. **叙事式提交信息与"注释即决策记录"**的习惯。

### 6.2 参考重做：技术方案（按 agora 需要取舍）

| 项 | 参考什么 | 重做什么 |
|---|---|---|
| tmux 承载 | Invariant 1–3 的实现路径、`new-session ; set-option` 合并、`remain-on-exit`、`=name` 精确匹配、采纳外部 session | 抽象为 `Runtime` trait；session 级而非全局 tmux 选项；给子进程调用加超时并放到 blocking 线程 |
| Adapter 架构 | trait + 注册表 + `DetectionResult{state, confidence, reason}` + fake adapter | 把"对话身份"与"状态检测"拆成两个 trait；分层来源 hook > process > text；状态机带驻留时间 |
| SessionStart hook drop-box | `--settings` 内联 JSON、只写自己文件、`cat && mv` 原子写 | 扩展到 Notification / Stop / PreToolUse / SubagentStop，作为 WAITING / FINISHED 的一级来源 |
| Terminal gateway | `Utf8Stream`、VEOF 处理、keepalive 20s/65s、takeover 语义 | Web 端实现心跳与重连；考虑二进制帧 |
| WS 协议 | `{input/output/resize/ping}` + `{output/status/exit/detached}` | 事件流做完整或不做 |
| Attention 模型 | 分数表 FAILED 100 > WAITING 90 > … > UNKNOWN 10，按等待时长微调；首页先看需要处理的 | 加入"最近状态变化 / 未读"权重（MISSION 写了未实现） |
| Command Palette / 键盘优先 | fuzzy 三端钉同一组分数；Alt+N 跳行；快捷键用 Alt / ⌘ 组合避开终端 Ctrl 键（附录 C：Web `App.tsx:411-466`、Mac `CommandMenu`） | 直接沿用设计 |
| 联邦纪律 | 节点自治、hub 只做快照、stale 而非消失、写至多一次、健康判定只归 ticker | 节点主动外连 + 共享密钥/mTLS + 事件推送替代 ssh 隧道 + 轮询 |
| 单进程多 daemon 集成测试 | `direct:` transport、`with_home`、隔离 tmux socket、假 ssh 注入 | 直接沿用 |
| 登录限流 | 阶梯锁定 + 全局兜底 + 受信网段豁免 | 直接沿用 |
| 三值 availability | 附录 A 借鉴第 8 条：`agent_availability` 区分"可用 / 不可解析 / 缺失"三值，`--help` 探针决定是否追加 flag | 直接沿用 |
| 客户端事件合并 | 附录 C 借鉴第 4 条（Mac 端 `AppModel.swift:124-188`）：事件就地 patch + 300 ms 合并重同步 + 相等数组不发布 | Web 端补齐（devcenter 的 Web 端每条事件全量 refresh） |

### 6.3 明确不借鉴

- **屏幕文本作为主状态源**（只能做最低优先级兜底）。
- **hub↔node 零认证的 ssh 隧道联邦**。
- **跨主机搬迁**（5000+ 行、强绑定 agent 私有格式）。
- **远程浏览器像素流**（Remote Desktop 的第一步）。
- **三份手写原生客户端**。
- **只读 viewer 的 URL 抓取路径**（SSRF 面换来的价值不高；文件路径部分可以更轻地做）。若 agora 将来有任何出站抓取，附录 B §4 的 canonicalize / 每跳判地址 / `--resolve` 三件套纪律沿用。
- **`server.rs` 单文件 + 手写本地/远端双分支 + JSON 往返调用 handler**。
- **`anyhow` 全程 + 字符串匹配分类错误**；**无版本号的增量迁移**。
- **轮询叠加每事件全量刷新**。

### 6.4 devcenter 留下的空白（agora 的潜在主战场）

devcenter 刻意不做、但"多 Agent 管理"很可能需要的：

1. **Agent 语义层**：任务 / 目标 / 进度 / 产出物 / 对话摘要 / token 与成本——devcenter 的 `recap` 两行和 `agent_session_id` 身份是这个方向唯一的探索，且已证明 transcript 可读（Claude jsonl、codex `session_meta`、pi `session`；见各 adapter 的 `recap` 实现）。
2. **结构化事件源**：Claude Code hooks（SessionStart / Notification / Stop / PreToolUse…）、Codex hooks/notify、Agent SDK——devcenter 只用了 SessionStart。
3. **多 Agent 协作**：编排、消息、DAG、并行审查、agent spawn agent——devcenter 全部 defer，且明确"没有 agent 能创建 session"（ADR-009）。
4. **多用户 / 权限**：principal-based 认证、审计。
5. **非 tmux 运行时**：容器、云沙箱、SDK 内嵌 agent；Windows。
6. **产出物追踪**：diff、PR、文件变更——devcenter defer 了 Git integration 与 Diff visualization。

### 6.5 许可与代码复用注意

devcenter 仓库**没有 LICENSE 文件**，且是他人的私有项目。本报告建议的所有"借鉴"都指**设计、理念、纪律与做法**；若要逐字复用代码片段（如 `Utf8Stream`、`LoginLimiter`、`unpack.rs` 的 tar 校验），需先取得作者许可。

### 6.6 建议的下一步

1. **写 agora 的 MISSION.md**：按 §6.1 第 1 条的结构，首先回答 §2.5 的"管理是哪一层"、目标场景的物理形态（几台机器、几个人、什么设备）、以及 Non-Goals。
2. **写前三篇 ADR**：运行时选择（tmux / 自管 PTY / SDK / 容器）；状态来源分层（hook > process > text）；节点认证模型。这三条是 devcenter 分别做对、做弱、做缺的三件事。
3. **在 Phase 0 就搭好 fake-agent + fixture + 单进程多节点的测试骨架**，并接入 CI。
4. **本机准备**：如果沿用 tmux 路线，先 `brew install tmux`（审查机 2026-09-01 未装）并把 devcenter 的集成测试跑通一遍，亲手验证 Invariant 1–3——这是最便宜的"体验一下这条路线的手感"。
