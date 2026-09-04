# testdata

fixture 驱动测试的数据（ADR-002 D10）。

- `<agent>/<version>/hooks/<scenario>.jsonl`：hook 事件序列。行格式见 `src/adapter/replay.rs`
  的模块文档；回放测试是 `tests/fixtures_replay.rs`，遍历这里的每个文件。
- `<agent>/<version>/pane/*.txt`：屏幕文本 fixture（文本层与预览用），尚无。

## claude/2.1.258

七个场景按 Claude Code 2.1.258 实测的 payload 键集合**合成**（agora-90t.3 注记记录了
SessionStart / UserPromptSubmit / PreToolUse / PostToolUse / PermissionRequest / Notification /
Stop / SessionEnd 的键；值已脱敏），不是 `agora hook --record` 录下的。录制器（agora-3la.1）
落地后用真录替换，届时 `tests/fixtures_replay.rs` 不用改：它只认行格式。

`respond` 行是 Dashboard 的答复，永远是合成的——agent 侧看不到它。
