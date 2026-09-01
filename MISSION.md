# agora — MISSION

> 状态：**骨架，待定稿**。本文件是 agora 的北极星与"宪法"：每个会话开工前先读它。
> 结构对应 `docs/analysis/devcenter/README.md` §6.1 第 1 条（北极星 / 核心动作序列 / Non-Goals / DoD / 不变量 / 施工规则 / 性能目标 / 验收清单 / V2 延期）。
> 标有 `TODO` 的段落是下一步要写的内容；标有"候选"的条目来自 devcenter 分析，定稿时逐条取舍。

## 0. 一句话

TODO：agora 是给 ___（谁）用的 ___（什么），让他们能 ___（核心动作）。

## 1. "管理"是哪一层

必须先回答（devcenter 报告 §2.5）：agora 站在哪一层、要不要向上走、向上走到哪为止。

| 层次 | 管理什么 | agora 的态度（TODO：覆盖 / 后续 / 不做） |
|---|---|---|
| L1 终端 / 进程级 | session 活着吗、在跑还是在等人、切过去回答它 | TODO |
| L2 任务 / 语义级 | 每个 agent 在做什么任务、目标、进度、产出、成本、对话历史 | TODO |
| L3 协作 / 编排级 | 多 agent 分工、依赖、消息、DAG、并行审查 | TODO |

TODO：写明理由，以及"L1 是不是 L2/L3 的前提"（devcenter 证明 L1 一天可出 MVP，但天花板也在 L1）。

## 2. 目标场景的物理形态

TODO，逐项写死，后面的 ADR 都依赖这些数字：

- 机器：几台、什么系统（macOS / Linux / Windows？）、在哪（本机 / 局域网 / 云）
- 人：单人还是多人；多人时是否需要权限隔离
- 设备：从哪里看（桌面浏览器 / 手机 / 终端）
- agent：哪些（Claude Code / Codex / 其他）、同时多少个、运行在哪（tmux / 容器 / SDK）
- 一天里典型的"上手 → 干活 → 离开 → 回来"是什么样

## 3. 北极星问题与核心动作序列

TODO：用户最常做的一件事，从打开到完成的 5–10 步。

## 4. Non-Goals

TODO。候选：devcenter 报告 §6.3 列出的不借鉴项（跨主机搬迁、远程浏览器像素流、三端原生客户端、屏幕文本做主状态源、hub↔node 零认证隧道）；beads 评估 §6.5 第 4 条（server 模式 / formulas / gates / 外部 tracker 同步）。

## 5. 架构不变量

TODO。候选（devcenter §3.2，逐条决定是否继承）：

1. Browser / WS / daemon 崩溃 ≠ Agent 崩溃
2. Close Tab ≠ Kill；Kill ≠ Delete（生命周期动词分离：Detach / Close / Kill / Delete / Restart）
3. 一个 agent 坏不影响另一个
4. UI 状态可丢弃
5. 外部系统是活性真相，DB 只存 metadata（DB 永不记录"活着"）
6. 状态来源分层：hook / 事件 > 进程状态 > 屏幕文本（→ ADR-002）
7. 节点之间没有"隧道即信任"（→ ADR-003）

## 6. Coding-Agent 施工规则

候选（devcenter §6.1 第 3 条），定稿时确认：

- 不得替换核心架构；不得为 mock 破坏真实集成；每个 Phase 独立可运行
- agent 特化必须走 Adapter；破坏性操作必须显式（独立路由，不是 query 参数）
- 守卫必须有测试钉死：安全围栏逐条关掉验证测试变红
- 日志不记 terminal 内容 / prompts / 凭据

**任务跟踪纪律（已决定，见 `docs/analysis/beads/README.md` §6.5）：**

1. 开工必 `bd update <id> --claim`；不领取不动手
2. commit subject 引用 issue id，格式 `... (agora-xxxx)`
3. 干活中发现的问题：`bd create ... --deps discovered-from:<当前 id>`，不塞进当前 issue
4. agent 不自动关闭需要人验证的任务；会话结束必 `bd dolt push`（本仓库已在 AGENTS.md 明确授权；git commit / push 仍需当次授权）
5. 规划分层：MISSION / ADR 是 markdown；路线图与任务在 beads；`ROADMAP.md` 由 `scripts/roadmap-view.sh` 生成，不手改、不放 checkbox
6. agent 指令只维护 `AGENTS.md`（`CLAUDE.md` 是符号链接）；`bd remember` 只存指令文件里没有的、干活中学到的事实

## 7. Definition of Done

TODO：一个特性"做完"的定义（代码 + 测试 + 文档 + 演示？）。

## 8. 量化性能目标

TODO（devcenter 参考值：idle 9 MB、单 binary、50 sessions × 多客户端不显形）。

## 9. MVP 验收清单

TODO：可逐条打勾的 10–20 条；对应 beads 里 M-epic 的 `--acceptance`。

## 10. V2 延期清单

TODO。
