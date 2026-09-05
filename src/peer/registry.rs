//! 节点名 → [`PeerTransport`]（agora-7ku.11 ②；MISSION §3.5；ADR-004）。
//!
//! 一张启动时装好的表：本机 `node.id` 不在其中——本机不是自己的 peer，本机会话走直达。
//! 查不到的名字就是 `node_unknown` 的依据：会话 id `<node>:<id>` 的前缀既不是本机又不是任何
//! 已配置的 peer，转发层（agora-7ku.7）只能 404（docs/spec/api.md 错误类型表）。

use std::collections::BTreeMap;
use std::fmt;
use std::sync::Arc;

use super::transport::PeerTransport;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum RegistryError {
    /// `peers[].name` 写成了本机 `node.id`：本机不是自己的 peer。
    #[error("peer 名 {0:?} 就是本机 node.id")]
    LocalName(String),
    #[error("peer 名 {0:?} 重复")]
    Duplicate(String),
}

/// 一个节点名该往哪走。
#[derive(Clone)]
pub enum Route {
    /// 本机：直达 SessionManager，不经传输层。
    Local,
    /// 已配置的 peer：经它的 transport 一跳转发。
    Peer(Arc<dyn PeerTransport>),
    /// 既不是本机也不是任何 peer → `node_unknown`。
    Unknown,
}

impl fmt::Debug for Route {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Route::Local => f.write_str("Local"),
            Route::Peer(t) => write!(f, "Peer({})", t.name()),
            Route::Unknown => f.write_str("Unknown"),
        }
    }
}

pub struct PeerRegistry {
    local: String,
    peers: BTreeMap<String, Arc<dyn PeerTransport>>,
}

impl PeerRegistry {
    /// `local` 是本机 `node.id`。
    pub fn new(local: &str) -> Self {
        PeerRegistry {
            local: local.to_owned(),
            peers: BTreeMap::new(),
        }
    }

    pub fn local(&self) -> &str {
        &self.local
    }

    /// 键取 `transport.name()`：名字只有一个出处，表里的键与 transport 自报的名不会对不上。
    pub fn insert(&mut self, transport: Arc<dyn PeerTransport>) -> Result<(), RegistryError> {
        let name = transport.name().to_owned();
        if name == self.local {
            return Err(RegistryError::LocalName(name));
        }
        if self.peers.contains_key(&name) {
            return Err(RegistryError::Duplicate(name));
        }
        self.peers.insert(name, transport);
        Ok(())
    }

    pub fn route(&self, node: &str) -> Route {
        if node == self.local {
            return Route::Local;
        }
        match self.peers.get(node) {
            Some(t) => Route::Peer(t.clone()),
            None => Route::Unknown,
        }
    }

    /// 只查 peer；本机名与未知名一样是 `None`（要区分用 [`route`](Self::route)）。
    pub fn get(&self, node: &str) -> Option<Arc<dyn PeerTransport>> {
        self.peers.get(node).cloned()
    }

    pub fn is_local(&self, node: &str) -> bool {
        node == self.local
    }

    /// 全部 peer，按名字序；客户端任务（agora-7ku.5）每个起一个。
    pub fn peers(&self) -> impl Iterator<Item = &Arc<dyn PeerTransport>> {
        self.peers.values()
    }

    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.peers.keys().map(String::as_str)
    }

    pub fn len(&self) -> usize {
        self.peers.len()
    }

    pub fn is_empty(&self) -> bool {
        self.peers.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::peer::transport::{BoxFuture, PeerWs, TransportError};
    use axum::body::Body;
    use axum::http::{Request, Response};
    use std::time::Duration;

    /// 只有名字的 transport：注册表只看 `name()`。
    struct Named(&'static str);

    impl PeerTransport for Named {
        fn name(&self) -> &str {
            self.0
        }
        fn timeout(&self) -> Duration {
            Duration::ZERO
        }
        fn request(
            &self,
            _req: Request<Body>,
        ) -> BoxFuture<'_, Result<Response<Body>, TransportError>> {
            unreachable!("注册表测试不发请求")
        }
        fn connect_ws<'a>(
            &'a self,
            _path: &'a str,
        ) -> BoxFuture<'a, Result<PeerWs, TransportError>> {
            unreachable!("注册表测试不建 WS")
        }
    }

    #[test]
    fn unknown_node_is_not_found_and_local_is_not_a_peer() {
        let mut reg = PeerRegistry::new("mac");
        assert!(reg.is_empty());
        reg.insert(Arc::new(Named("zuan"))).unwrap();

        assert!(matches!(reg.route("mac"), Route::Local));
        assert!(matches!(reg.route("zuan"), Route::Peer(t) if t.name() == "zuan"));
        // 既不是本机也不是 peer：node_unknown 的依据（docs/spec/api.md）。
        assert!(matches!(reg.route("nobody"), Route::Unknown));
        assert!(reg.get("nobody").is_none());
        // `get` 只查 peer：本机名也查不到，区分要用 route。
        assert!(reg.get("mac").is_none());
        assert!(reg.is_local("mac") && !reg.is_local("zuan"));
        assert_eq!(reg.names().collect::<Vec<_>>(), ["zuan"]);
        assert_eq!(reg.len(), 1);
    }

    #[test]
    fn local_name_and_duplicates_are_refused() {
        let mut reg = PeerRegistry::new("mac");
        assert_eq!(
            reg.insert(Arc::new(Named("mac"))).unwrap_err(),
            RegistryError::LocalName("mac".into())
        );
        reg.insert(Arc::new(Named("zuan"))).unwrap();
        assert_eq!(
            reg.insert(Arc::new(Named("zuan"))).unwrap_err(),
            RegistryError::Duplicate("zuan".into())
        );
        assert_eq!(reg.len(), 1, "被拒的插入不改表");
    }
}
