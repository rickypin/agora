# agora — ROADMAP（视图）

> **由 `scripts/roadmap-view.sh` 生成，不要手改。** 真相源是 beads：阶段 = epic，阶段门 = epic 之间的 `blocks` 依赖，验收标准 = epic 的 `--acceptance`，演示剧本 = epic 的 `--design`（下方"演示剧本"一节）。
> 本文件不放任务 checkbox（避免 devcenter 式双轨，见 `docs/analysis/beads/README.md` §6.3 / §8.2）。任务级细节：`bd ready`、`bd dep tree <epic>`。
> 生成时间：2026-09-07

| 阶段 | epic | 目标 | 阶段门（被谁阻塞） | 验收要点 | 状态 / 进度 |
|---|---|---|---|---|---|
| M0 | `agora-90t` | MISSION 定稿与首批 ADR | — | MISSION.md 各节无 TODO、候选标记全部清除；ADR-001/002/003 状态 Accepted 且 docs/adr/README.md 索引更新；M1a/M1b/M2/M3 epic 已在 beads 建立（agora-xqa / agora-dvh / agora-7ku / agora-h1k）并带阶段门依赖（M1a→M1b→{M2∥M3}），验收引用 §12 编号；M1a/M1b 已拆任务（agora-90t.5）；ROADMAP.md 视图已刷新。 | closed  |
| M1a | `agora-xqa` | 终端底座 | `agora-90t` | MISSION §12：A1、A2、A3、A4、A5、A6–A12、A20、A21、A24、A25（A1 跨两个阶段：已登记会话的展示在本 epic；运行时里未登记的 Unknown Agent 展示与采纳入口是 A22 的前置，随 A22 归 M1b agora-dvh.12 / agora-7cu。本 epic 关闭时 A1 只勾已登记那一半，MISSION §12 的 A1 要两半都做完才算过）；A36 中不变量 1–5、7 的测试在本 epic 钉死（10 在 M1b、8/11 在 M2 补齐）。逐条可打勾；每条对应 epic 内至少一个 issue。（A4 曾悬置待 Grok hooks 实测，2026-09-02 实测证实走 hook 路线后归位本 epic，见 agora-90t.3。） | closed  |
| M1b | `agora-dvh` | Agent 感知 | `agora-xqa` | MISSION §12：A14–A18、A22、A23、A32；另接过 A1 的未登记会话那一半（Unknown Agent 的侧栏展示与采纳入口，A22 的前置，agora-dvh.12 / agora-7cu），A1 两半都做完才算过；补 A36 中不变量 10 的测试。逐条可打勾；每条对应 epic 内至少一个 issue。 | closed  |
| M2a | `agora-7ku` | peer 核心 | `agora-dvh` | MISSION §12：A27、A30、A31、A33、A34、A38。（agora-7ku.12 是 A29 的策略半边，A29 本身由 M2b 认领；A36 不变量 8、11 的收口在 M2b。） | open 7/7 |
| M2b | `agora-s4r` | 安装运维与收口 | `agora-dvh` | MISSION §12：A26、A29、A39；补齐 A36 中不变量 8、11 的测试。 | open 4/5 |
| M3 | `agora-h1k` | 产出与起会话增强 | `agora-dvh` | MISSION §12：A40–A44。 | open 5/5 |
| V2-1 | `agora-thc` | 手机客户端与 PWA（iOS / Android） | `agora-7ku`, `agora-s4r` | MISSION §11 手机条目：A13、A19、A28、A35、A37。 | open 0/8 |

## 演示剧本（epic 的 design 字段；agent 先代检，人只看 👁 步骤与代检报告后关闭 epic，MISSION §1.5）

### M1a `agora-xqa`

M1a 演示剧本（人按此关闭 epic；tests/invariants.rs 是它的机械版）
前置：干净的 AGORA_HOME；本机装有 tmux ≥ 3.2 与 claude / codex / grok。
1. 启动 daemon，`agora open` 打开浏览器并配对；刷新页面不再要求配对。
2. New Agent：Project 从扫描列表选 agora，Agent = shell → Create；侧栏出现该会话，终端里 `ls`、输入中文、拖动窗口后 `tput cols` 随之变化（A1 A5 A6 A7）。
3. 再各起一个 Claude Code、Codex、Grok 会话，TUI 可见可输入（A2 A3 A4；状态感知归 M1b）。
4. 关闭标签页再重开 → 同一会话，scrollback 还在，pane pid 不变（A8 A20）。另开一个**新的浏览器标签页**打开 agora → 点侧栏打开一个会话 → 等到「已连接」→ 不点终端区域、直接键入 `echo p29` 回车 → 字符进 pane 并执行（agora-p29；点一下终端才能打字就是没修好）。
5. 完全关闭浏览器，等一分钟，重开 → 会话仍在，agent 未退出（A9 A10）。
6. `kill -9` daemon 再启动 → 列表重新发现全部会话；在 tmux 里 `exit 7` 的会话显示 FAILED、退出码可读（A11 A12）。
7. Kill 运行中的 shell → 出确认框；确认后进程死、行仍在、输出可看；Restart 它 → 同一会话内重生，前一轮输出仍在（A21）。
8. Delete metadata 一个活着的会话 → 进程仍活（`tmux -L agora ls` 可见），且从侧栏消失、`GET /api/sessions` 把它归入 `unregistered`（curl 或 devtools 看一眼即可）。侧栏里的 Unknown Agent 分区与采纳入口是 A22 的前置，随 A22 归 M1b（agora-dvh.12 / agora-7cu），本阶段不看。
9. 在自己的终端 `tmux kill-server`（默认 socket）→ agora 会话不受影响；`tmux -L agora attach -t =ag-…` 逃生口可用。
10. CI 绿；提交信息里有逐条关掉守卫、对应测试变红的记录（A36 的不变量 1–5、7；AGENTS.md 的小切片提交规则）。
全程不需要 Redis / Postgres / K8s，单机 daemon 即全部（A24 A25）。

### M1b `agora-dvh`

M1b 演示剧本（人按此关闭 epic）
前置：M1a 剧本通过；本机 Claude Code 与 Grok 已装；Codex 按 agora-dvh.2 的结论决定是否入列。
1. `agora hooks install claude`：装前显示 diff，`~/.claude/settings.json` 多出 agora 条目，重复安装不重复；grok 同理。
2. fake-agent 起十个会话：4 RUNNING / 2 WAITING / 2 TURN_DONE / 1 FINISHED / 1 FAILED → Dashboard 排序 FAILED 最上、WAITING 与 TURN_DONE 次之、RUNNING 最下；每行带任务标签与两行预览；再让一个无 hook 的 fake 会话安静 60 s → IDLE，六态齐全（A14 A17 A23）。
3. 真实 Claude Code：New Agent 起会话，任务是写一个文件 → RUNNING（source=hook）→ 权限请求 → WAITING 行展开显示问题 → Dashboard 点 allow，不开终端 → 文件被创建 → TURN_DONE 两行显示最后回复 → Dashboard 输入下一条指令 → RUNNING（A15）。
4. 同一场景改在终端按 1 → Dashboard 收到 decision.resolved，行退出 WAITING。
5. Grok 的权限请求：WAITING 行只有"打开终端"（ADR-002 D2，decision_via_hook = false；A15 的 Grok 边界）。
6. RUNNING → WAITING 弹浏览器通知，点击落到该行就地回答（A18）。
7. Terminal.app 裸起 `claude` → 列表出现 origin = external，状态随 hook 变、无终端（A16）；在默认 socket 的 tmux 里起 `claude` → 可 adopt 并 attach（A22）。
8. 停掉 daemon，让 agent 触发权限请求 → hook fail-open，TUI 照常提示；起 daemon → inbox 重放，状态正确；Restart 后旧代次事件不污染新会话（不变量 10）。
9. Restart 一个 Claude 会话 → `--resume <id>` 恢复同一对话；pane 里 `/clear` 后再 Restart → 恢复的是新对话（A32）。
10. 关掉 hooks（disableAllHooks）→ 到 silence_after 后该会话显示 UNKNOWN reason hooks silent。
CI 绿；A36 不变量 10 的守卫逐条关掉变红。

### M2a `agora-7ku`

M2a 演示剧本（agent 代检，无 👁 步骤；人看代检报告后关闭 epic，MISSION §1.5）
前置：M1b 剧本通过；zuan（Ubuntu 24.04）已开 sshd 供 agent 代检，Mac 与 zuan 网络可达（同一局域网或 tailnet 均可）；zuan 上有 agora 二进制（Mac 上 cargo zigbuild --target x86_64-unknown-linux-gnu 交叉编译即可，安装脚本归 M2b）与 tmux ≥ 3.2；两边各有 agora 仓库 clone。
1. zuan：agora peer token create mac → 得到 apt_ 机器 token；agora tls fingerprint 得到 sha256 指纹。Mac：peers 里写一行 { name: zuan, url, token_file, cert_fingerprint }，Mac 自身仍只听 loopback（A27；agora-7ku.2 token、agora-7ku.10 证书与指纹）。
2. Mac 打开 127.0.0.1 → 侧栏同列两台节点的会话，每行标节点；zuan 上起一个 Claude 会话，Mac 上几秒内出现（A27 A30；agora-7ku.5）。改错指纹 → Header 显示'指纹不匹配'而非离线；用错 token → zuan 日志 401、Mac 显示该 peer 未授权；一台未配对的笔记本直连 zuan 的 TLS 端口 → 401（A30 A31 A34；agora-7ku.2 / agora-7ku.10 / agora-7ku.12）。
3. 把 Mac 的前端指向一个报不同 api_version 的 fake 节点（或改 zuan 的版本号）→ 顶部横幅提示版本不一致，peer 显示'版本不兼容'而非离线，没有错读的数据（A33；agora-7ku.4 横幅、agora-7ku.5 客户端比版本、agora-7ku.12 状态模型）。
4. zuan 上的会话触发权限请求 → Mac 的 127.0.0.1 上就地 allow（一跳转发）；Kill zuan 的会话 → 确认框由 zuan 判断（转发节点不替你确认）；attach zuan 会话的终端，输入命令有回显（A38，ADR-003 D8；agora-7ku.7）。
5. CI 绿；agora-7ku.2 / agora-7ku.10 点名的 A31 / A34 守卫（未签发 token 拒绝一切 Bearer、Bearer 只上 TLS、明文拒绝非 loopback、TLS 永不降级）逐条关掉变红。
全程不碰安装脚本与升级（M2b）、不碰手机（V2-1）。zuan 断网 / Mac 睡眠后的 stale 与重连在 M2b 剧本。

### M2b `agora-s4r`

M2b 演示剧本（agent 代检，👁 只有第 2 步的物理变体；tests/invariants_peer.rs 是不变量 8、11 的机械版；人看 👁 与代检报告后关闭 epic，MISSION §1.5）
前置：M2a 剧本通过；zuan 已开 sshd。
1. zuan：一条命令安装节点（tmux ≥ 3.2、launchd/systemd 自启、LANG=C.UTF-8、<AGORA_HOME>/bin/agora 链接）；重启 zuan 后 daemon 自起，pane 里 CJK 输出正常（A26；agora-7ku.1）。
2. 代检：ssh 在 zuan 上把网络接口 down 60 s 再 up（nohup，或 tailscale down / up）等效断线；👁 一次：真拔 zuan 网线、合盖 Mac 再唤醒——两种情况下 zuan 的会话变 stale 并显示'上次见到 hh:mm'，Mac 本机会话照常操作；恢复后 30 s 内自动重连，行恢复正常，Header 显示每台节点在线 / 上次见到（A29，不变量 8；agora-7ku.6 视图，策略与状态模型在 M2a 的 agora-7ku.12）。
3. 两台各跑一条命令升级 agora：升级期间 zuan 上一个真实 agent 会话不死、升级后会话列表与名字/任务标签完整、hook 事件在窗口期落箱重放；<AGORA_HOME>/bin/agora 指向新二进制，Codex 不需要重新 /hooks（A39，不变量 3；agora-7ku.8）。
4. CI 绿；提交信息里有逐条关掉不变量 8、11 守卫、对应测试变红的记录；两个 daemon 实例之间走真 TLS 与 Bearer 的整栈集成测试在此（A36；agora-7ku.9）。
5. V2-1（agora-thc）已拆任务级 issue 并有演示剧本（agora-7ku.3）。
全程手机、PWA、推送、TOTP、浏览器可信证书都不出现（V2-1）。

### M3 `agora-h1k`

M3 演示剧本（agent 代检，无 👁 步骤；人看代检报告后关闭 epic，MISSION §1.5）
前置：M1b 剧本通过；本机 agora 仓库有 beads（bd ready 非空）；config 里 worktree_root 用默认值。
1. New Agent：Project 选 agora → Task 字段列出 bd ready 的任务，选一个 → Name、Worktree 名（= issue id）、首条 prompt（含 issue id 与 claim 提示）自动填好；Worktree 选'新建…' → 创建后自动选中，git worktree list 里多一行、分支基于主 worktree 当前分支（A43 A44；agora-h1k.2 / agora-h1k.1）。Create 后 bd show 该 issue 仍是 open：agora 没写 beads（不变量 12）。
2. 会话行展开 → 显示该任务的验收标准全文（与 bd show 一致、可折叠）；在 beads 里改验收标准，几秒后展开区跟着变，agora 的库里没有这段文字（A40；agora-h1k.3）。
3. 让 agent 改两个文件后回 TURN_DONE → 行展开列出改动文件；点'看 diff'→ 新标签页开一个只读终端显示 git diff，打字无效、关掉标签即消失、侧栏不多一行；期间 git status 与 reflog 证明没有任何写操作（A41；agora-h1k.5）。
4. 停 daemon，在 tmux 里让一个会话退出，等 1 分钟再起 daemon → 该会话 ended_at 是退出时刻而不是 daemon 起来的时刻；停 daemon 期间一个会话进入 WAITING，起 daemon 重放投递箱后'waiting Nm'从事件时间算起（A42；agora-h1k.4）。
5. CI 绿；tests/arch_boundary.rs 的 git 子进程白名单与 beads 零写入守卫逐条关掉变红。
全程单节点、桌面、只读：不做合并、不做 diff 组件、不写 beads（§11）。

### V2-1 `agora-thc`

V2-1 演示剧本（agent 先代检，人只看 👁 步骤与代检报告后关闭 epic，MISSION §1.5）
前置：M2a、M2b 剧本通过（zuan 装好并自启，Mac ↔ zuan 互为 peer，一跳转发可用）；zuan 与 iPhone 在 tailnet（tail5fb9b.ts.net），Android 已加入 tailnet；zuan 出网可达 Let's Encrypt 与 FCM。
1. zuan：tls.mode external + tailscale cert，public_url 设为 https://zuan.tail5fb9b.ts.net:7681；Mac 上不带 -k 的 curl 得 200、证书链到 ISRG Root；到期前 renew_command 被调用并热加载；health 的 tls 字段为 external（A35 证书半边；agora-thc.3、agora-7ku.10、agora-ltb）。
2. zuan 上 agora pair 打印 QR → 手机扫码 → 进 Dashboard；Mac 的 Dashboard「配对新设备」也能出 QR；设备列表里出现手机，从 Mac 吊销手机后手机下一次请求 401（A37 凭据半边；agora-thc.1）。👁 一次：扫码后彻底退出 Safari / 关掉 PWA 再打开仍免登录——iOS 站点存储回收策略在桌面不等效。
3. 手机浏览器「添加到主屏幕」/「安装应用」→ 从主屏打开直接进 Dashboard、无地址栏；SW 只在安全上下文注册，明文 127.0.0.1 上不注册；升级 agora 后 PWA 里是新 bundle（A35 可安装半边、A37；agora-thc.4）。👁：安装 UI 是系统弹层，agent-browser 到不了。
4. 手机上（或桌面 390×844 视口）：侧栏是 drawer，Dashboard 里 WAITING 行展开就地回答，Kill 出确认框且按得到，终端能键入有回显、软键盘弹出后 pane 重排（A37 界面半边；agora-thc.5）。👁 一次：真机用手指走完「看 Dashboard → 回答 WAITING → 确认 Kill」。
5. Mac 开 tls_listen 成为 zuan 的 peer；手机在 zuan 的 Dashboard 看到 node=mac 的行，回答 Mac 上的 WAITING → Mac 上文件被创建；attach Mac 会话终端键入有回显；Mac 关掉标签再从手机打开同一会话 scrollback 还在（A28、A13；agora-thc.8，机械在 agora-7ku.5 / 7ku.7）。👁 两条：真手机走一遍；Mac 合盖 60 s 再唤醒，手机上 mac 从「上次见到」恢复在线。
6. 手机 PWA 里开启推送 → 订阅只注册在 zuan；让任一节点的会话 RUNNING → WAITING → 手机在 PWA 完全关闭时收到推送，点开落到该行就地回答；把 zuan 到 FCM 的路断掉（或 Android 不可达 FCM 时）→ health.push.fcm=false 带原因、Dashboard 显示「推送不可达」且 PWA 打开期间仍实时更新（A19；agora-thc.6 服务端、agora-thc.7 客户端）。👁 两条：iPhone 与 Android 各收到一次推送——到达真机只能在真机上看。
7. 仅当第 1 步的 external 路径在目标部署走不通：按 agora-thc.2 评估 self-ca，结论回填 ADR-003 D4（由人采纳）。
8. CI 绿；tests/push.rs 的加密向量、订阅与吊销联动、降级守卫逐条关掉变红；api_version 按 §7.3 bump 并写进兼容规则。
全程不做：手机终端 ergonomics（软键盘 Ctrl / Esc、手势、剪贴板、文件上传）、TOTP / 登录限流（ADR-003 已否决）、多跳转发、peer 历史。
