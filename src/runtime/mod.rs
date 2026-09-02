//! 会话运行时抽象（ADR-001）。
//!
//! `Runtime` trait 与 tmux 实现在 agora-xqa.7 落地。这里先钉两条施工约束：
//! - 子进程只经 [`exec`] 一个入口（超时 + blocking + stderr 有界）；
//! - tmux 的一切标识符只许出现在 [`tmux`] 子模块（`tests/arch_boundary.rs`）。

pub mod exec;
pub mod tmux;
