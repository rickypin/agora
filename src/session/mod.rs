//! Session Manager：metadata 表与生命周期映射（ADR-001 D4；MISSION §4.2 §4.5 §4.6）。
//!
//! SQLite 只存 metadata；status / alive / exit 每次从运行时现算（不变量 7，
//! `tests/schema.rs::no_liveness_columns`）。这里只认 `runtime::Runtime` 与 `status`
//! 给出的抽象，不得出现任何 agent 的 payload 键（ADR-002 规则 5）。

pub mod db;
pub mod manager;
pub mod model;

pub use db::{Db, DbError};
pub use manager::{
    AdoptSession, NewSession, ReconcileReport, SessionError, SessionManager, SessionView,
};
pub use model::{Origin, SessionRecord};
