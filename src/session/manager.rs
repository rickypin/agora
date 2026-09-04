//! 生命周期映射（ADR-001 D4）：会话 = metadata 行 ⨝ 运行时会话，每次现算。
//!
//! 所有方法同步阻塞（内部会起运行时子进程），API 层用 `spawn_blocking` 调。

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::clock::age_secs;

use rusqlite::{params, OptionalExtension, Row};
use serde::Serialize;

use super::db::{Db, DbError};
use super::model::{Origin, SessionRecord};
use crate::runtime::{
    AttachSpec, LaunchSpec, Runtime, RuntimeError, RuntimeRef, RuntimeSession, RuntimeStatus, Size,
};
use crate::status::{self, Assessment, Status};

/// Kill 的宽限：TERM → 5 s → KILL（ADR-001 D2）。
pub const KILL_GRACE: Duration = Duration::from_secs(5);
/// 运行时名前缀（MISSION §4.5；config 里运行时段的 `prefix`）。
pub const RUNTIME_NAME_PREFIX: &str = "ag-";

#[derive(Debug, thiserror::Error)]
pub enum SessionError {
    #[error("会话不存在: {0}")]
    NotFound(String),
    /// 清理只对已退出的会话（MISSION §4.6）。
    #[error("会话进程仍在运行: {0}")]
    StillAlive(String),
    /// external 会话没有运行时句柄，不能 kill / restart / cleanup。
    #[error("会话没有运行时会话: {0}")]
    NoRuntime(String),
    /// adopt 一个已经登记过的运行时会话。
    #[error("运行时会话已登记: {0}")]
    AlreadyRegistered(String),
    #[error(transparent)]
    Runtime(#[from] RuntimeError),
    #[error(transparent)]
    Db(#[from] DbError),
}

impl From<rusqlite::Error> for SessionError {
    fn from(e: rusqlite::Error) -> Self {
        SessionError::Db(DbError::Sql(e))
    }
}

#[derive(Debug, Clone)]
pub struct NewSession {
    pub display_name: String,
    pub agent_type: String,
    pub working_directory: PathBuf,
    pub worktree: Option<String>,
    pub task_ref: Option<String>,
    pub command: String,
    pub env: Vec<(String, String)>,
    pub size: Size,
}

#[derive(Debug, Clone)]
pub struct AdoptSession {
    pub runtime_ref: String,
    pub display_name: Option<String>,
    pub agent_type: Option<String>,
    pub working_directory: Option<PathBuf>,
}

/// 对外的一条会话：metadata + 运行时实时事实 + 状态判定。
#[derive(Debug, Clone, Serialize)]
pub struct SessionView {
    #[serde(flatten)]
    pub record: SessionRecord,
    /// 一行显示的名字：没改过名时 pane title 赢，改过之后 display_name 永远赢（§4.5）。
    pub name: String,
    pub alive: bool,
    pub exit: Option<crate::runtime::Exit>,
    pub pid: Option<u32>,
    pub managed: bool,
    #[serde(flatten)]
    pub assessment: Assessment,
}

impl SessionView {
    /// Kill / Restart 会不会真的杀掉一个正在干活的 agent（MISSION §8 "确认跟着杀走"）。
    /// 依据是 agent 状态而非运行时会话是否 alive：FINISHED / FAILED 的会话进程已经不在，
    /// 运行时会话（dead pane）还在也不算；运行时会话不在了更不算。其余（含 UNKNOWN）都算——
    /// 不知道它在不在干活时宁可多问一次。
    pub fn would_kill(&self) -> bool {
        self.assessment.source != status::Source::None
            && !matches!(self.assessment.status, Status::Finished | Status::Failed)
    }
}

/// daemon 重启时 `list()` ⨝ SQLite 的六种情况（ADR-001 D4）。
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize)]
pub struct ReconcileReport {
    /// 已知 ref 活着 → 重建视图。
    pub known_alive: Vec<String>,
    /// 已知 ref 已死 → 按 exit 报 FINISHED / FAILED，ended_at 补齐。
    pub known_dead: Vec<String>,
    /// 已知 ref 不在 → ended_at 补今天、UNKNOWN。
    pub known_missing: Vec<String>,
    /// 未知 ref 在 agora socket → 未注册（多半是库丢了）。
    pub unregistered_managed: Vec<RuntimeRef>,
    /// 未知 ref 在采纳 socket → 可采纳的未注册会话。
    pub unregistered_adoptable: Vec<RuntimeRef>,
    /// origin = external：没有运行时句柄，只有 hook 看得见。
    pub external: Vec<String>,
}

/// 没有任何内存状态：会话 = metadata 行 ⨝ 运行时会话，daemon 重启后视图与重启前一致。
/// "用户 Kill 过"曾是内存集合，重启后被 Kill 的会话变 FAILED（agora-xqa.16），现落 `killed_at`。
pub struct SessionManager {
    db: Arc<Db>,
    runtime: Arc<dyn Runtime>,
    prefix: String,
    /// 运行时可用性的实时结论（ADR-001 D7）：每次读运行时都会更新它，`/api/health` 读它。
    runtime_status: Arc<RuntimeStatus>,
}

impl SessionManager {
    pub fn new(db: Arc<Db>, runtime: Arc<dyn Runtime>) -> Self {
        Self::with_prefix(db, runtime, RUNTIME_NAME_PREFIX)
    }

    /// 运行时名前缀来自配置（`runtime.<kind>.prefix`）。
    pub fn with_prefix(db: Arc<Db>, runtime: Arc<dyn Runtime>, prefix: &str) -> Self {
        SessionManager {
            db,
            runtime,
            prefix: prefix.to_owned(),
            runtime_status: Arc::new(RuntimeStatus::default()),
        }
    }

    /// `/api/health` 与 daemon 启动流程共用的那一个实时结论。
    pub fn runtime_status(&self) -> &Arc<RuntimeStatus> {
        &self.runtime_status
    }

    pub fn db(&self) -> &Db {
        &self.db
    }

    /// 同一个库的另一个持有者（`project::Projects`）：projects 表与 sessions 表同库。
    pub fn db_handle(&self) -> Arc<Db> {
        self.db.clone()
    }

    // ---------- 创建 ----------

    /// 先外部资源后 metadata（不变量 7 同构）：写库失败 → remove 运行时会话回滚。
    pub fn create(&self, new: &NewSession) -> Result<SessionView, SessionError> {
        let id = self.fresh_id()?;
        let runtime_name = format!("{}{id}", self.prefix);
        let mut env = new.env.clone();
        env.push(("AGORA_SESSION_ID".into(), id.clone()));
        env.push(("AGORA_EPOCH".into(), "1".into()));
        let spec = LaunchSpec {
            name: runtime_name,
            command: new.command.clone(),
            cwd: new.working_directory.clone(),
            env,
            size: new.size,
        };
        let r#ref = self.runtime.create(&spec)?;

        let inserted = self.db.conn().execute(
            "INSERT INTO sessions (id, runtime_ref, display_name, name_locked, agent_type,
                working_directory, worktree, task_ref, command, epoch, created_at, spawned_at,
                updated_at, origin)
             VALUES (?1, ?2, ?3, 0, ?4, ?5, ?6, ?7, ?8, 1, strftime('%Y-%m-%dT%H:%M:%SZ','now'),
                strftime('%Y-%m-%dT%H:%M:%SZ','now'), strftime('%Y-%m-%dT%H:%M:%SZ','now'),
                'agora')",
            params![
                id,
                r#ref.0,
                new.display_name,
                new.agent_type,
                new.working_directory.to_string_lossy(),
                new.worktree,
                new.task_ref,
                new.command,
            ],
        );
        if let Err(err) = inserted {
            tracing::warn!(component = "session", %id, %err, "写 metadata 失败，回滚运行时会话");
            // 刚创建、还没人看过，直接杀掉再删；两步任一失败都只记日志——库错误才是主因。
            if let Err(e) = self.runtime.terminate(&r#ref, Duration::from_secs(1)) {
                tracing::warn!(component = "session", %id, %e, "回滚 terminate 失败");
            }
            if let Err(e) = self.runtime.remove(&r#ref) {
                tracing::warn!(component = "session", %id, %e, "回滚 remove 失败");
            }
            return Err(err.into());
        }
        self.get(&id)
    }

    fn fresh_id(&self) -> Result<String, SessionError> {
        for attempt in 0u32..16 {
            let nanos = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0);
            let mix = nanos ^ ((std::process::id() as u128) << 40) ^ ((attempt as u128) << 96);
            let id = format!("{:06x}", (fnv(mix) & 0xff_ffff) as u32);
            let exists: bool = self.db.conn().query_row(
                "SELECT EXISTS(SELECT 1 FROM sessions WHERE id = ?1)",
                [&id],
                |r| r.get(0),
            )?;
            if !exists {
                return Ok(id);
            }
        }
        Err(SessionError::Db(DbError::Sql(
            rusqlite::Error::QueryReturnedNoRows,
        )))
    }

    // ---------- 采纳（MISSION §5.5） ----------

    /// 把运行时里一个未注册的会话登记进 metadata：`origin = adopted`，cwd 取运行时的现状，
    /// `spawned_at` 留空（不知道它什么时候起的，不算 STARTING）。已登记的 ref 直接报重复。
    pub fn adopt(&self, adopt: &AdoptSession) -> Result<SessionView, SessionError> {
        let r#ref = RuntimeRef(adopt.runtime_ref.clone());
        let live = self.runtime.inspect(&r#ref)?;
        let exists: bool = self.db.conn().query_row(
            "SELECT EXISTS(SELECT 1 FROM sessions WHERE runtime_ref = ?1)",
            [&adopt.runtime_ref],
            |r| r.get(0),
        )?;
        if exists {
            return Err(SessionError::AlreadyRegistered(adopt.runtime_ref.clone()));
        }
        let id = self.fresh_id()?;
        let display_name = adopt
            .display_name
            .clone()
            .filter(|n| !n.trim().is_empty())
            .unwrap_or_else(|| live.name.clone());
        let agent_type = adopt
            .agent_type
            .clone()
            .filter(|n| !n.trim().is_empty())
            .unwrap_or_else(|| "unknown".into());
        let cwd = adopt
            .working_directory
            .clone()
            .unwrap_or_else(|| live.cwd.clone());
        self.db.conn().execute(
            "INSERT INTO sessions (id, runtime_ref, display_name, name_locked, agent_type,
                working_directory, epoch, created_at, updated_at, origin)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, 1, strftime('%Y-%m-%dT%H:%M:%SZ','now'),
                strftime('%Y-%m-%dT%H:%M:%SZ','now'), 'adopted')",
            params![
                id,
                adopt.runtime_ref,
                display_name,
                adopt.display_name.is_some(),
                agent_type,
                cwd.to_string_lossy(),
            ],
        )?;
        self.get(&id)
    }

    /// 运行时里有、metadata 里没有的会话（Unknown Agent，可采纳；A1 列表含全部运行时会话）。
    pub fn unregistered(&self) -> Result<Vec<RuntimeSession>, SessionError> {
        let known: HashSet<String> = self
            .all_records()?
            .into_iter()
            .filter_map(|r| r.runtime_ref)
            .collect();
        Ok(self
            .live_all()?
            .0
            .into_iter()
            .filter(|s| !known.contains(&s.r#ref.0))
            .collect())
    }

    // ---------- 读 ----------

    pub fn list(&self) -> Result<Vec<SessionView>, SessionError> {
        let records = self.all_records()?;
        let (live, degraded) = self.live_all()?;
        Ok(records
            .into_iter()
            .map(|rec| self.view(rec, &live, degraded.as_deref()))
            .collect())
    }

    /// 全部运行时会话 + 本次是否降级。运行时整体不可用时**不报错也不当成"会话都没了"**：
    /// 读路径退化成"没有活性信息"，由 [`SessionManager::view`] 把它报成 UNKNOWN（ADR-001 D7）。
    /// 写路径与 [`SessionManager::reconcile`] 仍然照常报错——那两条路上"读不到"绝不能当"已经死了"。
    fn live_all(&self) -> Result<(Vec<RuntimeSession>, Option<String>), SessionError> {
        let r = self.runtime.list();
        self.runtime_status.observe(&r);
        match r {
            Ok(v) => Ok((v, None)),
            Err(e) if e.degrades_runtime() => Ok((Vec::new(), Some(e.to_string()))),
            Err(e) => Err(e.into()),
        }
    }

    pub fn get(&self, id: &str) -> Result<SessionView, SessionError> {
        let rec = self.record(id)?;
        let mut degraded = None;
        let live = match &rec.runtime_ref {
            Some(r) => {
                let r = self.runtime.inspect(&RuntimeRef(r.clone()));
                self.runtime_status.observe(&r);
                match r {
                    Ok(s) => vec![s],
                    Err(RuntimeError::NotFound(_)) => Vec::new(),
                    // 运行时整体不可用：同 list，退化成"没有活性信息"→ UNKNOWN（D7）。
                    Err(e) if e.degrades_runtime() => {
                        degraded = Some(e.to_string());
                        Vec::new()
                    }
                    Err(e) => return Err(e.into()),
                }
            }
            None => Vec::new(),
        };
        Ok(self.view(rec, &live, degraded.as_deref()))
    }

    /// Terminal Gateway 要的 attach 规格（ADR-001 D5）。external 会话没有运行时句柄。
    /// 这里不检查会话是否 alive：dead pane 的输出还在，attach 上去看 scrollback 是 Kill 之后
    /// "输出保留"承诺的一部分（MISSION §4.6）。
    pub fn attach(&self, id: &str, size: Size) -> Result<AttachSpec, SessionError> {
        let rec = self.record(id)?;
        let r = rec
            .runtime_ref
            .ok_or_else(|| SessionError::NoRuntime(id.into()))?;
        Ok(self.runtime.attach(&RuntimeRef(r), size)?)
    }

    fn view(
        &self,
        rec: SessionRecord,
        live: &[RuntimeSession],
        degraded: Option<&str>,
    ) -> SessionView {
        let rt = rec
            .runtime_ref
            .as_deref()
            .and_then(|r| live.iter().find(|s| s.r#ref.0 == r));
        let killed = rec.killed_at.is_some();
        // STARTING 只看本代进程的起始时刻。updated_at 不行：rename / kill / cleanup 都刷新它，
        // 改名后两秒内会把跑了一天的会话报成 STARTING（agora-xqa.15，2026-09-03）。
        let spawn_age = rec.spawned_at.as_deref().and_then(age_secs);
        let assessment = match (rec.origin, rt) {
            // 运行时此刻不可信：不知道就报 UNKNOWN，不许拿"读不到"当"已经死了"（ADR-001 D7）。
            _ if degraded.is_some() && rec.runtime_ref.is_some() => Assessment {
                status: Status::Unknown,
                source: status::Source::None,
                reason: Some(format!(
                    "runtime unavailable: {}",
                    degraded.unwrap_or_default()
                )),
            },
            (Origin::External, None) => Assessment {
                status: Status::Unknown,
                source: status::Source::None,
                reason: Some("external session: no runtime, hook only".into()),
            },
            _ => status::process_layer(rt, spawn_age, killed),
        };
        let name = match rt {
            Some(s) if !rec.name_locked && !s.title.trim().is_empty() => s.title.clone(),
            _ => rec.display_name.clone(),
        };
        SessionView {
            name,
            alive: rt.is_some_and(|s| s.alive),
            exit: rt.and_then(|s| s.exit.clone()),
            pid: rt.and_then(|s| s.pid),
            managed: rt.is_some_and(|s| s.managed),
            assessment,
            record: rec,
        }
    }

    // ---------- 改名 ----------

    /// 改名是两件事：存名字，并且把 agent 的 title 挡在外面——**改成同名字符串也落锁**。
    pub fn rename(&self, id: &str, display_name: &str) -> Result<SessionView, SessionError> {
        let n = self.db.conn().execute(
            "UPDATE sessions SET display_name = ?2, name_locked = 1,
                updated_at = strftime('%Y-%m-%dT%H:%M:%SZ','now') WHERE id = ?1",
            params![id, display_name],
        )?;
        if n == 0 {
            return Err(SessionError::NotFound(id.into()));
        }
        self.get(id)
    }

    // ---------- 生命周期 ----------

    /// Kill = terminate；会话保留为 dead pane，scrollback 还在（ADR-001 D4）。
    pub fn kill(&self, id: &str) -> Result<SessionView, SessionError> {
        let rec = self.record(id)?;
        let r#ref = self.require_ref(&rec)?;
        // 先记事实再动手：terminate 成功即进程已死，若先杀后写，中间那一瞬 get 会报 FAILED。
        // terminate 失败则撤回，免得日后被别人用信号杀掉时冒充"用户杀的"。
        self.set_killed_at(id, true)?;
        if let Err(e) = self.runtime.terminate(&r#ref, KILL_GRACE) {
            self.set_killed_at(id, false)?;
            return Err(e.into());
        }
        self.mark_ended(id)?;
        self.get(id)
    }

    /// Restart = respawn：同会话同名同 cwd，epoch +1 经 `AGORA_EPOCH` 交给 agent。
    /// resume 参数由 M1b 的 Adapter 接入，现在先原命令。
    pub fn restart(&self, id: &str, env: &[(String, String)]) -> Result<SessionView, SessionError> {
        let rec = self.record(id)?;
        let r#ref = self.require_ref(&rec)?;
        let epoch = rec.epoch + 1;
        let mut env = env.to_vec();
        env.push(("AGORA_SESSION_ID".into(), rec.id.clone()));
        env.push(("AGORA_EPOCH".into(), epoch.to_string()));
        let spec = LaunchSpec {
            name: runtime_name(&r#ref),
            command: rec.command.clone().unwrap_or_default(),
            cwd: PathBuf::from(rec.working_directory.clone().unwrap_or_else(|| "/".into())),
            env,
            size: Size::default(),
        };
        match self.runtime.respawn(&r#ref, &spec) {
            Ok(()) => {}
            // 运行时会话不在了（daemon 重启时发现 missing 的那种）：退化为同名 create，无 scrollback。
            Err(RuntimeError::NotFound(_)) => {
                let created = self.runtime.create(&spec)?;
                debug_assert_eq!(created, r#ref);
            }
            Err(e) => return Err(e.into()),
        }
        self.db.conn().execute(
            "UPDATE sessions SET epoch = ?2, ended_at = NULL, killed_at = NULL,
                spawned_at = strftime('%Y-%m-%dT%H:%M:%SZ','now'),
                updated_at = strftime('%Y-%m-%dT%H:%M:%SZ','now') WHERE id = ?1",
            params![id, epoch],
        )?;
        self.get(id)
    }

    /// Delete metadata ≠ kill：活着 → 留在 socket 上成未注册；已死 → 顺手 remove。
    pub fn delete_metadata(&self, id: &str) -> Result<(), SessionError> {
        let rec = self.record(id)?;
        if let Some(r) = &rec.runtime_ref {
            let r#ref = RuntimeRef(r.clone());
            match self.runtime.inspect(&r#ref) {
                Ok(s) if !s.alive => {
                    if let Err(e) = self.runtime.remove(&r#ref) {
                        tracing::warn!(component = "session", %id, %e, "删 metadata 时清理运行时会话失败");
                    }
                }
                Ok(_) => {
                    tracing::info!(component = "session", %id, "删 metadata，进程仍活，会话留在运行时成未注册");
                }
                Err(RuntimeError::NotFound(_)) => {}
                Err(e) => return Err(e.into()),
            }
        }
        self.db
            .conn()
            .execute("DELETE FROM sessions WHERE id = ?1", [id])?;
        Ok(())
    }

    /// 已退出会话的清理：只对 dead pane；活着 → StillAlive。由用户确认触发，V1 不做定时 GC。
    pub fn cleanup(&self, id: &str) -> Result<(), SessionError> {
        let rec = self.record(id)?;
        let r#ref = self.require_ref(&rec)?;
        match self.runtime.remove(&r#ref) {
            Ok(()) => {}
            Err(RuntimeError::StillAlive(_)) => return Err(SessionError::StillAlive(id.into())),
            Err(RuntimeError::NotFound(_)) => {}
            Err(e) => return Err(e.into()),
        }
        self.mark_ended(id)?;
        Ok(())
    }

    // ---------- daemon 重启 ----------

    /// `list()` ⨝ SQLite：六种情况按 ADR-001 D4 表处理。
    pub fn reconcile(&self) -> Result<ReconcileReport, SessionError> {
        let records = self.all_records()?;
        let live = self.runtime.list()?;
        let mut report = ReconcileReport::default();
        let mut known: HashSet<&str> = HashSet::new();

        for rec in &records {
            let Some(r) = rec.runtime_ref.as_deref() else {
                report.external.push(rec.id.clone());
                continue;
            };
            known.insert(r);
            match live.iter().find(|s| s.r#ref.0 == r) {
                Some(s) if s.alive => report.known_alive.push(rec.id.clone()),
                Some(s) => {
                    if rec.ended_at.is_none() {
                        self.mark_ended_at(&rec.id, s.exited_at)?;
                    }
                    report.known_dead.push(rec.id.clone());
                }
                None => {
                    if rec.ended_at.is_none() {
                        self.mark_ended(&rec.id)?;
                    }
                    report.known_missing.push(rec.id.clone());
                }
            }
        }
        for s in &live {
            if known.contains(s.r#ref.0.as_str()) {
                continue;
            }
            if s.managed {
                report.unregistered_managed.push(s.r#ref.clone());
            } else {
                report.unregistered_adoptable.push(s.r#ref.clone());
            }
        }
        tracing::info!(
            component = "session",
            alive = report.known_alive.len(),
            dead = report.known_dead.len(),
            missing = report.known_missing.len(),
            unregistered = report.unregistered_managed.len(),
            adoptable = report.unregistered_adoptable.len(),
            external = report.external.len(),
            "reconcile"
        );
        Ok(report)
    }

    // ---------- 内部 ----------

    fn require_ref(&self, rec: &SessionRecord) -> Result<RuntimeRef, SessionError> {
        rec.runtime_ref
            .clone()
            .map(RuntimeRef)
            .ok_or_else(|| SessionError::NoRuntime(rec.id.clone()))
    }

    fn mark_ended(&self, id: &str) -> Result<(), SessionError> {
        self.mark_ended_at(id, None)
    }

    fn set_killed_at(&self, id: &str, set: bool) -> Result<(), SessionError> {
        self.db.conn().execute(
            "UPDATE sessions SET
                killed_at = CASE WHEN ?2 THEN strftime('%Y-%m-%dT%H:%M:%SZ','now') ELSE NULL END,
                updated_at = strftime('%Y-%m-%dT%H:%M:%SZ','now')
             WHERE id = ?1",
            params![id, set],
        )?;
        Ok(())
    }

    fn mark_ended_at(&self, id: &str, at: Option<SystemTime>) -> Result<(), SessionError> {
        let secs = at
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map(|d| d.as_secs() as i64);
        self.db.conn().execute(
            "UPDATE sessions SET
                ended_at = COALESCE(ended_at, CASE WHEN ?2 IS NULL
                    THEN strftime('%Y-%m-%dT%H:%M:%SZ', 'now')
                    ELSE strftime('%Y-%m-%dT%H:%M:%SZ', ?2, 'unixepoch') END),
                updated_at = strftime('%Y-%m-%dT%H:%M:%SZ','now')
             WHERE id = ?1",
            params![id, secs],
        )?;
        Ok(())
    }

    /// 只读库里的 metadata 行，不碰运行时：hook 重放按 epoch 过滤要它，每条事件问一次运行时太贵。
    pub fn record(&self, id: &str) -> Result<SessionRecord, SessionError> {
        self.db
            .conn()
            .query_row(&format!("{SELECT} WHERE id = ?1"), [id], row_to_record)
            .optional()?
            .ok_or_else(|| SessionError::NotFound(id.into()))
    }

    fn all_records(&self) -> Result<Vec<SessionRecord>, SessionError> {
        let conn = self.db.conn();
        let mut stmt = conn.prepare(&format!("{SELECT} ORDER BY created_at, id"))?;
        let rows = stmt.query_map([], row_to_record)?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }
}

const SELECT: &str =
    "SELECT id, runtime_ref, display_name, name_locked, agent_type, working_directory,
    worktree, task_ref, command, agent_session_id, epoch, transcript_path, created_at, ended_at,
    updated_at, origin, spawned_at, killed_at FROM sessions";

fn row_to_record(row: &Row<'_>) -> rusqlite::Result<SessionRecord> {
    let origin: String = row.get(15)?;
    Ok(SessionRecord {
        id: row.get(0)?,
        runtime_ref: row.get(1)?,
        display_name: row.get(2)?,
        name_locked: row.get(3)?,
        agent_type: row.get(4)?,
        working_directory: row.get(5)?,
        worktree: row.get(6)?,
        task_ref: row.get(7)?,
        command: row.get(8)?,
        agent_session_id: row.get(9)?,
        epoch: row.get(10)?,
        transcript_path: row.get(11)?,
        created_at: row.get(12)?,
        ended_at: row.get(13)?,
        updated_at: row.get(14)?,
        origin: Origin::parse(&origin).unwrap_or(Origin::Agora),
        spawned_at: row.get(16)?,
        killed_at: row.get(17)?,
    })
}

/// 运行时名就是 ref 的最后一段（`<kind>:<socket>:<name>`）；这里不解析 socket。
fn runtime_name(r: &RuntimeRef) -> String {
    r.0.rsplit(':').next().unwrap_or("").to_owned()
}

fn fnv(x: u128) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in x.to_le_bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    h
}
