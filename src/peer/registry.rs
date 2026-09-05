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
