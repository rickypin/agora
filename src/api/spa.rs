//! 内嵌前端（ADR-001 D8：rust-embed）。
//!
//! `web/dist` 在 `cargo build` 时被打进 binary；未先 `npm --prefix web run build`
//! 时目录为空，这里回一个说明页而不是 panic，让 `cargo test` 在没有 node 的环境
//! 也能跑（CI 顺序是先 build 前端再 cargo）。

use axum::{
    http::{header, StatusCode, Uri},
    response::{IntoResponse, Response},
};
use rust_embed::Embed;

#[derive(Embed)]
#[folder = "web/dist/"]
#[exclude = ".gitkeep"]
struct Assets;

const NOT_BUILT: &str = "<!doctype html><meta charset=utf-8><title>agora</title>\
<p>前端尚未构建：<code>npm --prefix web ci &amp;&amp; npm --prefix web run build</code> 后重新 <code>cargo build</code>。</p>";

pub async fn serve(uri: Uri) -> Response {
    let path = uri.path().trim_start_matches('/');
    if path.starts_with("api/") {
        return StatusCode::NOT_FOUND.into_response();
    }

    // 命中静态文件就返回，否则退回 index.html（SPA 路由；ADR-003 的配对链接走 fragment）。
    // MIME 要按**实际返回的那个文件**猜：以前按请求路径猜，未带扩展名的 /m 回退到 index.html 时
    // 被猜成 application/octet-stream，浏览器把它当下载，PWA 直接白屏（agora-thc.5）。
    let candidate = if path.is_empty() { "index.html" } else { path };
    let (served, file) = match Assets::get(candidate) {
        Some(file) => (candidate, Some(file)),
        None => ("index.html", Assets::get("index.html")),
    };

    match file {
        Some(file) => {
            let mime = mime_guess::from_path(served).first_or_octet_stream();
            ([(header::CONTENT_TYPE, mime.as_ref())], file.data).into_response()
        }
        None => (
            StatusCode::SERVICE_UNAVAILABLE,
            [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
            NOT_BUILT,
        )
            .into_response(),
    }
}
