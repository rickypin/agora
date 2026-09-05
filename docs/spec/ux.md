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
| Alt/Option + ] / [ | Next / Previous Agent |
| Alt/Option + N | New Agent |

浏览器全局快捷键必须避免吞掉终端内 Ctrl+C / Ctrl+D / Ctrl+Z / Ctrl+R / Ctrl+A / Ctrl+E 等常见操作。**能留给 agora 的只有浏览器自己没占的组合**：Cmd/Ctrl+数字（标签页）、Cmd/Ctrl+Shift+] / [（下/上一个标签页）、Cmd/Ctrl+N（新窗口）都在浏览器 UI 层被吃掉，页面连 keydown 都收不到，`preventDefault` 也救不回来（macOS Chrome 人眼实测 2026-09-04，agora-rzn：这三类原先都写在本表里，按下去响应的是 Chrome）。所以除了面板与过滤这两个 Cmd/Ctrl 组合，其余一律走 Alt/Option。**验证纪律**：jsdom 没有保留键这回事，agent-browser 经 CDP 把按键直接注入渲染进程、绕过浏览器的加速键处理——两者对 Cmd/Ctrl 系键位都只会给出假阳性，只有人在真浏览器里按过才算数。

实现落在 `web/src/keys.ts` 一层（终端侧接在 TerminalView 的 `attachCustomKeyEventHandler`，全局侧接在 Workspace 的 window keydown）：那六个 Ctrl 组合被写成名单，全局层一律不认，哪怕将来给它们绑了动作。Alt/Option 系一律认 `event.code`（`Digit3` / `BracketRight` / `KeyN`）而不是 `event.key`——macOS 上 Option+3 的 key 是 `£`、Option+] 是 `‘`、Option+N 是死键 `Dead`。

### 终端要回来的键（agora-xqa.3）

| 按键 | agora 发给 pane | 为什么不能交给 xterm.js |
|---|---|---|
| Shift + Enter | `ESC CR`（= Option/Alt+Enter） | xterm.js 默认发裸 CR，TUI 分不出"换行"与"发送"，一按就提交。不用 kitty/CSI-u 的 `ESC[13;2u`：TUI 没打开 kitty keyboard protocol 时会把它当乱码打进输入框；`ESC CR` 是 Claude Code / Codex 今天就认的换行，也是各家 terminal-setup 给 iTerm2 装的那条约定 |
| Cmd + ← / → | `ESC[H` / `ESC[F`（Home / End） | 浏览器把 Cmd+方向键留给了历史前进后退，不 `preventDefault` 会真的退出页面、顺手丢掉终端视图；xterm.js 又不转发它们 |

Option+← / → 按词跳、粘贴、resize 重排、滚轮回看都走 xterm.js 原路，这一层不碰。这些字节进 pane 前还要过一道 `tmux attach` 客户端：它认得的键会按 pane 的终端类型**重新编码**（实测 tmux 3.7c 把 `ESC[H` / `ESC[F` 改发成 `ESC[1~` / `ESC[4~`），键义不变——守卫 `tests/terminal_keys.rs` 因此两种编码都接受。

### 终端焦点（agora-p29）

打开一个会话就该能直接打字，不用先点终端。TerminalView 在三处交焦点：① 挂载时 `term.focus()` 一次；② WS 收到 `status: attached` 再一次——新开的浏览器标签页里挂载那一次偶尔不生效（2026-09-03 目检：attach 成功、键入不进 pane、点一下终端才好），但用户这会儿已经在别的文本输入（侧栏过滤、Rename、New Agent 表单）里打字的话不抢；③ `.term-host` 上的 pointerdown 把焦点交回 xterm 的 helper textarea，点在 `.xterm` 之外的 padding 上也算——xterm 自己的 mousedown 只覆盖 `.xterm` 内部，padding 那一圈由 agora 取消浏览器缺省的 mousedown，不然焦点会被挪到 body。不在 pointerdown 上 `preventDefault`：那会连带取消后面的 mousedown / click，xterm 的选区靠它们。守卫 `web/src/TerminalView.test.tsx`。人工复现步骤在 M1a 演示剧本第 4 步（新标签页打开会话 → 直接键入）。

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

侧栏选中行下方是就地 respond 区（MISSION §6.3 §7.3，`web/src/Respond.tsx`）：WAITING 且 `reason = permission`、`respond_via = hook` → 问题文本（`detail`）+ Allow / Deny / 打开终端，`respond_within_secs` 短于 5 分钟（Codex 20 s）时再加一行"N 秒内没答会交回终端"；WAITING 的其它情形（`question`，或 `respond_via = terminal`）→ 只有问题文本与"打开终端"；TURN_DONE → `↳` 最后一条回复 + "下一条指令"输入框（发 text，尾部带换行）。Allow / Deny 撞上 `no_pending_decision`（终端先答了 / 过期）只显示一行提示，行状态随事件自己变。

实现（`web/src/attention.ts`，agora-dvh.10）：侧栏就是 Dashboard——行按分数降序 → bd 优先级升序（`task.priority`，无 bd 视为 P2）→ `status_since` 早的在前排好，再把 NEEDS ATTENTION（分数 ≥ FINISHED）整体提到 RUNNING 前面；过滤只删不换序，所以 Alt/Option+N 跳的第 N 条永远等于眼睛看到的第 N 条。第一列 `taskLabel`：`task.id + title` > `task_ref`（issue id 或首条 prompt 摘要）> 名字。header 下一行是各状态计数。

每行下面的两行（`❯` 用户最后输入 / `↳` agent 正在做或最后说的，MISSION §6.3；都来自 hook 的 `prompt` / `progress`，没有 hook 的会话只有一行 pane `preview`）：

```
⚠ frontend / Claude @ zuan    waiting 3m
  ❯ 把 sidebar 的 workspace chip 换成可折叠的
  ↳ 改完了，144 个 e2e 全绿，要不要 push？
```

装了 hook 却一条事件都没收到过的会话（`hooks_unheard` 非空，`docs/spec/api.md`；Codex 未在 `/hooks` 信任是最常见的一种，agora-dvh.15）在预览下面多一行黄色 `⚠ hook 没接上：…`，全文放在 title 里；服务端判定，第一条事件到达就撤。

## 浏览器通知（MISSION §6.6；A18）

`web/src/notify.ts`（agora-dvh.11）。该不该发是服务端的事（`notification` 事件只在 RUNNING 或 IDLE → WAITING / TURN_DONE / FINISHED / FAILED 上来，`docs/spec/api.md`）；前端只管权限、弹、点击：

- 权限问一次：`Notification.permission` 还是 `default` 时主区顶部有一条"agent 需要你时弹浏览器通知？ [允许通知] [以后再说]"，答过（granted / denied）或点了"以后再说"就没了，之后不再弹权限框；denied 时通知静默丢掉。
- 弹：同一会话在通知中心只占一格（弹新的先 close 旧的；tag 每条唯一——macOS 上同 tag 替换只静默更新不弹横幅，2026-09-04 人眼验收实测）。
- 点击：窗口拉到前面，该行成为侧栏 active 行——WAITING / TURN_DONE 的就地回答区随行展开（Allow / Deny / 下一条指令），不是把人扔进终端。

## 运行时 degraded 横幅（MISSION §10.3；agora-bgr）

```
┌─────────────────────────────────────────────────────────────┐
│ ⚠ 运行时 degraded：运行时 server 不可用: protocol version   │
│   mismatch (client 8, server 7)。会话状态暂不可知，进程没有被杀。 │
├─────────────────────────────────────────────────────────────┤
│ [tabs…]                                                     │
```

主区顶部一行，`--warn` 色、暗黄底，只在 `/api/health` 的 `runtime.status = degraded` 时出现（agora 对 tmux 失明：版本过低、client / server 协议不匹配，ADR-001 D7）。文案 = `reason` 原文 + 固定的后半句；原文太长截断，完整原因放 `title`。没有它，用户看到的是一屋子 UNKNOWN 而不知道为什么。数据源是 `web/src/health.ts` 的 `HealthWatcher`（健康 60 s、degraded 10 s 重拉，见 `docs/spec/api.md` Health 节），运行时恢复后横幅自己消失，不用刷新。未认证门页不拉完整报告、没有这条横幅。守卫 `web/src/Workspace.test.tsx`、`web/src/health.test.ts`。

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

未注册 / 未识别的会话在侧栏已登记列表之下单列一段（`UNREGISTERED n`，过滤时不显示），每项显示为：

```
? emergency
  Unknown Agent（像 claude）    ← 括号是进程树给的 hint，认不出就没有
```

点一下展开采纳表单：Name（默认 pane 标题 / 会话名）、Project（默认 pane 的当前目录）、Agent（默认 hint，可改；用户填的优先）；「采纳」发 `POST /api/sessions/adopt`，会话随 `session_created` 进列表并开成 Tab。

Kill 确认框（MISSION §8：确认跟着"杀"走；Kill 只杀进程，运行时会话与输出保留到清理，§4.6）。确认之后到节点返回之前，面板显示一行"正在结束…（先请进程退出，最多等 7 秒）"——Kill 是 TERM → 5 s → KILL 的宽限（ADR-001 D2），交互式 shell 会吃满，只把按钮变灰会让人以为没点上。采纳时没记下启动命令的会话（`command` 为 null）Restart 按钮禁用并在 title 里说明（API 侧是 409 `no_command`）：

```
Kill sglog / Codex @ zuan?
The running agent process will be killed. Its output stays until you clean it up.
[Cancel] [Kill]
```
