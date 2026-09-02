# docs/spec — 实现规格

MISSION 只写决定；这里放决定的**形状**：端点、消息、配置文件、schema、线框、键位。

- 内容源自 devcenter 的默认做法加 agora 的改动；ADR-001 / 002 / 003 定稿后已逐份对齐（2026-09-02；2026-09-03 全文档交叉验证再对齐一次），之后随代码改。
- 随代码改：与代码冲突时以代码为准并回写这里；不得回写 MISSION。
- MISSION 里的原则优先于这里的任何细节；冲突时改这里。

| 文件 | 内容 | 对应 MISSION |
|---|---|---|
| [api.md](api.md) | REST 端点、WebSocket 消息、hook 投递、id 与版本字段、health | §7.3、§7.4、§10.3 |
| [config.md](config.md) | 节点配置 YAML、SQLite schema、token 文件 | §9 |
| [ux.md](ux.md) | 主界面 / Attention Dashboard / New Agent 对话框 / 确认框线框、快捷键表、视觉参考 | §5.5、§6、§8 |
| [architecture.md](architecture.md) | 总体架构图、当前实例的 peer 形态 | §3 |
| [instance.md](instance.md) | 当前实例实测清单（硬件、网络、已装 agent）——各 ADR 的输入 | §0.2 |
