# UX 线框、键位与视觉参考

产品决定在 MISSION §6（四个 screen、attention 表、创建对话框字段）；本文是线框与键位表，随实现改。

## 主界面线框

```
┌─────────────────────────────────────────────────────────────┐
│ agora                          mac ●   zuan ●        │
├─────────────────┬───────────────────────────────────────────┤
│ AGENTS          │ agora / claude @ mac                      │
│                 │                                           │
│ ● agora   mac   │ ┌───────────────────────────────────────┐ │
│   Claude        │ │                                       │ │
│   working 02:31 │ │                                       │ │
│                 │ │             TERMINAL                  │ │
│ ⚠ sglog   zuan  │ │                                       │ │
│   Codex         │ │                                       │ │
│   NEED INPUT    │ │                                       │ │
│                 │ └───────────────────────────────────────┘ │
│ ✓ tests   zuan  │                                           │
│   Grok          │                                           │
│   finished      │                                           │
│                 │                                           │
│ + New Agent     │                                           │
└─────────────────┴───────────────────────────────────────────┘
```

## 快捷键

| 快捷键 | 动作 |
|---|---|
| Cmd/Ctrl + K | Command Palette |
| Cmd/Ctrl + F | Filter sidebar（name / node / agent / preview；Enter 打开第一条） |
| Alt/Option + 1…9 | 跳到侧栏第 N 条（按当前过滤后的显示顺序） |
| Cmd/Ctrl + Shift + ] / [ | Next / Previous Agent |
| Cmd/Ctrl + N | New Agent |

浏览器全局快捷键必须避免吞掉终端内 Ctrl+C / Ctrl+D / Ctrl+Z / Ctrl+R / Ctrl+A / Ctrl+E 等常见操作；Cmd/Ctrl+数字被浏览器保留，所以数字跳转用 Alt/Option。

Command Palette 支持 fuzzy search sessions / projects / nodes / actions（`New Claude in agora @ zuan`）。目标是让管理 20–50 个 agent 时仍然高效。手机端没有键盘，快捷键与命令面板只在桌面生效。

## 视觉参考

关键词：**fast、dense、keyboard-first、low visual noise、dark-mode-first**。风格参考 Linear + Warp + 现代终端管理器 + tmux。可用 shadcn/ui，但不应演化为传统 enterprise dashboard。

## Attention Dashboard 线框（MISSION §6.3）

```
mac ● zuan ●          Agents: 12
Running 5   Needs Input 2   Turn Done 1   Finished 3   Idle 1

NEEDS ATTENTION
⚠ agora-90t.4 ADR-003     / Claude @ zuan    waiting 3m
⚠ 修 migration 回滚        / Codex @ mac     waiting 1m
◆ agora-90t.1 写 MISSION   / Claude @ mac    turn done 2m
✓ agora-3la 测试骨架       / Claude @ mac    finished 9m
RUNNING
● 重构 sglog parser        / Codex @ zuan
```

行展开的两行（`❯` 用户最后输入 / `↳` agent 正在做或最后说的，MISSION §6.3）：

```
⚠ frontend / Claude @ zuan    waiting 3m
  ❯ 把 sidebar 的 workspace chip 换成可折叠的
  ↳ 改完了，144 个 e2e 全绿，要不要 push？
```

## New Agent 对话框线框（MISSION §6.4）

```
New Agent
Node:     [ zuan ▼ ]              # 本机 + 在线的 peer；选 peer 则经一跳转发执行
Project:  [ ~/code/agora ]
Worktree: [ main ▼ / 新建… ]      # 新建 = git worktree add，base 默认主 worktree 当前分支；合并/销毁归人（MISSION §6.4）
Agent:    [ Claude Code ▼ ]
Task:     [ agora-90t.1 ▼ / 一句话 ]  # 有 bd 的仓库从 issue 列表选，否则一句话；留空取首条 prompt
Name:     [ agora-mission ]
Command:  [ claude ]
[ Create ]
```

## 未注册会话与危险操作确认（MISSION §5.5 / §8）

未注册 / 未识别的会话在列表中显示为：

```
? emergency
  Unknown Agent
```

Kill 确认框（MISSION §8：确认跟着"杀"走）：

```
Kill sglog / Codex @ zuan?
The running process and its session will be terminated.
[Cancel] [Kill]
```
