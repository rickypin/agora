# testdata

fixture 驱动测试的数据（ADR-002 D10）。

- `<agent>/<version>/hooks/<scenario>.jsonl`：hook 事件序列。行格式见 `src/adapter/replay.rs`
  的模块文档；回放测试是 `tests/fixtures_replay.rs`，遍历这里的每个文件。
- `<agent>/<version>/pane/*.txt`：屏幕文本 fixture（文本层与预览用），尚无。
- 录制（agora-3la.1）：`agora hook … --record <file>`（或环境变量 `AGORA_HOOK_RECORD=<file>`，hook 进程从
  agent 继承）让每次 hook 调用在投递之前把脱敏后的 payload 追加成一行 `{"at","hold","payload"}`——`at` 是相对
  文件头 `# recorded host=… t0=…` 的秒数，`hold` 是这条 hook 挂起过；脱敏规则见 `src/adapter/scrub.rs`（枚举键与 id
  形状保留、路径与自由文本抹掉，盐是 t0，所以同一录制里 id 对得上、跨录制对不上）。交互场景的录法：给 agent 的启动
  命令加 `AGORA_HOOK_RECORD=…`，在 tmux 里把剧本走一遍，再按 `src/status/machine.rs` 的规则手工补 `expect`。
- `<agent>/<version>/hooks/headless.jsonl`（agora-3la.2）由冒烟测试录：`AGORA_SMOKE_RECORD=1 cargo test --test hook_smoke -- --ignored`，
  文件不存在写到位，已存在写成 `.jsonl.new` 供人工比对。正常模式的冒烟只比对 SessionStart 与 Stop 的顶层键集合，
  agent 版本不在表里、目录里没 fixture、键集合漂移都红并提示录新 fixture；没装的 agent 跳过。
- **版本目录只长两种样子**（规则与兜底在 `tests/fixtures_replay.rs`）：**实测目录**七个场景一个不缺（下面的
  claude/2.1.260、claude/2.1.261、claude/2.1.291、codex/0.152.1、grok/1.0.13、pi/1.0.4）；或**冒烟-only 目录**——除 `headless.jsonl`
  外一条都没有（下面的 claude/2.1.270、claude/2.1.296、codex/0.153.4、grok/1.0.30、grok/1.0.50、pi/1.1.0）。这么分是因为冒烟守卫按**精确版本号**
  点名要 fixture，而补录七个真交互场景是一个 epic 的量：不给这一档，agent 每次小版本升级都要花掉一整天的
  录制才能消红灯，结果就是红灯一直红（2026-09-14）。录新冒烟 fixture 时把 `expect` 行照上一版本补上（值用
  `<prompt>` / `<last_assistant_message>` 占位），于是回放测试会拿新版本的**真 payload** 再过一遍状态机——
  它验的是“同样的结论”，比键集合比对硬得多。录完要在文件头注明与上一版实测目录的差别（没差别也要写“逐键一致”）。

## claude/2.1.261

2026-09-05 在本机 Claude Code 2.1.261 上用 `agora hook --record` **真录**的全部八个场景（tmux 内交互式
`--permission-mode default`；`permission_dashboard` 的 `respond` 行照例合成；`headless.jsonl` 由冒烟测试录）。
与 2.1.260 的合成件相比，真录否定了两处猜测、又多了几处实测：payload 多了 `scratchpad_dir`，`effort` 是对象
`{"level"}`；`StopFailure` 的错误键是 `error`（值仍是那 11 个枚举）而不是文档的 `error_type`；中断（Esc）**一个事件
都不发**，没有 PostToolUseFailure，挂起中的权限要到下一条 prompt 才清；一条消息里的两个 Read 实际按 Pre/Post/Pre/Post
串行；manual 模式下 `echo` / `wc` 免审批，要触发 PermissionRequest 得用 `touch` 这类写操作；假模型名不发 StopFailure，
要 `ANTHROPIC_BASE_URL` 指向拒绝连接的端口、等 10 次重试耗尽（约 3 分钟）才来 `error=server_error`。

`task_notification.jsonl`（agora-3s5，2026-09-05 真录）是第九个场景：`claude -p` 让 Bash 以 run_in_background 起一个
`sleep`，第一轮 Stop 之后宿主把 `<task-notification>…` 当 **UserPromptSubmit** 发出，agent 再跑一轮。脱敏器对 `prompt`
键只留开头的注入标签（`<task-notification>\n<prompt>`），正文照抹；`expect.prompt` 断言 `❯` 行没被它改写。另两处实测：
无头模式 payload 没有 `scratchpad_dir`；Stop 多了 `background_tasks` / `session_crons`。

## claude/2.1.260

已被 2.1.261 的真录取代，目录保留作对照（合成件的猜测哪几处错了见上一节）。

- `ask_user_question.jsonl` 是 2026-09-04 在本机 Claude Code 2.1.260 上**真录**的（从
  `~/.agora/hooks/done/` 取出、脱敏），它证实了两件事：`PermissionRequest` 不带 `tool_use_id`；
  AskUserQuestion 在 PreToolUse 之后还会走一次 PermissionRequest。
- 其余七个场景按 2.1.258 / 2.1.260 实测的 payload 键集合**合成**（值已脱敏），不是录下的；
  录制器（agora-3la.1）落地后逐个用真录替换，`tests/fixtures_replay.rs` 不用改：它只认行格式。
  `parallel_tools` 因为上面那条实测只能用两个不同名工具。

`respond` 行是 Dashboard 的答复，永远是合成的——agent 侧看不到它。

## grok/1.0.13

2026-09-04 在本机 Grok 1.0.13 上录的（tmux 内交互式 `--permission-mode default` + `-p` 无头，探针 hook
落盘 stdin；路径与 id 脱敏，`toolResult` 的字节数组换成空串）：`turn_complete`、`permission_terminal`、
`permission_dashboard`（其实是终端拒绝路径——Grok 的 Dashboard 答不了，见文件头）、`clear`、`interrupted`
五个的事件名、键集合、顺序与时间间隔都来自真录；`api_error`、`parallel_tools` 按文档合成。真录证实的反直觉处
写在 `src/adapter/grok.rs` 模块文档。`headless.jsonl` 是 2026-09-05 冒烟测试录制模式录的 `grok -p` 一轮。

## codex/0.152.1

2026-09-05 在本机 Codex 0.152.1 上录的（tmux 内 TUI `approval_policy=on-request` + `sandbox_mode=workspace-write`，探针 hook
落盘 stdin；路径与 id 脱敏）：`turn_complete`、`permission_dashboard`、`permission_terminal`、`clear`、`interrupted` 五个的
事件名、键集合、顺序与时间间隔来自真录；`api_error`、`parallel_tools` 按真录的键集合合成。两个反直觉处：PermissionRequest
不带 `tool_use_id`（PreToolUse 带）；挂起期间 TUI 不显示审批提示，hook 退出后才弹——见 ADR-002 附录 A。
`headless.jsonl` 是 2026-09-05 冒烟测试录制模式录的 `codex exec` 一轮（`--dangerously-bypass-hook-trust`：无头下没法在 TUI
`/hooks` 信任 hook，见 `src/adapter/codex.rs`）。

## claude/2.1.291

2026-10-08 在本机 Claude Code 2.1.291 上**真录**七个交互场景 + 冒烟无头（tmux 内 `--permission-mode manual`，
启动命令加 `AGORA_HOOK_RECORD`；`permission_dashboard` 的 `respond` 行照例合成；`headless.jsonl` 由
`AGORA_SMOKE_RECORD=1 cargo test --test hook_smoke claude -- --ignored` 录），外加从 `bd show agora-7d1j` 的 notes
**转载**的第八个场景 `resume.jsonl`（`claude --resume <id>` 只发一条 `SessionStart(source=resume)`、`session_id`
仍是被 resume 的原 id，同一行从 FINISHED 被唤回 STARTING）。

与 2.1.261 的差别：**八个场景全部逐键一致**（每个 fixture 的文件头各自写了这句；`turn_complete` 连 Stop 的
`background_tasks` / `session_crons` 也逐键对上）。两处记录条件不同不是版本漂移：① 2.1.291 的 CLI 把
`--permission-mode default` 改名成 `manual`（payload 里的 `permission_mode` 仍报 `"default"`，7d1j 没加这个 flag
所以 `resume.jsonl` 报 `"auto"`）；② 本机 modelSettings 给 opus-5-5 定的 `effort.level` 是 `xhigh`（2.1.261 那次是
`high`）。另有两条实测写在各文件头：`permission_suggestions` 的形状随命令变（`touch` → `[addDirectories, setMode]`、
前台 `python3` → `[addRules]`，同一版本内两种都见过，**不是**版本差异）；Esc 中断仍**一个事件都不发**（没有
PostToolUse / PostToolUseFailure，挂起要等下一条 prompt 的 UserPromptSubmit 才清），与 2.1.261 相同。

录制隔离：隔离 `AGORA_HOME`（hook 投递与 `bin/agora` 链接都指向它）+ 临时 `HOME`（`settings.json` 从
`~/.claude/settings.json` 删掉 `hooks` 后复制，再用 `agora hooks install claude --home <隔离> --user-home <临时>`
写入本仓的 hook 条目）+ 专用 tmux socket；`~/.claude` 与真 daemon 一个字节没动。`resume.jsonl` 不在本机重录，
原文与脱敏映射在 `bd show agora-7d1j` 的 notes 里。

## claude/2.1.270

冒烟-only。2026-09-14 本机 2.1.270 录的 `claude -p` 一轮：事件名与四个事件的顶层键集合与 claude/2.1.261 的
`headless.jsonl` 逐键一致（无头仍不带 `model` / `scratchpad_dir`；Stop 仍有 `background_tasks` / `session_crons`，
`effort` 仍是 `{"level"}` 对象），所以七个交互场景没重录，`expect` 照 2.1.261 补。

本次重录暴露的不是漂移而是测试自己的坑：Linux 上 `~/.claude/.credentials.json` 不存在，登录态在
`~/.claude/settings.json` 的 `env.CLAUDE_CODE_OAUTH_TOKEN` 里，`seed_credentials` 只复制前两个文件时
`claude -p` 直接 `Not logged in · Please run /login`，拿到的事件缺 Stop、只有 `StopFailure`。修在
`tests/hook_smoke.rs::copy_settings_without_hooks`：只搬删掉 `hooks` 键的那一份 settings.json——整份复制会把
用户自己的 hook 条目（指向 `~/.agora`）带进临时 HOME，这一轮假会话就会投到开发机上真的 daemon 里。

## claude/2.1.296

冒烟-only。2026-10-10 在 **zuan** 上录的 `claude -p` 一轮：四个事件（SessionStart → UserPromptSubmit → Stop →
SessionEnd）的顶层键集合与 claude/2.1.291（以及 2.1.270）的 headless 逐键一致——无头仍不带 `model` /
`scratchpad_dir`，Stop 仍有 `background_tasks` / `session_crons`、`effort` 仍是 `{"level"}` 对象。2.1.295 / 2.1.296
的 hook 侧变化（新增 `onFailure: "block"` 选项、修复 hook 输出中像 plugin hint tag 的文本被改写）没有改
hook 载荷形状，所以七个交互场景没重录——交互真录基线仍是 `claude/2.1.291/`（Mac 上录），本目录只加冒烟。
Linux 登录态走 settings.json 的 `env`（见 2.1.270 一节），录制隔离与冒烟入口同其余版本。

## codex/0.153.4

冒烟-only。2026-09-14 本机 0.153.4（`gpt-6-astra`）录的 `codex exec --skip-git-repo-check
--dangerously-bypass-hook-trust` 一轮：四个事件的事件名与顶层键集合与 codex/0.152.1 逐键一致，唯一变化是
`model` 的值（不在 `expect` 里），七个交互场景没重录。

## grok/1.0.30

冒烟-only。2026-09-14 本机 1.0.30 录的 `grok -p` 一轮：序列仍是 session_start(new) → user_prompt_submit →
stop(end_turn) → session_end(shutdown) → 退出时再一次 stop(shutdown)，顶层键集合与 grok/1.0.13 逐键一致。
**一处形态变化**：别名键 `hook_event_name` 的值从小写蛇形改成 PascalCase（`session_start` → `SessionStart`），
首选键 `hookEventName` 仍是小写蛇形——Grok 在向 Claude 的配置形态靠。`src/adapter/grok.rs::event_key` 本来就是
“去掉 `_` 再小写”归一，两种写法进同一个事件；新 fixture 的 `expect` 行就是这件事的守卫（回放跑的是本次
真录的 payload，不只是键集合）。七个交互场景没重录。

## grok/1.0.50

冒烟-only。2026-10-10 在 **zuan** 上录的 `grok -p` 一轮：序列与 grok/1.0.30 相同（session_start(new) →
user_prompt_submit → stop(end_turn) → session_end(shutdown) → 退出时再一次 stop(shutdown)），五个事件的顶层
键集合逐键一致；别名键两种形态也没变（`hook_event_name` = PascalCase、`hookEventName` = 小写蛇形），
`src/adapter/grok.rs::event_key` 的归一守卫照旧。七个交互场景没重录。

## pi/1.0.4

2026-10-07 在本机 pi 1.0.4 上录的（交互式 TUI：隔离 HOME + `AGORA_HOOK_RECORD` 在 tmux 里走剧本；
`headless.jsonl` 由冒烟测试录制模式录 `pi -p`）。七个场景齐全，逐场景：

- `turn_complete`：`session_start(idle=true)` → `before_agent_start` → `agent_settled` → `session_shutdown(quit)`。
- `permission_terminal` / `permission_dashboard`：**pi 没有审批**（工具默认不问人，ADR-002 附录 A 第 7 行），
  两个文件都是**无 hold 的工具轮**（prompt → bash → agent_settled），`hold` 全 false；文件名保留是为了对齐
  `tests/fixtures_replay.rs` 的目录规则。
- `parallel_tools`：一条消息两次 bash 调用，pi 逐个跑（两个 `tool_execution_start`，串行）。
- `clear`：`/new` → `session_shutdown(reason=new)`（**不结束行**：adapter 只认 quit）→ 新 id 的
  `session_start(idle=true)`。
- `interrupted`：`sleep 60` 的工具跑到一半按 Esc → `agent_settled`（末条 assistant 文本为空）；pi 的
  失败与中断在 hook 层同形——没有 Claude 的 Interrupt / StopFailure 那种事件。
- `api_error`：临时 HOME 的 `models.json` 把 ais 的 baseUrl 改成 `http://127.0.0.1:1/v1`（连不上），
  14 s 后 `agent_settled`（末条 assistant 文本同样为空）。

三处与其它宿主不同、写在这里防后来者当 bug：① 登记带 `idle`（扩展报 `ctx.isIdle()`，真值表 x14/a24）；
② 轮次边界是 `agent_settled` 不是 `turn_end`（后者每个模型轮都发）；③ 空 assistant 文本被脱敏器写成
`<last_assistant_message>` 占位符（它把空串也替掉），空串那一格由 adapter / 状态机单测钉。④ 每条载荷都带
`input_channel: 1`（扩展收 `$AGORA_HOME/input` 队列、`pi.sendUserMessage` 注入；ADR-002 D11），
fixture 里也补上了——无句柄的 pi 行因此 `text_via = host`，是这个键把“只能到终端”改成“手机上能发话”的。
冒烟见 `tests/hook_smoke.rs::pi`（键集合基线 `headless.jsonl`）与 `::pi_input_channel`
（真 pi + 真 tmux：往队列里写一件 → TUI 里出现那句话 → `.done` = ack）。

## pi/1.1.0

冒烟-only。2026-10-10 在 **zuan** 上录的 `pi -p` 一轮：四个事件（session_start(idle=true) →
before_agent_start → agent_settled → session_shutdown(quit)）的顶层键集合与 pi/1.0.4 逐键一致——1.1.0 给
扩展 / JSON 事件新增的 `aborted` 没有进 hook 载荷，载荷仍带 `input_channel: 1`。同一版本上
`tests/hook_smoke.rs::pi_input_channel`（真 pi + 真 tmux 的宿主注入端到端）也通过：扩展通道在 1.1.0 上仍工作。
1.1.0 的 OSC 7501 只对支持该协议的终端发报告（且经 tmux 到不了，见 agora-dg1x），与 hook 载荷无关。七个交互
场景没重录，交互真录基线仍是 `pi/1.0.4/`。

## generic/pane

文本兜底（ADR-002 D6）的屏幕 fixture：首行 `# expect: waiting [secret] | none`，其余是屏幕内容
（可含 ANSI）。每条 WAITING 模式一个文件；`scrollback_source` / `question_in_history` 是 scrollback
污染反例（devcenter 的教训：`cat` 出来的提示文本把会话钉在 WAITING）。回放测试 `tests/text_layer.rs`。
