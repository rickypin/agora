//! ADR-003 D6：AGORA_HOME 0700 自检；unix socket 一问一答。

use agora::local::{self, HomeError, Request, Response};
use std::os::unix::fs::PermissionsExt;

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

#[test]
fn overlong_home_is_refused_with_the_limit_in_the_message() {
    // agora-kkd：socket 路径超过 SUN_LEN 时底层只说 "path must be shorter than SUN_LEN"；
    // 要在起 daemon 之前就说清上限与出路。
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("x".repeat(120));
    let err = local::ensure_home(&home).unwrap_err();
    assert!(matches!(err, HomeError::PathTooLong { .. }), "{err}");
    let msg = err.to_string();
    assert!(
        msg.contains(&local::max_socket_path_len().to_string()),
        "{msg}"
    );
    assert!(msg.contains("AGORA_HOME"), "{msg}");
    assert!(!home.exists(), "太长的目录不该被创建");
}

#[tokio::test]
async fn socket_answers_same_uid_one_line_per_request() {
    // 其他 uid 的拒绝（peer_cred）没法在单用户测试里演；这里验证同 uid 的通路与文件权限。
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("agora.sock");
    let handler: local::Handler = local::sync_handler(|req| match req {
        Request::Ping => Response::Pong,
        Request::Pair { origin } => Response::Pair {
            url: format!("{}/#pair=tok", origin.unwrap_or_else(|| "http://x".into())),
        },
        Request::Hook { .. } => Response::Error {
            message: "not here".into(),
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
    // 没有 daemon → NotRunning，而不是挂住。abort 掉的 serve 会自己删 socket 文件（agora-apr
    // 的清理守卫随 future 一起 drop），但 abort 是异步的，这里不赌它先到。
    let _ = std::fs::remove_file(&path);
    assert!(matches!(
        local::request(&path, &Request::Ping).await,
        Err(local::SocketError::NotRunning(_))
    ));
}

/// ADR-003 D6：对端 uid 不对 → 一个字节不读就关，连错误应答都没有。
/// 单用户机器上造不出第二个 uid，所以把 daemon 期望的 uid 换成别的：同一段代码、同一条路径。
#[tokio::test]
async fn socket_rejects_other_uid() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("agora.sock");
    let handler = local::sync_handler(|_| Response::Pong);
    // SAFETY: getuid 没有前置条件。
    let me = unsafe { libc::getuid() };
    let p = path.clone();
    let server = tokio::spawn(async move { local::serve_for_uid(&p, handler, me + 1).await });
    for _ in 0..50 {
        if path.exists() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    let reply = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        local::request(&path, &Request::Ping),
    )
    .await
    .expect("连接应被立即关闭而不是挂住");
    assert!(
        !matches!(reply, Ok(Response::Pong)),
        "其他 uid 不该得到应答: {reply:?}"
    );
    server.abort();
}

/// agora-apr：退出清理只删自己绑的那个文件。同路径被换成别的实例绑的新文件后，旧句柄的
/// `remove()` 与 Drop 都不动它；活实例在时 `bind` 报 AlreadyRunning，一个字节不动。
#[tokio::test]
async fn cleanup_only_removes_the_socket_it_bound() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("agora.sock");
    let (first, first_cleanup) = local::bind(&path).await.unwrap();
    // 有人在监听（还没 accept 也算：connect 进 backlog 即成功）→ 第二个绑不上、不 unlink。
    assert!(matches!(
        local::bind(&path).await,
        Err(local::SocketError::AlreadyRunning(_))
    ));
    assert!(path.exists());
    // 模拟"路径上换成了别的实例的文件"：把旧文件挪走（保证两个 inode 都活着、绝不复用），
    // 再在原路径绑一个新的。
    let moved = dir.path().join("agora.sock.old");
    std::fs::rename(&path, &moved).unwrap();
    let (second, second_cleanup) = local::bind(&path).await.unwrap();
    assert!(!first_cleanup.remove(), "不是自己绑的文件不该删");
    assert!(path.exists());
    drop(first_cleanup);
    assert!(path.exists(), "Drop 也不删别人的");
    drop(first);
    drop(second);
    assert!(second_cleanup.remove(), "自己绑的才删");
    assert!(!path.exists());
    assert!(!second_cleanup.remove(), "已经没了 → false");
}
