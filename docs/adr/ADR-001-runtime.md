# ADR-001: 持久化运行时选择

- 状态：Proposed（待写）
- 日期：—
- beads：`agora-90t.2`
- 依赖：MISSION §1.3（哪一层）、§0.2（物理形态）、§2.2（是否继承不变量 1–3）

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

## MISSION 迁入的 devcenter 默认值（2026-09-02）

MISSION v0.6 起只保留原则，下面是原文里的 tmux 路线默认做法与数值，定稿时逐条采纳、改写或否决。

### 运行时会话（原 §4.1）

每一个 agent session 对应一个 persistent 运行时会话。tmux 路线下【ADR-001】：

```bash
# 三步必须合并为一次 tmux 调用（\;）：消除 new-session 与 set-option 之间的竞态窗口——agent 秒退时 remain-on-exit 尚未生效会丢退出码
tmux new-session -d -s ag-a81c28 "cd ~/code/agora && claude" \; \
     set-option -t =ag-a81c28: remain-on-exit on \; \
     set-option -t =ag-a81c28: status off
# remain-on-exit：保留 dead pane 供 observer 读退出码；status off：UI 已显示名字与状态，状态栏是多余副本
```

浏览器断开不影响运行时中的 agent。`status off` 是 **session 作用域**：只作用于 agora 创建的 session，被 adopt 的 session 保持用户原有配置。

### Observer 轮询（原 §3.4）

**不要通过 WebSocket client output 来判断 agent 状态**，否则没有浏览器连着时无法检测 agent。

```
   agent hooks ─────────────┐
                            │
              运行时         │
                │           │
        ┌───────┴───────┐   │
        │               │   │
    Browser PTY    State Observer
```

State Observer 有两个输入：① **agent 的 hook 事件**——Claude Code / Codex 的 hook 命令把事件投递给本机 daemon（drop-box 文件或 `POST 127.0.0.1:7680/api/hooks/<agent>`，形式由 ADR-002 定）；② **运行时轮询**——tmux 路线下周期性 `tmux capture-pane -p -t <session> -S -200` + 进程树，用于没有 hook 的 agent（Grok）、退出码与兜底。两者都不依赖浏览器连接。轮询的优势仍然成立：非侵入、不改 agent 命令、实现简单；性能不足时再升级为 `pipe-pane`。

### Scrollback 与 tail buffer（原 §5.7）

MVP 依赖运行时 scrollback（tmux：`history-limit 100000`），不自行构建 terminal transcript database。

State Detector 可以维护一个有限 tail buffer（如 last 64 KB），仅用于 state detection / recent activity / UI preview。**不得将无限 terminal output 全部保存在内存。**

### 自适应轮询（原 §10.1）

状态扫描不得产生明显 CPU 消耗；50 sessions 每 2 秒 capture-pane 开销过高时用 adaptive polling：WAITING / IDLE 5 s、RUNNING 2 s、FINISHED 30 s；配置默认 `detector_interval: 2s`、`idle_after: 60s`、`terminal.scrollback: 10000`。

### 后端依赖（原 §7.1）

要求：PTY support 成熟、WebSocket 简单、process management 简单、并发模型适合大量 terminal stream、单 binary、部署简洁、内存占用小、**能在 macOS / Linux / Windows 三个平台出单 binary**（§0.1）。

devcenter 选 Rust（Go 亦满足）：

```
tokio           # async runtime
axum            # HTTP + WebSocket
portable-pty    # PTY
rusqlite        # SQLite（bundled）
serde           # 序列化
```

依赖保持最少；不得为普通 CRUD 引入大型 backend framework 或重型 ORM。agora 的语言与运行时在 ADR-001 一并决定，本节先沿用 devcenter 的默认。

### 前端栈（原 §7.2）

```
React + TypeScript + Vite
xterm.js + xterm-addon-fit
shadcn/ui（可选）
PWA manifest + service worker（手机安装与推送）——**手机阶段必选，不是增强**：iOS 上没有它就没有推送（§6.6、§8）；V1 桌面上可选
```

### 仓库结构（原 §7.5）

```
agora/
├── src/
│   ├── api/          # REST + WebSocket handlers
│   ├── config/
│   ├── session/      # Session Manager
│   ├── terminal/     # Terminal Gateway (PTY ↔ WS)
│   ├── runtime/      # 运行时封装（tmux 或 ADR-001 所选）
│   ├── process/      # process tree inspection
│   ├── agent/        # adapter + claude / codex / grok / generic
│   ├── status/       # State Detector / state machine
│   ├── events/       # global event bus
│   └── storage/      # SQLite
├── web/              # React + PWA
├── testdata/         # claude/ codex/ grok/ fixtures
├── scripts/          # fake-agent, roadmap-view.sh
├── docs/adr/
├── MISSION.md  AGENTS.md  README.md  ROADMAP.md
```

### 本 ADR 还要回答的新输入（MISSION v0.8）

- 已退出会话的清理策略（§4.6）：FINISHED / FAILED 的 dead pane 何时、由谁清。
- 一跳转发终端流对运行时的要求（§3.5）。
- Windows 节点 = 第二个运行时实现的留位（§0.1、§11）。
- 升级路径：一条命令升级、升级期间 agent 不死（A39）。
- 见 beads `agora-90t.2` 注记的输入清单。
