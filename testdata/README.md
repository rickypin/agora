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

## generic/pane

文本兜底（ADR-002 D6）的屏幕 fixture：首行 `# expect: waiting [secret] | none`，其余是屏幕内容
（可含 ANSI）。每条 WAITING 模式一个文件；`scrollback_source` / `question_in_history` 是 scrollback
污染反例（devcenter 的教训：`cat` 出来的提示文本把会话钉在 WAITING）。回放测试 `tests/text_layer.rs`。
