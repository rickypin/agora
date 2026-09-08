# UX 线框、键位与视觉参考

产品决定在 MISSION §6（四个 screen、attention 表、创建对话框字段）；本文是线框与键位表，随实现改。

## 主界面线框

```
┌─────────────────────────────────────────────────────────────┐
│ agora                          mac ●   zuan ●        │
├─────────────────┬───────────────────────────────────────────┤
│ AGENTS          │ agora / claude @ mac    [Settings] [关闭] │
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
| Cmd/Ctrl + K、Alt/Option + K | Command Palette |
| Cmd/Ctrl + F、Alt/Option + F | Filter sidebar（name / node / agent / preview；Enter 打开第一条） |
| Alt/Option + 1…9 | 跳到侧栏第 N 条（按当前过滤后的显示顺序） |
| Alt/Option + ] / [ | Next / Previous Agent |
| Alt/Option + N | New Agent |

**终端有焦点时 Ctrl 组合一律归 pane**（agora-82g，2026-09-05 代检：Ctrl+K / F 曾被 xterm 吞成 `^K` / `^F`，面板与过滤开不了）：Ctrl+K 是 readline 的 kill-line、Ctrl+F 是 vim / less 的翻页，agora 不抢——§6.5 的硬约束不只那六个键。终端里开面板 / 过滤用 Alt/Option+K / F（macOS 上 Cmd+K / F 也行）；焦点在侧栏、过滤框或 body 上时 Ctrl+K / F 照旧。实现：终端层（`handleTerminalKey`）把 `matchShortcut` 认的、不带 Ctrl 的键返回 false 让 xterm 别碰，事件照常冒泡到 window 由全局层处理——不然 xterm 处理完 Alt+字母（Linux / Windows 发 `ESC x`）也会 stopPropagation，Alt 系键位在终端聚焦时同样按不动。Alt+F 在 Linux 的 readline 里是 forward-word，Alt+→ 同义且照发；macOS 上 Option+F 本来只会打出 `ƒ`。Chrome 在 Windows / Linux 把 Alt+F 当菜单加速键，页面 `preventDefault` 能不能压住它只有人在真浏览器里按过才算数（验证纪律见下）。

浏览器全局快捷键必须避免吞掉终端内 Ctrl+C / Ctrl+D / Ctrl+Z / Ctrl+R / Ctrl+A / Ctrl+E 等常见操作。**能留给 agora 的只有浏览器自己没占的组合**：Cmd/Ctrl+数字（标签页）、Cmd/Ctrl+Shift+] / [（下/上一个标签页）、Cmd/Ctrl+N（新窗口）都在浏览器 UI 层被吃掉，页面连 keydown 都收不到，`preventDefault` 也救不回来（macOS Chrome 人眼实测 2026-09-04，agora-rzn：这三类原先都写在本表里，按下去响应的是 Chrome）。所以除了面板与过滤这两个 Cmd/Ctrl 组合，其余一律走 Alt/Option。**验证纪律**：jsdom 没有保留键这回事，agent-browser 经 CDP 把按键直接注入渲染进程、绕过浏览器的加速键处理——两者对 Cmd/Ctrl 系键位都只会给出假阳性，只有人在真浏览器里按过才算数。

实现落在 `web/src/keys.ts` 一层（终端侧接在 TerminalView 的 `attachCustomKeyEventHandler`，全局侧接在 Workspace 的 window keydown）：那六个 Ctrl 组合被写成名单，全局层一律不认，哪怕将来给它们绑了动作。Alt/Option 系一律认 `event.code`（`Digit3` / `BracketRight` / `KeyN`）而不是 `event.key`——macOS 上 Option+3 的 key 是 `£`、Option+] 是 `‘`、Option+N 是死键 `Dead`。

### 终端要回来的键（agora-xqa.3）

| 按键 | agora 发给 pane | 为什么不能交给 xterm.js |
|---|---|---|
| Shift + Enter | `ESC CR`（= Option/Alt+Enter） | xterm.js 默认发裸 CR，TUI 分不出"换行"与"发送"，一按就提交。不用 kitty/CSI-u 的 `ESC[13;2u`：TUI 没打开 kitty keyboard protocol 时会把它当乱码打进输入框；`ESC CR` 是 Claude Code / Codex 今天就认的换行，也是各家 terminal-setup 给 iTerm2 装的那条约定 |
| Cmd + ← / → | `ESC[H` / `ESC[F`（Home / End） | 浏览器把 Cmd+方向键留给了历史前进后退，不 `preventDefault` 会真的退出页面、顺手丢掉终端视图；xterm.js 又不转发它们 |
| Option/Alt + ← / →（按词跳） | 浏览器在 macOS / iOS 上：`ESC b` / `ESC f`（readline 的 backward-word / forward-word）；其它平台：`ESC[1;5D` / `ESC[1;5C`（= Ctrl+←/→）。非 mac 的 Alt+↑/↓ 同理发 `ESC[1;5A/B` | xterm.js 5.x 在自己的 Keyboard.ts 里做这条改写，6.0.0 的 #5346 把它删了、交给 embedder，改发裸 `ESC[1;3D/C`，zsh 不认：2026-09-06 实测 `echo foo bar` → Option+← → `X` 得到 `echo foo bar3DX`（没跳、残片进命令行）。agora 升到 6.0 时（agora-hhu）照 5.5.0 的字节接管，平台按**浏览器**的 `navigator.platform` 判（xterm 5.5 也是这么判的，键位随用户手里的键盘走），TerminalView 挂载时算一次传给 `keys.ts`。带 Shift / Ctrl 的 Alt+方向键与 mac 上的 Option+↑/↓ 5.5 本来就不改写（`ESC[1;4D`、`ESC[1;3A`…），仍交回 xterm。非 mac 的 Chrome 把 Alt+←/→ 留给了历史前进后退，所以这几个键也 `preventDefault`。守卫：`web/src/keys.test.ts`「Option/Alt+←/→ 按词跳由 agora 键位层发字节」（mac / 非 mac 各一组、带 Shift 不接管、Cmd+← 仍是 Home），`web/src/TerminalView.test.tsx`「passes the browser platform to the key layer」（TerminalView 传对了平台） |

粘贴、resize 重排、滚轮回看都走 xterm.js 原路，这一层不碰。这些字节进 pane 前还要过一道 `tmux attach` 客户端：它认得的键会按 pane 的终端类型**重新编码**（实测 tmux 3.7c 把 `ESC[H` / `ESC[F` 改发成 `ESC[1~` / `ESC[4~`），键义不变——守卫 `tests/terminal_keys.rs` 因此两种编码都接受。

### 终端焦点（agora-p29）

打开一个会话就该能直接打字，不用先点终端。TerminalView 在三处交焦点：① 挂载时 `term.focus()` 一次；② WS 收到 `status: attached` 再一次——新开的浏览器标签页里挂载那一次偶尔不生效（2026-09-03 目检：attach 成功、键入不进 pane、点一下终端才好），但用户这会儿已经在别的文本输入（侧栏过滤、Rename、New Agent 表单）里打字的话不抢；③ `.term-host` 上的 pointerdown 把焦点交回 xterm 的 helper textarea，点在 `.xterm` 之外的 padding 上也算——xterm 自己的 mousedown 只覆盖 `.xterm` 内部，padding 那一圈由 agora 取消浏览器缺省的 mousedown，不然焦点会被挪到 body。不在 pointerdown 上 `preventDefault`：那会连带取消后面的 mousedown / click，xterm 的选区靠它们。守卫 `web/src/TerminalView.test.tsx`。④ 点**已经激活**的侧栏行（agora-vcc，2026-09-05 代检）：视图状态不变，TerminalView 不重挂也不会再 focus，焦点留在刚点的按钮上——Workspace 在这种命中时显式调用挂着的终端的 `focus()`（TerminalView 经 `focusRef` 暴露），不在行按钮的 mousedown 上 `preventDefault`，侧栏的键盘可达性不变。守卫 `web/src/Workspace.focus.test.tsx`。人工复现步骤在 M1a 演示剧本第 4 步（新标签页打开会话 → 直接键入）。

Command Palette 支持 fuzzy search sessions / projects / nodes / actions（`New Claude in agora @ zuan`）。目标是让管理 20–50 个 agent 时仍然高效。手机端没有键盘，快捷键与命令面板只在桌面生效（`isDesktop()`：视口窄于 700 px 就不装 window keydown，命令面板也开不出来）。

面板里选 `New <agent> in <project> @ <node>` 直接起会话，走的是和 New Agent 对话框同一个 `POST /api/sessions`，字段取默认值（项目名当 display_name、agent 的默认命令）——面板的意义就是不填表；要填 Task / Worktree 的走对话框（面板末尾那条 `New Agent…`）。本机一组之外，每个**在线**的 peer 各一组（A45，agora-fna）：项目与 agent 从那台机器取（`?node=`），条目带 `node`、节点一跳转发；离线的 peer 没有条目、也不去拉（`web/src/CommandPalette.test.tsx`）。侧栏过滤与面板共用 `web/src/fuzzy.ts` 的打分（连续命中、词首命中加分，同分保持原顺序），所以 Alt/Option+N 跳的第 N 条永远等于眼睛看到的第 N 条。

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
▸ FINISHED 3
```

展开 `▸ FINISHED 3` 之后（收起时这三行不画，但 Alt/Option+N 的序号照数）：

```
▾ FINISHED 3
✓ 回一句 closewin          / Claude external   finished 1m
✓ 回一句 ctrlc             / Claude external   finished 1m
✓ own-kill                 / shell             finished 9m
```

侧栏选中行下方是就地 respond 区（MISSION §6.3 §7.3，`web/src/Respond.tsx`）：WAITING 且 `reason = permission`、`respond_via = hook` → 问题文本（`pending_decision.summary`，形如 `Bash: git push origin main`——工具名 + `tool_input` 主参数的首行，截到 200 字符加 `…`；载荷没带 `tool_input` 才只剩工具名，`src/adapter/hooks.rs permission_summary`）+ Allow / Deny / 打开终端，`respond_within_secs` 短于 5 分钟（Codex 20 s）时再加一行"N 秒内没答会交回终端"；WAITING 的其它情形（`question`，或 `respond_via = terminal`）→ 只有问题文本与"打开终端"；TURN_DONE → `↳` 最后一条回复 + "下一条指令"输入框（发 text，尾部带换行）。Allow / Deny 撞上 `no_pending_decision`（终端先答了 / 过期）只显示一行提示，行状态随事件自己变。

选中行的展开区从上到下（`web/src/SessionRow.tsx`；agora-h1k.3 定）：① 就地 respond（上一段，WAITING / TURN_DONE 才有）；② **验收标准**折叠块——`task.acceptance`（`docs/spec/api.md`，读自 beads 的 acceptance_criteria、不复制进 agora 的库，不变量 12）全文、多行原样，一行 summary `▾ 验收标准 · agora-h1k.3` 可折叠，默认展开（行展开就是为了看"做完算什么"，MISSION §6.3 看结果 / A40）；没有任务或 beads 里没写不占位。respond 在上：回答问题 / 给下一条指令是先做的事，对照验收是看结果时的事；③ **改动文件**（`web/src/Changes.tsx`；A41，agora-h1k.5）接在验收标准之后，两者并排对照——`GET /api/sessions/:id/changes`（`docs/spec/api.md`「只读产出」，该 worktree 的只读 `git status`）的列表 `<单字母> <path>`（M / A / D / R / C / T / U / ?，与 `git status --short` 同一习惯），标题带分支名，空列表一行「无改动」，`reason` 按类型一行灰字（`not_a_repo` 不是 git 仓库、`no_directory` 工作目录不存在、`no_git` 本机没有 git、`timeout` git status 超时、`git` git 失败——只按类型不按文本，MISSION §2.3 规则 10）；status 是 TURN_DONE / FINISHED / FAILED（做完了看结果）或 RUNNING（瞄一眼进度）时显示，每次 status 变化拉一次、**不轮询**，WAITING / IDLE / STARTING / UNKNOWN 不占位——此刻该做的是回答问题。旁边的「看 diff」把主区切成这一行的 diff 视图（agora-a46 之后没有标签页，主区一次只显示选中行的终端或它的 diff）：crumb 是 `git diff / <会话名>` 带「关闭 diff」、没有 Settings（它不是会话），里面是同一个 TerminalView 以 `WS /api/sessions/:id/diff` 挂的**只读**终端——在该 worktree 跑 `git --no-pager diff HEAD`，连接状态显示「只读」，键入不发也不进 PTY（两边各守一半），跑完显示退出码、按钮「重新运行」再跑一次；「关闭 diff」或再点这一行回到它的终端，点别的行则切到那一行的终端，三者都关 WS、git 进程被收走；侧栏不多一行、`GET /api/sessions` 行数不变；会话被删时主区回到空。`reason` 非空时按钮禁用（没有可 diff 的仓库）。在 beads 里改了验收标准，`TaskIndex` 的 TTL（5 min）到期重查后展开区跟着变。守卫 `web/src/SessionRow.test.tsx`、`web/src/Changes.test.tsx`、`web/src/Workspace.test.tsx`（看 diff 一节）。

```
◆ agora-h1k.3 会话行展开显示任务的验收标准 / Claude @ mac    turn done 2m
  ❯ 做 h1k.3
  ↳ 改完了，vitest 全绿
  │ ↳ 改完了，vitest 全绿
  │ [下一条指令            ] [发送]
  │ ▾ 验收标准 · agora-h1k.3
  │ tests/task_info.rs::acceptance_is_read_not_stored（库里无该字段、API 有）；
  │ 前端 vitest：展开显示与折叠；docs/spec/api.md task 形态回写。
  │ 改动 · agora-h1k.3                              [看 diff]
  │ M src/session/manager.rs
  │ ? web/src/Acceptance.test.tsx
```

点「看 diff」后主区：

```
git diff / agora-h1k.3                              [关闭 diff]
只读
diff --git a/src/session/manager.rs b/src/session/manager.rs
@@ -12,6 +12,9 @@
+    pub acceptance: Option<String>,
```

实现（`web/src/attention.ts`，agora-dvh.10）：侧栏就是 Dashboard——行按分数降序 → bd 优先级升序（`task.priority`，无 bd 视为 P2）→ `status_since` 早的在前排好，再拼成三段：NEEDS ATTENTION（分数 ≥ FINISHED，除去下面收起来的）→ RUNNING → FINISHED 折叠区；过滤只删不换序、折叠只是不画不是不数，所以 Alt/Option+N 跳的第 N 条永远等于展开后眼睛看到的第 N 条（收起时第 N 条可能正在折叠区里）。第一列 `taskLabel`：`task.id + title` > `task_ref`（issue id 或首条 prompt 摘要）> 名字。header 下一行是各状态计数。

**FINISHED 分来源与「看过」**（MISSION §4.6「看过」的三条证据、§6.3 排序表；A46，agora-j4w.1；`finishedCollapsed` / `sectionOf`）：`origin = external` 的 FINISHED 一律直接进折叠区——它的工作面在别的窗口，人是在终端里自己结束的会话（证据 ②），agora 这边没有 pane 也没有 Restart，能给的只有两行摘要；不按 `reason` 分（Claude / Grok 连关窗口都发 SessionEnd、Codex 关窗口不发，分不可靠也不必要）。`origin = agora / adopted` 的 FINISHED 先留在 NEEDS ATTENTION，**看过**（证据 ①：在本浏览器里被选中展开过一次）之后才进折叠区。「看过」是浏览器视图状态，不加服务端字段：`Workspace` 在**离开**那一行（切到别的行 / 关闭视图）的时刻记下——记在选中那一刻它会立刻掉进收起的折叠区、主区还开着它的终端而侧栏找不到这一行；离开时看它当时的状态，选中时还在跑、离开后才 FINISHED 的不算看过。集合存 `localStorage`（键 `agora.seen-finished`，读写都包 try/catch，丢了的代价只是几行回到 NEEDS ATTENTION 再看一眼）；行被删（Delete metadata）或又跑起来（Restart）记号作废，下一次 FINISHED 是新结果。折叠区标题 `▸ FINISHED N` 是按钮（`data-testid="section-finished"`，`aria-expanded`），默认收起、刷新回到收起，选中行落进收起的折叠区（Alt/Option+N、finished 通知点击、命令面板都能从外面选中它）时自动展开一次、人仍能手动收回（agora-4nk；守卫 `Sidebar.test.tsx`「auto-expands the collapsed Finished section」），N 是折叠区里的行数（Header 计数行里的 `Finished` 仍按状态数、不变——它变成一键清理的按钮是 agora-j4w.2）。守卫 `web/src/attention.test.ts`（external FINISHED 不 needsAttention、agora FINISHED 看过前后、三段拼接顺序等于 sortByAttention 顺序且与过滤可交换）、`web/src/Sidebar.test.tsx`（默认折叠带计数、展开后可选中且序号连续、NEEDS ATTENTION 无 external FINISHED）、`web/src/Workspace.test.tsx`「an agora FINISHED row stays in NEEDS ATTENTION until it has been opened and left」。不做 Archive（MISSION §11）、不按项目 / 节点分组（与 Alt+N 序号冲突，agora-a46 的教训）。

行上的 `@ <node>`（MISSION §3.5 "每行标明节点"；`web/src/SessionRow.tsx`，agora-7ku.5）只给 **node ≠ 本机** 的行：单机满屏 `@ mac` 是噪音，上面线框里本机的 `@ mac` 在实现里不显示；本机 id 取 `/api/system` 的 `node`（与 Header 本机那一枚同源），还没拉到之前谁都不标——先满屏 `@` 再消失更难看。peer 断线后行带 `stale: true` 与 `last_seen`（`docs/spec/api.md`「peer 视图」；agora-7ku.6）：行**不消失**，整行淡显（`li.stale`，hover / 选中回到可读），`.meta` 里 `@ zuan` 之后多一段 `○ 上次见到 23:10`——黄点与 `clockText`（本地时区 HH:MM）都与 Header 那一枚 stale 节点同源，完整 UTC 放 title；非 stale 行没有这一段。点开 stale 行与点开别的行没有区别（建终端 WS、就地 respond 照常）：节点看到人碰了 stale peer 的会话就插一次重连（`docs/spec/architecture.md`「立即重试」），恢复后事件流把行刷回正常、淡显消失，浏览器不用多做任何事。

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
│ agora / claude @ mac                    [Settings] [关闭]   │
```

主区顶部一行，`--warn` 色、暗黄底，只在 `/api/health` 的 `runtime.status = degraded` 时出现（agora 对 tmux 失明：版本过低、client / server 协议不匹配，ADR-001 D7）。文案 = `reason` 原文 + 固定的后半句；原文太长截断，完整原因放 `title`。没有它，用户看到的是一屋子 UNKNOWN 而不知道为什么。数据源是 `web/src/health.ts` 的 `HealthWatcher`（健康 60 s、degraded 10 s 重拉，见 `docs/spec/api.md` Health 节），运行时恢复后横幅自己消失，不用刷新。未认证门页不拉完整报告、没有这条横幅。守卫 `web/src/Workspace.test.tsx`、`web/src/health.test.ts`。

## New Agent 对话框线框（MISSION §6.4）

```
New Agent
Node:     [ zuan ▼ ]              # 本机 + 在线的 peer；选 peer 则经一跳转发执行
Project:  [ ~/code/agora ]
Worktree: [ main ▼ / 新建… ]      # 新建 = git worktree add，base 默认主 worktree 当前分支；合并/销毁归人（MISSION §6.4）
Agent:    [ Claude Code ▼ ]
Task:     [ agora-90t.1 ▼ / 一句话 ]  # 有 bd 的仓库从 bd ready 选，否则一句话；留空取首条 prompt
Name:     [ agora-mission ]
Command:  [ claude ]
Prompt:   [ 任务 agora-90t.1：… ]    # 只对接受首条 prompt 的 agent 显示；选任务后预填模板，可改
[ Create ]
```

Node（A45，agora-fna；`web/src/NewAgentDialog.tsx`）：第一项本机（名字取 `/api/system` 的 `node`），其后每个已配置的 peer 一项，数据源是 Header 同一份 `nodeStatuses`（`HealthWatcher` 的 peers 段 + 事件流的 `peer_changed`，不另起轮询）。在线的 peer 可选；离线的**列出来但不可选**（`<option disabled>`），文字按类型加一段——`版本不兼容`（`last_error = incompatible_version`）、见过的离线 `stale`、没见过的 `未连接`——选了只会得到 502，不如在下拉里就说清楚；对话框开着时所选 peer 掉线，它变灰、Create 也禁用。选了 peer：Project / Worktree / Task / Agent 四个下拉**全部改从那台机器取**（五个 catalog 端点带 `node=`，`docs/spec/api.md`「在 peer 上起会话」），上一台机器的项目一个都不留在下拉里；「新建…」与 Create 的 body 带 `node`，在那边执行；201 的 `id` 带它的前缀，新行随 peer 视图进侧栏、带 `@ <node>`。守卫 `web/src/NewAgentDialog.test.tsx`。

V1 的取舍（agora-xqa.12）：Project 是可输入的下拉
（`<input list>`）——列表来自扫描并按最近使用排序，但 `project_roots` 默认为空，只给下拉的话
新装的 agora 一个会话都起不了；Worktree 列现有的，末尾一项「新建…」（A44，agora-h1k.1）：选中后
出现名字输入框（默认取 Name 栏的值；issue id 默认随 A43）与「建」按钮，`POST /api/projects/worktrees`
成功后重拉列表并选中新项，失败按错误类型给文案（同名 worktree / 分支 / 目录已存在、名字不合法、git
失败）；选主 worktree 等于选仓库本身；Agent 的名字与默认命令来自 `GET /api/agents`，
末尾多一项 `custom`——它没有 Adapter，Command 必填。Name 与 Command 有默认值，用户手改过之后
换项目 / 换 agent 不再覆盖。

**Task 从就绪任务选**（MISSION §6.4「从就绪任务起会话」；A43，agora-h1k.2；`web/src/NewAgentDialog.tsx`）：
选了项目就与 worktrees 并行拉 `GET /api/projects/tasks?path=`（该仓库 `bd ready --json`，epic 已滤掉）。
有任务时 Task 是下拉（`<select id="na-task">`）：第一项「一句话…」（选它时下面出现原来的自由文本框
`na-task-text`），其余每项 `<id> · <title>`，顺序照 bd 给的。选中一个任务 → `task_ref` = issue id、
Name = issue 标题（用户手改过 Name 就不覆盖，沿用上面的规则）、Worktree「新建…」的默认名 = issue id
（这就是 A44 留给 A43 的入口；没选任务时仍是 Name）、Prompt 框（`na-prompt`，只在所选 agent 的
`prompt` 标志为 true 时显示——Claude / Codex 有，Grok / shell / custom 没有）预填下面的模板，用户可改，
非空才随 `POST /api/sessions` 的 `prompt` 发；换回「一句话…」时没改过的 Name 回项目名、Prompt 清空。
没有任务（`reason` 非空）时 Task 保持一句话输入，旁边一行灰字**按 `reason` 的类型**给文案（MISSION §2.3
规则 10）：`no_bd`「没装 bd」、`no_beads`「该仓库没有 beads」、`timeout`「bd 没有在 10 s 内应答」、
`bad_output`「bd 的输出读不懂」，不认识的类型不提示。claim 由 agent 自己做——页面对 beads 什么都不写
（不变量 12）。守卫 `web/src/NewAgentDialog.test.tsx`（选任务后的预填、无 bd 的提示、prompt 标志为
false 的 agent 不显示也不发）、`web/src/taskPrompt.test.ts`。

prompt 模板（`web/src/taskPrompt.ts` 的 `taskPrompt(id, title)`，原文以此为准；节点把整段单引号包住
接到启动命令尾、不进库、Restart 不重发，见 `docs/spec/api.md`「从就绪任务起会话」）：

```
任务 <id>：<title>

先 `bd update <id> --claim`，再 `bd show <id>` 读任务书（description / notes / acceptance）。
按 AGENTS.md 的任务纪律干活：commit subject 末尾带 (<id>)；干活中发现的别的问题用 `bd create ... --deps discovered-from:<id>` 另立。
做完 `bd close <id> --reason "<证据>"` 写证据。
```

## 未注册会话与危险操作确认（MISSION §5.5 / §8）

未注册 / 未识别的会话在侧栏已登记列表之下单列一段（`UNREGISTERED n`，过滤时不显示），每项显示为：

```
? emergency
  Unknown Agent（像 claude）    ← 括号是进程树给的 hint，认不出就没有
```

点一下展开采纳表单：Name（默认 pane 标题 / 会话名）、Project（默认 pane 的当前目录）、Agent（默认 hint，可改；用户填的优先）；「采纳」发 `POST /api/sessions/adopt`，会话随 `session_created` 进列表并成为选中行、主区打开它的终端。

Kill 确认框（MISSION §8：确认跟着"杀"走；Kill 只杀进程，运行时会话与输出保留到清理，§4.6）。从确认起到该行变成 FINISHED 之前，面板显示一行"正在结束…（先请进程退出，最多等 7 秒）"——Kill 是 TERM → 5 s → KILL 的宽限（ADR-001 D2），交互式 shell 会吃满，只把按钮变灰会让人以为没点上。节点只同步等 1 s，没退就立即返回仍 alive 的行并在后台把宽限走完（否则经 peer 转发的 Kill 会撞上 5 s 转发超时报"节点不可达"，agora-284；`docs/spec/api.md` POST /kill），所以这一行的依据是"请求还在飞"或"行上 `killed_at` 已写而 `alive` 仍为 true"，进程一退、行推成 FINISHED 就消失。采纳时没记下启动命令的会话（`command` 为 null）Restart 按钮禁用并在 title 里说明（API 侧是 409 `no_command`）：

```
Kill sglog / Codex @ zuan?
The running agent process will be killed. Its output stays until you clean it up.
[Cancel] [Kill]
```

## Header 节点状态（MISSION §10.3；agora-7ku.12）

```
agora                       AGENTS 12
mac ●    zuan ✗ 指纹不匹配 · 上次见到 23:10
Running 5 · Needs Input 2 · …
```

侧栏就是 Dashboard，header 就是它的首行（上文 Attention Dashboard 线框的 `mac ● zuan ●`）；实现 `web/src/Header.tsx`，节点一行放在 `agora` 标题下面而不挤进标题行——260 px 的侧栏放不下 `zuan ✗ 指纹不匹配 · 上次见到 23:10`。本机排第一（名字是 `/api/system` 报的本机 node.id——`web/src/health.ts` 的 `VersionWatcher` 同一次拉取带出，不另起轮询；还没拉到之前叫"本机"。agora-7ku.5），peer 按 `/api/health` peers 段的键序。每枚：

| 状态 | 显示 | 依据 |
|---|---|---|
| 在线 | `zuan ●`（绿） | `online: true`；在线只有名字和点，title 里有 `上次见到 <UTC>` |
| stale | `zuan ○ 上次见到 23:10`（黄点） | `online: false`、无 `last_error`、有 `last_seen`——保留最后一眼，绝不消失（不变量 8） |
| 异常 | `zuan ✗ 指纹不匹配 · 上次见到 23:10`（红） | `last_error` 的五个类型各一句：`incompatible_version` 版本不兼容、`fingerprint_mismatch` 指纹不匹配、`unauthorized` 未授权、`unreachable` 不可达、`misconfigured` 配置错误（agora-41e；ADR-003 D3：本机这一行 `peers[]` 配错了——token_file 权限 / 属主 / 内容、url、指纹——要改文件，不是等网络）；没见过就没有"上次见到" |
| 未连接 | `zuan ○ 未连接`（灰） | 配置了、还没试过：`last_seen`、`last_error` 都是 null |
| 本机 | `本机 ●` / `本机 ✗ 不可达` / `本机 … 探测中` | 上一次拉 `/api/health` 成功 / 失败 / 还没拉过 |

"上次见到"是本节点时钟打的时间（MISSION §3.5，不信 peer 报的），显示本地时区 `HH:MM`，完整 UTC 在 title；`retrying: true` 时 title 尾部加"重试中"——`misconfigured` 的 `retrying` 恒为 false（节点不重试、每 10 s 重读配置，`docs/spec/api.md` Health 节），所以它的 title 永远没有"重试中"，人看到的是"去改文件"而不是"等一等"；改好后节点自己恢复，这枚变回绿点，不需要重启。原因文案只按 `last_error` 的类型选（MISSION §2.3 规则 10），不解析任何消息文本；不认识的值当没有错误。数据源是 `HealthWatcher` 同一次拉取带出的 peers 段（`nodesSnapshot` / `subscribeNodes`，与 degraded 横幅各自订阅），没有第二条轮询。守卫 `web/src/Header.test.tsx`（在线 / stale / 指纹不匹配 / 版本不兼容 / 配置错误各一测，另一测覆盖其余文案与"未连接"）、`web/src/health.test.ts`、`web/src/Workspace.test.tsx`。
