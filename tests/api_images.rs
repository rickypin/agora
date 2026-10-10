//! `POST /api/sessions/:id/images`（agora-lmz2；MISSION §6.9；docs/spec/api.md「附图」）：图落进会话
//! 工作目录下自我忽略的 `.agora-uploads/`、返回绝对路径；只收图片魔数；没有文本通道的行、
//! 符号链接目录一律不写。peer 一跳转发见 `tests/forward.rs::image_upload_forwarded_to_owner`。

mod common;

use std::path::Path;

use axum::body::Body;
use axum::http::{header, Method, Request, StatusCode};
use base64::Engine;
use http_body_util::BodyExt;
use serde_json::{json, Value};
use tower::ServiceExt;

use common::{Fx, HOST};

const PNG: &[u8] = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR\0\0\0\x01\0\0\0\x01\x08\x02\0\0\0";

fn b64(bytes: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

async fn call(fx: &Fx, method: Method, path: &str, body: Option<Value>) -> (StatusCode, Value) {
    let mut req = Request::builder()
        .method(method)
        .uri(path)
        .header(header::HOST, HOST)
        .header(header::ORIGIN, format!("http://{HOST}"))
        .header(header::COOKIE, fx.cookie());
    let body = match body {
        Some(v) => {
            req = req.header(header::CONTENT_TYPE, "application/json");
            Body::from(v.to_string())
        }
        None => Body::empty(),
    };
    let resp = fx.app().oneshot(req.body(body).unwrap()).await.unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

async fn create_in(fx: &Fx, dir: &Path) -> String {
    let (status, body) = call(
        fx,
        Method::POST,
        "/api/sessions",
        Some(json!({ "display_name": "w", "agent_type": "shell", "working_directory": dir })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    body["id"].as_str().unwrap().to_owned()
}

async fn upload(fx: &Fx, gid: &str, data: &str) -> (StatusCode, Value) {
    call(
        fx,
        Method::POST,
        &format!("/api/sessions/{gid}/images"),
        Some(json!({ "data": data })),
    )
    .await
}

fn git(dir: &Path, args: &[&str]) -> Option<String> {
    let out = std::process::Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .output()
        .ok()?;
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    Some(String::from_utf8_lossy(&out.stdout).into_owned())
}

#[tokio::test]
async fn an_image_lands_in_the_working_directory_and_git_stays_clean() {
    let fx = Fx::new();
    let wd = tempfile::tempdir().unwrap();
    let has_git = git(wd.path(), &["init", "-q"]).is_some();
    let gid = create_in(&fx, wd.path()).await;

    let (status, body) = upload(&fx, &gid, &b64(PNG)).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let path = Path::new(body["path"].as_str().unwrap());
    assert!(path.is_absolute(), "{body}");
    assert_eq!(path.parent().unwrap(), wd.path().join(".agora-uploads"));
    assert_eq!(path.extension().unwrap(), "png", "扩展名按魔数定");
    assert_eq!(std::fs::read(path).unwrap(), PNG);
    assert_eq!(
        std::fs::read_to_string(wd.path().join(".agora-uploads/.gitignore")).unwrap(),
        "*\n"
    );
    if has_git {
        // 自我忽略：仓库里多了一个目录，git 一个字都不说（agora 不改仓库的 git 状态，MISSION §1.4）。
        assert_eq!(git(wd.path(), &["status", "--porcelain"]).unwrap(), "");
    }

    // 第二张不覆盖第一张。
    let (_, again) = upload(&fx, &gid, &b64(PNG)).await;
    assert_ne!(again["path"], body["path"]);
}

#[tokio::test]
async fn non_images_and_bad_base64_are_rejected_without_writing() {
    let fx = Fx::new();
    let wd = tempfile::tempdir().unwrap();
    let gid = create_in(&fx, wd.path()).await;
    for data in [
        b64(b"#!/bin/sh\nrm -rf ~\n"),
        b64(b"<svg/>"),
        "%%%not base64".to_owned(),
        String::new(),
    ] {
        let (status, body) = upload(&fx, &gid, &data).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{data:?}: {body}");
        assert_eq!(body["error"], "bad_image", "{body}");
    }
    assert!(
        !wd.path().join(".agora-uploads").exists(),
        "没过校验不该建目录"
    );
}

#[tokio::test]
async fn oversized_images_are_refused() {
    let fx = Fx::new();
    let wd = tempfile::tempdir().unwrap();
    let gid = create_in(&fx, wd.path()).await;
    let mut big = PNG.to_vec();
    big.resize(agora::session::images::MAX_BYTES + 1, 0);
    let (status, body) = upload(&fx, &gid, &b64(&big)).await;
    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE, "{body}");
    assert_eq!(body["error"], "image_too_large");
    assert!(!wd.path().join(".agora-uploads").exists());
}

#[tokio::test]
async fn a_symlinked_upload_dir_is_refused() {
    // 仓库里提交了一个同名符号链接：顺着它写就写到了工作目录之外。
    let fx = Fx::new();
    let wd = tempfile::tempdir().unwrap();
    let elsewhere = tempfile::tempdir().unwrap();
    std::os::unix::fs::symlink(elsewhere.path(), wd.path().join(".agora-uploads")).unwrap();
    let gid = create_in(&fx, wd.path()).await;
    let (status, body) = upload(&fx, &gid, &b64(PNG)).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["error"], "unsafe_upload_dir");
    assert_eq!(std::fs::read_dir(elsewhere.path()).unwrap().count(), 0);
}

#[tokio::test]
async fn rows_without_a_text_channel_take_no_images() {
    // 采纳行只读（text_via = none）：图交不出去，就不在它的目录里留文件。
    let fx = Fx::new();
    fx.rt.insert("fake:default:manual", true, None, false);
    let (status, body) = call(
        &fx,
        Method::POST,
        "/api/sessions/adopt",
        Some(json!({ "runtime_ref": "fake:default:manual", "agent_type": "shell" })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert_eq!(body["text_via"], "none", "{body}");
    let gid = body["id"].as_str().unwrap().to_owned();
    let (status, body) = upload(&fx, &gid, &b64(PNG)).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["error"], "no_runtime");

    let (status, body) = upload(&fx, "nope", &b64(PNG)).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
}

#[tokio::test]
async fn an_unknown_session_subpath_is_405_which_is_what_an_older_node_answers() {
    // 版本错位（api.md「附图」）：没有这个端点的老节点对 POST 走 SPA 兜底、回 405——页面靠它
    // 说「那台节点版本旧」，而不是把图静默丢掉只发文字。新节点上同一形态的未知子路径也必须是 405。
    let fx = Fx::new();
    let (status, _) = upload_to(&fx, "/api/sessions/x:1/no-such-endpoint").await;
    assert_eq!(status, StatusCode::METHOD_NOT_ALLOWED);
}

async fn upload_to(fx: &Fx, path: &str) -> (StatusCode, Value) {
    call(fx, Method::POST, path, Some(json!({ "data": b64(PNG) }))).await
}
