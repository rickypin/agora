//! 内嵌前端（ADR-001 D8：rust-embed）。
//!
//! `web/dist` 在 `cargo build` 时被打进 binary；未先 `npm --prefix web run build`
//! 时目录为空，这里回一个说明页而不是 panic，让 `cargo test` 在没有 node 的环境
//! 也能跑（CI 顺序是先 build 前端再 cargo）。

use axum::{
    http::{header, HeaderValue, StatusCode, Uri},
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
            // PWA 的两个文件按名字钉类型并禁缓存（agora-thc.4）：mime_guess 对 .webmanifest 不一定
            // 认，而 SW 必须每次向网络要新脚本（no-cache），否则升级后手机上停在旧版本（A39）。
            let mime = match served {
                "sw.js" => "text/javascript".to_string(),
                "manifest.webmanifest" => "application/manifest+json".to_string(),
                _ => mime_guess::from_path(served)
                    .first_or_octet_stream()
                    .to_string(),
            };
            let mut resp = ([(header::CONTENT_TYPE, mime)], file.data).into_response();
            // index.html 也要 no-cache：手机把旧 HTML 缓存住后，升级后它引用的旧 assets 在新
            // binary 里已经不存在，表现为白屏或「点了没反应」（2026-10-07 iPhone 验收实测）。
            // 带内容 hash 的 assets 反过来可以长缓存。
            if served == "sw.js" || served == "manifest.webmanifest" || served == "index.html" {
                resp.headers_mut()
                    .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
            } else if served.starts_with("assets/") {
                resp.headers_mut().insert(
                    header::CACHE_CONTROL,
                    HeaderValue::from_static("public, max-age=31536000, immutable"),
                );
            }
            resp
        }
        None => (
            StatusCode::SERVICE_UNAVAILABLE,
            [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
            NOT_BUILT,
        )
            .into_response(),
    }
}
