//! Session Manager：metadata 表与生命周期映射（ADR-001 D4；agora-xqa.8）。
//!
//! 只认 `runtime::Runtime` 与 `status` 给出的抽象；不得出现任何 agent 的 payload 键
//! （ADR-002 规则 5，`tests/arch_boundary.rs`）。
