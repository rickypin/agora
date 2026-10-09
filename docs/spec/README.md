# docs/spec — 实现规格

实现规格导航；文档职责与冲突处理见 [MISSION](../../MISSION.md) 文首，执行纪律见 [AGENTS](../../AGENTS.md)。按本次改动选择相关小节，不要求逐份通读。

| 文件 | 内容 | 对应 MISSION |
|---|---|---|
| [api.md](api.md) | REST 端点、WebSocket 消息、hook 投递、id 与版本字段、health | §7.3、§7.4、§10.3 |
| [status.md](status.md) | 会话状态真值表：origin × status × process × source 的合法格与不可能格，逐行的守卫 id | §4.2、§4.3 |
| [config.md](config.md) | 节点配置 YAML、SQLite schema、token 文件 | §9 |
| [design-system.md](design-system.md) | 跨设备设计原则、token、组件契约与 agent 验证规则 | §1、§6 |
| [ux.md](ux.md) | 主界面 / Attention Dashboard / New Agent 对话框 / 确认框线框、快捷键表、视觉参考 | §5.5、§6、§8 |
| [architecture.md](architecture.md) | 总体架构图、当前实例的 peer 形态 | §3 |
| [instance.md](instance.md) | 当前实例实测清单（硬件、网络、已装 agent）——各 ADR 的输入 | §0.2 |
