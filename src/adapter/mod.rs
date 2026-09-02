//! Adapter：agent 特定代码唯一允许的目录（ADR-002 D2/D7/D9；agora-dvh.5）。
//!
//! 只解析 `--version`，不解析帮助输出与错误文本（ADR-002 规则 10）；
//! Restart 绝不用"继续上一次"类参数，只用自报的对话 id（ADR-002 D7）。
