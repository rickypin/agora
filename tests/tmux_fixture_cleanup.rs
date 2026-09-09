//! agora-quy：fixture 销毁必须连同 tmux 的 socket 文件一起清理。

mod common;

use std::os::unix::net::{UnixListener, UnixStream};
use std::process::Command;

use agora::runtime::tmux::socket_path;
use common::node::TmuxNode;

#[tokio::test]
async fn node_drop_removes_live_and_stale_sockets_after_crash() {
    for stale in [false, true] {
        let mut node = TmuxNode::new();
        node.serve().await;
        let session = node.create_fake("cleanup", "sleep 300000");
        let path = socket_path(&node.socket);
        assert!(UnixStream::connect(&path).is_ok());
        node.crash();
        assert!(node.pane_alive(&session), "crash must preserve the agent");
        if stale {
            assert!(Command::new("tmux")
                .args(["-L", &node.socket, "kill-server"])
                .status()
                .unwrap()
                .success());
            // 确定性重现 server 已退出但文件残留，不依赖某个 tmux 版本是否 unlink。
            let _ = std::fs::remove_file(&path);
            drop(UnixListener::bind(&path).unwrap());
            common::isolate::wait_socket_refuses(&path);
        }
        assert!(path.exists());
        let home = node.home.clone();
        drop(node);
        assert!(!path.exists(), "node left socket {path:?} (stale={stale})");
        assert!(!home.exists());
    }
    drop(TmuxNode::new());
}
