//! `GET /api/health`。
//!
//! 未认证只回 `{ "status": "ok" }`（MISSION §10.3；ADR-003 D1 白名单）。
//! 认证后的完整报告（runtime / database / tls / push / peers）随 Principal 提取器在
//! agora-xqa.9 加上；这里刻意不暴露任何能区分节点配置的字段。

use axum::Json;
use serde::Serialize;

#[derive(Debug, Serialize)]
pub struct PublicHealth {
    pub status: &'static str,
}

pub async fn public() -> Json<PublicHealth> {
    Json(PublicHealth { status: "ok" })
}
