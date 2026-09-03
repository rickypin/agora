//! ADR-003 D5：明文监听器只在 loopback。TLS 监听器随 agora-7ku.2。

use agora::api::{plaintext_listen, ListenError};

#[test]
fn plaintext_listener_refuses_non_loopback() {
    for bad in [
        "0.0.0.0:7680",
        "192.168.1.2:7680",
        "[::]:7680",
        "100.64.0.1:7680",
    ] {
        assert!(
            matches!(plaintext_listen(bad), Err(ListenError::NotLoopback(_))),
            "{bad} 应被拒绝"
        );
    }
    assert!(matches!(
        plaintext_listen("not an address"),
        Err(ListenError::Parse(_))
    ));
    for ok in ["127.0.0.1:7680", "127.0.0.2:1", "[::1]:7680"] {
        assert!(plaintext_listen(ok).is_ok(), "{ok} 是 loopback");
    }
}
