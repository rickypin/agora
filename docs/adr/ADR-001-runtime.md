# ADR-001: 持久化运行时选择

- 状态：Proposed（待写）
- 日期：—
- beads：`agora-90t.2`
- 依赖：MISSION §1（哪一层）、§2（物理形态）、§5（是否继承不变量 1–3）

## Context（预填）

谁拥有 agent 进程的生命周期，决定了"浏览器关了 / daemon 重启了，agent 还在不在"。devcenter 把生命周期交给 tmux，用最少代码换来不变量 1–3、scrollback、多客户端与外部 session 采纳；代价是硬依赖 tmux、macOS/Linux only、tmux 泄漏进 session 模型。

## 决策问题

agora 的 agent 进程由谁托管，托管层如何被抽象。

## 备选项（预填，定稿时补优缺点）

| 备选 | 一句话 | devcenter 的经验 |
|---|---|---|
| tmux 承载 | daemon 永不直接 spawn agent | ✅ 做对的一件事；报告 §3.4 第 1 行、§6.2 第 1 行 |
| 自管 PTY + supervisor | daemon 内 portable-pty + 独立 supervisor 进程 | 未验证；需要自己实现 detach / 多客户端 / 崩溃隔离 |
| Agent SDK 内嵌 | 不跑 TUI，直接以 SDK 驱动 agent | 报告 §6.4 第 5 条留白；状态源天然结构化，但失去"采纳外部 session" |
| 容器 / 云沙箱 | 每个 agent 一个容器 | 报告 §6.4 第 5 条留白；Windows / 远程友好，成本与延迟高 |

## 预置约束

- 无论选哪个，抽象为 `Runtime` 接口，tmux 不得泄漏到 session 模型之外（报告 §3.4）。
- 子进程调用必须有超时并放到 blocking 线程（devcenter `tmux.rs:88` 无超时拖住 observer）。
- 本机（2026-09-01）未安装 tmux；若沿 tmux 路线，先 `brew install tmux` 并跑通 devcenter 集成测试亲手验证不变量 1–3（报告 §6.6 第 4 条）。

## 参考

- `docs/analysis/devcenter/README.md` §3.2、§3.4、§3.5 第 6 条、§6.2、§6.4
- `docs/analysis/devcenter/appendix-a-backend.md`
