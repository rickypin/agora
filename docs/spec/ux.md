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

实现落在 `web/src/keys.ts` 一层（终端侧接在 TerminalView 的 `attachCustomKeyEventHandler`，全局侧接在 Workspace 的 window keydown）：那六个 Ctrl 组合被写成名单，全局层一律不认，哪怕将来给它们绑了动作。Alt/Option+数字认 `event.code`（`Digit3`）而不是 `event.key`——macOS 上 Option+3 的 key 是 `£`。

### 终端要回来的键（agora-xqa.3）

| 按键 | agora 发给 pane | 为什么不能交给 xterm.js |
|---|---|---|
| Shift + Enter | `ESC CR`（= Option/Alt+Enter） | xterm.js 默认发裸 CR，TUI 分不出"换行"与"发送"，一按就提交。不用 kitty/CSI-u 的 `ESC[13;2u`：TUI 没打开 kitty keyboard protocol 时会把它当乱码打进输入框；`ESC CR` 是 Claude Code / Codex 今天就认的换行，也是各家 terminal-setup 给 iTerm2 装的那条约定 |
| Cmd + ← / → | `ESC[H` / `ESC[F`（Home / End） | 浏览器把 Cmd+方向键留给了历史前进后退，不 `preventDefault` 会真的退出页面、顺手丢掉终端视图；xterm.js 又不转发它们 |

Option+← / → 按词跳、粘贴、resize 重排、滚轮回看都走 xterm.js 原路，这一层不碰。这些字节进 pane 前还要过一道 `tmux attach` 客户端：它认得的键会按 pane 的终端类型**重新编码**（实测 tmux 3.7c 把 `ESC[H` / `ESC[F` 改发成 `ESC[1~` / `ESC[4~`），键义不变——守卫 `tests/terminal_keys.rs` 因此两种编码都接受。

Command Palette 支持 fuzzy search sessions / projects / nodes / actions（`New Claude in agora @ zuan`）。目标是让管理 20–50 个 agent 时仍然高效。手机端没有键盘，快捷键与命令面板只在桌面生效（`isDesktop()`：视口窄于 700 px 就不装 window keydown，命令面板也开不出来）。

面板里选 `New <agent> in <project> @ <node>` 直接起会话，走的是和 New Agent 对话框同一个 `POST /api/sessions`，字段取默认值（项目名当 display_name、agent 的默认命令）——面板的意义就是不填表；要填 Task / Worktree 的走对话框（面板末尾那条 `New Agent…`）。侧栏过滤与面板共用 `web/src/fuzzy.ts` 的打分（连续命中、词首命中加分，同分保持原顺序），所以 Alt/Option+N 跳的第 N 条永远等于眼睛看到的第 N 条。

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

V1 的取舍（agora-xqa.12）：Node 只有本机（peer 归 M2），下拉禁用；Project 是可输入的下拉
（`<input list>`）——列表来自扫描并按最近使用排序，但 `project_roots` 默认为空，只给下拉的话
新装的 agora 一个会话都起不了；Worktree 只列现有的（新建归 M3 A44），选主 worktree 等于选仓库
本身；Task 只有一句话（从 bd 就绪任务选归 M3 A43）；Agent 的名字与默认命令来自 `GET /api/agents`，
末尾多一项 `custom`——它没有 Adapter，Command 必填。Name 与 Command 有默认值，用户手改过之后
换项目 / 换 agent 不再覆盖。

## 未注册会话与危险操作确认（MISSION §5.5 / §8）

未注册 / 未识别的会话在列表中显示为：

```
? emergency
  Unknown Agent
```

Kill 确认框（MISSION §8：确认跟着"杀"走；Kill 只杀进程，运行时会话与输出保留到清理，§4.6）：

```
Kill sglog / Codex @ zuan?
The running agent process will be killed. Its output stays until you clean it up.
[Cancel] [Kill]
```
