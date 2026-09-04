# agora — ROADMAP（视图）

> **由 `scripts/roadmap-view.sh` 生成，不要手改。** 真相源是 beads：阶段 = epic，阶段门 = epic 之间的 `blocks` 依赖，验收标准 = epic 的 `--acceptance`，演示剧本 = epic 的 `--design`（下方"演示剧本"一节）。
> 本文件不放任务 checkbox（避免 devcenter 式双轨，见 `docs/analysis/beads/README.md` §6.3 / §8.2）。任务级细节：`bd ready`、`bd dep tree <epic>`。
> 生成时间：2026-09-04

| 阶段 | epic | 目标 | 阶段门（被谁阻塞） | 验收要点 | 状态 / 进度 |
|---|---|---|---|---|---|
| M0 | `agora-90t` | MISSION 定稿与首批 ADR | — | MISSION.md 各节无 TODO、候选标记全部清除；ADR-001/002/003 状态 Accepted 且 docs/adr/README.md 索引更新；M1a/M1b/M2/M3 epic 已在 beads 建立（agora-xqa / agora-dvh / agora-7ku / agora-h1k）并带阶段门依赖（M1a→M1b→{M2∥M3}），验收引用 §12 编号；M1a/M1b 已拆任务（agora-90t.5）；ROADMAP.md 视图已刷新。 | closed  |
| M1a | `agora-xqa` | 终端底座 | `agora-90t` | MISSION §12：A1、A2、A3、A4、A5、A6–A12、A20、A21、A24、A25（A1 跨两个阶段：已登记会话的展示在本 epic；运行时里未登记的 Unknown Agent 展示与采纳入口是 A22 的前置，随 A22 归 M1b agora-dvh.12 / agora-7cu。本 epic 关闭时 A1 只勾已登记那一半，MISSION §12 的 A1 要两半都做完才算过）；A36 中不变量 1–5、7 的测试在本 epic 钉死（10 在 M1b、8/11 在 M2 补齐）。逐条可打勾；每条对应 epic 内至少一个 issue。（A4 曾悬置待 Grok hooks 实测，2026-09-02 实测证实走 hook 路线后归位本 epic，见 agora-90t.3。） | open 17/17 |
| M1b | `agora-dvh` | Agent 感知 | `agora-xqa` | MISSION §12：A14–A18、A22、A23、A32；另接过 A1 的未登记会话那一半（Unknown Agent 的侧栏展示与采纳入口，A22 的前置，agora-dvh.12 / agora-7cu），A1 两半都做完才算过；补 A36 中不变量 10 的测试。逐条可打勾；每条对应 epic 内至少一个 issue。 | open 0/14 |
| M2 | `agora-7ku` | peer 与安装运维 | `agora-dvh` | MISSION §12：A26、A27、A29–A31、A33、A34、A38、A39；补齐 A36 中不变量 8、11 的测试。 | open 0/3 |
| M3 | `agora-h1k` | 产出与起会话增强 | `agora-dvh` | MISSION §12：A40–A44。 | open  |
| V2-1 | `agora-thc` | 手机客户端与 PWA（iOS / Android） | `agora-7ku` | MISSION §11 手机条目：A13、A19、A28、A35、A37。 | open 0/2 |

## 演示剧本（epic 的 design 字段；人按此关闭 epic，MISSION §1.5）

### M1a `agora-xqa`

M1a 演示剧本（人按此关闭 epic；tests/invariants.rs 是它的机械版）
前置：干净的 AGORA_HOME；本机装有 tmux ≥ 3.2 与 claude / codex / grok。
1. 启动 daemon，`agora open` 打开浏览器并配对；刷新页面不再要求配对。
2. New Agent：Project 从扫描列表选 agora，Agent = shell → Create；侧栏出现该会话，终端里 `ls`、输入中文、拖动窗口后 `tput cols` 随之变化（A1 A5 A6 A7）。
3. 再各起一个 Claude Code、Codex、Grok 会话，TUI 可见可输入（A2 A3 A4；状态感知归 M1b）。
4. 关闭标签页再重开 → 同一会话，scrollback 还在，pane pid 不变（A8 A20）。
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
