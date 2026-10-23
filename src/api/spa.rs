//! 内嵌前端（ADR-001 D8：rust-embed）。
//!
//! `web/dist` 在 `cargo build` 时被打进 binary；未先 `npm --prefix web run build`
//! 时目录为空，这里回一个说明页而不是 panic，让 `cargo test` 在没有 node 的环境
//! 也能跑（CI 顺序是先 build 前端再 cargo）。

use std::borrow::Cow;
use std::collections::HashMap;
use std::io::Write;
use std::sync::{Mutex, OnceLock};

use axum::{
    body::{Body, Bytes},
    http::{header, HeaderMap, HeaderValue, StatusCode, Uri},
    response::{IntoResponse, Response},
};
use flate2::{write::GzEncoder, Compression};
use rust_embed::Embed;

#[derive(Embed)]
#[folder = "web/dist/"]
#[exclude = ".gitkeep"]
struct Assets;

const NOT_BUILT: &str = "<!doctype html><meta charset=utf-8><title>agora</title>\
<p>前端尚未构建：<code>npm --prefix web ci &amp;&amp; npm --prefix web run build</code> 后重新 <code>cargo build</code>。</p>";

/// 小于这个长度不压缩：头与 CPU 的固定开销比省下的字节还多。
const GZIP_MIN_BYTES: usize = 256;

/// 同一份内嵌资源只压一次（agora-vmt6）。键是内容 hash：dev 下 rust-embed 每次读磁盘，
/// 改了 `web/dist` 也不会拿到旧压缩体。
static GZIP_CACHE: OnceLock<Mutex<HashMap<u64, Bytes>>> = OnceLock::new();

/// 内嵌前端的构建号（`web/dist/build.txt`，agora-xu12）：`/api/system` 的 `web_build` 用。
/// 前端没构建（`web/dist` 为空）时是 `None`——那时 index.html 都回说明页，谈版本没有意义。
pub fn web_build() -> Option<String> {
    let file = Assets::get("build.txt")?;
    let text = String::from_utf8(file.data.to_vec()).ok()?;
    let trimmed = text.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_owned())
}

/// 资源路径（缺了就是 404）与前端路由（缺了回 index.html）的分界（agora-f0gv）。
///
/// 只看请求路径，不看 `Accept`：`/m`、`/m/…`、`/` 是路由；`assets/**`、`assets`、裸 `api`
/// 以及**最后一段带扩展名**的路径都是资源。此前一律回 index.html，于是缺一个带 hash 的
/// `*.js` / `*.css` 会拿到 `200 text/html`：`nosniff` + 模块脚本/样式表的严格 MIME 校验会拒绝
/// 执行 / 应用，页面停在白屏；拼错的图标路径也被伪装成 200。
fn is_asset_path(path: &str) -> bool {
    // `index.html` 不进这条：前端没构建时它必须落到下面的 503 说明页，而不是 404
    // （本地 `cargo test` 可能没有 node / web/dist）。
    if path == "index.html" {
        return false;
    }
    matches!(path, "api" | "assets")
        || path.starts_with("assets/")
        || path
            .rsplit('/')
            .next()
            .is_some_and(|last| last.contains('.'))
}

/// FNV-1a 64：只做 ETag / gzip 缓存键，不做安全用途（输入是内嵌资源，不是敌意输入）。
fn content_hash(data: &[u8]) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    for byte in data {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

/// 浏览器是否接受 gzip（`gzip;q=0` 表示不接受；`*` 也算，`q` 缺省是 1）。
fn accepts_gzip(headers: &HeaderMap) -> bool {
    headers
        .get_all(header::ACCEPT_ENCODING)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .any(|value| {
            value.split(',').any(|part| {
                let mut bits = part.split(';');
                let coding = bits.next().unwrap_or_default().trim();
                if !(coding.eq_ignore_ascii_case("gzip") || coding == "*") {
                    return false;
                }
                bits.find_map(|bit| bit.trim().strip_prefix("q="))
                    .and_then(|q| q.parse::<f32>().ok())
                    .is_none_or(|q| q > 0.0)
            })
        })
}

/// `If-None-Match` 的强 / 弱比较（只服务 GET，不需要 `If-Match` 的语义）。
fn etag_matches(if_none_match: &str, etag: &str) -> bool {
    if_none_match.split(',').any(|candidate| {
        let candidate = candidate.trim();
        candidate == "*" || candidate == etag || candidate.strip_prefix("W/") == Some(etag)
    })
}

fn compressible(mime: &str) -> bool {
    mime.starts_with("text/")
        || matches!(
            mime,
            "application/javascript"
                | "application/json"
                | "application/manifest+json"
                | "image/svg+xml"
        )
}

fn gzip_bytes(hash: u64, data: &[u8]) -> Bytes {
    let cache = GZIP_CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    // 压缩在锁内做：一个资源只在第一次请求时压一次，10 ms 级别，比放锁外重复压简单。
    let mut cache = cache
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(hit) = cache.get(&hash) {
        return hit.clone();
    }
    let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
    encoder
        .write_all(data)
        .expect("gzip 写入内存缓冲区不会失败");
    let compressed = Bytes::from(encoder.finish().expect("gzip 结束内存流不会失败"));
    cache.insert(hash, compressed.clone());
    compressed
}

pub async fn serve(uri: Uri, headers: HeaderMap) -> Response {
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
        // 资源缺失是 404，不是 SPA 页面（agora-f0gv）；真实资源仍由上面的分支返回。
        None if is_asset_path(candidate) => return StatusCode::NOT_FOUND.into_response(),
        None => ("index.html", Assets::get("index.html")),
    };

    let Some(file) = file else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
            NOT_BUILT,
        )
            .into_response();
    };

    // PWA 的两个文件按名字钉类型并禁缓存（agora-thc.4）：mime_guess 对 .webmanifest 不一定
    // 认，而 SW 必须每次向网络要新脚本（no-cache），否则升级后手机上停在旧版本（A39）。
    let mime = match served {
        "sw.js" => "text/javascript".to_string(),
        "manifest.webmanifest" => "application/manifest+json".to_string(),
        _ => mime_guess::from_path(served)
            .first_or_octet_stream()
            .to_string(),
    };
    let data = file.data;
    let hash = content_hash(&data);
    let gzip = compressible(&mime) && data.len() >= GZIP_MIN_BYTES && accepts_gzip(&headers);
    // 不同 Content-Encoding 是不同表示，强 ETag 要分开：否则同一个 If-None-Match 会跨表示 304，
    // 缓存可能把 identity 的 body 当成 gzip 的 body 复用。
    let etag = if gzip {
        format!("\"{hash:016x}-{}-gz\"", data.len())
    } else {
        format!("\"{hash:016x}-{}\"", data.len())
    };

    let mut builder = Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, mime)
        .header(header::ETAG, etag.clone());
    // index.html 也要 no-cache：手机把旧 HTML 缓存住后，升级后它引用的旧 assets 在新
    // binary 里已经不存在，表现为白屏或「点了没反应」（2026-10-07 iPhone 验收实测）。
    // 带内容 hash 的 assets 反过来可以长缓存。
    // build.txt 是"手机上的前端是不是最新"的判据（agora-xu12），缓存住就失去意义。
    if matches!(
        served,
        "sw.js" | "manifest.webmanifest" | "index.html" | "build.txt"
    ) {
        builder = builder.header(header::CACHE_CONTROL, "no-cache");
    } else if served.starts_with("assets/") {
        builder = builder.header(
            header::CACHE_CONTROL,
            HeaderValue::from_static("public, max-age=31536000, immutable"),
        );
    }
    if gzip {
        builder = builder.header(header::CONTENT_ENCODING, "gzip");
        // 同一 URL 有两种表示，必须让缓存按 Accept-Encoding 分开存。
        builder = builder.header(header::VARY, "Accept-Encoding");
    }

    if headers
        .get(header::IF_NONE_MATCH)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| etag_matches(value, &etag))
    {
        // 304 仍带 ETag / Cache-Control / Vary：缓存拿到后更新元数据，不用重传 1.5 KB HTML。
        return builder
            .status(StatusCode::NOT_MODIFIED)
            .body(Body::empty())
            .expect("304 的静态响应头合法");
    }

    let body = if gzip {
        Body::from(gzip_bytes(hash, &data))
    } else {
        match data {
            Cow::Borrowed(bytes) => Body::from(Bytes::from_static(bytes)),
            Cow::Owned(bytes) => Body::from(Bytes::from(bytes)),
        }
    };
    builder.body(body).expect("静态响应头合法")
}
