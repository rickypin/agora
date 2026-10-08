# UX 线框、键位与视觉参考

产品决定在 MISSION §6（四个 screen、attention 表、创建对话框字段）；本文是线框与键位表，随实现改。

## 主界面线框

```
┌─────────────────────────────────────────────────────────────┐
│ agora                          mac ●   zuan ●        │
├─────────────────┬───────────────────────────────────────────┤
│ AGENTS 2        │ agora / claude @ mac    [Settings] [关闭] │
│ [需要我][按项目]│                                           │
│                 │ ┌───────────────────────────────────────┐ │
│ ● agora         │ │                                       │ │
│   ✦ Claude @mac │ │                                       │ │
│   agora ⎇ main  │ │             TERMINAL                  │ │
│   working 02:31 │ │                                       │ │
│                 │ │                                       │ │
│ ⚠ sglog         │ │                                       │ │
│   ◆ Codex @zuan │ └───────────────────────────────────────┘ │
│   sglog ⎇ main  │                                           │
│   NEED INPUT    │                                           │
│                 │                                           │
│ + New Agent     │                                           │
└─────────────────┴───────────────────────────────────────────┘
```

侧栏有两种视图（下文「Attention Dashboard 线框」的头一段），上图是默认的「需要我」；行身份三件（品牌徽标 / 节点 chip / 仓库 ⎇ 分支）见该节末尾几段。

侧栏宽度可拖、双击回默认、记在浏览器（拖柄 min 220 / max 视口 50%，默认 260 px；窄于 700 px 不渲染拖柄。agora-uvd.5，2026-09-09）。

## 快捷键

| 快捷键 | 动作 |
|---|---|
| Cmd/Ctrl + K、Alt/Option + K | Command Palette |
| Cmd/Ctrl + F、Alt/Option + F | Filter sidebar（name / node / agent / preview；Enter 打开第一条） |
| Alt/Option + 1…9 | 跳到侧栏第 N 条（按当前视图过滤后的显示顺序：「需要我」是 attention 顺序、「按项目」是树的 DFS 顺序；折叠的行照数，被折进「历史对话」里的 superseded 旧行也照数，跳到它们时自动展开那一枚） |
| Alt/Option + ] / [ | Next / Previous Agent |
| Alt/Option + N | New Agent |
| Alt/Option + G | 切换侧栏视图 |
| Alt/Option + R | 聚焦回答面板的输入框（TURN_DONE）或第一个按钮（WAITING）；面板不在时不做事 |

**终端有焦点时 Ctrl 组合一律归 pane**（agora-82g，2026-09-05 代检：Ctrl+K / F 曾被 xterm 吞成 `^K` / `^F`，面板与过滤开不了）：Ctrl+K 是 readline 的 kill-line、Ctrl+F 是 vim / less 的翻页，agora 不抢——§6.5 的硬约束不只那六个键。终端里开面板 / 过滤用 Alt/Option+K / F（macOS 上 Cmd+K / F 也行）；焦点在侧栏、过滤框或 body 上时 Ctrl+K / F 照旧。实现：终端层（`handleTerminalKey`）把 `matchShortcut` 认的、不带 Ctrl 的键返回 false 让 xterm 别碰，事件照常冒泡到 window 由全局层处理——不然 xterm 处理完 Alt+字母（Linux / Windows 发 `ESC x`）也会 stopPropagation，Alt 系键位在终端聚焦时同样按不动。Alt+F 在 Linux 的 readline 里是 forward-word，Alt+→ 同义且照发；macOS 上 Option+F 本来只会打出 `ƒ`。Chrome 在 Windows / Linux 把 Alt+F 当菜单加速键，页面 `preventDefault` 能不能压住它只有人在真浏览器里按过才算数（验证纪律见下）。

浏览器全局快捷键必须避免吞掉终端内 Ctrl+C / Ctrl+D / Ctrl+Z / Ctrl+R / Ctrl+A / Ctrl+E 等常见操作。**能留给 agora 的只有浏览器自己没占的组合**：Cmd/Ctrl+数字（标签页）、Cmd/Ctrl+Shift+] / [（下/上一个标签页）、Cmd/Ctrl+N（新窗口）都在浏览器 UI 层被吃掉，页面连 keydown 都收不到，`preventDefault` 也救不回来（macOS Chrome 人眼实测 2026-09-04，agora-rzn：这三类原先都写在本表里，按下去响应的是 Chrome）。所以除了面板与过滤这两个 Cmd/Ctrl 组合，其余一律走 Alt/Option。**验证纪律**：jsdom 没有保留键这回事，agent-browser 经 CDP 把按键直接注入渲染进程、绕过浏览器的加速键处理——两者对 Cmd/Ctrl 系键位都只会给出假阳性，只有人在真浏览器里按过才算数。

实现落在 `web/src/keys.ts` 一层（终端侧接在 TerminalView 的 `attachCustomKeyEventHandler`，全局侧接在 Workspace 的 window keydown）：那六个 Ctrl 组合被写成名单，全局层一律不认，哪怕将来给它们绑了动作。Alt/Option 系一律认 `event.code`（`Digit3` / `BracketRight` / `KeyN` / `KeyG` / `KeyR`）而不是 `event.key`——macOS 上 Option+3 的 key 是 `£`、Option+] 是 `‘`、Option+N 是死键 `Dead`、Option+G 是 `©`、Option+R 是 `®`。

### 终端要回来的键（agora-xqa.3）

| 按键 | agora 发给 pane | 为什么不能交给 xterm.js |
|---|---|---|
| Shift + Enter | `ESC CR`（= Option/Alt+Enter） | xterm.js 默认发裸 CR，TUI 分不出"换行"与"发送"，一按就提交。不用 kitty/CSI-u 的 `ESC[13;2u`：TUI 没打开 kitty keyboard protocol 时会把它当乱码打进输入框；`ESC CR` 是 Claude Code / Codex 今天就认的换行，也是各家 terminal-setup 给 iTerm2 装的那条约定 |
| Cmd + ← / → | `ESC[H` / `ESC[F`（Home / End） | 浏览器把 Cmd+方向键留给了历史前进后退，不 `preventDefault` 会真的退出页面、顺手丢掉终端视图；xterm.js 又不转发它们 |
| Option/Alt + ← / →（按词跳） | 浏览器在 macOS / iOS 上：`ESC b` / `ESC f`（readline 的 backward-word / forward-word）；其它平台：`ESC[1;5D` / `ESC[1;5C`（= Ctrl+←/→）。非 mac 的 Alt+↑/↓ 同理发 `ESC[1;5A/B` | xterm.js 5.x 在自己的 Keyboard.ts 里做这条改写，6.0.0 的 #5346 把它删了、交给 embedder，改发裸 `ESC[1;3D/C`，zsh 不认：2026-09-06 实测 `echo foo bar` → Option+← → `X` 得到 `echo foo bar3DX`（没跳、残片进命令行）。agora 升到 6.0 时（agora-hhu）照 5.5.0 的字节接管，平台按**浏览器**的 `navigator.platform` 判（xterm 5.5 也是这么判的，键位随用户手里的键盘走），TerminalView 挂载时算一次传给 `keys.ts`。带 Shift / Ctrl 的 Alt+方向键与 mac 上的 Option+↑/↓ 5.5 本来就不改写（`ESC[1;4D`、`ESC[1;3A`…），仍交回 xterm。非 mac 的 Chrome 把 Alt+←/→ 留给了历史前进后退，所以这几个键也 `preventDefault`。守卫：`web/src/keys.test.ts`「Option/Alt+←/→ 按词跳由 agora 键位层发字节」（mac / 非 mac 各一组、带 Shift 不接管、Cmd+← 仍是 Home），`web/src/TerminalView.test.tsx`「passes the browser platform to the key layer」（TerminalView 传对了平台） |

粘贴、resize 重排、滚轮回看都走 xterm.js 原路，这一层不碰。这些字节进 pane 前还要过一道 `tmux attach` 客户端：它认得的键会按 pane 的终端类型**重新编码**（实测 tmux 3.7c 把 `ESC[H` / `ESC[F` 改发成 `ESC[1~` / `ESC[4~`），键义不变——守卫 `tests/terminal_keys.rs` 因此两种编码都接受。

### 终端焦点（agora-p29）

打开一个会话就该能直接打字，不用先点终端。TerminalView 在三处交焦点：① 挂载时 `term.focus()` 一次；② WS 收到 `status: attached` 再一次——新开的浏览器标签页里挂载那一次偶尔不生效（2026-09-03 目检：attach 成功、键入不进 pane、点一下终端才好），但用户这会儿已经在别处操作可交互控件（侧栏过滤、Rename、New Agent 表单、回答面板的 Allow 按钮）的话不抢（agora-y3h：WAITING 面板聚焦的是 `<button>`，旧实现只认文本输入，点通知落到未打开的 WAITING 行时 attached 把焦点抢回终端）；③ `.term-host` 上的 pointerdown 把焦点交回 xterm 的 helper textarea，点在 `.xterm` 之外的 padding 上也算——xterm 自己的 mousedown 只覆盖 `.xterm` 内部，padding 那一圈由 agora 取消浏览器缺省的 mousedown，不然焦点会被挪到 body。不在 pointerdown 上 `preventDefault`：那会连带取消后面的 mousedown / click，xterm 的选区靠它们。守卫 `web/src/TerminalView.test.tsx`。④ 点**已经激活**的侧栏行（agora-vcc，2026-09-05 代检）：视图状态不变，TerminalView 不重挂也不会再 focus，焦点留在刚点的按钮上——Workspace 在这种命中时显式调用挂着的终端的 `focus()`（TerminalView 经 `focusRef` 暴露），不在行按钮的 mousedown 上 `preventDefault`，侧栏的键盘可达性不变。守卫 `web/src/Workspace.focus.test.tsx`。人工复现步骤在 M1a 演示剧本第 4 步（新标签页打开会话 → 直接键入）。

Command Palette 支持 fuzzy search sessions / projects / nodes / actions（`New Claude in agora @ zuan`）。目标是让管理 20–50 个 agent 时仍然高效。手机端没有键盘，快捷键与命令面板只在桌面生效（`isDesktop()`：视口窄于 700 px 就不装 window keydown，命令面板也开不出来）。

面板里选 `New <agent> in <project> @ <node>` 直接起会话，走的是和 New Agent 对话框同一个 `POST /api/sessions`，字段取默认值（项目名当 display_name、agent 的默认命令）——面板的意义就是不填表；要填 Task / Worktree 的走对话框（面板末尾那条 `New Agent…`）。本机一组之外，每个**在线**的 peer 各一组（A45，agora-fna）：项目与 agent 从那台机器取（`?node=`），条目带 `node`、节点一跳转发；离线的 peer 没有条目、也不去拉（`web/src/CommandPalette.test.tsx`）。侧栏过滤与面板共用 `web/src/fuzzy.ts` 的打分（连续命中、词首命中加分，同分保持原顺序），所以 Alt/Option+N 跳的第 N 条永远等于眼睛看到的第 N 条。

## 视觉参考

关键词：**fast、dense、keyboard-first、low visual noise、dark-mode-first**。风格参考 Linear + Warp + 现代终端管理器 + tmux。可用 shadcn/ui，但不应演化为传统 enterprise dashboard。

## Attention Dashboard 线框（MISSION §6.3）

**两种视图**（A47，agora-uvd.2，2026-09-09）：默认永远是「需要我」（今天的 attention 排序，MISSION §6.3 首页原则）。「按项目」是树（A48，agora-uvd.3，线框与规则见下）。切换入口三处：侧栏头部 segment（「需要我」/「按项目」）、命令面板「切换侧栏视图（需要我 / 按项目）」、Alt/Option+G。记忆键 `agora.sidebar-mode`（localStorage，读写包 try/catch，读不到或不合法 → attention）。两种视图共用同一个过滤框与 `rowHaystack`，Alt/Option+N / ]/[ / 过滤框 Enter 都走同一条 `visible`（`web/src/sidebarMode.ts` 的 `visibleOrder`）。

**「按项目」线框**（A48，agora-uvd.3，2026-09-09；`web/src/sidebarTreeModel.ts` 是规则、`web/src/SidebarTree.tsx` 是画法）：

```
mac ●
▾ agora                                  /Users/ricky/code/agora
  ▾ agora ⎇ main 主
    ◆ agora-03k 侧栏展开区可读性差     Claude    turn done 2m
    ● 重构 sglog parser                 Codex     working
      ▸ 历史对话 2
  ▸ agora-uvd.3 ⎇ agora-uvd.3                        1 需要关注
▾ 其它目录
    ○ /tmp                              shell     idle 5m
zuan ●
▾ agora ⎇ main 主
    ⚠ agora-90t.4 ADR-003               Claude    waiting 3m
```

规则（唯一的硬规则是第一条，用户 2026-09-08 的第一条反馈「行随状态自己换位置，人跟不上」）：
- **行的位置只随创建 / 删除变，不随状态变**：`treeOrder` 不拿 `status` / `status_since` 排序（守卫 `web/src/sidebarTreeModel.test.ts`「order never changes when status or status_since changes」逐元素断言）。让一行进 WAITING，它的状态符号变 ⚠、位置不动；切到「需要我」它才在 NEEDS ATTENTION 顶部。只有一条例外（下一条）：被新对话换掉的旧行从顶层拿掉。**没有任何一条规则按“紧急程度”搬行**。
- **一进程一行：superseded 旧行折进当前行的历史**（决策 agora-5gg.8 选 A，实施 agora-5gg.19，2026-09-21）：同一个 agent 进程反复换对话会留下一堆同名行（2026-09-18 现场：notes×8、dbs-operator×9），哪一行是「当前那个」只能猜。数据模型一个字不动——身份仍是 `(host, agent_session_id)`（ADR-002 D7，Restart 要 resume 具体对话）——只改呈现：`end_cause = superseded` 的旧行（判据是 5gg.6 那个封闭枚举，不是 `reason` 那句人话，MISSION §2.3 规则 10）从顶层拿掉，挂到它下面的历史里，主列表只画当前那一行。
  - 认「当前那一行」：同一个桶里**最新的那条不是 superseded 的行**。桶 = 同节点 + 同 `pid`（agora-5gg.5 之后 external 行也报进程号），`pid` 探不到时退到同 `working_directory`（Codex Desktop 那一类：`process = unknown`、`pid` 为 null）。三条边界：桶里**没有当前行就不折**（进程退了、新对话没登记出来或已被 `sessions.external_finished_ttl` 删了）——那些旧行是仅存的记录，按普通 FINISHED 留在顶层；**pid 优先于目录**（同进程才是一条工作线，同目录只是同名）；**不跨组搬行**（同进程换了 worktree 的旧行按普通行画，否则会出现空掉的 worktree 组与一个说不出位置依据的行）。一条链 A→B→C 平铺进 C，不套两层折叠。只有无句柄的行（`origin = external / headless`，`isHandleless`）会被折：supersede 只发在那两种来源上（`src/session/manager.rs` 的 `supersede_handleless_rows`），有 `runtime_ref` 的行身份是 pane、同一进程再发 SessionStart 落回同一行，那是 resume 不是换对话——把它藏起来是唯一不可接受的错（当前行那一侧不问来源）。
  - 画法：当前行下面一枚「▸ 历史对话 N」（`data-testid="row-history-<id>"`，`aria-expanded`，默认收起），展开后是几行普通的淡显行——多缩一格（`li.tree-row.history.depth-4`）、复用 `<SidebarRow>`，所见的就是那两行摘要（❯ prompt / ↳ progress）；点它与点普通行走同一条 `onOpen`。计数 N 是这行名下的旧行数，不是折叠组那种「N 需要关注」。
  - 序号：历史行**照数**（与折叠组、与 A46 Finished 区同一条规则：不画不是不数，MISSION §6.5）。于是一条行被折进来时总行数不变，别人的号一个不抖（反例设计：把它们从 `treeOrder` 里删掉，Alt/Option+N 会在你眼皮底下换号）。从外面选中一行历史（Alt/Option+N、命令面板、通知点击）自动展开那一枚，人仍能手收回（与折叠组那条 agora-4nk 同一条规则）。
  - 这枚折叠只进内存、不进 localStorage：组的折叠是人的浏览习惯（刷新还得认得那棵树），而行 id 会随 TTL 与一键清理死掉，记住一个已经不存在的对话没有收益。**只改「按项目」视图**：「需要我」视图里 external 的 FINISHED 本来就直接进 A46 那个默认折叠区，不需要第二种折叠。
  - `end_cause` 读不到的行（老节点 minor < 1.8、5gg.6 之前写下的 hook 检查点）一律不折：「没有原因」不等于「是 superseded」，也不等于「不是 superseded」——宁可多画一行，不能把还在用的行藏掉。事件流里这个字段随 `status_changed` 就地 patch（`web/src/events.ts`），否则旧行要等刷新页面才折得起来。
- 分组键三层 + 其它目录，固定不自适应：节点组 `node:<node>`（本机第一，其余按 Header 那一排 `nodes` 的顺序，不在里面的按名字；组头是 Header 同源的点 + 名字）→ 仓库组 `repo:<node>:<project.repo>`（组内按 `project.name` 字母序；组头显示 name，title 是仓库路径）→ worktree 组 `wt:<node>:<project.worktree>`（主 worktree 第一并带「主」标记，其余按目录最后一段字母序；组头 `<目录最后一段> ⎇ <branch>`，detached 显示 `⎇ detached`）→ 行按 `created_at` 升序、同值按 id。`project == null` 的行归该节点的 **其它目录** 组 `other:<node>`，排在该节点所有仓库组之后、组内平铺不再按目录分。`project` 字段来自 `docs/spec/api.md`「每条会话的形态」（agora-uvd.1）。
- 组头是按钮（`data-testid="tree-group-<key>"`，`aria-expanded`）；折叠按组 key 记 localStorage `agora.sidebar-tree-collapsed`（不按节点整体记）。**折叠只是不画不是不数**：Alt/Option+N 的序号是树的 DFS 顺序里会话行的次序，折叠组里的行序号照数（与 A46 Finished 区同一条规则，MISSION §6.5）；选中行落进折叠组时那条路径上的组自动展开一次、人仍能手动收回（agora-4nk 同一写法）。折叠时组头右侧显示这组里 `needsAttention` 的行数「N 需要关注」（`data-testid="tree-group-attention-<key>"`，按过滤前的全部行算，0 不显示）。
- 树视图的行不画「仓库 ⎇ 分支」那一行（`RowIdentity` 的 `showProject=false`，agora-uvd.8）：组头已经说了，行上重复是噪音；「需要我」视图照画。
- **FINISHED 行不搬家**：留在原组原位淡显（`li.done`，与 `li.stale` 同一组规则，hover / 选中回到可读）；树视图没有四段标题、没有 Finished 折叠区。
- **树里不做状态分组**（agora-trvm，2026-10-08 审查定稿，二选一里的「不分组」）：树是按项目导航（MISSION §6.3、A48），行的位置只随创建 / 删除变；再插一层状态分组会和这个目的打架——同一个 worktree 的会话会被按状态拆开，「这个项目上有什么」一眼看不全，状态一变还得搬行（正是 2026-09-08「行随状态自己换位置，人跟不上」要消灭的事）。不分组不等于信息缺：unknown 行复用 `<SidebarRow>`，服务端 `reason` 与按 `origin` 分的出口都在行上（`unclear-<id>`，agora-5gg.11），状态符号 `?` 与「需要我」视图同一个 `statusSymbol`、同一档配色（`.st-unknown` 与 `.st-idle` 同为 `--muted`，agora-7vu0）——信息只换位置不换内容，切回「需要我」的 UNCLEAR 段就是按「说不清」找行的那个视图。守卫 `web/src/SidebarTree.test.tsx`「an unknown row keeps its reason and exit line in the tree: no status grouping needed (agora-trvm)」——钉「信息不缺」，不钉分组。
- **组头的就地动作**（A48，agora-uvd.4，2026-09-09）：worktree 组头右侧两个按钮——「+」（`data-testid="tree-new-agent-<key>"`，title「在此起 agent」）带着这个组的 Node / Project / Worktree 打开 New Agent 对话框，对话框里只剩选 Agent（预填是初值不是受控值：三项各在自己的列表到达时选中一次，用户改选后不再覆盖）；「shell」（`data-testid="tree-new-shell-<key>"`，title「在此开 shell」）不开对话框、不问名字，直接 `POST /api/sessions`（`display_name` = worktree 目录最后一段、`agent_type: "shell"`、`working_directory` = 该 worktree、`worktree` = 分支名（主 worktree 为 null，与对话框同一条规则）），成功后走 Workspace 的 `pendingOpen` 自动选中，要改名走 Settings。
- 节点组头右侧一个「+」（`data-testid="tree-new-agent-node-<node>"`）只预填 Node；仓库组头与「其它目录」组头没有按钮。stale 节点上组头按钮一律 `disabled` + title「节点离线」（一跳转发到不了，按了只会得到 502），本机 health 还没回来的「探测中」不算离线。
- 点组头按钮不折叠这一组：动作按钮是组头按钮的兄弟节点（按钮不能套按钮）并显式 `stopPropagation`。「shell」失败时按 `WriteResult` 的错误类型 + message 在组头下一行显示 5 s（`data-testid="tree-shell-error-<key>"`），不弹窗；POST 期间**只有发起的那个组头**禁用（在途状态按组 key 存，不是一个全局标志——同时对两个 worktree 起 shell 是正当用法，不该互相挡，更不该在途期间把别的组的点击静默吞掉；agora-x1k 修）。这层禁用只覆盖一次 POST 往返，别指望它挡连点：本机实测 22–42 ms（隔离 daemon 上连打五次 `POST /api/sessions`：22 / 22 / 23 / 28 / 42 ms，2026-09-09），比真人 100–300 ms 的双击间隔短一个量级。本机真正让人不重复点的是反馈快——`announce_created` 在 handler 返回前同步 publish（`src/api/sessions.rs`），新行几乎与 201 同时到、`pendingOpen` 立刻选中它；in-flight 禁用真正用得上的是慢链路（peer 一跳转发、手机远程），那里窗口才够长到看得见。守卫 `web/src/SidebarTree.test.tsx`（+ 预填且不折叠 / shell 只发一条并就地报错 / in-flight 期间自己禁用且第二次点击不再发 POST / in-flight 期间别的 worktree 的 shell 按钮不受影响 / stale 节点禁用）、`web/src/NewAgentDialog.test.tsx`（预填只生效一次 / initial.node 预选 peer）。
- 过滤只删不换序：过滤后只剩有行的组（组是从行推出来的，空组不存在）。
- Header 计数行的「Finished N」一键清理与视图无关：清理对象仍是 attention 折叠区的定义（`sectionOf(r, seen) === "finished"`），两种视图下按钮 title 的行数相同（守卫 `web/src/Sidebar.test.tsx`「the Finished clear count is the same in both modes」）。
- 守卫：`web/src/sidebarTreeModel.test.ts`（分组与创建序、其它目录、本机第一、状态不改序、折叠照数、过滤删空组、superseded 折在当前行下且不占顶层 / 无当前行时按普通行留在顶层 / 桶的认法 pid 优先于目录）、`web/src/SidebarTree.test.tsx`（组头分支 / 主标记 / 折叠计数、FINISHED 原位 done、折叠记忆、active 自动展开、「历史对话 N」默认收起、只展开自己那一枚、选中被折起来的历史自动展开）、`web/src/events.test.ts`（`end_cause` 随事件流就地更新、词表外读成「没说」）、`web/src/Sidebar.test.tsx`「tree mode renders SidebarTree and no section headings」、`web/src/keyboard.test.tsx`「Alt/Option+N in tree mode follows DFS order across groups」。

```
mac ● zuan ●          Agents: 12
Running 4   Needs Input 2   Turn Done 1   Finished 3   Idle 1   Unknown 1

NEEDS ATTENTION
⚠ agora-90t.4 ADR-003     ✦ Claude  @ zuan   waiting 3m
⚠ 修 migration 回滚        ◆ Codex   @ mac    waiting 1m
◆ agora-03k …              ✦ Claude  @ mac    turn done 2m
  agora / agora-03k ⎇ agora-03k
✓ agora-3la 测试骨架       ✦ Claude  @ mac    finished 9m
  agora ⎇ main
UNCLEAR
? codex-desktop-2d         ◆ Codex   @ mac    unknown 2h
  hooks silent; no process handle · 等下一条 hook，或去它自己的窗口看（满 external_unknown_ttl 自动删记录）
WORKING
● 重构 sglog parser        ◆ Codex   @ zuan
◆ agora-1qd 补一轮回归      ✦ Claude  @ mac    turn done 4h
▸ FINISHED 3
```

（WORKING 段最后那行是**看过一次**的 TURN_DONE：选中展开过一次就从中段继续待着，等下一次 TURN_DONE 换个 `status_since` 再回 NEEDS ATTENTION，MISSION §4.6 证据 ①、agora-5gg.21。它不在 `▸ FINISHED` 里——那一区的行是「Finished N」一键清理的删除对象，而这行的进程还活着。）

**计数行是状态清单，四段是动作分段**（agora-l4r3）：header 那一行数的是 `status`（Running = running + starting、Needs Input = waiting、Turn Done = turn_done、Finished、Failed、Idle、Unknown），回答"这些状态各有多少行"；四段（NEEDS ATTENTION / UNCLEAR / WORKING / FINISHED）回答"现在该看哪一段"，是按分数与"看过"派出来的动作分段。两者**本就不该相等**，也不要去对齐：`Turn Done 3` 可能两行在 NEEDS ATTENTION、一行（看过一次的）在 WORKING；`Finished 2` 可能一行没看过的留在 NEEDS ATTENTION、一行已收进折叠区；`Running 1` 与非无头的 `Idle` 可能同处 WORKING。唯一的例外是 `origin = headless` 的行（agora-2x3z）：它永远收进 FINISHED 折叠区（`finishedCollapsed` 对 headless **不看状态**），计数就跟着它被显示的那一段走——无头行不论停在 running / turn_done / unknown，都只计进 `Finished`，不再进 Running / Turn Done 等（否则 `Running N` 里有一行在侧栏哪一段都找不到）。这条口径顺带说清 `Finished N` 与 `▸ FINISHED N` 不是同一个数：前者是状态清单（含留在 NEEDS ATTENTION 里没看过的 FINISHED 行、含全部无头行），后者是折叠区里画着的行数；一键清理的名单按 `sectionOf(r, seen) === "finished"` 算，名单人数写在按钮 title 里（`清理 Finished 区里的 N 行`）。守卫 `web/src/attention.test.ts`（在跑的 headless 行不进 Running）、`web/src/Sidebar.test.tsx`（计数行与折叠区同源：Running 1 · Finished 1）。

行上的时长（`waiting 3m` / `starting ≥8h`）以**节点那只钟**为基准（agora-au5）：`GET /api/sessions` 响应顶层带报告方自己那只钟的读数 `now`（`docs/spec/api.md`「peer 视图」），页面只把「自收到快照以来走过的时间」加到它上面（`anchoredNow`），页面与节点的钟差在减法里自己抵消——手机没跟节点校时也不会把满屏时长整体平移。peer 行的 `≥` 只说明起点是并入方的下界（ADR-004），与基准无关；本机行的起点由节点打、照旧精确。

展开 `▸ FINISHED 3` 之后（收起时这三行不画，但 Alt/Option+N 的序号照数）：

```
▾ FINISHED 3
✓ 回一句 closewin          ✦ Claude @ mac external   finished 1m
✓ 回一句 ctrlc             ✦ Claude @ mac external   finished 1m
✓ own-kill                 $ shell  @ mac            finished 9m
```

主区 crumb 之下、终端之上是**回答面板**（MISSION §6.3 §7.3，`web/src/RespondPanel.tsx`；A50，agora-4yr.1，2026-09-10 从侧栏选中行下方搬来——260 px 的窄列装不下几百行的 TURN_DONE 回复，输入框还沉在全文底下，agora-03k）：WAITING 且 `reason = permission`、`respond_via = hook` → 问题文本（`pending_decision.summary`，形如 `Bash: git push origin main`——工具名 + `tool_input` 主参数的首行，截到 200 字符加 `…`；载荷没带 `tool_input` 才只剩工具名，`src/adapter/hooks.rs permission_summary`）+ Allow / Deny / 打开终端，`respond_within_secs` 短于 5 分钟（Codex 20 s）时再加一行"N 秒内没答会交回终端"；WAITING 的其它情形（`question`，或 `respond_via = terminal`）→ 只有问题文本与"打开终端"；TURN_DONE → **"下一条指令"输入框在最上面**（发 text，尾部带换行），其下是 `↳` 最后一条回复（按 markdown 排版、默认折叠到 12 行，见下一段；最多 40vh、内部滚动）。Allow / Deny 撞上 `no_pending_decision`（终端先答了 / 过期）只显示一行提示，行状态随事件自己变。

最后一条回复与 WAITING 的问题文本按 **markdown 子集**排版（**例外**：`reason = permission` 那条 `pending_decision.summary` 逐字符原样显示、等宽字体、不过 markdown——它是一行命令，人照着它批准，元字符被当语法吃掉就等于看到的与要批准的不是同一串；agora-k1s，守卫 `web/src/RespondPanel.test.tsx` 的 `a permission summary is shown verbatim`）（`web/src/markdown.ts` 出 AST、`web/src/MarkdownView.tsx` 出 React 元素；A50，agora-4yr.2，兑现 agora-03k——在此之前是 `white-space: pre-wrap` 的纯文本，标题、表格、代码块挤成一坨）。子集固定：ATX 标题 `#`–`######`（渲染成 h2–h6，递降一级，页面 h1 只有一个）、段落（单个换行 = 硬换行）、无序与有序列表（两级嵌套，缩进 ≥ 2 空格算第二级，更深的并进第二级）、围栏代码块（三个反引号或 `~~~`，原文原样、语言名进 `data-lang`、不高亮）、引用 `>`、表格（第二行必须是列数与表头相同的 `|---|` 分隔线才算表）、水平线 `---`，行内 `` `code` ``、`**bold**`、`*italic*` / `_italic_` 与 markdown 行内链接；其余一律当段落文本，不含语法高亮、任务列表 checkbox、脚注、setext 标题、反斜杠转义。词边界规则：`_` 两侧是词字符时不算强调（`pending_decision` 不会被切成斜体）。

回复是 agent 写的**不可信文本**：原始 HTML 标签当纯文本显示（`<script>` 就是屏幕上的那串字符），链接只放行 `http:` / `https:` 前缀，其余协议（`javascript:`、`data:`）连同相对路径把整段链接原样显示成文字——人看得见有人往回复里塞了什么；放行的链接带 `target="_blank"` 与 `rel="noopener noreferrer"`。前端一处 HTML 注入口都没有，守卫 `tests/arch_boundary.rs` 的 `frontend_never_uses_innerhtml`（扫 web/src 全文，注释也算）。

折叠按**源文本的行数**：最后一条回复默认只渲染前 **12 行**，超过时下面一个「展开全文（N 行）」按钮（`respond-last-more`），点开显示全文、按钮随之消失（不提供「收起」——回复是看结果的东西，看完就该给上面的输入框下一条指令）；12 行以内不出按钮。折叠态**不持久化**：换一行、同一行来了新回复、刷新页面都回到折叠。按行数截而不是 CSS 限高，是因为限高会随字号 / 表格 / 代码块飘，同一段回复在不同机器上折叠位置不一样。截断落在围栏中间时，未闭合的代码块一路渲染到结尾（不退回原文）；落在表头与分隔线之间时，表头行退回成一行普通文字。`.respond-last` 那条 40vh 限高与折叠是两件事：折叠管默认露多少，40vh 管展开之后最多占多高。守卫 `web/src/markdown.test.ts`、`web/src/RespondPanel.test.tsx`。

状态既不是 WAITING 也不是 TURN_DONE 时面板一格都不占；看 diff 时不画（那一格在看结果，不在回答；同时**留着**的看结果面板见下一段）。「打开终端」在主区里的意思是**把焦点交给下面的终端**（面板与终端本来就上下相邻，不再是切标签页）；external 会话没有终端可交（MISSION §5.5），这个按钮不画，经 hook 的 Allow / Deny 照旧。「下一条指令」输入框按 `textVia(row)` 分（MISSION §5.5、ADR-002 D11）：`runtime` 与 `host` 都画，`none`（旧扩展 / 老节点 / 本就只读的宿主）不画——70p2 当时拿掉输入是因为 external 一律 409 `no_runtime`，宿主自报输入通道后这条理由在 `host` 行上不成立了（agora-t5kf.3）；不画的那些只留 `↳` 看结果与 `respond-terminal-only` 说明。输入框里按 Escape：有终端的把焦点还给终端，宿主通道的行没有终端可还，就让它失焦。终端焦点规则（agora-p29 / agora-vcc）不变：点行仍然聚焦终端，面板只在 Alt/Option+R 与通知点击两条路径上拿焦点。守卫 `web/src/RespondPanel.test.tsx`、`web/src/Workspace.test.tsx`（crumb → 面板 → pane 的 DOM 顺序、diff 视图只藏这一个面板、通知点击与 Alt/Option+R 的落点）、`web/src/Sidebar.test.tsx`（侧栏一个 `respond-` testid 都没有）、`web/src/keys.test.ts`（Alt+R 认 code）。

```
agora-03k 侧栏展开区可读性差 / claude @ mac        [Settings] [关闭]
┌ 回答面板 ───────────────────────────────────────────────┐
│ [下一条指令                                    ] [发送] │
│ ↳ 结论先说：可以做局部性能优化…                         │
│   ## 1. 总判 …（最多 40vh，内部滚动）                   │
├ 看结果面板 ─────────────────────────────────────────────┤
│ ▾ 验收标准 · agora-03k                                  │
│ 回答面板 + 验收 + 改动三段都在主区；侧栏行里一个都没有。│
│ ▾ 改动 · agora-4yr.3                          [看 diff] │
│ M web/src/RowResult.tsx                                 │
│ M web/src/index.css                                     │
└──────────── 两个面板合计最多 50vh，超过内部滚动 ────────┘
┌ TERMINAL ───────────────────────────────────────────────┐
│                                                         │
└─────────────────────────────────────────────────────────┘
```

**主区面板从上到下**（`web/src/RowResult.tsx`；A50，agora-4yr.3，2026-09-10 从侧栏选中行下方搬来，兑现 agora-03k）：① **回答面板**（上一段）② **验收标准** ③ **改动列表**。②③ 合成紧随回答面板的一个 `<section class="result-panel" data-testid="result-panel-<id>">`——分成两个 section 而不是一个，是因为两者生命周期不同：回答面板看 WAITING / TURN_DONE，看结果面板看另一个状态集合，而且 diff 视图下只藏前者。顺序由 agora-h1k.3 定，不变：回答问题 / 给下一条指令是先做的事，对照验收看结果是后做的事。两个面板**合计**最多 50vh、超过整块内部滚动（`.panels`），终端至少还剩一半；②③ 两段各自可折叠、默认都展开；两段都没有内容时整个看结果面板一格不占。侧栏行至此只剩行按钮本身（`web/src/SessionRow.tsx`），260 px 的窄列不再承担可读性。

② **验收标准**折叠块——`task.acceptance`（`docs/spec/api.md`，读自 beads 的 acceptance_criteria、不复制进 agora 的库，不变量 12）全文、多行原样，一行 summary `▾ 验收标准 · agora-h1k.3`（`acceptance-toggle-<id>`）可折叠，默认展开（选中一行就是为了看"做完算什么"，MISSION §6.3 看结果 / A40）；没有任务或 beads 里没写不占位。③ **改动文件**（`web/src/Changes.tsx`；A41，agora-h1k.5）接在验收标准之后，两者并排对照——`GET /api/sessions/:id/changes`（`docs/spec/api.md`「只读产出」，该 worktree 的只读 `git status`）的列表 `<单字母> <path>`（M / A / D / R / C / T / U / ?，与 `git status --short` 同一习惯），一行 summary `▾ 改动 · <分支名>`（`changes-toggle-<id>`）可折叠、默认展开——**折叠只藏列表、不卸载组件**：拉取挂在 mount 的 useEffect，卸载即丢 state，收起来再打开就白白重发一次 `GET /changes`；「看 diff」按钮留在 summary 那一行、不随折叠消失。空列表一行「无改动」，`reason` 按类型一行灰字（`not_a_repo` 不是 git 仓库、`no_directory` 工作目录不存在、`no_git` 本机没有 git、`timeout` git status 超时、`git` git 失败——只按类型不按文本，MISSION §2.3 规则 10）；status 是 TURN_DONE / FINISHED / FAILED（做完了看结果）或 RUNNING（瞄一眼进度）时显示，每次 status 变化拉一次、**不轮询**，WAITING / IDLE / STARTING / UNKNOWN 不占位——此刻该做的是回答问题。旁边的「看 diff」把主区切成这一行的 diff 视图（agora-a46 之后没有标签页，主区一次只显示选中行的终端或它的 diff）：crumb 是 `git diff / <会话名>` 带「关闭 diff」、没有 Settings（它不是会话），里面是同一个 TerminalView 以 `WS /api/sessions/:id/diff` 挂的**只读**终端——在该 worktree 跑 `git --no-pager diff HEAD`，连接状态显示「只读」，键入不发也不进 PTY（两边各守一半），跑完显示退出码、按钮「重新运行」再跑一次；「关闭 diff」或再点这一行回到它的终端，点别的行则切到那一行的终端，三者都关 WS、git 进程被收走；侧栏不多一行、`GET /api/sessions` 行数不变；会话被删时主区回到空。`reason` 非空时按钮禁用（没有可 diff 的仓库）。**diff 视图下回答面板隐藏、看结果面板留着**与 diff 并排对照（MISSION §6.3；2026-09-06 定、2026-09-10 随 agora-4yr.3 复核沿用）：两个都藏会让「关闭 diff」必然重拉一次 `GET /changes`，而并排对照验收标准 / 改动列表看 diff 本来就是这一格该有的样子。在 beads 里改了验收标准，`TaskIndex` 的 TTL（5 min）到期重查后面板跟着变。守卫 `web/src/RowResult.test.tsx`、`web/src/Changes.test.tsx`（折叠不重拉）、`web/src/Sidebar.test.tsx`（侧栏 DOM 里 `respond-` / `acceptance-` / `changes-` 三类 testid 一个都没有，两种视图各一次）、`web/src/Workspace.test.tsx`（分段顺序、WAITING 行只有回答面板、diff 视图只藏回答面板且 `GET /changes` 计数不变、看 diff 一节）。

选中之后侧栏那一行**什么都不长出来**（2026-09-10 起，agora-4yr.3）——上面那两段全在主区：

```
◆ agora-h1k.3 会话行展开显示任务的验收标准 [Claude] [mac]    turn done 2m
  ❯ 做 h1k.3
  ↳ 改完了，vitest 全绿
```

点「看 diff」后主区：

```
git diff / agora-h1k.3                              [关闭 diff]
只读
diff --git a/src/session/manager.rs b/src/session/manager.rs
@@ -12,6 +12,9 @@
+    pub acceptance: Option<String>,
```

实现（`web/src/attention.ts`，agora-dvh.10）：侧栏就是 Dashboard——行按分数降序 → bd 优先级升序（`task.priority`，无 bd 视为 P2）→ `status_since` 早的在前排好，再拼成四段：NEEDS ATTENTION（分数 ≥ FINISHED，除去下面收起来的）→ UNCLEAR（说不清的行）→ WORKING（不需要人的一切 + 看过一次的 TURN_DONE）→ FINISHED 折叠区（2026-09-19 四段化，agora-5gg.11，见下一段）；过滤只删不换序、折叠只是不画不是不数，所以 Alt/Option+N 跳的第 N 条永远等于展开后眼睛看到的第 N 条（收起时第 N 条可能正在折叠区里）。第一列 `taskLabel`：`task.id + title` > `task_ref`（issue id 或首条 prompt 摘要）> 名字。header 下一行是各状态计数。

**FINISHED 分来源与「看过」**（MISSION §4.6「看过」的三条证据、§6.3 排序表；A46，agora-j4w.1；`finishedCollapsed` / `sectionOf`）：`origin = external` 的 FINISHED 一律直接进折叠区——它的工作面在别的窗口，人是在终端里自己结束的会话（证据 ②），agora 这边没有 pane 也没有 Restart，能给的只有两行摘要；不按 `reason` 分（Claude / Grok 连关窗口都发 SessionEnd、Codex 关窗口不发，分不可靠也不必要）。`origin = headless` 的行走同一个函数（`isHeadless`）但**不看状态**：它是宿主自己起的一次性会话（`claude -p` / 子代理，裁决 agora-5gg.7 选 B，实施 agora-5gg.20），停在 TURN_DONE / UNKNOWN 也直接收起，因为「等你回看」这件事在它身上不成立；它同时也进 Header「Finished N」一键清理的删除名单——一行还在跑的无头会话可以被那一键删掉记录，与它满 24 h 不论状态被 `sessions.external_finished_ttl` 自动删是同一条口径。主区那一格与 external 一样是「没有终端」（判据 `isHandleless`，`Workspace.tsx` / `RespondPanel.tsx` / `RowIdentity.tsx` 的 origin 标签同一处）。`origin = agora / adopted` 的 FINISHED 先留在 NEEDS ATTENTION，**看过**（证据 ①：在本浏览器里被选中过一次；A50 之后行不再展开，选中即把它的面板画在主区，判定不变）之后才进折叠区。「看过」是浏览器视图状态，不加服务端字段：`Workspace` 在**离开**那一行（切到别的行 / 关闭视图）的时刻记下——记在选中那一刻它会立刻掉进收起的折叠区、主区还开着它的终端而侧栏找不到这一行；离开时看它当时的状态，选中时还在跑、离开后才 FINISHED 的不算看过。集合存 `localStorage`（键 `agora.seen-finished`，读写都包 try/catch，丢了的代价只是几行回到 NEEDS ATTENTION 再看一眼）；元素是 `seenKey` = `<id>@<status_since>` 而不是裸 id（agora-23h）：记号跟着"这一次完成"走——行被删（Delete metadata）或又跑起来（Restart）记号作废，而 Restart 后 running → finished 被同一批事件（300 ms 合并窗）或断线重连的 resync 跳过中间态时，新一次 FINISHED 的 `status_since` 不同、旧键对不上，同样回到 NEEDS ATTENTION（守卫 `Workspace.test.tsx`「a seen mark dies with its completion」）。折叠区标题 `▸ FINISHED N` 是按钮（`data-testid="section-finished"`，`aria-expanded`），默认收起、刷新回到收起，选中行落进收起的折叠区（Alt/Option+N、finished 通知点击、命令面板都能从外面选中它）时自动展开一次、人仍能手动收回（agora-4nk；守卫 `Sidebar.test.tsx`「auto-expands the collapsed Finished section」），N 是折叠区里的行数。**一键清理**（A46，agora-j4w.2）：Header 计数行里的 `Finished N` 按上面那条口径（状态 finished + 全部无头行），但折叠区里有行时它是按钮（`data-testid="clear-finished"`，下划线提示）——点了弹 ConfirmDialog，写明将删的行数与其中 agora / adopted 来源的行数（它们已退出的运行时会话与输出会随 DELETE 一并清掉，MISSION §4.6「已退出的顺手清理」），确认后对折叠区里的每一行各发一次 `DELETE /api/sessions/:id`（没有批量端点，也不加：MISSION §11 不引入 Archive），NEEDS ATTENTION 里没看过的 FINISHED 行不发；清理的对象按过滤前的全部行算（过滤只是暂时少画几行，不改变哪些行「可以清」）；peer stale 的行跳过（一跳转发到不了）；跑完计数行下面一句 `已清理 N 行[，跳过 S 行（节点离线）][，失败 F 行]`（`data-testid="clear-finished-note"`，8 s 后消失）。不可逆所以要确认；不是 kill，不走 Kill 的「正在结束…」面板。守卫 `web/src/attention.test.ts`（external FINISHED 不 needsAttention、agora FINISHED 看过前后、分段拼接顺序等于 sortByAttention 顺序且与过滤可交换）、`web/src/Sidebar.test.tsx`（默认折叠带计数、展开后可选中且序号连续、NEEDS ATTENTION 无 external FINISHED、一键清理只对折叠区每行各发一次 DELETE 且 stale 行跳过 / 取消不发）、`web/src/Workspace.test.tsx`「an agora FINISHED row stays in NEEDS ATTENTION until it has been opened and left」。不做 Archive（MISSION §11）。2026-09-08 反转（agora-uvd.3）：这里以前写死侧栏一律平铺、不做分组，理由是分组会与 Alt+N 的序号打架（agora-a46 的教训）——「按项目」视图的序号按 DFS 顺序数，与折叠照数同一条规则，那个冲突不存在；「需要我」视图本身仍然平铺，分组只发生在另一种视图里。

**TURN_DONE 看过即降**（决策 agora-5gg.10 选 B 为主、A 可选，实施 agora-5gg.21；`needsAttention` / `sortByAttention` / `seenRelevant`）：上一段那套「看过」的判据 2026-09-19 起也管 TURN_DONE，两条改动。① **看过一次降到中段**：`needsAttention` 对 `turn_done` 且记号在集合里的行返回 false，于是它落进 WORKING 段（当时那段还叫 RUNNING，四段化与改名归 agora-5gg.11），不再压着没看过的新完成；新一次 TURN_DONE 的 `status_since` 不同、键对不上，又回到 NEEDS ATTENTION。**降进的是中段而不是 Finished 折叠区**——折叠区的行是 Header「Finished N」一键清理逐行发 `DELETE /api/sessions/:id` 的对象，而 TURN_DONE 那一行 pane 里的进程还活着、下一条指令随时要发；折叠区还默认收起，收进去等于看不见。记号照用 `seenKey` = `<id>@<status_since>`（同一集合直接复用，键名 `agora.seen-finished` 不改：改了会把老浏览器里已看的 FINISHED 记号全丢掉，那些行会集体回到 NEEDS ATTENTION），写入与作废的时机与上一段完全相同（离开那一行才记、行不再是 finished / turn_done 即作废，`seenRelevant` 是那唯一判据）；external 的 TURN_DONE **照记照降**——上一段对 external 的豁免只属于 FINISHED（证据 ②"人在终端里自己结束了会话"对还在跑的行不成立）。② **TURN_DONE 段内按完成时间倒序**（新完成在前），WAITING / FAILED 仍升序（等得久在前）：`sortByAttention` 的第三层方向按状态取，`turn_done` 乘 −1；同分必然同状态，取一侧的状态判方向就够。没有 `status_since` 的行（旧节点 / 测试桩）先按"不知道何时完成"排在有时刻的行之后，与方向无关——别让"不知道"冒充"最新完成"钉在段首。优先级那一层仍在时长之前：P0 的旧完成不会被 P4 的新完成压住（三层顺序一个字没改）。守卫 `web/src/attention.test.ts`（看过的 TURN_DONE 不在 attention 段、`finishedCollapsed` 仍为 false 即不可清理、新的 `status_since` 回到 attention 段、TURN_DONE 段内新完成在前而 WAITING / FAILED 等得久在前）、`web/src/Workspace.test.tsx`（选中 turn_done 行时记号不写、离开后写入 `localStorage` 且行落进 WORKING 段、新一轮 RUNNING → TURN_DONE 记号作废并回到 NEEDS ATTENTION、external 的 TURN_DONE 记号照写而 external 的 FINISHED 不写）、`web/src/Sidebar.test.tsx`（看过的 TURN_DONE 画在 WORKING 段、没有折叠区标题、一键清理的删除名单里没有它）。

**四段：NEEDS ATTENTION / UNCLEAR / WORKING / FINISHED**（MISSION §6.3；agora-5gg.11，2026-09-19；`sectionOf` / `unclearStatus` / `partitionByAttention`）：2026-09-18 Mac 截图上叫 RUNNING 的那一段 9 行没有一行在跑——UNKNOWN / STARTING / IDLE 全塞在里面，段名把一个筐说成了"在跑"。**分数表一个字没动**（FAILED 100 / WAITING 90 / TURN_DONE 85 / FINISHED 80 / UNKNOWN 40 / IDLE 30 / STARTING 20 / RUNNING 10），只改分段：`unknown` 单独成 UNCLEAR 段，`running / starting / idle` 与看过一次的 TURN_DONE 共用 WORKING 段。UNKNOWN 的分数本来就高于 IDLE / STARTING / RUNNING（§6.3「看不清，值得瞟一眼」），混在中段里只会被段名说成在跑；单独一段才配得上"这一行的状态说不清"这句话。不认识的状态名（旧节点报来的新状态）分数落到 unknown 档，同一段——`unclearStatus` 是段与行共用的唯一判据，不会出现「段里没有 reason 的行」。

段标题的规则：**标题标的是交界**——某段有行、且列表里不止它一段时才在段首画（`Sidebar.tsx` 的 `hasBoundary`）；只有一段时上面什么都没有可标，就不画标题（只有 running / idle 的列表原本也没有 RUNNING 标题，四段化不改这条）。两个例外都是既有行为：NEEDS ATTENTION 有行就画（它是段首、人打开页面找的就是它），FINISHED 的 `▸ FINISHED N` 是折叠按钮、不只是为了标交界。**UNCLEAR 段的行上一律带「为什么说不清 + 出口」**（`SessionRow.tsx` 的 `unclear-<id>`，2026-09-18 盘点的原话：UNKNOWN 必须带可执行的原因，而且必须是暂态——要么 agora 很快能弄清，要么给人一个出口）：服务端那句 `reason` 原样显示（不翻译成人的话，那已经是人写的句子，再加工只会把"说不清"遮掉一层），后面按 `origin` 接一句能做的事——有 pane 的行（agora / adopted）"选中它，打开它的终端看一眼"，external 行没有运行时句柄、没有终端可开（主区那段 `no-terminal` 的话）"等下一条 hook，或去它自己的窗口看"；没有 reason 也照样给出口，全文另放 `title`（`.row .preview` 是 nowrap，窄侧栏会裁）。UNKNOWN 行因此不再走 `lines` 那条"两者都没有时退回状态理由"的退路，同一句话不会画两遍。**出口本身的实现不在这个任务**：`unknown_cause` 封闭枚举归 5gg.6、无句柄 UNKNOWN 的 TTL 淘汰归 e08、"运行时会话没了不是 UNKNOWN 而是 FINISHED"归 u5p——这一步只管把这些行摆到看得见、且一眼知道下一步做什么的地方。守卫 `web/src/attention.test.ts`（unknown 落 unclear、running/starting/idle 落 working、Alt/Option+N 序号跨段连续）、`web/src/Sidebar.test.tsx`（四段标题文案、空段不画标题、NEEDS ATTENTION 空着时 UNCLEAR 照画）、`web/src/SessionRow.test.tsx`（reason + 出口、不重复画、别的状态不占这一行）、`web/src/stableOrder.test.ts`（冻结期间新来的 unknown 行不切断各段）。

**冻结规则**（MISSION §6.3；A51，agora-4yr.4；`web/src/stableOrder.ts`）：「需要我」视图按状态排序，状态一变行就换位——用户 2026-09-08「agent 根据状态自己变动位置，人类用户跟不上」。规则一句话：**我在看 / 在操作的时候别动；我走开了再落位；动了要让我看见**。触发条件只看指针与键盘，不看焦点（焦点常年在终端里，按焦点判会几乎永远冻着）：指针进侧栏立刻冻住顺序，离开后 **3 s** 落位；点行、敲过滤框、Alt/Option+N 与 ]/[ 都算「我在操作侧栏」，各自把那 3 s 重新计时（3 s 写死，不做配置）。冻结期间**只冻位置，不冻内容**：状态符号、`waiting 3m`、两行预览、header 计数全部照常跟着事件走；**新行照插**（插到它在实时顺序里前一个老行之后，没有就插最前），不等解冻；**不许隐藏行**——冻的是「顺序 + 分段归属」两件事，所以一行在冻结期间变成 FINISHED / 被写进「看过」时它仍留在原位、仍算原来那一段，`▸ FINISHED N` 的位置与计数一个字不变（只冻顺序的话，四段表头是按位置推的、收起的 FINISHED 行根本不渲染，那一行会当场从 DOM 消失，正是这条规则要防的）。解冻落位时，真正动了的行加一次 `li.moved`（共有 id 上按最长上升子序列求最小移动集：一行跳到最前只点亮它，被挤着挪格的不算；agora-2ef。`@keyframes row-moved`，1.2 s 背景高亮，不做滑动动画：一屏十几行同时滑动比不滑更难跟），下一次落位自然清空；纯粹的增删与过滤不点亮任何行。**序号按冻结期间的显示顺序**：Alt/Option+N、]/[ 与过滤框 Enter 用的都是同一条 `visible`，所以第 N 条永远等于眼睛看到的第 N 条（序号挂在 `<li data-ordinal>` 上，行里的 `.ord` 仍只画 1…9，agora-5ri）。**树视图不适用**：那边的行位置只随创建 / 删除变，本来就不重排（`frozen` 恒为 false，也不记冻结顺序——不然从树切回「需要我」时冻着的会是树的顺序）。守卫 `web/src/stableOrder.test.ts`、`web/src/Workspace.test.tsx`「Workspace · 重排稳定」一节。

行上的节点 chip（MISSION §3.5 "每行标明节点"；A49，agora-uvd.7；`web/src/RowIdentity.tsx`）**本机也标**：两台机常态并行，用户反馈「在本机还是 zuan 不明显」，2026-09-09 反转 agora-7ku.5（当时判断满屏 `@ mac` 是噪音——那是单机场景）。chip 带 `data-node`，peer 按节点名字符码求和映射到 8 档色板着色（`nodeHue`，相邻 45°），本机 muted 边框 + 正常文字不着色。还没拉到 `/api/system` 的 `node`（与 Header 本机那一枚同源）之前谁都不标——先满屏 chip 再把本机改成不着色更难看。极窄侧栏下 chip 拆成不可截的「@」（`.node-at`）与可截的名字（`.node-label`，block 化走省略号），节点全名放 chip 的 `title`（agora-yaf，2026-09-10：`.node` 是 `inline-flex`，`text-overflow` 在 flex 容器上不生效，合在一个 span 里只能硬裁成「@ works」）。同一行前面是 agent 品牌徽标（`agentBadge`：每个 adapter 一个 glyph + 短标签 + 色相，`data-agent`；shell / custom 不着色）。peer 断线后行带 `stale: true` 与 `last_seen`（`docs/spec/api.md`「peer 视图」；agora-7ku.6）：行**不消失**，整行淡显（`li.stale`，hover / 选中回到可读），`.meta` 里 `@ zuan` 之后多一段 `○ 上次见到 23:10`——黄点与 `clockText`（本地时区 HH:MM）都与 Header 那一枚 stale 节点同源，完整 UTC 放 title；非 stale 行没有这一段。点开 stale 行与点开别的行没有区别（建终端 WS、回答面板照常）：节点看到人碰了 stale peer 的会话就插一次重连（`docs/spec/architecture.md`「立即重试」），恢复后事件流把行刷回正常、淡显消失，浏览器不用多做任何事。

meta 之下还有一行「仓库 ⎇ 分支」（`.line-project`，`data-testid="project-<id>"`；A49，agora-uvd.8，2026-09-09；数据是 `docs/spec/api.md` 会话行的只读 `project` 字段，agora-uvd.1）：`project` 非 null → `<name> ⎇ <branch>`，linked worktree（`main = false`）在名字后加 ` / <worktree 目录最后一段>`（`agora / agora-03k ⎇ agora-03k`），detached HEAD 写字面 `detached`、不显示 commit 短 hash；`project` 为 null（不是仓库 / 目录不存在 / 没 git）但有 `working_directory` → 目录最后一段（`tmp`）；两者都没有不占位。只显示名字与分支，完整 worktree 路径（或 working_directory）放 title。**树视图不画**这一行（`RowIdentity` 的 `showProject=false`）：组头已说明仓库与分支，行上再画是噪音。守卫 `web/src/SessionRow.test.tsx`「shows a repo ⎇ branch line in attention mode」「falls back to the directory name … absent when showProject is false」「detached HEAD shows ⎇ detached」。

每行下面的两行（`❯` 用户最后输入 / `↳` agent 正在做或最后说的，MISSION §6.3；都来自 hook 的 `prompt` / `progress`，没有 hook 的会话只有一行 pane `preview`）：

```
⚠ frontend  ✦ Claude  @ zuan    waiting 3m
  ❯ 把 sidebar 的 workspace chip 换成可折叠的
  ↳ 改完了，144 个 e2e 全绿，要不要 push？
```

装了 hook 却一条事件都没收到过的会话（`hooks_unheard` 非空，`docs/spec/api.md`；Codex 未在 `/hooks` 信任是最常见的一种，agora-dvh.15）在预览下面多一行黄色 `⚠ hook 没接上：…`，全文放在 title 里；服务端判定，第一条事件到达就撤。

## 移动端交互收件箱 /m（MISSION §6.9；A37 / A52）

手机不是桌面 Dashboard 的窄版，也不是终端：它是「看谁在等我 + 当场处置」的交互收件箱（PWA 的 `start_url` = `/m`）。五屏：

```
1 门                             2 收件箱（默认）
┌──────────────────────┐         ┌──────────────────────────────┐
│ agora                │         │ 需要我 2                      │
│ 承载节点 zuan ●      │         │  ⚠ frontend ✦ Claude @ zuan │
│ [添加到主屏幕引导]   │         │    把 sidebar 换成…… waiting 3m│
│ [粘贴配对链接] [配对]│         │  ↳ Bash: git push …          │
└──────────────────────┘         │ 不用你 4   已完成 12（折叠） │
                                  └──────────────────────────────┘
3 会话卡（即时消息语法）          4 设置
┌────────────────────────────────┐   ┌──────────────────────────┐
│ ⚠ frontend ✦ Claude @ zuan     │   │ 推送 [开]                 │
│ 任务 agora-5gg · turn done 5m  │   │ 权限：已允许 / 原因       │
│                                │   │ 订阅：正常 / 已重订       │
│              ╭───────────────╮ │   │ 显示 小 标准 大 特大       │
│              │ ❯ 加一个菜单   │ │   │ 诊断 [显示边框] [复制报告] │
│              ╰───────────────╯ │   │ 前端 <构建号> [重新加载]   │
│ ╭────────────────────────────╮ │   │ 本设备：frontend-iPhone   │
│ │ ↳ 加好了，144 个 e2e 全绿、 │ │   │ [吊销本设备]              │
│ │   要不要 push？    [展开]   │ │   └──────────────────────────┘
│ ╰────────────────────────────╯ │
│ [Restart] [Kill]（更多 ▾）     │
│ ┌────────────────────────┐     │
│ │ 下一条指令…        [发送]│    │
│ └────────────────────────┘     │
└────────────────────────────────┘
5 新建（预设；agora-prdg.4）
┌────────────────────────────────┐
│ ← 新建                         │
│ ╭────────────────────────────╮ │
│ │ pktmask          ✦ Claude  │ │
│ │ PktMask · --model opus     │ │
│ ╰────────────────────────────╯ │
│ ╭────────────────────────────╮ │
│ │ run-tests            π pi  │ │
│ │ agora · 跑一遍测试并总结失败 │ │
│ ╰────────────────────────────╯ │
│ （没有预设时：一句 agora preset │
│   add … 的 CLI 指引，看不见表单）│
└────────────────────────────────┘
```

### 新建（预设）（agora-prdg.4，2026-10-08）

手机要能「开始一件事」（agora-uqpi 的拍板），但**能起什么冻结在桌面侧**：预设只在终端里用
`agora preset add <名字> --agent <agent> --dir <目录> [--args "…"] [--prompt "…"]` 定义
（S1，见 `docs/spec/config.md` 的「预设」），手机只消费。

- **入口**：收件箱 Header 的「新建」（`data-testid="mobile-new-open"`）→ 独立的「新建」屏
  （`mobile-preset-screen`）。打开时拉一次 `GET /api/presets`——只读端点，预设的增删改只在桌面
  终端，手机上改不了（被临时拿到的手机只能选已经批准过的那几条）。
- **一屏按钮**：每条按钮（`mobile-preset-<name>`）第一行名称 + agent 徽标，第二行目录**末段**
  （完整路径在 `title` 里）+ 参数摘要（`mobile-preset-args`，等宽）+ 固定首句
  （`mobile-preset-prompt`，只取第一行）。排序用节点给的顺序（`preset::list` 按名字，确定、好扫）；
  「updated_at 倒序是不是更好」是留给人的裁决，改的时候只动服务端那一处与这一句。
- **点一下直接起**：`POST /api/sessions { preset }`（节点展开成 agent / 目录 / 启动参数 / 首句，
  `display_name` 缺省是预设名）→ **不要第二段确认**——预设本身就是「预先批准」，确认与「少点几下」
  的初衷相悖。起成功不立刻关屏：201 先于 `session_created` 到达，立刻关会先闪一下收件箱；留在
  这一屏（按钮上「正在起…」、在途期间整列禁用）等新行进列表，一到就落它的卡片，可发送
  （turn_done / idle）时把焦点放进卡片现成的 composer——第一句在那儿说（可用 iOS 听写）。返回键
  不等了：会话已经起了，回收件箱点那一行就是。
- **零打字是这一屏的验收**：整屏**没有任何 `input` / `textarea`**（DOM 守卫
  `web/src/MobileApp.test.tsx`），桌面形态的那批 testid（`new-agent` / `create` / `term*` /
  `diff-*` / `acceptance-*` / `changes-*` / `mobile-stream` / `mobile-load-more` / `.xterm`）仍为 0
  ——A52 的回写只把「创建」改成「创建自由表单（只有预设按钮）」，名单一条没删。空态（没有预设）
  也是一句 CLI 指引：「还没有预设：在终端跑 `agora preset add <名字> --agent <agent> --dir <目录>`
  加一条」，同样零打字。
- **失败**：留在原屏 + 节点给的中文错误（未知预设 404 `preset_unknown` / 目录被删 400 / 起不来），
  不落卡片、不猜。

### 卡片吸顶与 composer 钉底（agora-o975.1，2026-10-08）

会话卡的滚动容器是 `.mobile-card`（`overflow-y: auto`），头与 composer 都是它的普通子元素——真机上
内容一长，头跟着滚掉、composer 要滚到底才看得见（收件箱的 `.mobile-top` 早就是 sticky，卡片这页没
跟上）。修法是两者各自吸住滚动口：`.mobile-card-head` `position: sticky; top: 0`，`.mobile-composer`
`position: sticky; bottom: 0`；两者都带不透明 `background: var(--bg)`（气泡不许从底下透出）与
`z-index: 2`（滚过来的内容不许画在它们上面），composer 的 padding 里继续吃掉 `--safe-bottom`
（主屏指示条不盖发送键）。守卫 `web/src/mobileCss.test.ts`（raw 断言：两条 sticky 规则都要在、都带
背景与 z-index）与 `web/src/MobileCard.test.tsx`（结构：头在最前、composer 与头共用同一个 scrollport）。

### 手机端的段名与状态词（agora-o975.4，2026-10-08）

四段回答的是「**要不要我管**」，不是「在不在跑」：`working` 段装的是**一切不需要你的行**——`running` /
`starting` / `idle`，以及看过一次的 `turn_done`（agora-5gg.21 的降段）。真机上用户在「需要我」里点开
一行只记了「看过」，行没在跑、agent 也没动，段名却写着「在跑」——名不副实。所以手机端的段名改成
**「不用你」**（只改 `MobileApp` 的 `OPEN_SECTIONS`，桌面四段不动），行上的状态词同时换成中文，词表只
放在 `web/src/mobileStatus.ts`（不动桌面共用的 `attention.ts` `STATUS_TEXT`）：

| 状态 | 手机端的词 | 条件 |
|---|---|---|
| `waiting` | 等你 |  |
| `waiting` 且 `reason = "permission"` | 等你批准 | 人的动作是照命令批准 / 拒绝，与“回答问题”分开说 |
| `turn_done` | 回完了 | 没看过（本设备 seen 集合里没有这一行的记号） |
| `turn_done` | 已看过 | 看过一次（`seen.has(seenKey(row))`；降进「不用你」段的是同一枚记号） |
| `running` | 在跑 |  |
| `starting` | 启动中 |  |
| `idle` | 闲着 |  |
| `finished` | 已结束 |  |
| `failed` | 失败 |  |
| `unknown` | 说不清 |  |

时长接在词后（`等你 3m`），peer 行的 `≥` 下界语义照旧（ADR-004，只换词不换口径）；认不出的状态
原样显示，不猜也不翻译。守卫 `web/src/mobileStatus.test.ts`（逐状态 + seen/unseen 两态 + permission
特例）与 `web/src/MobileApp.test.tsx`（段名与「已看过」逐字）。

### 运行动态信号与秒级时长（agora-o975.3，2026-10-08）

真机反馈第 3 条：符号是静态字符、时长 30 s 才走一格，且不到 1 分钟 `formatAgo` 返回空——刚发出去的
几十秒界面完全静止、看不出 agent 在动。三处改动都在手机端（桌面不动）：

- `running` / `starting` 行的符号带 `.mobile-symbol.live` 脉冲（收件箱列表与会话卡同一个类名、同一条
  `@keyframes mobile-pulse`）；`@media (prefers-reduced-motion: reduce)` 里 `animation: none` 关掉
  ——状态词与秒级时长还在，不动的东西也不是没信号。
- 手机端自己一份时长 formatter `mobileAgo`（`web/src/mobileStatus.ts`）：不到 60 s 显示 `42s`，60 s
  起沿用桌面共用的 `formatAgo`（`3m` / `2h` / `5d`）；负值（页面钟比节点快）夹到 `0s`，peer 行的
  `≥` 下界语义照旧。
- 会话卡在**这一行在跑（或发送在途，见下）**时用 1 s 心跳（`MobileCard` 本地加秒，父级的 `now`
  一到就对齐）；收件箱列表保持 30 s 一格——整屏每秒重排不值。

守卫 `web/src/mobileCss.test.ts`（keyframes、`.live`、reduced-motion 覆盖）、
`web/src/mobileStatus.test.ts`（`42s` / `1m` / 负值夹 0）、`web/src/MobileCard.test.tsx`（假计时器推
1 s 断言时长文本变化；不在跑的行不心跳）。

### 横屏用满宽度（agora-x70t，2026-10-08）

`/m` 的手机壳有一条 `max-width: 44rem`：它是给"桌面误开 /m"用的（居中成一条手机宽的栏、两侧各一条
边框），但同一条规则在**手机横屏**下变成了枷锁——iPhone 16 Pro 横屏是 874×402，而 44rem 的 rem 基准
是 13px（`:root` 的 `--fs-3`），于是内容被钉在 572px 居中、两侧各留 150px 空带，用户报的正是
「横屏后没有自适应横屏宽度」。规则改成：

```css
@media (orientation: landscape) and (max-height: 520px) { .mobile { max-width: none; border: 0; } }
```

判据用"矮视口"而不是单纯 `orientation`：桌面窗口也可以是横向的，而高度 ≤ 520px 基本只有手机横屏成立
（1200×800 的桌面窗口仍走 44rem 的手机列，实测 574px 居中 ✓）。横屏时安全区左右是刘海（`env()` 直接
用在左右 padding 上），上下由 `--safe-top`/`--safe-bottom` 给——判"是否全屏"时**两个维度都要比**：
横屏下 `innerHeight` 是 402 而 `screen.height` 在 iOS 上仍是竖屏的 874，只看高度必然误判成"非全屏"、
把横屏底部 21px 的 home indicator 让位丢掉（`web/src/mobileInsets.ts`，按排序后的长短边比，
两种 `screen` 口径都成立）。

### 「需要我」的新鲜度窗口（2026-10-08，agora-82x7）

「需要我」回答的是**现在**需要你，不是"曾经需要过你"。分段规则（`web/src/attention.ts`）：

- `waiting` / `failed` **不设上限**：一条挂起的权限、一次崩掉的运行，三小时后照样是现在的事；
- `turn_done` / `finished`（"等你回看"的那两种）超过 `ATTENTION_WINDOW_SECS`（12 h）就不占「需要我」——
  现场 8 行里有 6 行是 9/20 与 10/6 的旧账、最老 15 天，把今天真正的事压下去了。降段**不等于消失**：
  `turn_done` 落到「在跑」段（与"看过一次"同一个去处，agora-5gg.21），`finished` 落到折叠的「已完成」区
  并进入 Header「Finished N」一键清理的名单（名单按 `sectionOf === "finished"` 算）。手机上同理：降段是
  段位变化，行还在列表里。
- 时刻取 `peer_status_since ?? status_since`（与「看过」的键同一个口径：peer 行要用 peer 自己那只钟）；
  两个都没有的行（旧节点 / 测试桩）**不判**——"不知道何时完成的"不该被当成"很久以前完成的"。

### 「已完成」的清理（agora-off0，2026-10-08）

手机上瞟一眼觉得旧行可以清了时不必回桌面：「已完成」段头与折叠按钮同一行有 `清理 N 行`
（`data-testid="mobile-clear-finished"`，只在段里有行时出现；`N` 是**实际会删的行数**——peer 离线而
`stale` 的行不数，全是 stale 时按钮置灰并说明）。点开是二次确认（不可逆），确认后逐行
`DELETE /api/sessions/:id`：没有批量端点，与桌面 Header 的「Finished N」一键清理是同一条链、同一个
删除名单（`sectionOf(r, seen) === "finished"`，`web/src/attention.ts`）——`external` / `headless` 直接
进这段，`agora` / `adopted` 要看过（在手机上打开过卡片）才进，所以「需要我」里没看过的 FINISHED 行
天然清不到。跳过 `stale` 的 peer 行并计入结果（`已清理 N 行[，跳过 S 行（节点离线）][，失败 F 行]`，
`data-testid="mobile-clear-note"`，8 s 后消失，与桌面同一句文案）。确认框报的是**去掉 stale 后的行数**
——与桌面确认框报含 stale 的 `clearable.length`（已知小瑕）有意不同，手机不把不会删的行报进去；
「其中 N 行是 agora 起的会话」也只从实际会删的行里数。守卫 `web/src/MobileApp.test.tsx`（入口只在
有行时出现 / 逐行 DELETE / 跳过 stale / 确认框计数 / 没看过的行清不到）。

### 运行中排队发送（agora-shze，2026-10-08）

「它正跑着、我在外面想说一句」是最常见的一刻：`text_via = host`（目前只有 pi 的扩展）的行在
`running` / `starting` 时也**留 composer**，发送照走 `POST /api/sessions/:id/input`（daemon 收下就
回 200、扩展取件后写 `.done`），卡片落与 host-idle 路径同一套乐观态「已发送」。它下面那句
`mobile-host-running-note` 是「它还在跑；发出去会排队，等它跑完这一轮就交进去」——**「这一轮」是
pi 的 followUp 语义，不是"一个工具批次"**：pi 只在 agent 不再有工具调用、这一轮跑完之后才把排队的
消息交进去（`steer` 才是"当前 assistant turn 的工具批次之后"），中途不会有 `agent_settled`，也不会
再发一条 `before_agent_start`；若写成「等它做完这一步」，说的其实是 `steer` 的时机，与本实现不符
（ADR-002 D11，2026-10-08 两次真 TUI 实测 + pi 1.0.4 的 SDK 注释）。`runtime`（PTY）的行**不动**：
PTY 没有队列，键击直接落进 TUI 的输入区，各家对「跑着的时候打字」解释不同（Claude 会排队、别的可能
当快捷键），继续是 `mobile-running-note`「它还在跑，等它停下来或回完这一轮再发」；`waiting` 也沿用
状态门（只给决定按钮，不给排队）。守卫 `web/src/MobileCard.test.tsx`（host+running 开 composer 且
文案说「排队…跑完这一轮」；runtime+running 仍禁用且原文案；host+running 发送后落「已发送」）。

### 按钮上的 flex 必须自己写 align-items（agora-x70t，2026-10-08）

手机端收件箱的每一行是一个 `<button class="mobile-row">`，里面是列方向的 flex（第一行名字+状态、
第二行发信人+内容、第三行摘要）。**WebKit 的 UA 样式给 `<button>` 塞了 `align-items: flex-start`**，
而我们只写了 `display: flex; flex-direction: column` —— UA 的值生效后，列方向下每个子元素变成
"按内容自适应宽度"，一行里最长的那句话（`✦ Claude@zuan /goal 打算大幅改进…`）把整行的内容宽度撑到
1100+ 像素，再被行自己的 `overflow: hidden` 裁掉。用户看到的就是「文本被卡片外框盖住」，在设置页
打开「显示边框」则整屏黄框（被裁）；现场报告里是 `row.w = 377` 而 `row.sw = 405 / 1123 / 1229`、
子元素宽度等于各自内容宽度。

**为什么我在本地量不出来**：Playwright 的 WebKit 对同一元素的 `align-items` 计算值是 `normal`
（Chromium 也是），只有 iOS Safari 的 `<button>` 拿到 UA 的 `flex-start` —— 同一份 CSS 在两个 WebKit
上表现不同，所以"本地过一遍 WebKit"并不足以证明手机是对的（agora-c03z 那轮教训的加强版）。
复现方式：在任意 WebKit 页面上给该元素加 `align-items: flex-start`，数字立刻对上；改回 `stretch` 即恢复。

修法是显式写出 `align-items: stretch`（让 UA 值没有机会生效），并加**通用守卫**：任何一个用在
`<button className="…">` 上的类，只要有一条 CSS 规则给它 `display: flex`，那条规则必须自己写
`align-items`（`web/src/buttonFlex.test.ts`；桌面侧栏的 `.row` 早就写了 `align-items: flex-start`——它
是横排、本意如此）。同类坑以后在提交前就会红，而不是等手机上截图。

### 前端构建号与硬刷新（agora-xu12）

设置页底部那行 `前端 <构建号> · [重新加载]` 是**给"手机上跑的是不是最新界面"的一个确定答案**，
不是装饰：iOS 主屏 PWA 会把 `start_url` 钉在缓存里、从任务切换器回来常常只是"恢复旧页面"，于是
修完的界面在手机上一直不出现，而开发机上的浏览器全是新的——两边各说各话，2026-10-08 因此白排查了
两轮。判据分两半：页面构建时把构建号（UTC 时刻）烤进产物，服务端把内嵌的那份经 `/api/system` 的
`web_build` 报出来（API 版本 1.13）；两者不同时设置页出现一行「有新版本（服务端已是 …）」并指向
「重新加载」。**不提示**的两种情形：服务端没报（旧节点）或本页是 dev 构建——宁可少提示，也不能让
用户天天去点刷新（`web/src/webVersion.ts` 的保守规则）。「重新加载」会先清 Cache Storage 再用带
随机查询串的地址重新导航：同 scope 仍在 PWA 里，下次冷启回到 start_url。

顺带一条安全区规则（同一次排障的副产物）：页面不再直接写 `env(safe-area-inset-top/bottom)`，而是
`--safe-top` / `--safe-bottom` 两个变量，由 JS 判「视口是否覆盖整屏」后写值（`:root` 里仍是 `env()`
默认）。同一台 iPhone 上主屏全屏时 `inner=[402,874]`（那 62px 该让），而浏览器里 `inner=[402,812]`
（系统已经把状态栏让出去了，再让就是凭空多一条空白）——两种都在，写死哪一种都会错一半
（`web/src/mobileInsets.ts`）。

WAITING 的变体：决策原文（等宽逐字、不过 markdown）作为一张内联卡片画在 composer 上方，其下是 [Allow] [Deny]；`respond_via = terminal` 时这两个按钮换成「需要到桌面」，composer 不出现。

信息预算（「手机不显示细节」的执行口径，A52）：

- 收件箱行：状态 + 等待时长、agent 徽标、任务标签、节点、一行摘要（≤ 80 字）；没有树视图、仓库/分支行、source / confidence。
- 会话卡：任务标题；**最近一轮用两个气泡**——`❯` 你最后一句（`prompt`，首行）+ `↳` agent 最后回复（`detail`，与桌面同一 markdown 子集，默认折 6 行、**仅最后一条可展开一次**，不做更早消息的翻页）；WAITING 时决策原文（等宽逐字、不过 markdown）作为内联卡片画在 composer 上方；Restart / Kill 收进「更多」（确认框，确认逻辑在所属节点）。
- composer 与发送：固定在底部（拇指区）。**状态门**：turn_done / idle 开放文本；waiting 只给决定按钮；running / starting 上 `host` 通道**可排队**（见「运行中排队发送」），`runtime` 置灰并说明「它还在跑」。**能不能发看 `textVia(row)` 而不是有没有句柄**（agora-t5kf.3，ADR-002 D11）：`runtime`（有 PTY）与 `host`（无句柄但宿主收文本，目前是 pi 的扩展）都给 composer；`none`（Claude / Codex / Grok、旧扩展、老节点）没有 composer，并给一句说明（`mobile-terminal-only`：在跑时说“它还在跑；只能在桌面终端里回复”、其余说“没有可写的运行时”——agora-71p2，不能让它看着像输入框没画出来）。host 行在跑时 composer 仍在，其下是 `mobile-host-running-note`“它还在跑；发出去会排队，等它跑完这一轮就交进去”——排的是 pi 的 followUp，语义与实测见「运行中排队发送」。发送是**乐观的**：气泡先以「发送中」出现，`POST /api/sessions/:id/input` 成功后转「已发送」；失败时文本回填输入框（不覆盖在途时新打的字）、失败气泡上保留「重试」，失败原因贴着气泡按错误码说人话（agora-jidm）：`no_runtime` 与 `mobile-terminal-only` 同一句人话「回复要到桌面终端」、`runtime_session_not_found`（`text_via=runtime` 但 pane 没了）「这个会话的终端已经不在了；到桌面看它」、`read_only`（采纳行）「这一行只能看不能写」；`host_timeout`（504，宿主没来取件）与 `host_rejected`（502，扩展 `.failed` 的原话）把服务端 / 宿主那句话直接显示、不加壳——504 的队列里那件已被 daemon 删掉，重试不会跑两遍。
- 排版与自适应（agora-x70t，2026-10-08 在 iPhone 16 Pro 上按计算样式与几何量测定的牙）：手机壳用**一套 `--m-fs` 令牌**（`web/src/index.css` 的 `.mobile` / `.gate-mobile`），字号、间距（`--m-1..--m-5`）、触控下限（`--m-tap`）都由它派生——换一档字，行距与留白跟着变，否则字号一大人就觉得挤。基准 **17px**（Apple HIG 的 body；桌面壳仍是 13px 的 dense 工作台，两套不互相牵连），设置屏有四档 **小 15 / 标准 17 / 大 19 / 特大 22**，存 localStorage（键 `agora.mobile-text`，`data-text` 是与 CSS 的接口）；默认档就是 17px，因为"整体偏小"正是这一轮要修的。两条**平台下限是绝对的**、不跟档位缩：输入框 ≥16px（低于它 iOS 一聚焦就整页放大——"点输入框页面就放大"的根因）、可点目标 ≥44pt（Apple 触控下限）。长词与 URL 到处可折（`overflow-wrap: anywhere`；无空格长串以前会画出卡片外，量测 `.mobile-card-task` scrollWidth 2210 / clientWidth 384），收件箱行的单行省略号保留（那是设计）。**引擎差异要有硬夹断兜底**：WebKit（iOS 上唯一的引擎）对没有 `overflow`/`min-width: 0` 的 flex 项不给收缩——80 字符无空格的行名在那里盒子 1138px 直接顶出卡片框，而 Chromium 会换行、量不出来（2026-10-08，agora-c03z：Playwright WebKit 复现）；所以 `.mobile-row` 有 `overflow: hidden`、`.mobile-card` 有 `overflow-x: hidden`、行头子项有 `min-width: 0`，名字/状态再叠 `overflow-wrap: anywhere`。视口：`100dvh`（Safari 工具栏不把 composer 顶出屏幕）、四向 `env(safe-area-inset-*)`（横屏刘海）、`interactive-widget=resizes-content`（Chromium 系键盘缩内容）、`-webkit-text-size-adjust: 100%`（横屏不放大文字），composer 聚焦后 `scrollIntoView({block:"nearest"})` 兜住 iOS 键盘。守卫：`web/src/mobileCss.test.ts`（把 CSS 当数据钉上述不变式）、`mobileText.test.ts`、`MobileSettings.test.tsx` / `MobileApp.test.tsx`（档位读写与 `data-text`）、`MobileCard.test.tsx`（聚焦滚动、失败草稿回填与四类失败文案）。
- 不做：终端（xterm 不加载）、New Agent 自由表单（「新建」= 预设一屏，见上）、diff / 验收 / 改动列表、命令面板与快捷键、多节点切换（只配一个承载节点）、消息流与完整对话历史（Conversation indexing 见 §11，承接 `agora-ghl3`）。
- `respond_via = terminal`（Grok 权限、AskUserQuestion）显示「需要到桌面」，不提供「打开终端」；不注入键击（MISSION §1.2）。

行为：推送点击进 `/m?session=<node>:<id>` 并定位该行（PWA 已开则聚焦；可发送的行把焦点放进 composer）；「看过」沿用每设备 localStorage（与桌面不共享，v1 接受）；承载节点离线时门页显示不可达与上次同步时间，不做离线动作队列。守卫：web/src/Mobile*.test.tsx（新建）与 A52 的 DOM 断言（不含终端 / 创建自由表单（只有预设按钮，整屏无 input / textarea）/ diff / 验收 / 改动列表的 testid）。

## 设计令牌（全局一致性；agora-74nf）

两壳（桌面 13px dense 工作台 / 手机 `--m-fs` 派生）共用同一批**语义令牌**，定义只在 `index.css` 的 `:root` 里，别处一律 `var()`——散着写的字面量正是"手机改了字号、桌面没人比对"的来路（守卫 `web/src/designTokens.test.ts`：`:root` 外不许出现 hex、px 字号、px 圆角，引用的令牌必须都有定义）。

| 组 | 令牌 | 值 / 用途 |
|---|---|---|
| 语义色 | `--bg` `--panel` `--panel-2` `--fg` `--muted` `--accent` `--border` `--warn` `--ok` `--bad` | 深色底、两级面板、正文/次要文字、强调蓝、边框、警示/成功/失败 |
| 状态底 | `--sel` `--danger-border` `--warn-bg` `--flash` | 选中/高亮底（侧栏行、命令面板、手机行）、danger 边框、degraded 横幅底、换位闪一下 |
| 圆角 | `--r-1..--r-4` = 3 / 4 / 6 / 10px | 徽标 chip 一级小件、按钮/输入、对话框/面板、气泡级大件 |
| 字号（桌面） | `--fs-1..--fs-4` = 11 / 12 / 13 / 14px | 面板里的注释、meta、正文（`:root` 基准）、页面标题 |
| 字号（手机） | `--m-fs` + `--m-fs-mini/-note`、`--m-1..--m-5`、`--m-tap` | 见上一节：一个旋钮派生全屏，四档 15/17/19/22 |
| 间距 | `--s-1..--s-7` = 2 / 4 / 6 / 8 / 10 / 12 / 16px | 新代码从这里取；老代码按语义逐步换 |

两条跨壳约定：**同一控件同一种语法**（选中态一律 `aria-pressed="true"` + accent 边框，如桌面 `.sidebar-mode` 与手机字号档位；danger 一律 `button.danger`）；**同一语义同一个令牌**（`--sel` 管所有"这一条被选中"的底，手机 `.mobile-row.selected` 与桌面 `.row.selected` 同色）。

## 浏览器通知（MISSION §6.6；A18）

`web/src/notify.ts`（agora-dvh.11）。该不该发是服务端的事（`notification` 事件只在 RUNNING 或 IDLE → WAITING / TURN_DONE / FINISHED / FAILED 上来，`docs/spec/api.md`）；前端只管权限、弹、点击：

- 权限问一次：`Notification.permission` 还是 `default` 时主区顶部有一条"agent 需要你时弹浏览器通知？ [允许通知] [以后再说]"，答过（granted / denied）或点了"以后再说"就没了，之后不再弹权限框；denied 时通知静默丢掉。
- 弹：同一会话在通知中心只占一格（弹新的先 close 旧的；tag 每条唯一——macOS 上同 tag 替换只静默更新不弹横幅，2026-09-04 人眼验收实测）。
- 点击：窗口拉到前面，该行成为侧栏 active 行——主区 crumb 之下画出它的回答面板（Allow / Deny / 下一条指令），焦点直接落进面板（TURN_DONE 是输入框、WAITING 是第一个按钮；A50，agora-4yr.1），不是把人扔进终端。

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
Worktree: [ — ▼ / main / 新建… ]  # 首项 — = 没选中任何 worktree（cwd 落回 Project 那个目录）；新建 = git worktree add，base 默认主 worktree 当前分支；合并/销毁归人（MISSION §6.4）
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
出现名字输入框（默认取 Name 栏的值；issue id 默认随 A43；与已有 worktree 的目录名或分支同名则换成
`name-2`、`-3`…——没选任务时 Name = 项目名，几乎总会和主 worktree 目录撞，原样 POST 是 409
`worktree_exists`，agora-h5x）与「建」按钮，`POST /api/projects/worktrees`
成功后重拉列表并选中新项，失败按错误类型给文案（同名 worktree / 分支 / 目录已存在、名字不合法、git
失败）；选主 worktree 等于选仓库本身；下拉**第一项固定是空项 `—`**，意思是「没选中任何 worktree」、cwd 落回
Project 那个目录（agora-tl5：这一项以前只在列表为空时渲染，列表非空而内部状态是空串时 select 没有匹配项、
DOM 显示成列表第一项「main」，显示值与内部状态不是一回事；不写成「仓库根目录」是因为项目本身可以是一个
linked worktree，那时它不是仓库根。守卫 `web/src/WorktreeSelect.test.tsx`）；Agent 的名字与默认命令来自 `GET /api/agents`，
末尾多一项 `custom`——它没有 Adapter，Command 必填。Name 与 Command 有默认值，用户手改过之后
换项目 / 换 agent 不再覆盖。

**Task 从就绪任务选**（MISSION §6.4「从就绪任务起会话」；A43，agora-h1k.2；`web/src/NewAgentDialog.tsx`）：
选了项目就与 worktrees 并行拉 `GET /api/projects/tasks?path=`（该仓库 `bd ready --json`，epic 已滤掉）。
有任务时 Task 是下拉（`<select id="na-task">`）：第一项「一句话…」（选它时下面出现原来的自由文本框
`na-task-text`），其余每项 `<id> · <title>`，顺序照 bd 给的。选中一个任务 → `task_ref` = issue id、
Name = issue 标题（用户手改过 Name 就不覆盖，沿用上面的规则）、Worktree「新建…」的默认名 = issue id
（这就是 A44 留给 A43 的入口；没选任务时仍是 Name；撞名时 WorktreeSelect 再换成 -2，agora-h5x）、Prompt 框（`na-prompt`，只在所选 agent 的
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

未注册 / 未识别的会话在侧栏已登记列表之下单列一段（`UNREGISTERED n`，过滤时不显示）。这一段上面可能有一行灰字说明："有 N 个 socket 这次没扫到（<socket 名>）——未登记列表可能不完整"（快照的 `unregistered_unreadable` 非空时，agora-ebfa；此时空态也不再说"还没有会话"——两句话自相矛盾）。每项显示为：

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
