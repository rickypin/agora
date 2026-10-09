# ADR 索引与约定

约定（早期参考 devcenter 分析；本目录记录 agora 自己的决定）：

- 编号 `ADR-NNN`，文件名 `ADR-NNN-<slug>.md`，一经分配不复用。
- 状态：Proposed → Accepted / Rejected / Superseded by ADR-MMM。**被否决的 ADR 保留全文**，它记录的是"为什么不"。
- 每篇必含：Context、决策问题、备选项、Decision、Non-Goals、"什么会让它变危险"（守卫 + 钉死守卫的测试）、Consequences。
- 上线后的事故追加为该 ADR 的附录，不另起文件。
- 决策的**工作项**在 beads 里（`-t decision`，`--external-ref` 指向本目录文件）；决策的**记录**只在这里。
- 每篇 ADR 先从 MISSION 的目标、边界与不变量推导决策，再检查既有 Accepted ADR 和当前实现证据。`docs/analysis/devcenter/README.md` 的借鉴范围与层次分析是历史参考，不是决策前置条件；不因为别的项目这样做就照搬。
- 正文里的修订日期写**决策日**（拍板 / 改口径那一天）；它与落地提交日不同时不写进标题（`git log` 查得到），确有需要就在括号里补一句「落地 YYYY-MM-DD」（agora-ruh9，2026-10-07）。

| 编号 | 标题 | 状态 | beads |
|---|---|---|---|
| [ADR-001](ADR-001-runtime.md) | 持久化运行时选择 | Accepted（2026-09-02） | `agora-90t.2` |
| [ADR-002](ADR-002-state-source-layering.md) | Agent 状态来源分层 | Accepted（2026-09-02） | `agora-90t.3` |
| [ADR-003](ADR-003-node-authentication.md) | 节点认证模型 | Accepted（2026-09-02） | `agora-90t.4` |
| [ADR-004](ADR-004-node-topology.md) | 节点拓扑：互为 peer、一跳转发 | Accepted | —（实现验收在 M2a `agora-7ku` 与 M2b `agora-s4r`） |

模板：[TEMPLATE.md](TEMPLATE.md)
