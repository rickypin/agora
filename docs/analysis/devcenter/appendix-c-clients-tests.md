# 附录 C：三端客户端与测试体系评审

> 对象：`~/code/devcenter` v0.5.1（commit `1540ac4`，2026-08-28）。
> 范围：`web/`（React + xterm.js + Playwright）、`mac/`（SwiftUI + SwiftTerm）、`harmony/`（ArkTS + 内嵌 xterm.js）。
> 本附录是分领域评审的原始结论，综合评价见 [README.md](README.md)。行号以上述 commit 为准。

## 1. Web 客户端架构

**状态管理**：无状态库、无路由。`web/src/app/App.tsx` 1129 行，单组件持有约 30 个 `useState`（`App.tsx:93-184`），`Sidebar` 通过约 35 个 props/回调接线（`web/src/components/Sidebar.tsx:11-83`）。纯逻辑被抽到 `web/src/sessions/{sort,filter,tree,recap,workspace,state}.ts`（共 752 行），这是架构里最健康的部分。

**事件流合并**：没有合并。`setInterval(refresh, 3000)`（`App.tsx:209-212`，注释还写着 "Phase 2 replaces this with /api/events"，但并未替换），同时 `useEvents` 的每条 status 事件都再触发一次完整 `refresh()`（`App.tsx:328-332`）。对比 Mac：`AppModel.swift:157-176` 的 `apply()` 就地 patch 单行并把未知 key 合并成 300ms 一次的 `scheduleRefresh`（`:178-188`），注释直言"第一版每个事件都重拉，UI 永远不安定"；HarmonyOS 同样 `apply`+coalesce（`harmony/.../store/AppModel.ets:333` 起），轮询 10s。Web 是三端里唯一没修的。

**乐观更新**：刻意不做，`highlight()`（`App.tsx:600-610`）与 Mac `setHighlighted`（`AppModel.swift:687-698`）都以"轮询是唯一真相"为由写后重拉。

**视图结构**：`activeKey===null` 显示 Dashboard；`open[]` 中所有 Terminal/Viewer/Browser pane 全部挂载、仅隐藏，以保住 WS 与 scrollback；重启通过 key `${key}#${reattach}` 强制重挂（`App.tsx:997`）。打开的 pane 不持久化，刷新页面即回到 Dashboard（只持久化 expanded/sort/fontSize/width）。窄屏 `matchMedia('(max-width:700px)')` 切抽屉（`App.tsx:81-90`）。

**终端组件**（`web/src/terminal/TerminalView.tsx`）：xterm `scrollback:10000`、固定暗色主题；连接前按键进 4096 字节 `pending` 缓冲，open 后先 resize 再 flush（`:169-184`）；`ResizeObserver` 只在 `offsetParent!==null` 时 fit；OSC 52 写入走 `writeClipboard`，Safari 失败时给 toast + Copy 按钮，`'?'` 读请求永远不回应；链接用 capture 阶段 `mousedown/mouseup` 吞掉 xterm 的选区行为（`:277-298`），⌘-click 开新标签，普通点击走 `api.createTab` 开 viewer（`App.tsx:489-518`）；`SpecialKeys.tsx` 按钮 `tabIndex=-1`+`preventDefault` 保住终端焦点；触摸拖动合成 `WheelEvent`。**缺陷**：`ws.onclose` 只设 `detached`（`:196-198`），无心跳、无自动重连——ADR-005 定义了 ping/pong 但 Web 不用；`JSON.parse(ev.data)` 无 try（`:186`）。

**多会话性能**：`sessionRows()` 每次渲染重算（`App.tsx:340`），其后的 `useMemo([liveRows, heldOrder])`（`:352`）因 `liveRows` 每次都是新数组而失效；每次轮询替换 `sessions` 数组导致 App/Sidebar 全量重渲。几十个会话没问题，真正的瓶颈是"每个事件一次 `/api/sessions`"的请求风暴，且不检查 `document.hidden`。

**协议耦合**：`client.ts`（734 行）手写全部类型；错误靠字符串匹配：`String(err).startsWith('Error: 401')`（`App.tsx:192`）、`'no tmux session'`（`:644`）、`'not running'`（`:674`）；`MAX_TAGS=16` 注释 "Mirrors session::MAX_TAGS in the daemon"（`workspace.ts`）。终端 WS 帧无 TS 类型，浏览器 pane 反而有 `protocol.ts`。

## 2. 三端策略评价

**存在理由**：`mac/README.md` 给出的是"终端是你整天盯着的东西，SwiftTerm 用 CoreText 渲染、⌘K 归我们、~95MB RSS"；HarmonyOS 因无原生终端模拟器，把 xterm.js 塞进 ArkUI `Web` 组件，ArkTS 负责 socket/鉴权/重连（`harmony/.../terminal/TerminalSurface.ets`）。

**重复量化**：web src 7414 行 ts/tsx + 2263 行 css；mac 5972 行 Swift；harmony 10373 行 ets。会话领域逻辑：web 752 行 / mac ≈990 行（`Models.swift` 905 + `SessionSort.swift` 85 + `AppModel.swift:580-719` 的 rows/filter）/ harmony 1309 行 + `Models.ets` 498。排序写了三遍：`sort.ts` 4 种模式；`SessionSort.swift:10-15` 只有 host/recent/name，默认 host；`Sort.ets` 手写 `compareNatural` 以避免 locale 漂移。fuzzy 评分三份实现。树结构：`tree.ts` 361 行有环检测/上下文行/host run，Mac `AppModel.swift:615-648` 是简化版（无环检测，子节点按 name 排而非 created_at → ⌘N 与 Web 的 ⌥N 指向不同行）。

**共享层**：仅 `web/src/terminal/{clipboard,linkProvider,links}.ts` 被 `harmony/terminal-host/src/main.ts:20-22` 直接 import（esbuild 打包）。其余靠 "Mirrors web/src/..." 注释人肉同步（`AppModel.swift:677`、`ContentView.swift` 的 `durationSince`）。

**已出现的漂移**：WAITING 图标 Mac `◆` vs Web `⚠`；`durationSince` Web `3h5m` vs Harmony `3h05m`；Mac Dashboard 分组内按 `.recent` 而非 attention 排序；Mac 没有 attention 排序，README 理由是"hub 不发 waiting since"，但 `Models.swift:72-73,100` 明明解码了 `since_epoch_secs`；重命名种子 Mac 用 `displayName`、Web 用 `nameOf`；Mac viewer 把 HTML 当源码显示；Harmony 重命名走 PUT 别名；Mac `killGroup` 丢弃错误文本（`APIClient.swift:247-251`）。

**评价**：HarmonyOS 内嵌 web 终端是合理的——`WebTerminalSurface.ets` 做了 16ms 批量写入、1MB 上限 + overflow 提示，`TerminalConnection.ets:38-40` 有 20s 心跳/50s 判死/指数退避（`:306-329`），比 Web 自身的终端连接更健壮。代价在于领域模型第三次手工移植。Mac 原生的收益（渲染、IME、菜单）真实，但 6k 行加一份模型副本的维护成本已经以功能不对齐的形式兑现。

## 3. UX 设计落地程度

- **Attention Dashboard**：四组（NEEDS ATTENTION/RUNNING/FINISHED/IDLE）按 `byAttention` 排（`Dashboard.tsx`）；分数表 + 等待分钟数（`state.ts:68-76`），MISSION 要求的"最近状态变化 / 未读通知"加权未实现；Mac 组内按 recent。
- **Recap 两行**：`recap.ts` `hintOf` 规则三端一致，Mac `DashboardRow`、Harmony `Dashboard.ets`（`:206-210` 每行调 4 次 `hintOf`）都渲染 ❯/↳。
- **Command Palette**：Sessions/Actions/快速启动 "New X in Y"（`App.tsx:822-877`）有，MISSION 的 Projects 组没有；Mac 缺快速启动项。
- **快捷键**：`App.tsx:411-466` 用 `e.code` 处理 Alt+1..9、mod+K/F、mod+Shift+]/[、字号；Mac 用 `CommandMenu` ⌘1..9/⌘W/⌘F（`App.swift`）。MISSION 表中 Cmd/Ctrl+N 在 Web keydown 中未见。
- **通知**：`useEvents.ts` `shouldNotify` 覆盖 WAITING/FINISHED/FAILED，点击回跳；Mac `Notifier.swift` 3s 预热 + FAILED 用 critical 音。
- **Workspace**：tag 驱动，chips 仅在有 tag 时出现，选择刻意不持久化（`AppModel.swift:513-519`）。
- **Session tabs / viewer / browser**：`SessionTabs.tsx` 成员 <2 时返回 null；viewer 走 `<iframe sandbox="allow-popups ...">` 且无脚本，检测 SPA 空壳并提示 `ssh -L`；browser pane 用 JPEG 帧 + 坐标分数转发，隐藏时发 `stop`。
- **多设备**：仅 takeover（`detached/taken_over`），V2 controller/viewer 角色未做。

## 4. 测试体系评价

**E2E**：`playwright.config.ts:13-15` 单 worker、60s 超时、`webServer` 指向 `e2e/start-server.sh`；脚本 `cargo build` 后启动两个真实 daemon（node2 用独立 HOME、7898；hub 7899），专用 tmux socket，每次删库（`start-server.sh:23-36`）。稳定性手段：`sidebarItem` 用 filter+hasText，`focusTerminal` 轮询 activeElement，`echo mark$((1000+337))` 避免命令回显误匹配。24 个 spec 约 170 个 `test(`，tree 34、workspace 13 占大头，move 只有 1 个（却是双节点存在的理由）。**关键缺口**：所有交互测试用 `shell` agent；`scripts/fake-agent` + `src/agent/fake.rs` 在 `web/e2e` 里零引用，WAITING/FAILED 驱动的 Dashboard/通知流程没有浏览器级验证。纯函数单测寄生在 Playwright spec 中（`filter.spec.ts:7-9` 自承"唯一 runner"）。`perf.spec.ts` 用 <1000ms/<150ms 墙钟断言，CI 上必 flaky。

**Mac**：94 个 `@Test`，`ModelTests` 54 个纯逻辑；`PaletteTests.swift:12-31` 把 `fuzzy.ts` 跑出来的数值钉死；`LiveTerminalTests` 无 `DEVCENTER_TEST_HUB` 时静默 return（不是 skip）；`SnapshotTests.swift:22-29` 因 ImageRenderer 画不出 ScrollView 而不快照 sidebar/palette，真正的 UI 验证靠需要屏幕录制权限的 `uitest.sh`。

**HarmonyOS**：`test.sh` 用 esbuild `--loader:.ets=ts`，所有 `@kit.*` 换成抛错 Proxy，118 个 `it(` 全是纯逻辑；`Fuzzy.test.ets:26-36` 与 Mac 钉同一组数。ArkUI/Web/WebSocket 层完全无自动化。

**可信度**：领域逻辑（tree/sort/fuzzy）三端互钉，高；Web 交互流程在真 daemon + 真 tmux 上跑，中高；agent 状态驱动的 UX 与两个原生端的渲染，低。

## 5. 工程质量

**分解**：每端都有一个god file——`App.tsx` 1129、`Sidebar.tsx` 933、`SidebarView.swift` 865、`Index.ets` 1161、`AppModel.ets` 1117。

**类型安全**：TS strict 开启，但终端帧 `any`，Mac 请求体是 `[String: Any]`，服务端错误只有字符串。

**异味**：过期注释（`App.tsx:209-212`）；无 ESLint 却有 `eslint-disable`（`BrowserView.tsx:163,182`）；`BRIDGE_METHODS` 需三处手工对齐；`rows(for host:)` 参数被 `_ = host` 丢掉（`AppModel.swift:616`）；`TerminalConnection.swift:43-56` 在 `task.resume()` 后立即 `state = .attached`，尚未握手就显示已连接。

**具体潜在 bug**：
1. `App.tsx:262-276` 的 effect 以 `loaded.length === tab_count` 为退出条件并依赖 `tabs`；`loadTabs` 失败时写 `[]`（`:237`）。stale host 的 `tab_count>0` 而 `listTabs` 持续 404/500 时，每次 `setTabs` 产生新对象 → effect 再跑 → 无限请求循环。
2. `useEvents.ts:29-31` 固定 2s 重连无退避，叠加每事件一次 refresh，hub 短暂抖动会被客户端放大。
3. `useEvents.ts:60-63` 权限为 `default` 时调用 `requestPermission()` 后直接 return，首条通知丢失。
4. `client.ts:540` `deleteTab` 的 `tabId` 未 `encodeURIComponent`（其他路径都编了）。
5. `state.ts:68-76` 比较器内部调用 `Date.now()`，排序中途跨分钟边界可能不满足传递性；Harmony 把 `now` 作为参数传入是正确做法。
6. Web/Mac 终端连接断开后不重连（`TerminalView.tsx:196-198`、`TerminalConnection.swift:70-78`），用户必须手点 "Take it back"。

## 6. 借鉴与规避

**值得借鉴**
1. 扁平行列表同时驱动渲染与 ⌥N/⌘N 索引（`tree.ts` `SidebarRow`；`AppModel.swift:610-614`），杜绝"两套顺序"。
2. 指针停在行操作区时冻结排序（`tree.ts` `holdRowOrder` + `Sidebar` `onHoldOrder`），解决实时重排把按钮从鼠标下抽走的问题。
3. 传输/模拟器分离的接缝（`TerminalSurface.ets`）：ArkTS 持有 socket，页面可被回收；`WebTerminalSurface.ets` 的 16ms 批写 + 1MB 上限 + 溢出提示；`TerminalConnection.ets:38-40,306-329` 的心跳/判死/退避/前台 probe。
4. 事件就地 patch + 300ms 合并重同步 + 相等数组不发布（`AppModel.swift:124-188`）。
5. 跨端"金数据"钉死（`PaletteTests.swift:12-31`、`Fuzzy.test.ets:26-36`），应扩展到 sort/tree/recap。
6. E2E 用真 daemon + 真 tmux + 隔离 socket/db/HOME + 双节点（`start-server.sh`），并用 `mark$((1000+337))` 之类确定性标记。
7. 终端安全姿态三端一致：OSC52 读一律拒绝（`clipboard.ts`、`TerminalPane.swift:137-146`），路径只在显式点击时打开且交给文件所在节点（`AppModel.swift:351-354`），viewer 无脚本沙箱。
8. UI 微规则抽成纯函数并测试：`renameShouldSend`、`restartWillKill`、`sessionKey`/`viewerKey` 含 `#` 的 tmux 名用例。
9. "会解释自己"的状态：折叠组显示 hiddenCount、溢出横幅、粘贴被拒说明、SPA 空壳提示。

**应避免**
1. god file + 30 个 useState + 35 个 props；新项目从第一天用 store 按 feature 切片。
2. 轮询叠加"每事件全量刷新"（`App.tsx:209-212,328-332`）——同仓库的 Mac 已经写明这是错的。
3. 靠服务端错误文本做分支（`App.tsx:192,644,674`、`AppModel.swift:445`），应返回结构化错误码。
4. 三份手工移植的领域模型只用 "Mirrors ..." 注释维系；应共享 TS 核心（web+harmony 已证明 esbuild 可行）或从 Rust 生成类型。
5. 终端有 ping/pong 协议却不实现心跳与重连。
6. 把单元测试塞进 Playwright（`filter.spec.ts:7-9`）；加 vitest 成本远低于 60s 超时的纯函数测试。
7. E2E 墙钟性能断言（`perf.spec.ts`）。
8. 环境变量门控却静默通过的测试（`LiveTerminalTests`、`SnapshotTests.swift:12`），应记录为 skip。
9. 依赖自身写入状态、以长度相等为退出条件的 effect（`App.tsx:262-276`）；用显式 per-key 加载状态机替代。
10. 功能不对称无追踪（Mac 无 attention 排序且 README 理由已过期）——维护一张三端 parity 矩阵并纳入测试。
