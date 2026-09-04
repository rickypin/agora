# testdata

fixture 驱动测试的数据（ADR-002 D10）。

- `<agent>/<version>/hooks/<scenario>.jsonl`：hook 事件序列。行格式见 `src/adapter/replay.rs`
  的模块文档；回放测试是 `tests/fixtures_replay.rs`，遍历这里的每个文件。
- `<agent>/<version>/pane/*.txt`：屏幕文本 fixture（文本层与预览用），尚无。

## claude/2.1.260

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
写在 `src/adapter/grok.rs` 模块文档。

## generic/pane

文本兜底（ADR-002 D6）的屏幕 fixture：首行 `# expect: waiting [secret] | none`，其余是屏幕内容
（可含 ANSI）。每条 WAITING 模式一个文件；`scrollback_source` / `question_in_history` 是 scrollback
污染反例（devcenter 的教训：`cat` 出来的提示文本把会话钉在 WAITING）。回放测试 `tests/text_layer.rs`。
