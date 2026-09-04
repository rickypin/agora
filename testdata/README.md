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
