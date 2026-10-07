//! 内嵌前端的 SPA 回退（agora-thc.5；MISSION §6.9 的手机入口 /m）。
//!
//! 未命中静态文件的路径一律回 index.html，但 Content-Type 必须按**实际服务的那个文件**猜：
//! 按请求路径猜时 `/m`（没有扩展名）会被猜成 application/octet-stream，浏览器把它当下载，
//! PWA 白屏。这里钉住回退的 MIME 与回退内容，外加 `/api/*` 不被 SPA 吞掉。
//!
//! 需要 `web/dist` 已构建（CI 先 `npm --prefix web run build` 再 cargo；本地未构建时
//! `spa::serve` 回 503 说明页，这几条会红——那是"前端还没构建"，不是回退逻辑错了）。

mod common;

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use http_body_util::BodyExt;
use tower::ServiceExt;

async fn get(app: &axum::Router, path: &str) -> (StatusCode, String, String) {
    let resp = app
        .clone()
        .oneshot(Request::get(path).body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = resp.status();
    let content_type = resp
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_string();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        content_type,
        String::from_utf8_lossy(&bytes).into_owned(),
    )
}

#[tokio::test]
async fn extensionless_spa_fallback_is_served_as_html() {
    let fx = common::Fx::new();
    let app = fx.app();
    // `/`、`/m`（手机入口）与带 query 的深链都回 index.html；MIME 必须是 text/html，
    // 不是 octet-stream（agora-thc.5 修的正是这一格）。
    for path in ["/", "/m", "/m?session=zuan:abc"] {
        let (status, content_type, body) = get(&app, path).await;
        assert_eq!(status, StatusCode::OK, "{path}");
        assert!(
            content_type.starts_with("text/html"),
            "{path} 的 Content-Type 是 {content_type}（应是 text/html）"
        );
        assert!(
            body.contains("id=\"root\""),
            "{path} 回的不是前端 index.html：{body:.120}"
        );
    }
}

#[tokio::test]
async fn api_paths_are_never_swallowed_by_the_spa_fallback() {
    let fx = common::Fx::new();
    let (status, _, _) = get(&fx.app(), "/api/nope").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}
