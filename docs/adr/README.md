# ADR 索引与约定

约定（继承自 devcenter 报告 §3.3 / §6.1 第 2 条）：

- 编号 `ADR-NNN`，文件名 `ADR-NNN-<slug>.md`，一经分配不复用。
- 状态：Proposed → Accepted / Rejected / Superseded by ADR-MMM。**被否决的 ADR 保留全文**，它记录的是"为什么不"。
- 每篇必含：Context、决策问题、备选项、Decision、Non-Goals、"什么会让它变危险"（守卫 + 钉死守卫的测试）、Consequences。
- 上线后的事故追加为该 ADR 的附录，不另起文件。
- 决策的**工作项**在 beads 里（`-t decision`，`--external-ref` 指向本目录文件）；决策的**记录**只在这里。
- 每篇 ADR 先对照 `docs/analysis/devcenter/README.md` §6 的借鉴范围与 §2.5 的层次判断。

| 编号 | 标题 | 状态 | beads |
|---|---|---|---|
| [ADR-001](ADR-001-runtime.md) | 持久化运行时选择 | Proposed（待写） | `agora-90t.2` |
| [ADR-002](ADR-002-state-source-layering.md) | Agent 状态来源分层 | Proposed（待写） | `agora-90t.3` |
| [ADR-003](ADR-003-node-authentication.md) | 节点认证模型 | Proposed（待写） | `agora-90t.4` |

模板：[TEMPLATE.md](TEMPLATE.md)
