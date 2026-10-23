//! 内嵌前端的 SPA 回退（agora-thc.5；MISSION §6.9 的手机入口 /m；agora-f0gv）。
//!
//! **无扩展名**的未命中路径回 index.html，但 Content-Type 必须按**实际服务的那个文件**猜：
//! 按请求路径猜时 `/m`（没有扩展名）会被猜成 application/octet-stream，浏览器把它当下载，
//! PWA 白屏。资源路径（`assets/**`、`*.js`、`*.css`、`favicon.ico` 等）缺了是 404——回
//! index.html 会变成 `200 text/html`，模块脚本 / 样式表按 MIME 被拒（nosniff），同样是白屏，
//! 还会把拼写错误伪装成成功。`/api/*` 与裸 `/api` 不被 SPA 吞掉。
//!
//! 需要 `web/dist` 已构建（CI 先 `npm --prefix web run build` 再 cargo；本地未构建时
//! `spa::serve` 回 503 说明页，这几条会红——那是"前端还没构建"，不是回退逻辑错了）。

mod common;

use std::io::Read;

use axum::body::Body;
use axum::http::{header, HeaderMap, Request, StatusCode};
use http_body_util::BodyExt;
use tower::ServiceExt;

fn header_value(headers: &HeaderMap, name: header::HeaderName) -> String {
    headers
        .get(name)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_string()
}

/// 带请求头的版本：ETag / gzip 的守卫要用到 `If-None-Match` 与 `Accept-Encoding`。
async fn get_with(
    app: &axum::Router,
    path: &str,
    headers: &[(&str, &str)],
) -> (StatusCode, HeaderMap, Vec<u8>) {
    let mut req = Request::get(path);
    for (name, value) in headers {
        req = req.header(*name, *value);
    }
    let resp = app
        .clone()
        .oneshot(req.body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = resp.status();
    let headers = resp.headers().clone();
    let body = resp
        .into_body()
        .collect()
        .await
        .unwrap()
        .to_bytes()
        .to_vec();
    (status, headers, body)
}

async fn get(app: &axum::Router, path: &str) -> (StatusCode, String, String, String) {
    let resp = app
        .clone()
        .oneshot(Request::get(path).body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = resp.status();
    let header_str = |name: header::HeaderName| {
        resp.headers()
            .get(name)
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default()
            .to_string()
    };
    let content_type = header_str(header::CONTENT_TYPE);
    let cache_control = header_str(header::CACHE_CONTROL);
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        content_type,
        cache_control,
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
        let (status, content_type, cache_control, body) = get(&app, path).await;
        assert_eq!(status, StatusCode::OK, "{path}");
        // 旧 HTML 被手机缓存住 → 升级后引用已不存在的旧 assets（2026-10-07 实测）。
        assert_eq!(
            cache_control, "no-cache",
            "{path} 的 index.html 必须 no-cache"
        );
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

/// PWA 壳的两个文件（agora-thc.4）：按名字钉 Content-Type 与 no-cache——SW 拿到缓存副本，
/// 升级后的手机上就会一直停在旧版本（A39）。manifest 还必须指向 /m（手机入口）。
#[tokio::test]
async fn pwa_assets_are_served_with_their_types_and_no_cache() {
    let fx = common::Fx::new();
    let app = fx.app();

    let (status, content_type, cache_control, body) = get(&app, "/manifest.webmanifest").await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        content_type.starts_with("application/manifest+json"),
        "manifest 的 Content-Type 是 {content_type}"
    );
    assert_eq!(cache_control, "no-cache");
    let manifest: serde_json::Value = serde_json::from_str(&body).expect("manifest 是 JSON");
    assert_eq!(manifest["start_url"], "/m");
    assert_eq!(manifest["display"], "standalone");
    assert!(
        manifest["icons"].as_array().is_some_and(|a| a.len() >= 3),
        "manifest 要带 192 / 512 / maskable 图标：{body}"
    );

    let (status, content_type, cache_control, body) = get(&app, "/sw.js").await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        content_type.starts_with("text/javascript"),
        "sw.js 的 Content-Type 是 {content_type}"
    );
    assert_eq!(cache_control, "no-cache");
    // 升级后不能长期停在旧版本（A39）：装完就接管、激活就认领页面。
    assert!(body.contains("skipWaiting"), "sw 要 skipWaiting：{body}");
    assert!(
        body.contains("clients.claim"),
        "sw 要 clients.claim：{body}"
    );
    // 离线壳（agora-vmt6）：导航网络优先 + 超时回缓存；/api 永远直连；assets 才缓存优先。
    assert!(
        body.contains("addEventListener(\"fetch\""),
        "sw 要有 fetch handler（离线壳）：{body}"
    );
    assert!(
        body.contains("mode === \"navigate\""),
        "sw 要区分导航请求：{body}"
    );
    assert!(
        body.contains("startsWith(\"/api/\")"),
        "sw 必须让 /api 直连、不缓存：{body}"
    );
    assert!(
        body.contains("agora-shell-") && body.contains("NAV_TIMEOUT_MS"),
        "sw 要有壳缓存与导航超时：{body}"
    );
}

/// 反向的一半（2026-10-07）：带内容 hash 的构建产物可以 immutable 长缓存——文件一变 URL 就变。
#[tokio::test]
async fn hashed_assets_are_immutable() {
    let fx = common::Fx::new();
    let app = fx.app();
    let (_, _, _, body) = get(&app, "/m").await;
    let asset = body
        .split("src=\"/")
        .nth(1)
        .and_then(|s| s.split('"').next())
        .expect("index.html 里应当有 script src")
        .to_string();
    assert!(asset.starts_with("assets/"), "构建产物应当带 hash：{asset}");
    let (status, content_type, cache_control, _) = get(&app, &format!("/{asset}")).await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        content_type.starts_with("text/javascript") || content_type.starts_with("text/css"),
        "{asset} 的 Content-Type 是 {content_type}"
    );
    assert!(
        cache_control.contains("immutable"),
        "带 hash 的 assets 应当 immutable：{cache_control}"
    );
}

#[tokio::test]
async fn api_paths_are_never_swallowed_by_the_spa_fallback() {
    let fx = common::Fx::new();
    let (status, _, _, _) = get(&fx.app(), "/api/nope").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    // 裸 `/api` 没有尾斜杠，同样不是前端路由（agora-f0gv）。
    let (status, _, _, _) = get(&fx.app(), "/api").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

/// 资源缺失必须是 404（agora-f0gv）：回 index.html 会拿到 `200 text/html`，模块脚本 / 样式表
/// 按 MIME 被拒（`nosniff`），页面停在白屏；拼错的图标路径也被伪装成成功。
///
/// SPA 路由的判据是**无扩展名**：`/`、`/m`、`/m/…` 才回 index.html。真实资源仍走上一条的
/// immutable 长缓存，不能被这条误伤。
#[tokio::test]
async fn missing_assets_are_404_not_the_spa_page() {
    let fx = common::Fx::new();
    let app = fx.app();
    for path in [
        "/assets/index-DOESNOTEXIST.js",
        "/assets/missing.css",
        "/assets/",
        "/assets",
        "/favicon.ico",
    ] {
        let (status, content_type, _, body) = get(&app, path).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{path}");
        assert!(body.is_empty(), "{path} 的 404 不该带 index.html 正文");
        assert!(
            !content_type.starts_with("text/html"),
            "{path} 的 404 类型是 {content_type}（不该是 text/html）"
        );
    }

    // 对照：真实资源仍然 200（类型/缓存由 hashed_assets_are_immutable 钉住）。
    let (_, _, _, html) = get(&app, "/m").await;
    let asset = html
        .split("src=\"/")
        .nth(1)
        .and_then(|s| s.split('"').next())
        .expect("index.html 里应当有 script src")
        .to_string();
    let (status, content_type, _, _) = get(&app, &format!("/{asset}")).await;
    assert_eq!(status, StatusCode::OK, "{asset}");
    assert!(
        content_type.starts_with("text/javascript") || content_type.starts_with("text/css"),
        "{asset} 的 Content-Type 是 {content_type}"
    );
}

/// ETag（agora-vmt6）：`/m` 是 no-cache，但没有 validator 时手机每次冷启动都要重传 1.5 KB
/// HTML；有 ETag 才能 304。ETag 必须随内容变，不能把旧 HTML 的请求误判成新 HTML。
#[tokio::test]
async fn spa_html_revalidates_with_etag() {
    let fx = common::Fx::new();
    let app = fx.app();
    let (status, headers, body) = get_with(&app, "/m", &[]).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(header_value(&headers, header::CACHE_CONTROL), "no-cache");
    let etag = header_value(&headers, header::ETAG);
    assert!(
        etag.starts_with('"') && etag.ends_with('"'),
        "ETag 要带引号：{etag}"
    );
    assert!(!body.is_empty());

    let (status, headers, body) = get_with(&app, "/m", &[("if-none-match", &etag)]).await;
    assert_eq!(status, StatusCode::NOT_MODIFIED);
    assert!(body.is_empty(), "304 不能带正文");
    assert_eq!(header_value(&headers, header::ETAG), etag);

    let (status, _, body) = get_with(&app, "/m", &[("if-none-match", "\"stale\"")]).await;
    assert_eq!(status, StatusCode::OK);
    assert!(!body.is_empty(), "旧 ETag 不能把新 HTML 误判成 304");
}

/// gzip（agora-vmt6）：310 KB 的 entry 在 400 kbps 上要 7 s；只压文本类，尊重 `q=0`，
/// 并给同一 URL 的两种表示不同的强 ETag + `Vary`。
#[tokio::test]
async fn spa_assets_are_gzipped_only_when_accepted() {
    let fx = common::Fx::new();
    let app = fx.app();
    let (_, _, html) = get_with(&app, "/m", &[]).await;
    let html = String::from_utf8(html).expect("index.html 是 UTF-8");
    let asset = html
        .split("src=\"/")
        .nth(1)
        .and_then(|s| s.split('"').next())
        .expect("index.html 里应当有 script src")
        .to_string();

    let (status, headers, identity) = get_with(&app, &format!("/{asset}"), &[]).await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        !headers.contains_key(header::CONTENT_ENCODING),
        "没请求 gzip 时不能压"
    );
    assert!(identity.len() > 1000);

    let (status, headers, gz) =
        get_with(&app, &format!("/{asset}"), &[("accept-encoding", "gzip")]).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(header_value(&headers, header::CONTENT_ENCODING), "gzip");
    assert!(header_value(&headers, header::VARY).contains("Accept-Encoding"));
    let mut decoded = Vec::new();
    flate2::read::GzDecoder::new(gz.as_slice())
        .read_to_end(&mut decoded)
        .expect("gzip 能解码");
    assert_eq!(decoded, identity, "gzip 解码后必须与 identity 完全一致");
    assert!(
        gz.len() < identity.len() / 2,
        "entry 压完应至少小一半：{} → {}",
        identity.len(),
        gz.len()
    );

    // q=0 是明确拒绝，不是「没带这个头」。
    let (_, headers, body) = get_with(
        &app,
        &format!("/{asset}"),
        &[("accept-encoding", "gzip;q=0")],
    )
    .await;
    assert!(!headers.contains_key(header::CONTENT_ENCODING));
    assert_eq!(body, identity);

    // HTML 也压；两种表示的强 ETag 必须不同，否则缓存会跨表示复用 body。
    let (_, identity_headers, _) = get_with(&app, "/m", &[]).await;
    let (status, headers, gz) = get_with(&app, "/m", &[("accept-encoding", "gzip")]).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(header_value(&headers, header::CONTENT_ENCODING), "gzip");
    let mut decoded = Vec::new();
    flate2::read::GzDecoder::new(gz.as_slice())
        .read_to_end(&mut decoded)
        .expect("gzip 能解码");
    assert!(String::from_utf8_lossy(&decoded).contains("id=\"root\""));
    let gz_etag = header_value(&headers, header::ETAG);
    assert_ne!(
        gz_etag,
        header_value(&identity_headers, header::ETAG),
        "gzip 与 identity 的强 ETag 不能相同"
    );
    let (status, _, body) = get_with(
        &app,
        "/m",
        &[("accept-encoding", "gzip"), ("if-none-match", &gz_etag)],
    )
    .await;
    assert_eq!(status, StatusCode::NOT_MODIFIED);
    assert!(body.is_empty());
}

/// 前端构建号必须能被服务端读出来（agora-xu12）：手机端判"我是不是最新版"全靠它。
/// `build.txt` 不带 hash，所以必须 no-cache——缓存住就等于永远说"你是最新的"。
#[tokio::test]
async fn embedded_web_build_is_served_and_reported() {
    let fx = common::Fx::new();
    let app = fx.app();
    let (status, _ct, cache_control, body) = get(&app, "/build.txt").await;
    assert_eq!(status, StatusCode::OK, "web/dist/build.txt 应被内嵌");
    assert_eq!(cache_control, "no-cache");
    let baked = body.trim().to_string();
    assert!(!baked.is_empty(), "build.txt 不该是空的");

    // `/api/system` 报的必须是同一份值：两边不一致的话手机永远以为自己是旧的。
    // `/api/system` 要认证（`build.txt` 不要：它是静态资源、由 SPA 的 fallback 服务）。
    let resp = app
        .clone()
        .oneshot(
            Request::get("/api/system")
                .header(header::COOKIE, fx.cookie())
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    let body =
        String::from_utf8_lossy(&resp.into_body().collect().await.unwrap().to_bytes()).into_owned();
    assert_eq!(status, StatusCode::OK);
    let json: serde_json::Value = serde_json::from_str(&body).expect("system 应是 JSON");
    assert_eq!(json["web_build"], serde_json::Value::String(baked));
}

fn css_token(css: &str, key: &str) -> String {
    let needle = format!("{key}:");
    let start = css
        .find(&needle)
        .unwrap_or_else(|| panic!(":root 里要有 {key}"))
        + needle.len();
    let end = css[start..]
        .find(';')
        .unwrap_or_else(|| panic!("{key} 要有分号"))
        + start;
    let value = css[start..end].trim().to_string();
    assert!(
        value.starts_with('#') && value.len() == 7,
        "{key} 应是十六进制色：{value}"
    );
    value
}

/// PWA 壳的主题色 / 背景色必须跟 `:root` 的 `--bg` 一致（agora-u3nw）：2026-10-09 改过 `--bg`，
/// `index.html` / manifest 三处壳值没跟，浏览器状态栏与 PWA 主题色就停在旧色上，而且没有守卫。
#[test]
fn pwa_shell_colors_follow_the_css_background_token() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let css = std::fs::read_to_string(root.join("web/src/index.css")).expect("web/src/index.css");
    let bg = css_token(&css, "--bg");

    let html = std::fs::read_to_string(root.join("web/index.html")).expect("web/index.html");
    assert!(
        html.contains(&format!("name=\"theme-color\" content=\"{bg}\"")),
        "index.html 的 theme-color 要等于 --bg（{bg}）"
    );

    let manifest = std::fs::read_to_string(root.join("web/public/manifest.webmanifest"))
        .expect("web/public/manifest.webmanifest");
    let json: serde_json::Value = serde_json::from_str(&manifest).expect("manifest 是 JSON");
    assert_eq!(json["theme_color"], bg, "manifest.theme_color 要等于 --bg");
    assert_eq!(
        json["background_color"], bg,
        "manifest.background_color 要等于 --bg"
    );
}

/// 首帧兜底（agora-vmt6）：外链 CSS / entry 还没到时不能是浏览器默认白屏；10 s 看门狗给
/// 一句能照做的提示。颜色必须跟设计令牌同源（否则改 --bg 后兜底又漂）。
#[test]
fn spa_shell_paints_before_js_and_has_a_watchdog() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let css = std::fs::read_to_string(root.join("web/src/index.css")).expect("web/src/index.css");
    let bg = css_token(&css, "--bg");
    let fg = css_token(&css, "--fg");
    let muted = css_token(&css, "--muted");
    let html = std::fs::read_to_string(root.join("web/index.html")).expect("web/index.html");

    assert!(
        html.contains(&format!("background: {bg}")),
        "内联背景要等于 --bg（{bg}）"
    );
    assert!(
        html.contains(&format!("color: {fg}")),
        "内联正文色要等于 --fg（{fg}）"
    );
    assert!(
        html.contains(&format!("color: {muted}")),
        "内联提示色要等于 --muted（{muted}）"
    );
    assert!(html.contains("class=\"boot\""), "要有首帧占位");
    assert!(html.contains("<noscript>"), "禁用 JS 时也要说一句");
    assert!(
        html.contains("setTimeout") && html.contains("10000"),
        "要有 10 s 启动看门狗"
    );
    assert!(html.contains("重新加载"), "看门狗要能让人重新加载");
    let boot = html.find("class=\"boot\"").expect("boot");
    let module = html.find("src=\"/src/main.tsx\"").expect("module script");
    assert!(
        boot < module,
        "占位必须在 entry 之前（否则白屏窗口没有被覆盖）"
    );
}

/// `/m` 的首包不能再带桌面终端（agora-up0p）：`Workspace` 按需加载，entry 里没有 xterm；
/// 桌面 chunk 里仍然有（拆的是加载时机，不是功能）。
#[tokio::test]
async fn mobile_entry_does_not_ship_the_desktop_terminal() {
    let fx = common::Fx::new();
    let app = fx.app();
    let (_, _, _, html) = get(&app, "/m").await;
    let entry = html
        .split("src=\"/")
        .nth(1)
        .and_then(|s| s.split('"').next())
        .expect("index.html 里应当有 script src")
        .to_string();
    let (status, content_type, _, body) = get(&app, &format!("/{entry}")).await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        content_type.starts_with("text/javascript"),
        "{entry} 的 Content-Type 是 {content_type}"
    );
    assert!(
        !body.contains("xterm-helper-textarea"),
        "{entry} 仍带 xterm：Workspace 没有被拆成按需 chunk"
    );

    let assets = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("web/dist/assets");
    let mut found = false;
    for file in std::fs::read_dir(&assets).expect("web/dist/assets") {
        let path = file.expect("asset").path();
        if path.extension().is_some_and(|ext| ext == "js") {
            let text = std::fs::read_to_string(&path).unwrap_or_default();
            found |= text.contains("xterm-helper-textarea");
        }
    }
    assert!(
        found,
        "桌面工作台的 chunk 应当仍带 xterm（只是不在 /m 首包里）"
    );
}
