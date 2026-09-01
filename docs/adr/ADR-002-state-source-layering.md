# ADR-002: Agent 状态来源分层

- 状态：Proposed（待写）
- 日期：—
- beads：`agora-90t.3`
- 依赖：MISSION §1（L2 是否在范围内决定"状态"要多细）、ADR-001（运行时决定能拿到什么信号）

## Context（预填）

devcenter 用 `tmux capture-pane` 文本 + 正则判断 STARTING / RUNNING / WAITING / IDLE / FINISHED / FAILED，这是它产品差异化的根基，也是最脆弱的一层：全 tail `contains`、无防抖、无真正状态机、只对 Claude 可用、fixture 总量 4.3 KB（报告 §3.5 第 1 条、附录 A §5）。它自己的 SessionStart hook drop-box 已证明结构化事件源可行，但只用了这一个 hook。

## 决策问题

状态（以及后续 L2 的任务 / 进度 / 产出）从哪些来源获取，优先级如何，冲突时谁赢。

## 备选项（预填）

| 来源 | 信号 | 可靠性 | 覆盖 |
|---|---|---|---|
| Agent hooks / 事件 | Claude Code：SessionStart / Notification / Stop / PreToolUse / SubagentStop…；Codex：SessionStart / PreCompact / PostCompact / UserPromptSubmit… | 高（agent 自己说的） | 需每个 agent 适配；hook 缺席时无信号 |
| 进程状态 | 存活、子进程树、CPU / IO、退出码 | 高但语义粗 | 通用 |
| transcript / 会话文件 | Claude jsonl、codex `session_meta`… | 中（私有格式随版本漂移，报告 §3.4 搬迁行） | L2 的主要原料 |
| 屏幕文本 | capture-pane + 正则 | 低 | 通用兜底 |

## 预置约束

- 优先级反转为 **hook / 事件 > 进程状态 > 屏幕文本**；屏幕文本只做兜底且带驻留时间（报告 §3.4 第 2 行、§6.2 Adapter 行）。
- "对话身份"与"状态检测"拆成两个 trait；`DetectionResult{state, confidence, reason}` 的形态可沿用。
- 状态机带驻留时间 / 防抖；fixture 驱动测试先于真实 agent 集成。

## 参考

- `docs/analysis/devcenter/README.md` §3.4、§3.5 第 1–3 条、§6.2、§6.4 第 1–2 条
- `docs/analysis/devcenter/appendix-a-backend.md` §2、§5
