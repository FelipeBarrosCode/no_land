use std::path::Path;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use chrono::{TimeZone, Utc};
use noland_state_core::*;
use rusqlite::{params, Connection, OptionalExtension};
use uuid::Uuid;

use crate::schema::{MIGRATION_V1, MIGRATION_V2};
use crate::SCHEMA_VERSION;

pub struct StateDb {
    conn: Mutex<Connection>,
}

impl StateDb {
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let conn = Connection::open(path).map_err(db_err)?;
        conn.pragma_update(None, "journal_mode", "WAL")
            .map_err(db_err)?;
        conn.pragma_update(None, "synchronous", "NORMAL")
            .map_err(db_err)?;
        conn.pragma_update(None, "foreign_keys", "ON")
            .map_err(db_err)?;
        let db = Self {
            conn: Mutex::new(conn),
        };
        db.migrate()?;
        Ok(db)
    }

    pub fn open_in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory().map_err(db_err)?;
        conn.pragma_update(None, "journal_mode", "WAL")
            .map_err(db_err)?;
        conn.pragma_update(None, "foreign_keys", "ON")
            .map_err(db_err)?;
        let db = Self {
            conn: Mutex::new(conn),
        };
        db.migrate()?;
        Ok(db)
    }

    pub(crate) fn lock(&self) -> Result<std::sync::MutexGuard<'_, Connection>> {
        self.conn
            .lock()
            .map_err(|_| StateError::Database("sqlite mutex poisoned".into()))
    }

    pub fn integrity_check(&self) -> Result<String> {
        self.lock()?
            .query_row("PRAGMA integrity_check", [], |row| row.get(0))
            .map_err(db_err)
    }

    fn migrate(&self) -> Result<()> {
        let conn = self.lock()?;
        conn.execute_batch(MIGRATION_V1).map_err(db_err)?;
        conn.execute_batch(MIGRATION_V2).map_err(db_err)?;
        for version in 1..=SCHEMA_VERSION {
            conn.execute(
                "INSERT OR IGNORE INTO schema_migrations(version, applied_at) VALUES (?1, ?2)",
                params![version, now_secs()],
            )
            .map_err(db_err)?;
        }
        Ok(())
    }

    pub fn upsert_app(&self, identity: &AppIdentity) -> Result<()> {
        let now = now_secs();
        let aliases = serde_json::to_string(&identity.aliases)?;
        self.lock()?
            .execute(
                r#"
                INSERT INTO apps (
                    app_id, display_name, canonical_executable, desktop_entry_id,
                    steam_app_id, launcher_kind, aliases_json, identity_confidence,
                    icon_path, created_at, updated_at
                ) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?10)
                ON CONFLICT(app_id) DO UPDATE SET
                    display_name=excluded.display_name,
                    canonical_executable=COALESCE(excluded.canonical_executable, apps.canonical_executable),
                    desktop_entry_id=COALESCE(excluded.desktop_entry_id, apps.desktop_entry_id),
                    steam_app_id=COALESCE(excluded.steam_app_id, apps.steam_app_id),
                    launcher_kind=COALESCE(excluded.launcher_kind, apps.launcher_kind),
                    aliases_json=excluded.aliases_json,
                    identity_confidence=MAX(apps.identity_confidence, excluded.identity_confidence),
                    icon_path=COALESCE(excluded.icon_path, apps.icon_path),
                    updated_at=excluded.updated_at
                "#,
                params![
                    identity.app_id.as_str(),
                    identity.display_name,
                    identity
                        .canonical_executable
                        .as_ref()
                        .map(|p| p.to_string_lossy().into_owned()),
                    identity.desktop_entry_id,
                    identity.steam_app_id.map(|v| v as i64),
                    identity.launcher.map(|l| l.as_str().to_string()),
                    aliases,
                    identity.identity_confidence,
                    identity
                        .icon_path
                        .as_ref()
                        .map(|p| p.to_string_lossy().into_owned()),
                    now,
                ],
            )
            .map_err(db_err)?;
        Ok(())
    }

    pub fn get_app(&self, app_id: &AppId) -> Result<Option<AppIdentity>> {
        self.lock()?
            .query_row(
                r#"SELECT app_id, display_name, canonical_executable, desktop_entry_id,
                          steam_app_id, launcher_kind, aliases_json, identity_confidence, icon_path
                   FROM apps WHERE app_id=?1"#,
                params![app_id.as_str()],
                row_to_identity,
            )
            .optional()
            .map_err(db_err)
    }

    pub fn list_apps(&self) -> Result<Vec<AppIdentity>> {
        let conn = self.lock()?;
        let mut stmt = conn
            .prepare(
                r#"SELECT app_id, display_name, canonical_executable, desktop_entry_id,
                          steam_app_id, launcher_kind, aliases_json, identity_confidence, icon_path
                   FROM apps ORDER BY display_name"#,
            )
            .map_err(db_err)?;
        let rows = stmt
            .query_map([], row_to_identity)
            .map_err(db_err)?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(db_err)?;
        Ok(rows)
    }

    pub fn insert_session(&self, session: &AppSession) -> Result<()> {
        self.lock()?
            .execute(
                r#"INSERT INTO app_sessions (
                    session_id, app_id, root_pid, cgroup_path, source,
                    started_at, ended_at, identity_confidence
                ) VALUES (?1,?2,?3,?4,?5,?6,?7,?8)"#,
                params![
                    session.session_id.to_string(),
                    session.app_id.as_str(),
                    session.root_pid,
                    session.cgroup_path,
                    session.source.as_str(),
                    session.started_at.timestamp(),
                    session.ended_at.map(|t| t.timestamp()),
                    session.identity_confidence,
                ],
            )
            .map_err(db_err)?;
        self.attach_pid(session.session_id, session.root_pid, None, None)?;
        Ok(())
    }

    pub fn attach_pid(
        &self,
        session_id: Uuid,
        pid: i32,
        ppid: Option<i32>,
        executable: Option<&str>,
    ) -> Result<()> {
        self.lock()?
            .execute(
                r#"INSERT INTO session_pids (pid, session_id, ppid, executable, attached_at)
                   VALUES (?1,?2,?3,?4,?5)
                   ON CONFLICT(pid) DO UPDATE SET session_id=excluded.session_id"#,
                params![pid, session_id.to_string(), ppid, executable, now_secs()],
            )
            .map_err(db_err)?;
        Ok(())
    }

    pub fn detach_pid(&self, pid: i32) -> Result<()> {
        self.lock()?
            .execute("DELETE FROM session_pids WHERE pid=?1", params![pid])
            .map_err(db_err)?;
        Ok(())
    }

    pub fn session_for_pid(&self, pid: i32) -> Result<Option<AppSession>> {
        self.lock()?
            .query_row(
                r#"SELECT s.session_id, s.app_id, s.root_pid, s.cgroup_path, s.source,
                          s.started_at, s.ended_at, s.identity_confidence
                   FROM session_pids p
                   JOIN app_sessions s ON s.session_id = p.session_id
                   WHERE p.pid=?1"#,
                params![pid],
                row_to_session,
            )
            .optional()
            .map_err(db_err)
    }

    pub fn open_session_app_ids(&self) -> Result<Vec<AppId>> {
        let conn = self.lock()?;
        let mut stmt = conn
            .prepare("SELECT DISTINCT app_id FROM app_sessions WHERE ended_at IS NULL")
            .map_err(db_err)?;
        let app_ids = stmt
            .query_map([], |row| row.get::<_, String>(0).map(AppId))
            .map_err(db_err)?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(db_err)?;
        Ok(app_ids)
    }

    pub fn open_session_for_app(&self, app_id: &AppId) -> Result<Option<AppSession>> {
        self.lock()?
            .query_row(
                r#"SELECT session_id, app_id, root_pid, cgroup_path, source,
                          started_at, ended_at, identity_confidence
                   FROM app_sessions WHERE app_id=?1 AND ended_at IS NULL
                   ORDER BY started_at DESC LIMIT 1"#,
                params![app_id.as_str()],
                row_to_session,
            )
            .optional()
            .map_err(db_err)
    }

    pub fn end_session(&self, session_id: Uuid) -> Result<()> {
        self.lock()?
            .execute(
                "UPDATE app_sessions SET ended_at=?1 WHERE session_id=?2",
                params![now_secs(), session_id.to_string()],
            )
            .map_err(db_err)?;
        self.lock()?
            .execute(
                "DELETE FROM session_pids WHERE session_id=?1",
                params![session_id.to_string()],
            )
            .map_err(db_err)?;
        Ok(())
    }

    pub fn upsert_path(&self, canonical: &str) -> Result<i64> {
        self.lock()?
            .execute(
                "INSERT INTO paths (canonical_path) VALUES (?1) ON CONFLICT(canonical_path) DO NOTHING",
                params![canonical],
            )
            .map_err(db_err)?;
        self.lock()?
            .query_row(
                "SELECT path_id FROM paths WHERE canonical_path=?1",
                params![canonical],
                |row| row.get(0),
            )
            .map_err(db_err)
    }

    pub fn update_path_meta(&self, path_id: i64, record: &PathRecord) -> Result<()> {
        self.lock()?
            .execute(
                r#"UPDATE paths SET logical_root=?2, relative_path=?3, file_type=?4,
                       inode=?5, size=?6, mtime_ns=?7, mode=?8, uid=?9, gid=?10,
                       content_hash=?11, last_scanned_at=?12
                   WHERE path_id=?1"#,
                params![
                    path_id,
                    record.logical_root,
                    record.relative_path,
                    record.file_type,
                    record.inode,
                    record.size,
                    record.mtime_ns,
                    record.mode,
                    record.uid,
                    record.gid,
                    record.content_hash,
                    record.last_scanned_at,
                ],
            )
            .map_err(db_err)?;
        Ok(())
    }

    pub fn get_path_by_id(&self, path_id: i64) -> Result<Option<PathRecord>> {
        self.lock()?
            .query_row(
                r#"SELECT path_id, canonical_path, logical_root, relative_path, file_type,
                          inode, mount_id, size, mtime_ns, mode, uid, gid, content_hash, last_scanned_at
                   FROM paths WHERE path_id=?1"#,
                params![path_id],
                row_to_path,
            )
            .optional()
            .map_err(db_err)
    }

    pub fn get_path_by_canonical(&self, canonical: &str) -> Result<Option<PathRecord>> {
        self.lock()?
            .query_row(
                r#"SELECT path_id, canonical_path, logical_root, relative_path, file_type,
                          inode, mount_id, size, mtime_ns, mode, uid, gid, content_hash, last_scanned_at
                   FROM paths WHERE canonical_path=?1"#,
                params![canonical],
                row_to_path,
            )
            .optional()
            .map_err(db_err)
    }

    pub fn upsert_association(&self, assoc: &PathAssociation) -> Result<()> {
        let evidence = serde_json::to_string(&assoc.evidence)?;
        self.lock()?
            .execute(
                r#"
                INSERT INTO path_associations (
                    path_id, app_id, confidence, persistence_class, semantic_role,
                    evidence_json, first_seen_at, last_seen_at
                ) VALUES (?1,?2,?3,?4,?5,?6,?7,?8)
                ON CONFLICT(path_id, app_id) DO UPDATE SET
                    confidence=excluded.confidence,
                    persistence_class=excluded.persistence_class,
                    semantic_role=excluded.semantic_role,
                    evidence_json=excluded.evidence_json,
                    last_seen_at=excluded.last_seen_at
                "#,
                params![
                    assoc.path_id,
                    assoc.app_id.as_str(),
                    assoc.confidence,
                    assoc.persistence_class.as_str(),
                    assoc.semantic_role.as_str(),
                    evidence,
                    assoc.first_seen_at.timestamp(),
                    assoc.last_seen_at.timestamp(),
                ],
            )
            .map_err(db_err)?;
        Ok(())
    }

    pub fn associations_for_app(
        &self,
        app_id: &AppId,
    ) -> Result<Vec<(PathRecord, PathAssociation)>> {
        let conn = self.lock()?;
        let mut stmt = conn
            .prepare(
                r#"SELECT p.path_id, p.canonical_path, p.logical_root, p.relative_path, p.file_type,
                          p.inode, p.mount_id, p.size, p.mtime_ns, p.mode, p.uid, p.gid,
                          p.content_hash, p.last_scanned_at,
                          a.path_id, a.app_id, a.confidence, a.persistence_class, a.semantic_role,
                          a.evidence_json, a.first_seen_at, a.last_seen_at
                   FROM path_associations a
                   JOIN paths p ON p.path_id = a.path_id
                   WHERE a.app_id=?1
                   ORDER BY a.confidence DESC"#,
            )
            .map_err(db_err)?;
        let rows = stmt
            .query_map(params![app_id.as_str()], |row| {
                Ok((row_to_path(row)?, row_to_assoc_from(row, 14)?))
            })
            .map_err(db_err)?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(db_err)?;
        Ok(rows)
    }

    pub fn likely_backup_associations(
        &self,
        app_id: &AppId,
    ) -> Result<Vec<(PathRecord, PathAssociation)>> {
        let conn = self.lock()?;
        let mut stmt = conn
            .prepare(
                r#"SELECT p.path_id, p.canonical_path, p.logical_root, p.relative_path, p.file_type,
                          p.inode, p.mount_id, p.size, p.mtime_ns, p.mode, p.uid, p.gid,
                          p.content_hash, p.last_scanned_at,
                          a.path_id, a.app_id, a.confidence, a.persistence_class, a.semantic_role,
                          a.evidence_json, a.first_seen_at, a.last_seen_at
                   FROM path_associations a
                   JOIN paths p ON p.path_id = a.path_id
                   WHERE a.app_id=?1 AND (
                       a.persistence_class IN ('PERSISTENT_STATE','SHARED_STATE')
                       OR a.semantic_role IN ('USER_STATE','SECRET')
                       OR a.confidence >= 0.70
                       OR EXISTS (
                           SELECT 1 FROM path_policies policy
                           WHERE policy.canonical_path=p.canonical_path
                             AND (policy.app_id IS NULL OR policy.app_id=a.app_id)
                             AND policy.policy IN ('include','shared','secret','force-persistent')
                       )
                   )
                   ORDER BY p.canonical_path"#,
            )
            .map_err(db_err)?;
        let rows = stmt
            .query_map(params![app_id.as_str()], |row| {
                Ok((row_to_path(row)?, row_to_assoc_from(row, 14)?))
            })
            .map_err(db_err)?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(db_err)?;
        Ok(rows)
    }

    pub fn associations_for_path(&self, path_id: i64) -> Result<Vec<PathAssociation>> {
        let conn = self.lock()?;
        let mut stmt = conn
            .prepare(
                r#"SELECT path_id, app_id, confidence, persistence_class, semantic_role,
                          evidence_json, first_seen_at, last_seen_at
                   FROM path_associations WHERE path_id=?1"#,
            )
            .map_err(db_err)?;
        let rows = stmt
            .query_map(params![path_id], |row| row_to_assoc_from(row, 0))
            .map_err(db_err)?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(db_err)?;
        Ok(rows)
    }

    pub fn mark_dirty(
        &self,
        app_id: &AppId,
        path_id: Option<i64>,
        requires_reconciliation: bool,
    ) -> Result<()> {
        let now = now_secs();
        self.lock()?
            .execute(
                r#"
                INSERT INTO dirty_apps (app_id, first_dirty_at, last_dirty_at, requires_reconciliation)
                VALUES (?1,?2,?2,?3)
                ON CONFLICT(app_id) DO UPDATE SET
                    last_dirty_at=excluded.last_dirty_at,
                    requires_reconciliation=MAX(dirty_apps.requires_reconciliation, excluded.requires_reconciliation)
                "#,
                params![app_id.as_str(), now, requires_reconciliation as i64],
            )
            .map_err(db_err)?;
        if let Some(path_id) = path_id {
            self.lock()?
                .execute(
                    "INSERT OR IGNORE INTO dirty_paths (app_id, path_id) VALUES (?1,?2)",
                    params![app_id.as_str(), path_id],
                )
                .map_err(db_err)?;
        }
        Ok(())
    }

    pub fn clear_reconciliation_required(&self, app_id: &AppId) -> Result<()> {
        self.lock()?
            .execute(
                "UPDATE dirty_apps SET requires_reconciliation=0 WHERE app_id=?1",
                params![app_id.as_str()],
            )
            .map_err(db_err)?;
        Ok(())
    }

    pub fn clear_dirty(&self, app_id: &AppId) -> Result<()> {
        self.lock()?
            .execute(
                "DELETE FROM dirty_apps WHERE app_id=?1",
                params![app_id.as_str()],
            )
            .map_err(db_err)?;
        self.lock()?
            .execute(
                "DELETE FROM dirty_paths WHERE app_id=?1",
                params![app_id.as_str()],
            )
            .map_err(db_err)?;
        Ok(())
    }

    pub fn list_dirty_apps(&self) -> Result<Vec<DirtyState>> {
        let conn = self.lock()?;
        let mut stmt = conn
            .prepare(
                "SELECT app_id, first_dirty_at, last_dirty_at, requires_reconciliation FROM dirty_apps",
            )
            .map_err(db_err)?;
        let rows = stmt
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, i64>(3)?,
                ))
            })
            .map_err(db_err)?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(db_err)?;
        drop(stmt);
        let mut out = Vec::new();
        for (app_id, first, last, recon) in rows {
            let mut pstmt = conn
                .prepare("SELECT path_id FROM dirty_paths WHERE app_id=?1")
                .map_err(db_err)?;
            let dirty_paths = pstmt
                .query_map(params![app_id], |r| r.get(0))
                .map_err(db_err)?
                .collect::<std::result::Result<Vec<_>, _>>()
                .map_err(db_err)?;
            out.push(DirtyState {
                app_id: AppId(app_id),
                first_dirty_at: secs_to_dt(first),
                last_dirty_at: secs_to_dt(last),
                dirty_paths,
                requires_reconciliation: recon != 0,
            });
        }
        Ok(out)
    }

    pub fn add_known_root(&self, app_id: &AppId, kind: &str, path: &str) -> Result<()> {
        self.lock()?
            .execute(
                "INSERT OR IGNORE INTO known_roots (app_id, kind, canonical_path) VALUES (?1,?2,?3)",
                params![app_id.as_str(), kind, path],
            )
            .map_err(db_err)?;
        Ok(())
    }

    pub fn known_roots(&self, app_id: Option<&AppId>) -> Result<Vec<(AppId, String, String)>> {
        let mut out = Vec::new();
        if let Some(app_id) = app_id {
            let conn = self.lock()?;
            let mut stmt = conn
                .prepare("SELECT app_id, kind, canonical_path FROM known_roots WHERE app_id=?1")
                .map_err(db_err)?;
            let rows = stmt
                .query_map(params![app_id.as_str()], |row| {
                    Ok((
                        AppId(row.get::<_, String>(0)?),
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                })
                .map_err(db_err)?;
            for row in rows {
                out.push(row.map_err(db_err)?);
            }
        } else {
            let conn = self.lock()?;
            let mut stmt = conn
                .prepare("SELECT app_id, kind, canonical_path FROM known_roots")
                .map_err(db_err)?;
            let rows = stmt
                .query_map([], |row| {
                    Ok((
                        AppId(row.get::<_, String>(0)?),
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                })
                .map_err(db_err)?;
            for row in rows {
                out.push(row.map_err(db_err)?);
            }
        }
        Ok(out)
    }

    pub fn insert_installer(&self, tx: &InstallerTransaction) -> Result<()> {
        self.lock()?
            .execute(
                r#"INSERT INTO installer_transactions (
                    transaction_id, app_id, session_id, transaction_type, confidence,
                    candidate_roots_json, started_at, ended_at
                ) VALUES (?1,?2,?3,?4,?5,?6,?7,?8)"#,
                params![
                    tx.transaction_id.to_string(),
                    tx.app_id.as_str(),
                    tx.session_id.map(|s| s.to_string()),
                    tx.transaction_type.as_str(),
                    tx.confidence,
                    serde_json::to_string(&tx.candidate_roots)?,
                    tx.started_at.timestamp(),
                    tx.ended_at.map(|t| t.timestamp()),
                ],
            )
            .map_err(db_err)?;
        Ok(())
    }

    pub fn finish_installer(&self, transaction_id: Uuid) -> Result<()> {
        self.lock()?
            .execute(
                "UPDATE installer_transactions SET ended_at=?1 WHERE transaction_id=?2",
                params![now_secs(), transaction_id.to_string()],
            )
            .map_err(db_err)?;
        Ok(())
    }

    pub fn open_installers(&self) -> Result<Vec<InstallerTransaction>> {
        let conn = self.lock()?;
        let mut stmt = conn
            .prepare(
                r#"SELECT transaction_id, app_id, session_id, transaction_type, confidence,
                          candidate_roots_json, started_at, ended_at
                   FROM installer_transactions WHERE ended_at IS NULL"#,
            )
            .map_err(db_err)?;
        let rows = stmt
            .query_map([], row_to_installer)
            .map_err(db_err)?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(db_err)?;
        Ok(rows)
    }

    pub fn record_commit(
        &self,
        commit_id: Uuid,
        app_id: &AppId,
        bundle_id: Uuid,
        manifest_hash: &str,
        cloud_path: Option<&str>,
        state: CommitVisibility,
    ) -> Result<()> {
        let now = now_secs();
        let committed_at = if state == CommitVisibility::Committed {
            Some(now)
        } else {
            None
        };
        self.lock()?
            .execute(
                r#"INSERT INTO bundle_commits (
                    commit_id, app_id, bundle_id, manifest_hash, cloud_path, state, created_at, committed_at
                ) VALUES (?1,?2,?3,?4,?5,?6,?7,?8)
                ON CONFLICT(commit_id) DO UPDATE SET
                    state=excluded.state,
                    cloud_path=COALESCE(excluded.cloud_path, bundle_commits.cloud_path),
                    committed_at=COALESCE(excluded.committed_at, bundle_commits.committed_at)
                "#,
                params![
                    commit_id.to_string(),
                    app_id.as_str(),
                    bundle_id.to_string(),
                    manifest_hash,
                    cloud_path,
                    state.as_str(),
                    now,
                    committed_at,
                ],
            )
            .map_err(db_err)?;
        Ok(())
    }

    pub fn latest_commit(&self, app_id: &AppId) -> Result<Option<(Uuid, Uuid, String)>> {
        self.lock()?
            .query_row(
                r#"SELECT commit_id, bundle_id, manifest_hash FROM bundle_commits
                   WHERE app_id=?1 AND state='COMMITTED'
                   ORDER BY committed_at DESC, rowid DESC LIMIT 1"#,
                params![app_id.as_str()],
                |row| {
                    Ok((
                        Uuid::parse_str(&row.get::<_, String>(0)?).unwrap_or(Uuid::nil()),
                        Uuid::parse_str(&row.get::<_, String>(1)?).unwrap_or(Uuid::nil()),
                        row.get(2)?,
                    ))
                },
            )
            .optional()
            .map_err(db_err)
    }

    pub fn upsert_operation(&self, op: &OperationRecord) -> Result<()> {
        self.lock()?
            .execute(
                r#"INSERT INTO operations (
                    operation_id, kind, app_id, state, created_at, updated_at, last_error, detail_json
                ) VALUES (?1,?2,?3,?4,?5,?6,?7,?8)
                ON CONFLICT(operation_id) DO UPDATE SET
                    state=excluded.state,
                    updated_at=excluded.updated_at,
                    last_error=excluded.last_error,
                    detail_json=excluded.detail_json
                "#,
                params![
                    op.operation_id.to_string(),
                    op.kind,
                    op.app_id.as_ref().map(|a| a.as_str().to_string()),
                    op.state,
                    op.created_at.timestamp(),
                    op.updated_at.timestamp(),
                    op.last_error,
                    op.detail_json.to_string(),
                ],
            )
            .map_err(db_err)?;
        Ok(())
    }

    pub fn get_operation(&self, id: Uuid) -> Result<Option<OperationRecord>> {
        self.lock()?
            .query_row(
                r#"SELECT operation_id, kind, app_id, state, created_at, updated_at, last_error, detail_json
                   FROM operations WHERE operation_id=?1"#,
                params![id.to_string()],
                row_to_operation,
            )
            .optional()
            .map_err(db_err)
    }

    pub fn unfinished_operations(&self) -> Result<Vec<OperationRecord>> {
        let conn = self.lock()?;
        let mut stmt = conn
            .prepare(
                r#"SELECT operation_id, kind, app_id, state, created_at, updated_at, last_error, detail_json
                   FROM operations WHERE state NOT IN (
                     'COMPLETED','FAILED','CANCELLED','SEALED','ROLLED_BACK'
                   )"#,
            )
            .map_err(db_err)?;
        let rows = stmt
            .query_map([], row_to_operation)
            .map_err(db_err)?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(db_err)?;
        Ok(rows)
    }

    pub fn journal_put(
        &self,
        operation_id: &str,
        item_key: &str,
        direction: &str,
        state: &str,
        last_error: Option<&str>,
    ) -> Result<()> {
        self.lock()?
            .execute(
                r#"INSERT INTO sync_journal (operation_id, item_key, direction, state, attempts, last_error, updated_at)
                   VALUES (?1,?2,?3,?4,1,?5,?6)
                   ON CONFLICT(operation_id, item_key) DO UPDATE SET
                     state=excluded.state,
                     attempts=sync_journal.attempts+1,
                     last_error=excluded.last_error,
                     updated_at=excluded.updated_at"#,
                params![operation_id, item_key, direction, state, last_error, now_secs()],
            )
            .map_err(db_err)?;
        Ok(())
    }

    pub fn remember_chunk(&self, hash: &str, pack_id: Option<&str>, size: u64) -> Result<()> {
        self.lock()?
            .execute(
                r#"INSERT INTO chunk_index (chunk_hash, pack_id, size, created_at)
                   VALUES (?1,?2,?3,?4)
                   ON CONFLICT(chunk_hash) DO UPDATE SET pack_id=COALESCE(excluded.pack_id, chunk_index.pack_id)"#,
                params![hash, pack_id, size as i64, now_secs()],
            )
            .map_err(db_err)?;
        Ok(())
    }

    pub fn known_chunk(&self, hash: &str) -> Result<bool> {
        let found: Option<i64> = self
            .lock()?
            .query_row(
                "SELECT 1 FROM chunk_index WHERE chunk_hash=?1",
                params![hash],
                |row| row.get(0),
            )
            .optional()
            .map_err(db_err)?;
        Ok(found.is_some())
    }

    pub fn insert_baseline(
        &self,
        image_id: &str,
        path: &str,
        file_type: Option<&str>,
        size: Option<i64>,
        mode: Option<i64>,
        package_owner: Option<&str>,
        baseline_hash: Option<&str>,
    ) -> Result<()> {
        self.lock()?
            .execute(
                r#"INSERT OR REPLACE INTO image_baseline (
                    image_id, canonical_path, file_type, size, mode, package_owner, baseline_hash
                ) VALUES (?1,?2,?3,?4,?5,?6,?7)"#,
                params![
                    image_id,
                    path,
                    file_type,
                    size,
                    mode,
                    package_owner,
                    baseline_hash
                ],
            )
            .map_err(db_err)?;
        Ok(())
    }

    pub fn baseline_entry(
        &self,
        image_id: &str,
        path: &str,
    ) -> Result<Option<(Option<i64>, Option<String>, Option<String>)>> {
        self.lock()?
            .query_row(
                "SELECT size, package_owner, baseline_hash FROM image_baseline WHERE image_id=?1 AND canonical_path=?2",
                params![image_id, path],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()
            .map_err(db_err)
    }

    pub fn set_path_policy(&self, path: &str, app_id: Option<&AppId>, policy: &str) -> Result<()> {
        self.lock()?
            .execute(
                "INSERT OR REPLACE INTO path_policies (canonical_path, app_id, policy) VALUES (?1,?2,?3)",
                params![path, app_id.map(|a| a.as_str().to_string()), policy],
            )
            .map_err(db_err)?;
        Ok(())
    }

    pub fn path_policy(&self, path: &str, app_id: Option<&AppId>) -> Result<Option<String>> {
        if let Some(app_id) = app_id {
            if let Some(found) = self
                .lock()?
                .query_row(
                    "SELECT policy FROM path_policies WHERE canonical_path=?1 AND app_id=?2",
                    params![path, app_id.as_str()],
                    |row| row.get(0),
                )
                .optional()
                .map_err(db_err)?
            {
                return Ok(Some(found));
            }
        }
        self.lock()?
            .query_row(
                "SELECT policy FROM path_policies WHERE canonical_path=?1 AND app_id IS NULL",
                params![path],
                |row| row.get(0),
            )
            .optional()
            .map_err(db_err)
    }

    pub fn record_seal(&self, seal: &SealRecord, state: SealState) -> Result<()> {
        self.lock()?
            .execute(
                r#"INSERT INTO seals (seal_id, instance_id, image_id, state, payload_json, created_at, committed_at)
                   VALUES (?1,?2,?3,?4,?5,?6,?7)
                   ON CONFLICT(seal_id) DO UPDATE SET state=excluded.state, payload_json=excluded.payload_json,
                     committed_at=excluded.committed_at"#,
                params![
                    seal.seal_id.to_string(),
                    seal.instance_id.to_string(),
                    seal.image_id,
                    state.as_str(),
                    serde_json::to_string(seal)?,
                    now_secs(),
                    if state == SealState::Sealed {
                        Some(now_secs())
                    } else {
                        None
                    },
                ],
            )
            .map_err(db_err)?;
        Ok(())
    }

    pub fn latest_seal(&self) -> Result<Option<(SealRecord, SealState)>> {
        self.lock()?
            .query_row(
                "SELECT payload_json, state FROM seals ORDER BY created_at DESC LIMIT 1",
                [],
                |row| {
                    let payload: String = row.get(0)?;
                    let state: String = row.get(1)?;
                    Ok((payload, state))
                },
            )
            .optional()
            .map_err(db_err)?
            .map(|(payload, state)| {
                let record: SealRecord = serde_json::from_str(&payload)?;
                Ok((record, SealState::parse(&state)))
            })
            .transpose()
    }

    pub fn insert_restore(
        &self,
        restore_id: Uuid,
        bundle_id: Uuid,
        app_id: &AppId,
        staging: &str,
    ) -> Result<()> {
        let now = now_secs();
        self.lock()?
            .execute(
                r#"INSERT INTO restore_operations (
                    restore_id, bundle_id, app_id, state, staging_path, created_at, updated_at, last_error
                ) VALUES (?1,?2,?3,'QUEUED',?4,?5,?5,NULL)"#,
                params![
                    restore_id.to_string(),
                    bundle_id.to_string(),
                    app_id.as_str(),
                    staging,
                    now
                ],
            )
            .map_err(db_err)?;
        Ok(())
    }

    pub fn update_restore(
        &self,
        restore_id: Uuid,
        state: RestoreState,
        last_error: Option<&str>,
    ) -> Result<()> {
        self.lock()?
            .execute(
                "UPDATE restore_operations SET state=?2, updated_at=?3, last_error=?4 WHERE restore_id=?1",
                params![restore_id.to_string(), state.as_str(), now_secs(), last_error],
            )
            .map_err(db_err)?;
        Ok(())
    }
}

fn db_err(err: rusqlite::Error) -> StateError {
    StateError::Database(err.to_string())
}

fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn secs_to_dt(secs: i64) -> chrono::DateTime<Utc> {
    Utc.timestamp_opt(secs, 0).single().unwrap_or_else(Utc::now)
}

fn row_to_identity(row: &rusqlite::Row<'_>) -> rusqlite::Result<AppIdentity> {
    let aliases_json: String = row.get(6)?;
    let aliases = serde_json::from_str(&aliases_json).unwrap_or_default();
    let launcher: Option<String> = row.get(5)?;
    let exe: Option<String> = row.get(2)?;
    let icon: Option<String> = row.get(8)?;
    Ok(AppIdentity {
        app_id: AppId(row.get(0)?),
        display_name: row.get(1)?,
        canonical_executable: exe.map(std::path::PathBuf::from),
        desktop_entry_id: row.get(3)?,
        steam_app_id: row.get::<_, Option<i64>>(4)?.map(|v| v as u32),
        launcher: launcher.as_deref().and_then(LauncherKind::parse),
        aliases,
        identity_confidence: row.get(7)?,
        icon_path: icon.map(std::path::PathBuf::from),
    })
}

fn row_to_session(row: &rusqlite::Row<'_>) -> rusqlite::Result<AppSession> {
    let sid: String = row.get(0)?;
    let started: i64 = row.get(5)?;
    let ended: Option<i64> = row.get(6)?;
    let source: String = row.get(4)?;
    Ok(AppSession {
        session_id: Uuid::parse_str(&sid).unwrap_or_else(|_| Uuid::nil()),
        app_id: AppId(row.get(1)?),
        root_pid: row.get(2)?,
        cgroup_path: row.get::<_, Option<String>>(3)?.unwrap_or_default(),
        started_at: secs_to_dt(started),
        ended_at: ended.map(secs_to_dt),
        source: SessionSource::parse(&source),
        identity_confidence: row.get(7)?,
    })
}

fn row_to_path(row: &rusqlite::Row<'_>) -> rusqlite::Result<PathRecord> {
    Ok(PathRecord {
        path_id: row.get(0)?,
        canonical_path: row.get(1)?,
        logical_root: row.get(2)?,
        relative_path: row.get(3)?,
        file_type: row.get(4)?,
        inode: row.get(5)?,
        mount_id: row.get(6)?,
        size: row.get(7)?,
        mtime_ns: row.get(8)?,
        mode: row.get(9)?,
        uid: row.get(10)?,
        gid: row.get(11)?,
        content_hash: row.get(12)?,
        last_scanned_at: row.get(13)?,
    })
}

fn row_to_assoc_from(row: &rusqlite::Row<'_>, offset: usize) -> rusqlite::Result<PathAssociation> {
    let evidence_json: String = row.get(offset + 5)?;
    let evidence = serde_json::from_str(&evidence_json).unwrap_or_default();
    let class: String = row.get(offset + 3)?;
    let role: String = row.get(offset + 4)?;
    Ok(PathAssociation {
        path_id: row.get(offset)?,
        app_id: AppId(row.get(offset + 1)?),
        confidence: row.get(offset + 2)?,
        persistence_class: PersistenceClass::parse(&class),
        semantic_role: SemanticRole::parse(&role),
        evidence,
        first_seen_at: secs_to_dt(row.get(offset + 6)?),
        last_seen_at: secs_to_dt(row.get(offset + 7)?),
    })
}

fn row_to_installer(row: &rusqlite::Row<'_>) -> rusqlite::Result<InstallerTransaction> {
    let roots_json: String = row.get(5)?;
    let roots = serde_json::from_str(&roots_json).unwrap_or_default();
    let session: Option<String> = row.get(2)?;
    let ty: String = row.get(3)?;
    Ok(InstallerTransaction {
        transaction_id: Uuid::parse_str(&row.get::<_, String>(0)?).unwrap_or_else(|_| Uuid::nil()),
        app_id: AppId(row.get(1)?),
        session_id: session.and_then(|s| Uuid::parse_str(&s).ok()),
        transaction_type: InstallTransactionType::parse(&ty),
        confidence: row.get(4)?,
        candidate_roots: roots,
        started_at: secs_to_dt(row.get(6)?),
        ended_at: row.get::<_, Option<i64>>(7)?.map(secs_to_dt),
    })
}

fn row_to_operation(row: &rusqlite::Row<'_>) -> rusqlite::Result<OperationRecord> {
    let detail: String = row.get(7)?;
    Ok(OperationRecord {
        operation_id: Uuid::parse_str(&row.get::<_, String>(0)?).unwrap_or_else(|_| Uuid::nil()),
        kind: row.get(1)?,
        app_id: row.get::<_, Option<String>>(2)?.map(AppId),
        state: row.get(3)?,
        created_at: secs_to_dt(row.get(4)?),
        updated_at: secs_to_dt(row.get(5)?),
        last_error: row.get(6)?,
        detail_json: serde_json::from_str(&detail).unwrap_or(serde_json::json!({})),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wal_and_roundtrip() {
        let db = StateDb::open_in_memory().unwrap();
        assert_eq!(db.integrity_check().unwrap(), "ok");
        let app = AppIdentity::new(AppId::steam(42), "Test Game");
        db.upsert_app(&app).unwrap();
        assert_eq!(
            db.get_app(&app.app_id).unwrap().unwrap().display_name,
            "Test Game"
        );
        let session = AppSession::new(app.app_id.clone(), 1001, SessionSource::Steam);
        db.insert_session(&session).unwrap();
        assert!(db.session_for_pid(1001).unwrap().is_some());
    }
}
