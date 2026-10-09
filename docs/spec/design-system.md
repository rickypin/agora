# agora 设计系统

实现基线：agora-p7p0，2026-10-09。产品边界来自 [MISSION](../../MISSION.md) 的 §1、§2.2、§6；页面行为与键位见 [UX 规格](ux.md)。本文件是后续 agent 的设计入口，`web/src/index.css` 的 `:root` 是 token 值的唯一来源。

## 从任务推导设计

人的注意力有限；几十个会话里，首要问题是「谁需要我，下一步做什么」。因此界面按 **任务 → 当前状态 → 可执行动作 → 环境身份** 分层。默认首页给出待处理事项；侧栏负责切换；选中后主区负责阅读、回应与终端操作。禁止为了填满页面加入成本、虚构进度或分析图表。

手机与桌面共享状态、颜色、字体家族、反馈与危险操作语义。设备差异只影响容量和输入方式：桌面保留项目树、快捷键、创建表单、终端与只读 diff；手机继续用收件箱、触控导航、预设创建与会话回复。不得把手机的大卡片逐张塞进桌面侧栏，也不得把桌面的密度缩到手机上。

状态来源遵循 [ADR-002](../adr/ADR-002-state-source-layering.md)，呈现层不推测 agent 状态；节点与 stale 遵循 [ADR-004](../adr/ADR-004-node-topology.md)，离线会话仍可见；危险动作遵循 [ADR-003](../adr/ADR-003-node-authentication.md)，确认不因视觉重设计而被省略。运行时与终端所有权遵循 [ADR-001](../adr/ADR-001-runtime.md)，切换或返回概览只 detach。

## Token 契约

不再另设 mobile/desktop 两套配色。组件使用语义名；基础档位只用于组合角色。

| 角色 | CSS token | 用途 |
|---|---|---|
| 页面 / 面板 / 浮起表面 | `--bg` / `--panel` / `--panel-2` | 墨绿底、分层表面；避免每块内容都加卡片 |
| 正文 / 辅助文字 | `--fg` / `--muted` | 通过层级而非任意 opacity 降低存在感 |
| 主动作 / 动作上的字 | `--accent` / `--on-accent` | 薄荷色填充，深色文字 |
| 待回应 / 正常运行 / 失败 | `--warn` / `--ok` / `--bad` | 琥珀 / 柔绿 / 珊瑚；始终配文字或符号 |
| 结构边界 / 输入边界 | `--border` / `--control-border` | 低噪音分隔线与可辨认的控件边界分开 |
| 选中 / 焦点 | `--sel` / `--focus` | 选中底与键盘 outline 是两种独立状态 |
| 危险边 / 警示底 / 换位提示 | `--danger-border` / `--warn-bg` / `--flash` | 保持危险、降级和位置变化的区别 |
| UI / 代码字体 | `--font-ui` / `--font-mono` | UI 保留 PingFang SC 回退；命令与终端等宽 |
| 正文 / 标签 / 标题 | `--text-body` / `--text-label` / `--text-title` | 桌面 14 / 12 / 24px；根基准 13px 保留既有 rem 布局 |
| 行高 | `--line-body` / `--line-compact` | 阅读 1.6；紧凑身份 1.4 |
| 圆角 | `--r-1..--r-4`、`--r-card`、`--r-pill` | 4 / 8 / 12 / 16px、18px 卡片、胶囊；按结构角色选择 |
| 间距 | `--s-1..--s-10` | 2 / 4 / 6 / 8 / 10 / 12 / 16 / 24 / 32 / 48px；控件内 8–12、区块内 16–24、区块间 24–32 |
| 命中尺寸 | `--control-height` / `--control-touch` | 桌面通常至少 32px；粗指针至少 44px；文本内联操作允许独立排版 |
| 阅读 / 内容宽度 | `--layout-reading` / `--layout-content` | 76ch 的回复阅读上限；1080px 的概览内容上限 |
| 侧栏 | `--layout-sidebar` | 默认 320px，与 `sidebarWidth.ts` 的 DEFAULT 保持一致；保留既有用户拖拽宽度 |
| 终端 | `--terminal-bg` / `--terminal-fg` / `--terminal-font-size` | xterm 经 computed style 消费；不在 TS 另写主题值 |
| 遮罩 / 阴影 | `--overlay` / `--shadow-dialog` | 对话框与浮层；普通会话行不用阴影 |

手机保留 `--m-fs` 四档 15/17/19/22，`--m-fs-mini`、`--m-fs-note`、`--m-1..--m-5` 随字号派生；`--m-tap` 不低于 44px，输入不低于 16px。安全区、视口高度、断点属于设备约束，不为「数字全进 token」而给每个尺寸起名字。`--sidebar-w`、`--hue` 与安全区变量属于运行时输入，不是新主题。

## 组件规则

- **页面骨架**：固定顶部工具区；侧栏列表与主区各自滚动，底部设备入口可达。宽度小于 700px 沿用手机入口引导，700–1050px 收紧留白并让标题/动作换行，不把终端挤出视口。
- **注意力概览**：复用 `sectionOf`、已读记号、新鲜度窗口与排序；运行中、已读完成和 headless 不凭视觉逻辑抬到待处理卡片。没有事项时给出平静的空态和创建/搜索入口。卡片只调用现有 `onOpen`，不创建第二套选中状态。
- **会话行**：任务标题第一；状态、agent 与节点是辨认锚点；项目/分支和活动为辅助信息。选中使用背景 + 边界 + `aria-current`，不能只改变文字颜色。状态不能被截断，长任务在自己的盒子里省略，完整信息在主区可读。树的位置不因状态变化而移动。
- **主区**：任务标题与当前状态位于回应面板之前。请求显示原始命令，批准与拒绝并列；批准/发送是主动作，拒绝保留 danger。下一条指令在最新回复之前（既有用户约定）；回复可读宽度与行高有约束。结果与回复两块总高度仍不超过 50vh；短视口下先收缩为可滚动面板，终端至少保留 `--layout-terminal-min`（180px）与 30dvh 中较小的高度。
- **操作**：用明确动词；创建/批准/发送用 `.primary`，普通动作中性，破坏性动作 `.danger`。同一操作组只突出下一步。页头的「返回工作台」与 Kill 严格区分。设置为主区内浮层，窄窗口不再用固定右栏挤压终端。
- **焦点与反馈**：共享 `:focus-visible`，切勿全局 outline:none；hover、selected、focus、disabled、busy、error 各表达自己的事实。支持 reduced-motion；不能用动画代替状态文字。创建、命令、设备与危险确认共用 `useDialogFocus`，Tab 保持在当前弹窗，Escape 关闭并恢复触发控件，嵌套时只响应最上层。
- **状态语言**：两端共用 `mobileStatus.ts`（历史文件名保留），已读由 `seenKey` 判据传入，列表用分钟粒度、手机详情用秒。技术名称（agent、worktree、原始命令）不强行改写。

## Agent 实施规则与验证

修改 UI 前先说明该变化改善 launch / observe / switch / respond / resume 哪一步。优先复用组件与语义 token；发现缺少角色才增加 token，并同时补表与守卫。禁止壳内覆写基础语义色、组件内写十六进制/RGB 色、TS 内另造终端主题。QR 黑白模块与 agent/node 稳定身份色是功能性例外；状态色不允许由身份色覆盖。

旧 CSS 的历史几何微调不批量机械替换。新布局间距用档位，新增例外要在注释写原因与实测反例。不要修改排序、已读、审批 request_id、发送结果或生命周期来配合静态样稿。

`designTokens.test.ts` 守卫 token 定义/引用、壳内分叉、基础色对比度和终端消费；`DesktopOverview.test.tsx` 守卫首页集合、选择回调和新鲜度；现有 Workspace / Sidebar / mobile / terminal 测试继续守协议与焦点。浏览器验收至少覆盖 1440×1000、1024×768、700×700、手机 402×874 和 320×640（含特大字号），检查真实页面宽度、内部滚动、长任务/命令、键盘焦点、创建弹窗、待回应与回复页。

对比度目标采用 [WCAG 普通文字 4.5:1](https://www.w3.org/WAI/WCAG22/Understanding/contrast-minimum.html)；键盘控件保留[可见焦点](https://www.w3.org/WAI/WCAG22/Understanding/focus-visible.html)。32/44px 是本项目密度/触控选择，不能把它冒称为 [WCAG 2.2 的 24px 最低目标尺寸](https://www.w3.org/WAI/WCAG22/Understanding/target-size-minimum.html)。token 守卫不能证明整站无障碍合规，透明度、身份色、浏览器原生控件还需实际检查。
