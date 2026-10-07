//! 推送订阅的存取（SQLite，`push_subscriptions`，schema v7）。
//!
//! 一行 = 一台设备的浏览器订阅（端点是主键，浏览器重复订阅同一端点就更新密钥）。**设备是
//! 订阅的属主**：发送时与 `devices` 联查 `revoked_at IS NULL`，吊销设备即停发——不依赖
//! 清理任务，也不依赖浏览器配合（agora-thc.6 的裁决）。
//!
//! `p256dh` / `auth` 是端点的加密材料，等同秘密：进库、进加密、**不进日志**（失败原因只记
//! 类型，正文与密钥都不记；MISSION §10.1 的日志纪律）。

use std::sync::Arc;

use rusqlite::{params, OptionalExtension};

use crate::session::Db;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PushSubscription {
    pub endpoint: String,
    pub device_id: String,
    pub p256dh: String,
    pub auth: String,
    pub created_at: String,
    pub last_success_at: Option<String>,
    pub last_failure_at: Option<String>,
    /// 最近一次失败的**类型**（`410` / `timeout` / `tls` …），不是错误正文。
    pub failure: Option<String>,
}

#[derive(Clone)]
pub struct PushStore {
    db: Arc<Db>,
}

impl PushStore {
    pub fn new(db: Arc<Db>) -> Self {
        PushStore { db }
    }

    /// 新增或更新一条订阅（端点唯一）。返回该端点此前是否已存在（重订 vs 首订，调用方只记日志）。
    pub fn upsert(
        &self,
        device_id: &str,
        endpoint: &str,
        p256dh: &str,
        auth: &str,
    ) -> Result<bool, rusqlite::Error> {
        let conn = self.db.conn();
        let existed: Option<String> = conn
            .query_row(
                "SELECT endpoint FROM push_subscriptions WHERE endpoint = ?1",
                params![endpoint],
                |r| r.get(0),
            )
            .optional()?;
        conn.execute(
            "INSERT INTO push_subscriptions (endpoint, device_id, p256dh, auth, created_at)
             VALUES (?1, ?2, ?3, ?4, strftime('%Y-%m-%dT%H:%M:%SZ','now'))
             ON CONFLICT(endpoint) DO UPDATE SET
                device_id = excluded.device_id,
                p256dh = excluded.p256dh,
                auth = excluded.auth,
                -- 重订 = 新的订阅：旧失败记录作废。
                failure = NULL,
                last_failure_at = NULL",
            params![endpoint, device_id, p256dh, auth],
        )?;
        Ok(existed.is_some())
    }

    /// 按端点删（幂等）；返回是否真的删掉了一行。
    pub fn remove(&self, endpoint: &str) -> Result<bool, rusqlite::Error> {
        let n = self.db.conn().execute(
            "DELETE FROM push_subscriptions WHERE endpoint = ?1",
            params![endpoint],
        )?;
        Ok(n > 0)
    }

    /// 该发推送的订阅：设备未吊销。`revoked_at` 为空的设备才算——订阅表不复制吊销状态，
    /// 免得两处状态各自过期。
    pub fn list_active(&self) -> Result<Vec<PushSubscription>, rusqlite::Error> {
        let conn = self.db.conn();
        let mut stmt = conn.prepare(
            "SELECT s.endpoint, s.device_id, s.p256dh, s.auth, s.created_at,
                    s.last_success_at, s.last_failure_at, s.failure
             FROM push_subscriptions s
             JOIN devices d ON d.id = s.device_id
             WHERE d.revoked_at IS NULL
             ORDER BY s.created_at",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok(PushSubscription {
                endpoint: r.get(0)?,
                device_id: r.get(1)?,
                p256dh: r.get(2)?,
                auth: r.get(3)?,
                created_at: r.get(4)?,
                last_success_at: r.get(5)?,
                last_failure_at: r.get(6)?,
                failure: r.get(7)?,
            })
        })?;
        rows.collect()
    }

    /// 投递成功：清失败记录、记时刻。
    pub fn note_success(&self, endpoint: &str) -> Result<(), rusqlite::Error> {
        self.db.conn().execute(
            "UPDATE push_subscriptions
             SET last_success_at = strftime('%Y-%m-%dT%H:%M:%SZ','now'), failure = NULL
             WHERE endpoint = ?1",
            params![endpoint],
        )?;
        Ok(())
    }

    /// 投递失败：记**类型**（`timeout` / `tls` / `410` …），不记正文。
    pub fn note_failure(&self, endpoint: &str, kind: &str) -> Result<(), rusqlite::Error> {
        self.db.conn().execute(
            "UPDATE push_subscriptions
             SET last_failure_at = strftime('%Y-%m-%dT%H:%M:%SZ','now'), failure = ?2
             WHERE endpoint = ?1",
            params![endpoint, kind],
        )?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::{Auth, AuthConfig, PairedVia};

    fn store_with_device() -> (PushStore, Arc<Db>, String) {
        let db = Arc::new(Db::open_in_memory().unwrap());
        let auth = Auth::new(db.clone(), AuthConfig::default());
        let token = auth.mint_pair_token(PairedVia::Socket).unwrap();
        let (device, _) = auth.redeem(&token, Some("iPhone"), None).unwrap();
        (PushStore::new(db.clone()), db, device.id)
    }

    #[test]
    fn upsert_updates_keys_and_keeps_one_row_per_endpoint() {
        let (store, _db, device) = store_with_device();
        assert!(!store
            .upsert(&device, "https://web.push.apple.com/A", "k1", "a1")
            .unwrap());
        assert!(store
            .upsert(&device, "https://web.push.apple.com/A", "k2", "a2")
            .unwrap());
        let rows = store.list_active().unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].p256dh, "k2");
        assert_eq!(rows[0].auth, "a2");
        assert!(store.remove("https://web.push.apple.com/A").unwrap());
        assert!(
            !store.remove("https://web.push.apple.com/A").unwrap(),
            "删除幂等"
        );
        assert!(store.list_active().unwrap().is_empty());
    }

    #[test]
    fn revoked_device_stops_receiving_without_touching_rows() {
        let (store, db, device) = store_with_device();
        store
            .upsert(&device, "https://web.push.apple.com/B", "k", "a")
            .unwrap();
        db.conn()
            .execute(
                "UPDATE devices SET revoked_at = strftime('%Y-%m-%dT%H:%M:%SZ','now') WHERE id = ?1",
                params![device],
            )
            .unwrap();
        assert!(
            store.list_active().unwrap().is_empty(),
            "吊销设备即停发：发送侧联查吊销状态，不等清理"
        );
    }

    #[test]
    fn re_subscribing_from_another_device_drops_the_stale_failure() {
        let (store, _db, device) = store_with_device();
        store
            .upsert(&device, "https://web.push.apple.com/C", "k", "a")
            .unwrap();
        store
            .note_failure("https://web.push.apple.com/C", "410")
            .unwrap();
        let rows = store.list_active().unwrap();
        assert_eq!(rows[0].failure.as_deref(), Some("410"));
        store
            .upsert(&device, "https://web.push.apple.com/C", "k2", "a2")
            .unwrap();
        let rows = store.list_active().unwrap();
        assert_eq!(rows[0].failure, None, "重订不该继承旧失败");
        assert_eq!(rows[0].last_failure_at, None);
    }
}
