//! A25：单机 daemon 可起并回答 `/api/health` 公开子集。

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use tower::ServiceExt;

#[tokio::test]
async fn health_public_subset_is_exactly_status_ok() {
    let app = agora::api::router();
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
async fn unknown_api_path_is_404_not_spa() {
    let app = agora::api::router();
    let resp = app
        .oneshot(Request::get("/api/nope").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn root_serves_html_or_not_built_notice() {
    let app = agora::api::router();
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
