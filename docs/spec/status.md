# 会话状态真值表（origin × status × process × source）

一行会话同时给出四个事实：**谁起的**（`origin`）、**它在做什么**（`status`）、**它的进程在不在**（`process`）、**这一格是谁说的**（`source`）。四个值各自都合法，凑在一起却未必说得通。2026-09-18 的盘点第一次把它摊成一张表逐条问（`docs/analysis/session-status-audit-2026-09-18.md` §6），才看见那批错行：5 行钉在 `starting` 180 小时、10 行 `finished` + `alive: true`、7 行 `turn_done` + `alive: false` 与"真没了"在 API 上长得一样、4 行 UNKNOWN 一个出口都没有。

**本文件是这张表的规范出处**（原先只存在于分析文档里，而分析文档不随代码改；2026-09-21 搬进 docs/spec，agora-5gg.16）。与代码冲突时以代码为准并回写这里（`docs/spec/README.md`）。状态本身怎么算出来——两个入口、四层来源、裁决顺序——见 ADR-002 D1 与 `docs/spec/api.md`「会话形态」；这里只钉"哪一格说得通"。

守卫是 `tests/status_truth_table.rs`：

- 表里每一行有一个 id（`a01`… 有句柄、`x01`… 无句柄），**id 同时是对应测试名的前缀**，所以表里的任何一行都能拿 id 在测试名里检索到；`cargo test status_truth_table` 逐行重跑，几毫秒。
- `::spec_rows_and_code_rows_agree` 拿本文件的表与测试里那张 `ROWS` 逐行对账：新增一行不写守卫、删掉一行不删表、把 ✗ 改成 ✓，都会红。
- `::every_reachable_cell_is_in_the_table` 反方向查：把能喂的都喂一遍，状态机吐出的每一个落点都要在本档那一节的表里找得到一格。表少写一格（或把口径改了）就红——这一条管的是"表是不是闭合的"，上面那两条管的是"表与守卫是不是同一份"。

## 1. 四列的取值与判定符号

| 列 | 取值 | 出处 |
|---|---|---|
| `origin` | `agora` / `adopted` 有运行时句柄；`external` / `headless` 无句柄（`Origin::is_handleless()`）| MISSION §4.2；`docs/spec/api.md`「external 与 headless」 |
| `status` | STARTING / RUNNING / WAITING / TURN_DONE / IDLE / FINISHED / FAILED / UNKNOWN | MISSION §4.3 |
| `process` | `alive` / `gone` / `unknown` | 裁决 agora-5gg.4 选 A、实施 agora-5gg.18；`ProcessState::derive` |
| `source` | `hook` / `process` / `text` / `activity` / `none` | ADR-002 D1 |

`process` 与状态机内部的 `Liveness`（"最后看到的那个进程号此刻在不在"）不是一件事，它多两条裁决（`src/status/mod.rs`）：

- **FINISHED / FAILED 一律 `gone`**：对话结束即不再谈进程，哪怕那个号还在跑别的对话（现场那 10 行 `finished` + `alive: true` 全是 superseded / SessionEnd 留下的旧行）。
- **运行时整体读不到是 `unknown`，不是 `gone`**（ADR-001 D7「读不到 ≠ 已死」）。

判定符号：**✓** 合法，写明对使用者的含义与出口；**◐** 合法但只许短暂（≤ 一个 tick、≤ 启动宽限、≤ 下一条 hook 事件），持续出现就是 bug；**✗** 不可能，每一行各有一条反向断言。

三列写的都是**调用方看到的值**（`GET /api/sessions` 的 `status` / `process` / `source`）：`process` 不是状态机内部那个 `Liveness`（两者不一致的行在"含义"里点明），`source` 也不是"这一格的事实从哪一层来"——运行时整体读不到、还没有任何观测的那几格，进程层说不出结论，`source` 报的是 `none` 而不是 `process`（a17 / a23 / x12）。

## 2. origin = agora / adopted（有运行时句柄）

| 格 | status | process | source | 判定 | 对使用者的含义 / 出口 | 守卫 |
|---|---|---|---|---|---|---|
| a01 | STARTING | alive | process（本代起始 < 2 s）| ✓ | 刚起，等一下 | `tests/status_truth_table.rs::a01_starting_from_the_runtime_inside_the_starting_window` |
| a02 | STARTING | alive | hook（SessionStart 之后 ≤ 10 s）| ✓ | 刚起，等一下 | `…::a02_starting_from_a_hook_inside_the_startup_grace` |
| a03 | STARTING | alive | hook（> 10 s）| ✗ | 应已衰减成 TURN_DONE `awaiting first prompt`；同一条衰减不分 origin（`Machine::decay_starting`）| `…::a03_hook_starting_never_outlives_the_startup_grace` |
| a04 | RUNNING | alive | process | ✓ | 在干活，不用管 | `…::a04_running_from_the_process_layer` |
| a05 | RUNNING | alive | hook | ✓ | 在干活，不用管 | `…::a05_running_from_a_hook` |
| a06 | WAITING | alive | hook | ✓ | 答它（能经 hook 答的按 `respond_via = hook`，否则打开终端）| `…::a06_waiting_from_a_hook` |
| a07 | WAITING | alive | text，agent 有 hook | ✗ | ADR-002 D1：文本抬不起 WAITING，行留在 hook 说的那一格 | `…::a07_text_cannot_raise_a_hooked_row_to_waiting` |
| a08 | TURN_DONE | alive | hook | ✓ | 看结果、给下一条 | `…::a08_turn_done_from_a_hook` |
| a09 | TURN_DONE | alive | text 或 activity | ✗ | D1：「一轮做完」只有宿主自己说得出来（Stop / idle 通知）| `…::a09_neither_text_nor_activity_can_produce_turn_done` |
| a10 | IDLE | alive | activity（无 hook 的行）| ✓ | 安静了一阵，没人说为什么 | `…::a10_idle_from_activity_when_no_hook_ever_spoke` |
| a11 | IDLE | alive | activity，agent 有 hook | ✗ | D1：活动层不产 IDLE——hook 活着而它不说话，是说不清（a18）而不是空闲 | `…::a11_activity_never_produces_idle_for_a_hooked_row` |
| a12 | FINISHED | gone | process（退出码 0 / killed by user）| ✓ | 退了，看结果或清理；按过 Kill 的不弹通知（MISSION §4.6）| `…::a12_finished_from_the_process_layer` |
| a13 | FINISHED | gone | process（`runtime_gone`）| ✓ | 运行时会话没了：Restart 重建同名会话，或删除记录（`docs/spec/api.md`「WebSocket」末段）| `…::a13_finished_when_the_runtime_session_is_gone` |
| a14 | FINISHED | gone | hook（SessionEnd 先于进程退出被观测）| ◐ | 下一 tick 进程层带着退出码把 source 换成 process（同状态同分时不抢，agora-rzh）；`process` 在宿主说出结束的那一个 tick 就已经是 gone | `…::a14_a_host_end_seen_before_the_process_exit` |
| a15 | FAILED | gone | process（exit ≠ 0 / signal）| ✓ | 出错了，看终端 | `…::a15_failed_names_the_exit_that_caused_it` |
| a16 | FINISHED / FAILED | alive，持续 | 任何 | ✗ | 「进程退出压倒一切」+ Q4：结束了的行不许报出 `alive` | `…::a16_an_ended_row_never_reports_an_alive_process` |
| a17 | UNKNOWN | unknown | none（`runtime unavailable`）| ✓ | agora 失明（协议不匹配 / 超时），顶部横幅说明；运行时恢复即自愈，绝不写 `ended_at` | `…::a17_an_unreadable_runtime_is_unknown_not_gone` |
| a18 | UNKNOWN | alive | text（`hooks silent; screen: …` / `permission prompt gone`）| ✓ | hook 没声音而屏幕像在等人：打开终端看，或按 `hooks_unheard` 去查 hook | `…::a18_a_screen_only_unknown_still_reports_an_alive_process` |
| a19 | UNKNOWN | gone | none（旧版 `runtime session missing`：本代已过 STARTING 窗口）| ✗ | 运行时会话没了是**事实**、不是"看不清"：走 a13（2026-09-19 据 agora-u5p 修订 ADR-001 D4）。还在 STARTING 窗口里的那一格另算（a23）| `…::a19_a_missing_runtime_session_is_not_unknown` |
| a20 | UNKNOWN | 任何 | hook | ✗ | 有句柄的行里 UNKNOWN 只有 a17 / a18 / a22 / a23 四格；hook 层的词表不含 UNKNOWN | `…::a20_the_hook_layer_never_writes_unknown` |
| a21 | WAITING | alive | text（agent 无 hook）| ✓ | 答它：这一行没有 hook 可回，只能打开终端（`respond_via = terminal`）。与 a07 是同一层在两种行上的两种命运 | `…::a21_text_raises_waiting_only_on_a_row_without_hooks` |
| a22 | UNKNOWN | gone | process（`process exited, exit status not yet collected`）| ◐ | 只许停一个 tick：退出码到了落 a12 / a15，永远补不上就是会话连同 pane 没了，落 a13。不猜 FINISHED 也不猜 FAILED | `…::a22_a_missing_exit_status_is_unknown_not_a_guess` |
| a23 | UNKNOWN | gone | none（`runtime session missing`，本代还在 STARTING 窗口 < 2 s）| ◐ | 运行时这一 tick 还没报到它，不等于"没了"：绝不写 `ended_at`（守卫 `tests/session_manager.rs::starting_window_exempts_a_row_that_is_still_starting`），下一 tick 落 a01 | `…::a23_a_row_the_runtime_has_not_reported_yet_is_not_gone` |

## 3. origin = external / headless（无运行时句柄）

`process` 来自 hook 检查点里那个 agent 进程号（`AgentHooks::agent_pid`，Claude 的 `CLAUDE_PID`、Codex / Grok 的 hook ppid）的探活结果；没有可信进程号（Codex Desktop 的共用 app-server、丢了号的旧检查点、无头会话）就是 `unknown`。

| 格 | status | process | source | 判定 | 对使用者的含义 / 出口 | 守卫 |
|---|---|---|---|---|---|---|
| x01 | STARTING | alive 或 unknown | hook（≤ 10 s）| ✓ | 刚起，等一下 | `tests/status_truth_table.rs::x01_handleless_starting_inside_the_grace` |
| x02 | STARTING | 任何 | hook（> 10 s）| ✗ | 同 a03：衰减不分 origin、也不分进程号在不在（现场 zuan ef0e50 曾钉 180 h，agora-rkl）| `…::x02_handleless_starting_never_outlives_the_startup_grace` |
| x03 | RUNNING / WAITING / TURN_DONE | alive | hook | ✓ | 同 §2；WAITING 经挂起的 hook 可答（`respond_via = hook`）| `…::x03_hook_states_stay_put_while_the_handle_is_alive` |
| x04 | RUNNING / WAITING / TURN_DONE | unknown | hook | ✓ | 同上，但 agora 说不上进程在不在（Codex Desktop）；沉默满 `hooks.external_silent_after` 退到 x10 | `…::x04_hook_states_stay_put_without_a_handle` |
| x05 | RUNNING / WAITING / TURN_DONE | gone | 任何 | ✗ | 进程号探不到了就是结束，落在 x07（没有退出码，所以只有 FINISHED 一种结束）| `…::x05_a_gone_handle_ends_the_row_it_cannot_leave_it_working` |
| x06 | IDLE | 任何 | 任何 | ✗ | 无句柄行没有活动来源（没有 pane 可采输出）| `…::x06_no_activity_layer_means_no_idle` |
| x07 | FINISHED | gone | process（`process_gone`）| ✓ | 一个事件都没来、进程消失：崩溃、关窗口、机器重启；通知一次（`src/events.rs`）| `…::x07_a_silent_row_ends_when_its_process_goes` |
| x08 | FINISHED | gone | hook（`host_session_end{clear\|resume\|logout\|exit\|other}` / `superseded`）| ✓ | 人自己结束的、或同一进程换到了新对话：不通知（MISSION §4.6 证据 ②）；`process` 一律 gone，哪怕那个号还在跑新对话（Q4）| `…::x08_a_row_ended_by_its_host_or_by_a_new_conversation_reports_gone` |
| x09 | FAILED | 任何 | 任何 | ✗ | 没有退出码可拿，分不出两种退法：结束只有 FINISHED（x07 / x08）| `…::x09_no_exit_code_means_no_failed` |
| x10 | UNKNOWN | unknown | hook（`hooks silent; no process handle`）| ◐ | 暂态：下一条 hook 事件即恢复；否则满 `sessions.external_unknown_ttl` 走 DELETE（agora-e08）。沉默时长按**事件自己的时刻**算，重启 + 重放不清零（agora-5gg.2）| `…::x10_a_silent_handleless_row_falls_to_unknown_on_the_event_clock` |
| x11 | UNKNOWN | alive 或 gone | hook / text / process | ✗ | 行上说过话、或探到了进程事实，就不该"说不清"：沉默兜底只对无句柄且无进程号的那一格开 | `…::x11_a_handle_or_a_spoken_hook_rules_out_unknown` |
| x12 | UNKNOWN | 任何 | none（`no observation yet` / `external session: … hook only`）| ◐ | 只允许在检查点恢复 / 重放完成前的瞬间，第一条 hook 事件一到就走；持续出现 = 检查点丢了，属 bug | `…::x12_no_observation_yet_leaves_as_soon_as_a_hook_speaks` |
| x13 | 本节每一格 | 同 external | 同 external | ✓ | `origin = headless` 与 external 同一张表：同样 `runtime_ref` NULL、同样只有 hook 看得见（代码里一律问 `Origin::is_handleless()`）；三处不同见第 5 节 | `…::x13_headless_shares_every_cell_of_the_handleless_table` |

## 4. 贯穿两档的三条规则

1. **STARTING 不是筐**：hook 层的 STARTING 最多停 `startup_grace`（默认 10 s），到点归 TURN_DONE `awaiting first prompt`，不分 origin、不分进程号在不在（a03 / x02 共用 `Machine::decay_starting`）。
2. **UNKNOWN 必须带原因、必须有出口**：`unknown_cause` 是封闭集合（`runtime_unavailable | hooks_silent_screen | prompt_gone | hooks_silent_no_handle | no_observation | exit_status_missing`，形态与各自出口见 `docs/spec/api.md`「会话形态」）；表里只允许 a17 / a18 / a22 / a23 / x10 / x12 六格出现 UNKNOWN：六个值都有人认领（a18 一格承两档——屏幕沉默与提示消失对使用者是同一件事），而 `no_observation` 在有句柄与无句柄两档各占一格（a23 / x12，出口不一样：前者下一 tick 落 a01，后者等第一条 hook 事件）。守卫是 `::every_reachable_cell_is_in_the_table`：把 13 类 hook 事件 × 屏幕证据 × 进程事实 × 三种进程号状态喂一遍，每一个落点都要在表里找得到同一档（有句柄 / 无句柄）的一格——状态机私自产出一格新形状而表上没有人写过的话，红在这里。
3. **结束必须说得出原因**：`end_cause` 是封闭集合（`exit_code | signal | killed_by_user | host_session_end | superseded | process_gone | runtime_gone`）；`reason` 只是给人看的一句话，程序按枚举分支（MISSION §2.3 规则 10）。守卫 `::every_finished_and_failed_row_names_its_end_cause`、`::every_unknown_row_names_why_it_is_unknown`、`::cause_wire_vocabulary_is_the_locked_set`。

## 5. headless 与 external 不同的三件事

不是状态格，所以不进上面的表；三条都在 `docs/spec/api.md`「external 与 headless」（裁决 agora-5gg.7 选 B、实施 agora-5gg.20），各自的守卫是：

- 不发通知：`tests/events.rs::headless_rows_never_notify_on_any_transition`
- 侧栏一律进折叠区、主区不给「打开终端」：`web/src/attention.test.ts`（判据收在 `web/src/attention.ts::isHandleless` 一处）
- 满 `sessions.external_finished_ttl` 不论状态即删：`tests/hooks_external.rs::a_headless_session_registers_as_headless_and_expires_whatever_its_status_is`

## 6. peer 行

peer 行的四个值由**来源节点**按第 2 / 3 节算好后原样搬过来，本节点不重算、也不按 origin 分叉；本节点只改两件事：`status_since` 换成本节点第一次看见该行处于当前状态的时刻，以及 stale 标记。所以：

- 每一格的合法性与第 2 / 3 节同一张表；守卫 `tests/peer_view.rs::peer_sessions_are_merged_with_node_label`、`::peer_timestamps_use_local_clock`。
- `stale = true` 的行整行淡显且 `last_seen` 必在（不是删掉）：`tests/peer_stale.rs::offline_peer_is_stale_with_last_seen_not_removed`。
- 行上的等待时长是个**下界**，UI 画成 `≥`（本节点重启不把它清零）：`tests/peer_view.rs::a_local_restart_does_not_reset_a_peer_rows_wait`。

## 7. 现场对照（2026-09-18 的 79 行落在哪一格）

| 现场 | 当时的样子 | 该落的那格 |
|---|---|---|
| zuan ef0e50 / a3a2a0、Mac db5d48 / 4aa42b | `starting` 180 h / 10 h / 2 d / 15 h | x02 ✗ → 现在是 x01 之后衰减成 TURN_DONE |
| Mac b226b5 / 63a22a | `?` + `runtime session missing`，终端面板连不上 | a19 ✗ → 现在是 a13 |
| Mac 0e26ad / 14d791 / eb129d / b939bb | 沉默 55–63 h 仍 `◆ turn_done`（重放过的那批）| x04 合法、但沉默时钟错 → 现在按事件时刻落 x10 |
| Mac 72e5c2 / f9c572 / 32caf0 / 443d8e | `?` + `hooks silent; no process handle` 无出口 | x10 ◐ 却没出口 → 现在满 unknown_ttl 走 DELETE |
| finished + alive:true 10 行 | 宿主说了结束、进程号还在跑新对话 | a14 / x08：`process` 一律 gone（Q4）|
| turn_done + alive:false 7 行 | 无可信进程号的行与"真没了"长得一样 | x04 ✓：`unknown` 是一个取值，不是一个 false |
