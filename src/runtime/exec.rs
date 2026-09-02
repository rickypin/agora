//! 子进程的唯一入口（ADR-001 D8 施工约束 2）。
//!
//! 实现随 agora-xqa.7 一起来：`tokio::task::spawn_blocking` 里跑 `std::process::Command`，
//! 带超时、stderr 有界缓冲、退出码三值。其它模块**不得**直接碰 `std::process::Command` /
//! `tokio::process`——`tests/arch_boundary.rs` 会扫。
