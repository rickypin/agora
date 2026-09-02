# ADR-002: Agent 状态来源分层

- 状态：Proposed（待写）
- 日期：—
- beads：`agora-90t.3`
- 依赖：MISSION §1.3（L2 是否在范围内决定"状态"要多细）、ADR-001（运行时决定能拿到什么信号）

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

## MISSION 迁入的默认值（2026-09-02）

### Adapter 接口（原 §5.2）

agent-specific logic 不允许写死在核心层：

```
AgentAdapter {
    Name()
    HookSpec() []HookInstall                     # 要装哪些 hook、装到哪；无 hook 的 agent 返回空
    ParseEvent(raw HookEvent) DetectionResult    # hook 事件 → 状态
    Detect(process ProcessInfo, tail []byte) DetectionResult   # 兜底：进程 + 文本
    MatchProcess(processTree []ProcessInfo) bool
    DefaultCommand() string
}
```

实现：`ClaudeAdapter`、`CodexAdapter`、`GrokAdapter`、`GenericShellAdapter`；`PiAdapter` 为备选一等；未来允许 `CursorAdapter`、`CustomAgentAdapter`。核心 Session Manager 不应知道任何 agent 的具体输出格式。

### 本 ADR 必须回答的（MISSION v0.8 §3.3 / §3.4 / §5.1 / §5.6 / §7.3）

- hook 投递形式二选一：`POST 127.0.0.1/api/hooks/:agent`（每会话一次性 token，不适用 cookie）或投递箱文件；**daemon 不在时事件不得丢失**，重启后重放。
- hook 安装位置：装进用户自己的 agent 配置（幂等、可卸载、装前显示 diff），agora 起的会话不再重复注入。
- 结构化 respond：PreToolUse 同步返回 allow / deny，agora 的 hook 挂起等 Dashboard 答复；挂起上限与超时行为实测。
- 对话身份识别顺序：agent 自报（SessionStart，`/resume` `/clear` 再触发）> 启动时钉死 id > 用户从候选挑；绝不按 mtime 猜；每次命中覆盖 `agent_session_id`。
- Codex 覆盖度实测（hook 定义不可注入；`notify` agent-turn-complete）；Grok 无 hook 走文本兜底。
- 见 beads `agora-90t.3` 注记的输入清单。

### MISSION v0.8 迁入的论证与兜底细节（2026-09-02）

- **hook-first 是 V1 架构而非 V2 愿望的理由**：devcenter 把结构化事件放 V2 的前提（"MVP 不依赖非稳定 private API"）已失效——Claude Code 的 SessionStart / UserPromptSubmit / PreToolUse / Notification / Stop / SessionEnd 与 Codex 的 hooks / notify 都是文档化的稳定接口；本仓库自己就在用 SessionStart hook 注入 `bd prime`。三个一等 agent 里两个有 hook，只有 Grok 需要文本兜底。
- **文本兜底的模式清单**（沿用 devcenter v1 的启发式）：检测到 `"Would you like..."` / `"Approve..."` / `"Allow..."` / `"Continue?"` / `"Do you want to proceed?"` 等 → WAITING。
- **轮询兜底的优势**（保留它的理由）：非侵入、不改 agent 命令、实现简单。
