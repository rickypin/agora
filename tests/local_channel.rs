//! ADR-003 D6：AGORA_HOME 0700 自检；unix socket 一问一答。

use std::os::unix::fs::PermissionsExt;
use std::sync::Arc;

use agora::local::{self, HomeError, Request, Response};

#[test]
fn home_perms_too_open_refuses_start() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("agora-home");
    // 不存在 → 以 0700 创建。
    local::ensure_home(&home).unwrap();
    assert_eq!(
        std::fs::metadata(&home).unwrap().permissions().mode() & 0o777,
        0o700
    );
    // group / other 任一位 → 拒绝，并把 chmod 命令写进错误。
    std::fs::set_permissions(&home, std::fs::Permissions::from_mode(0o750)).unwrap();
    let err = local::ensure_home(&home).unwrap_err();
    assert!(matches!(err, HomeError::TooOpen { .. }), "{err}");
    assert!(
        err.to_string()
            .contains(&format!("chmod 700 {}", home.display())),
        "{err}"
    );
    std::fs::set_permissions(&home, std::fs::Permissions::from_mode(0o700)).unwrap();
    local::ensure_home(&home).unwrap();
}

#[tokio::test]
async fn socket_answers_same_uid_one_line_per_request() {
    // 其他 uid 的拒绝（peer_cred）没法在单用户测试里演；这里验证同 uid 的通路与文件权限。
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("agora.sock");
    let handler: local::Handler = Arc::new(|req| match req {
        Request::Ping => Response::Pong,
        Request::Pair { origin } => Response::Pair {
            url: format!("{}/#pair=tok", origin.unwrap_or_else(|| "http://x".into())),
        },
    });
    let p = path.clone();
    let server = tokio::spawn(async move { local::serve(&p, handler).await });
    for _ in 0..50 {
        if path.exists() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert_eq!(
        std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert_eq!(
        local::request(&path, &Request::Ping).await.unwrap(),
        Response::Pong
    );
    assert_eq!(
        local::request(&path, &Request::Pair { origin: None })
            .await
            .unwrap(),
        Response::Pair {
            url: "http://x/#pair=tok".into()
        }
    );
    server.abort();
    // 没有 daemon → NotRunning，而不是挂住。
    std::fs::remove_file(&path).unwrap();
    assert!(matches!(
        local::request(&path, &Request::Ping).await,
        Err(local::SocketError::NotRunning(_))
    ));
}
