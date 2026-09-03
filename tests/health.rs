//! A25：单机 daemon 可起并回答 `/api/health` 公开子集。

mod common;

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use http_body_util::BodyExt;
use tower::ServiceExt;

fn app() -> axum::Router {
    common::Fx::new().app()
}

#[tokio::test]
async fn health_public_subset_is_exactly_status_ok() {
    let app = app();
    let resp = app
        .oneshot(Request::get("/api/health").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    // 未认证只能看到 status 这一个键（ADR-003 D1）；多一个键就是泄漏。
    assert_eq!(json, serde_json::json!({ "status": "ok" }));
}

#[tokio::test]
async fn health_full_report_needs_principal() {
    // 带 cookie 才有 runtime / database 等字段（MISSION §10.3）。
    let fx = common::Fx::new();
    let cookie = fx.cookie();
    let resp = fx
        .app()
        .oneshot(
            Request::get("/api/health")
                .header(header::HOST, common::HOST)
                .header(header::COOKIE, cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(json["status"], "ok");
    assert_eq!(json["database"], true);
    assert_eq!(json["runtime"]["status"], "ok");
    assert!(json["runtime"]["path_source"].is_string());
    assert!(json["push"].is_object() && json["peers"].is_object());

    // 明文监听器上的 Bearer：不能靠"降级成公开子集"绕过 401。
    let resp = fx
        .app()
        .oneshot(
            Request::get("/api/health")
                .header(header::AUTHORIZATION, "Bearer apt_x_y")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn system_reports_api_version_and_node() {
    let fx = common::Fx::new();
    let cookie = fx.cookie();
    let resp = fx
        .app()
        .oneshot(
            Request::get("/api/system")
                .header(header::HOST, common::HOST)
                .header(header::COOKIE, cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(json["api_version"], agora::api::API_VERSION);
    assert_eq!(json["node"], common::NODE);
}

#[tokio::test]
async fn unknown_api_path_is_404_not_spa() {
    let app = app();
    let resp = app
        .oneshot(Request::get("/api/nope").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn root_serves_html_or_not_built_notice() {
    let app = app();
    let resp = app
        .oneshot(Request::get("/").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert!(matches!(
        resp.status(),
        StatusCode::OK | StatusCode::SERVICE_UNAVAILABLE
    ));
    let ct = resp.headers()["content-type"].to_str().unwrap().to_string();
    assert!(ct.starts_with("text/html"), "content-type = {ct}");
}
