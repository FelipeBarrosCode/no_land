use std::path::Path;
use std::str::FromStr;
use std::sync::{Mutex, MutexGuard};
use std::time::Duration;

use chrono::{DateTime, SecondsFormat, Utc};
use rusqlite::{params, Connection, OptionalExtension};
use uuid::Uuid;

use crate::model::{BackupApp, LifecycleSnapshot, LifecycleState, RankedApp};
use crate::{AgentError, Result};

pub struct Database {
    connection: Mutex<Connection>,
}

impl Database {
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let connection = Connection::open(path)?;
        Self::configure(connection)
    }

    #[cfg(test)]
    pub fn open_in_memory() -> Result<Self> {
        Self::configure(Connection::open_in_memory()?)
    }

    fn configure(connection: Connection) -> Result<Self> {
        connection.busy_timeout(Duration::from_secs(5))?;
        connection.execute_batch(
            "PRAGMA journal_mode=WAL;
             PRAGMA synchronous=FULL;
             PRAGMA foreign_keys=ON;
             PRAGMA trusted_schema=OFF;",
        )?;
        let database = Self {
            connection: Mutex::new(connection),
        };
        database.migrate()?;
        Ok(database)
    }

    fn connection(&self) -> Result<MutexGuard<'_, Connection>> {
        self.connection
            .lock()
            .map_err(|_| AgentError::new("database lock poisoned"))
    }

    fn migrate(&self) -> Result<()> {
        let connection = self.connection()?;
        connection.execute_batch(
            "CREATE TABLE IF NOT EXISTS app_usage (
                app_id TEXT PRIMARY KEY NOT NULL,
                foreground_active_ms INTEGER NOT NULL DEFAULT 0 CHECK (foreground_active_ms >= 0),
                process_runtime_ms INTEGER NOT NULL DEFAULT 0 CHECK (process_runtime_ms >= 0),
                launch_count INTEGER NOT NULL DEFAULT 0 CHECK (launch_count >= 0),
                last_active_at TEXT NOT NULL
             );
             CREATE TABLE IF NOT EXISTS lifecycle_runs (
                run_id TEXT PRIMARY KEY NOT NULL,
                created_at TEXT NOT NULL,
                timeout_reached_at TEXT NOT NULL,
                frozen_at TEXT,
                status TEXT NOT NULL,
                last_error TEXT,
                shutdown_started_at TEXT,
                completed_at TEXT
             );
             CREATE TABLE IF NOT EXISTS lifecycle_state (
                singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
                state TEXT NOT NULL,
                last_activity_at TEXT,
                timeout_reached_at TEXT,
                active_run_id TEXT REFERENCES lifecycle_runs(run_id),
                last_error TEXT,
                shutdown_started_at TEXT
             );
             CREATE TABLE IF NOT EXISTS backup_apps (
                run_id TEXT NOT NULL REFERENCES lifecycle_runs(run_id),
                app_id TEXT NOT NULL,
                rank INTEGER NOT NULL CHECK (rank BETWEEN 1 AND 10),
                status TEXT NOT NULL CHECK (status IN ('PENDING','RUNNING','VERIFIED','FAILED')),
                attempts INTEGER NOT NULL DEFAULT 0 CHECK (attempts BETWEEN 0 AND 3),
                operation_id TEXT,
                bundle_id TEXT,
                commit_id TEXT,
                last_error TEXT,
                updated_at TEXT NOT NULL,
                PRIMARY KEY (run_id, app_id),
                UNIQUE (run_id, rank)
             );
             CREATE TABLE IF NOT EXISTS seen_sessions (
                session_id TEXT PRIMARY KEY NOT NULL,
                app_id TEXT NOT NULL,
                first_seen_at TEXT NOT NULL
             );
             CREATE TRIGGER IF NOT EXISTS backup_apps_no_insert_after_freeze
             BEFORE INSERT ON backup_apps
             WHEN (SELECT frozen_at FROM lifecycle_runs WHERE run_id = NEW.run_id) IS NOT NULL
             BEGIN
                SELECT RAISE(ABORT, 'backup app selection is frozen');
             END;
             CREATE TRIGGER IF NOT EXISTS backup_apps_selection_immutable
             BEFORE UPDATE OF run_id, app_id, rank ON backup_apps
             BEGIN
                SELECT RAISE(ABORT, 'backup app selection is immutable');
             END;
             CREATE TRIGGER IF NOT EXISTS backup_apps_no_delete
             BEFORE DELETE ON backup_apps
             BEGIN
                SELECT RAISE(ABORT, 'backup app selection is immutable');
             END;
             INSERT OR IGNORE INTO lifecycle_state (
                singleton, state, last_activity_at, timeout_reached_at,
                active_run_id, last_error, shutdown_started_at
             ) VALUES (1, 'DISABLED', NULL, NULL, NULL, NULL, NULL);",
        )?;
        Ok(())
    }

    pub fn initialize_monitoring(&self, enabled: bool, now: DateTime<Utc>) -> Result<()> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction()?;
        let current: String = transaction.query_row(
            "SELECT state FROM lifecycle_state WHERE singleton = 1",
            [],
            |row| row.get(0),
        )?;
        let state = LifecycleState::from_str(&current)?;
        if state.run_is_active()
            || matches!(
                state,
                LifecycleState::BackupFailedSafe
                    | LifecycleState::ShutdownFailedSafe
                    | LifecycleState::Completed
            )
        {
            transaction.commit()?;
            return Ok(());
        }
        let next = if enabled {
            LifecycleState::MonitoringIdle
        } else {
            LifecycleState::Disabled
        };
        transaction.execute(
            "UPDATE lifecycle_state
             SET state = ?1, last_activity_at = ?2, timeout_reached_at = NULL,
                 active_run_id = NULL, last_error = NULL, shutdown_started_at = NULL
             WHERE singleton = 1",
            params![next.as_str(), format_time(now)],
        )?;
        transaction.commit()?;
        Ok(())
    }

    /// Re-arm monitoring after a terminal safety failure. Active runs are
    /// deliberately left untouched so saving settings cannot interrupt backup
    /// or shutdown work already in progress.
    pub fn reset_terminal_failure(&self, now: DateTime<Utc>) -> Result<bool> {
        let changed = self.connection()?.execute(
            "UPDATE lifecycle_state
             SET state = 'MONITORING/IDLE', last_activity_at = ?1,
                 timeout_reached_at = NULL, active_run_id = NULL,
                 last_error = NULL, shutdown_started_at = NULL
             WHERE singleton = 1
               AND state IN ('BACKUP_FAILED_SAFE', 'SHUTDOWN_FAILED_SAFE')",
            params![format_time(now)],
        )?;
        Ok(changed == 1)
    }

    pub fn record_accepted_activity(&self, now: DateTime<Utc>) -> Result<()> {
        self.connection()?.execute(
            "UPDATE lifecycle_state SET last_activity_at = ?1 WHERE singleton = 1",
            params![format_time(now)],
        )?;
        Ok(())
    }

    pub fn transition(
        &self,
        state: LifecycleState,
        now: DateTime<Utc>,
        error: Option<&str>,
    ) -> Result<()> {
        let timeout = (state == LifecycleState::TimeoutReached).then(|| format_time(now));
        self.connection()?.execute(
            "UPDATE lifecycle_state
             SET state = ?1,
                 timeout_reached_at = COALESCE(?2, timeout_reached_at),
                 last_error = ?3
             WHERE singleton = 1",
            params![state.as_str(), timeout, error],
        )?;
        Ok(())
    }

    pub fn mark_shutdown_started(&self, run_id: &str, now: DateTime<Utc>) -> Result<()> {
        let now = format_time(now);
        let mut connection = self.connection()?;
        let transaction = connection.transaction()?;
        transaction.execute(
            "UPDATE lifecycle_runs
             SET status = 'SHUTTING_DOWN', shutdown_started_at = COALESCE(shutdown_started_at, ?1)
             WHERE run_id = ?2",
            params![now, run_id],
        )?;
        transaction.execute(
            "UPDATE lifecycle_state
             SET state = 'SHUTTING_DOWN', shutdown_started_at = COALESCE(shutdown_started_at, ?1),
                 last_error = NULL
             WHERE singleton = 1 AND active_run_id = ?2",
            params![now, run_id],
        )?;
        transaction.commit()?;
        Ok(())
    }

    pub fn complete_run(&self, run_id: &str, now: DateTime<Utc>) -> Result<()> {
        let now = format_time(now);
        let mut connection = self.connection()?;
        let transaction = connection.transaction()?;
        transaction.execute(
            "UPDATE lifecycle_runs SET status = 'COMPLETED', completed_at = ?1, last_error = NULL
             WHERE run_id = ?2",
            params![now, run_id],
        )?;
        transaction.execute(
            "UPDATE lifecycle_state SET state = 'COMPLETED', last_error = NULL
             WHERE singleton = 1 AND active_run_id = ?1",
            params![run_id],
        )?;
        transaction.commit()?;
        Ok(())
    }

    pub fn fail_run(&self, run_id: Option<&str>, state: LifecycleState, error: &str) -> Result<()> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction()?;
        if let Some(run_id) = run_id {
            transaction.execute(
                "UPDATE lifecycle_runs SET status = ?1, last_error = ?2 WHERE run_id = ?3",
                params![state.as_str(), error, run_id],
            )?;
        }
        transaction.execute(
            "UPDATE lifecycle_state SET state = ?1, last_error = ?2 WHERE singleton = 1",
            params![state.as_str(), error],
        )?;
        transaction.commit()?;
        Ok(())
    }

    pub fn record_active_session(
        &self,
        session_id: &str,
        app_id: &str,
        runtime_ms: u64,
        now: DateTime<Utc>,
    ) -> Result<bool> {
        let now = format_time(now);
        let mut connection = self.connection()?;
        let transaction = connection.transaction()?;
        let inserted = transaction.execute(
            "INSERT OR IGNORE INTO seen_sessions (session_id, app_id, first_seen_at)
             VALUES (?1, ?2, ?3)",
            params![session_id, app_id, now],
        )? == 1;
        transaction.execute(
            "INSERT INTO app_usage (
                app_id, foreground_active_ms, process_runtime_ms, launch_count, last_active_at
             ) VALUES (?1, 0, ?2, ?3, ?4)
             ON CONFLICT(app_id) DO UPDATE SET
                process_runtime_ms = process_runtime_ms + excluded.process_runtime_ms,
                launch_count = launch_count + excluded.launch_count,
                last_active_at = excluded.last_active_at",
            params![app_id, to_i64(runtime_ms)?, i64::from(inserted), now],
        )?;
        transaction.commit()?;
        Ok(inserted)
    }

    pub fn record_foreground(
        &self,
        app_id: &str,
        foreground_ms: u64,
        now: DateTime<Utc>,
    ) -> Result<()> {
        self.connection()?.execute(
            "INSERT INTO app_usage (
                app_id, foreground_active_ms, process_runtime_ms, launch_count, last_active_at
             ) VALUES (?1, ?2, 0, 0, ?3)
             ON CONFLICT(app_id) DO UPDATE SET
                foreground_active_ms = foreground_active_ms + excluded.foreground_active_ms,
                last_active_at = excluded.last_active_at",
            params![app_id, to_i64(foreground_ms)?, format_time(now)],
        )?;
        Ok(())
    }

    pub fn ranked_apps(&self, limit: usize) -> Result<Vec<RankedApp>> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT app_id, foreground_active_ms, process_runtime_ms, launch_count, last_active_at
             FROM app_usage
             WHERE foreground_active_ms > 0 OR process_runtime_ms > 0
             ORDER BY foreground_active_ms DESC, last_active_at DESC,
                      process_runtime_ms DESC, app_id ASC
             LIMIT ?1",
        )?;
        let rows = statement.query_map(params![to_i64(limit as u64)?], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, i64>(3)?,
                row.get::<_, String>(4)?,
            ))
        })?;
        let mut apps = Vec::new();
        for row in rows {
            let (app_id, foreground, runtime, launches, last_active) = row?;
            apps.push(RankedApp {
                app_id,
                foreground_active_ms: to_u64(foreground)?,
                process_runtime_ms: to_u64(runtime)?,
                launch_count: to_u64(launches)?,
                last_active_at: parse_time(&last_active)?,
            });
        }
        Ok(apps)
    }

    pub fn create_run_with_frozen_apps(
        &self,
        timeout_reached_at: DateTime<Utc>,
        apps: &[RankedApp],
    ) -> Result<String> {
        if apps.is_empty() {
            return Err(AgentError::new(
                "cannot create a lifecycle run without applications",
            ));
        }
        if apps.len() > 10 {
            return Err(AgentError::new("cannot freeze more than 10 applications"));
        }
        let run_id = Uuid::new_v4().to_string();
        let now = format_time(timeout_reached_at);
        let timeout = now.clone();
        let mut connection = self.connection()?;
        let transaction = connection.transaction()?;
        transaction.execute(
            "INSERT INTO lifecycle_runs (
                run_id, created_at, timeout_reached_at, frozen_at, status
             ) VALUES (?1, ?2, ?3, NULL, 'BACKING_UP')",
            params![run_id, now, timeout],
        )?;
        for (index, app) in apps.iter().enumerate() {
            transaction.execute(
                "INSERT INTO backup_apps (
                    run_id, app_id, rank, status, attempts, updated_at
                 ) VALUES (?1, ?2, ?3, 'PENDING', 0, ?4)",
                params![run_id, app.app_id, (index + 1) as i64, now],
            )?;
        }
        transaction.execute(
            "UPDATE lifecycle_runs SET frozen_at = ?1 WHERE run_id = ?2",
            params![now, run_id],
        )?;
        transaction.execute(
            "UPDATE lifecycle_state
             SET state = 'BACKING_UP', active_run_id = ?1, timeout_reached_at = ?2,
                 last_error = NULL
             WHERE singleton = 1",
            params![run_id, timeout],
        )?;
        transaction.commit()?;
        Ok(run_id)
    }

    pub fn backup_apps(&self, run_id: &str) -> Result<Vec<BackupApp>> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT run_id, app_id, rank, status, attempts, operation_id,
                    bundle_id, commit_id, last_error
             FROM backup_apps WHERE run_id = ?1 ORDER BY rank ASC",
        )?;
        let rows = statement.query_map(params![run_id], |row| {
            Ok(BackupApp {
                run_id: row.get(0)?,
                app_id: row.get(1)?,
                rank: row.get::<_, i64>(2)? as u32,
                status: row.get(3)?,
                attempts: row.get::<_, i64>(4)? as u32,
                operation_id: row.get(5)?,
                bundle_id: row.get(6)?,
                commit_id: row.get(7)?,
                last_error: row.get(8)?,
            })
        })?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Into::into)
    }

    pub fn mark_app_running(
        &self,
        run_id: &str,
        app_id: &str,
        operation_id: &str,
        now: DateTime<Utc>,
    ) -> Result<()> {
        let changed = self.connection()?.execute(
            "UPDATE backup_apps
             SET status = 'RUNNING', attempts = attempts + 1, operation_id = ?1,
                 bundle_id = NULL, commit_id = NULL, last_error = NULL, updated_at = ?2
             WHERE run_id = ?3 AND app_id = ?4 AND status != 'VERIFIED' AND attempts < 3",
            params![operation_id, format_time(now), run_id, app_id],
        )?;
        if changed != 1 {
            return Err(AgentError::new(
                "backup application cannot start another attempt",
            ));
        }
        Ok(())
    }

    pub fn record_start_failure(
        &self,
        run_id: &str,
        app_id: &str,
        error: &str,
        now: DateTime<Utc>,
    ) -> Result<()> {
        let changed = self.connection()?.execute(
            "UPDATE backup_apps
             SET status = 'PENDING', attempts = attempts + 1, operation_id = NULL,
                 last_error = ?1, updated_at = ?2
             WHERE run_id = ?3 AND app_id = ?4 AND status != 'VERIFIED' AND attempts < 3",
            params![error, format_time(now), run_id, app_id],
        )?;
        if changed != 1 {
            return Err(AgentError::new("backup application attempts are exhausted"));
        }
        Ok(())
    }

    pub fn mark_app_pending_retry(
        &self,
        run_id: &str,
        app_id: &str,
        error: &str,
        now: DateTime<Utc>,
    ) -> Result<()> {
        self.connection()?.execute(
            "UPDATE backup_apps
             SET status = 'PENDING', operation_id = NULL, last_error = ?1, updated_at = ?2
             WHERE run_id = ?3 AND app_id = ?4 AND status != 'VERIFIED'",
            params![error, format_time(now), run_id, app_id],
        )?;
        Ok(())
    }

    pub fn mark_app_skipped(
        &self,
        run_id: &str,
        app_id: &str,
        reason: &str,
        now: DateTime<Utc>,
    ) -> Result<()> {
        self.connection()?.execute(
            "UPDATE backup_apps
             SET status = 'FAILED', last_error = ?1, updated_at = ?2
             WHERE run_id = ?3 AND app_id = ?4 AND status != 'VERIFIED'",
            params![
                format!("skipped: {reason}"),
                format_time(now),
                run_id,
                app_id
            ],
        )?;
        Ok(())
    }

    pub fn mark_app_failed(
        &self,
        run_id: &str,
        app_id: &str,
        error: &str,
        now: DateTime<Utc>,
    ) -> Result<()> {
        self.connection()?.execute(
            "UPDATE backup_apps
             SET status = 'FAILED', last_error = ?1, updated_at = ?2
             WHERE run_id = ?3 AND app_id = ?4 AND status != 'VERIFIED'",
            params![error, format_time(now), run_id, app_id],
        )?;
        Ok(())
    }

    pub fn mark_app_verified(
        &self,
        run_id: &str,
        app_id: &str,
        bundle_id: &str,
        commit_id: &str,
        now: DateTime<Utc>,
    ) -> Result<()> {
        self.connection()?.execute(
            "UPDATE backup_apps
             SET status = 'VERIFIED', bundle_id = ?1, commit_id = ?2,
                 last_error = NULL, updated_at = ?3
             WHERE run_id = ?4 AND app_id = ?5",
            params![bundle_id, commit_id, format_time(now), run_id, app_id],
        )?;
        Ok(())
    }

    pub fn all_apps_verified(&self, run_id: &str) -> Result<bool> {
        let (total, verified, skipped): (i64, i64, i64) = self.connection()?.query_row(
            "SELECT COUNT(*),
                    SUM(CASE WHEN status = 'VERIFIED' THEN 1 ELSE 0 END),
                    SUM(CASE WHEN status = 'FAILED' AND last_error LIKE 'skipped: %'
                             THEN 1 ELSE 0 END)
             FROM backup_apps WHERE run_id = ?1",
            params![run_id],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get::<_, Option<i64>>(1)?.unwrap_or(0),
                    row.get::<_, Option<i64>>(2)?.unwrap_or(0),
                ))
            },
        )?;
        Ok(verified > 0 && total == verified + skipped)
    }

    pub fn snapshot(&self) -> Result<LifecycleSnapshot> {
        let connection = self.connection()?;
        let row = connection.query_row(
            "SELECT state, last_activity_at, timeout_reached_at, active_run_id,
                    last_error, shutdown_started_at
             FROM lifecycle_state WHERE singleton = 1",
            [],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, Option<String>>(4)?,
                    row.get::<_, Option<String>>(5)?,
                ))
            },
        )?;
        Ok(LifecycleSnapshot {
            state: LifecycleState::from_str(&row.0)?,
            last_activity_at: row.1.as_deref().map(parse_time).transpose()?,
            timeout_reached_at: row.2.as_deref().map(parse_time).transpose()?,
            active_run_id: row.3,
            last_error: row.4,
            shutdown_started_at: row.5.as_deref().map(parse_time).transpose()?,
        })
    }

    pub fn active_run_id(&self) -> Result<Option<String>> {
        self.connection()?
            .query_row(
                "SELECT active_run_id FROM lifecycle_state WHERE singleton = 1",
                [],
                |row| row.get(0),
            )
            .optional()
            .map(|value| value.flatten())
            .map_err(Into::into)
    }

    #[cfg(test)]
    fn raw_connection(&self) -> Result<MutexGuard<'_, Connection>> {
        self.connection()
    }
}

fn format_time(value: DateTime<Utc>) -> String {
    value.to_rfc3339_opts(SecondsFormat::Millis, true)
}

fn parse_time(value: &str) -> Result<DateTime<Utc>> {
    Ok(DateTime::parse_from_rfc3339(value)?.with_timezone(&Utc))
}

fn to_i64(value: u64) -> Result<i64> {
    i64::try_from(value).map_err(|_| AgentError::new("counter exceeds SQLite integer range"))
}

fn to_u64(value: i64) -> Result<u64> {
    u64::try_from(value).map_err(|_| AgentError::new("database contains a negative counter"))
}

#[cfg(test)]
mod tests {
    use chrono::TimeZone;

    use super::*;

    fn now(seconds: i64) -> DateTime<Utc> {
        Utc.timestamp_opt(seconds, 0).unwrap()
    }

    #[test]
    fn ranking_uses_foreground_then_recency_then_runtime() {
        let db = Database::open_in_memory().unwrap();
        db.record_active_session("s1", "runtime-heavy", 50_000, now(10))
            .unwrap();
        db.record_active_session("s2", "older-foreground", 1_000, now(20))
            .unwrap();
        db.record_foreground("older-foreground", 2_000, now(20))
            .unwrap();
        db.record_active_session("s3", "recent-foreground", 500, now(30))
            .unwrap();
        db.record_foreground("recent-foreground", 2_000, now(30))
            .unwrap();

        let ranked = db.ranked_apps(10).unwrap();
        assert_eq!(ranked[0].app_id, "recent-foreground");
        assert_eq!(ranked[1].app_id, "older-foreground");
        assert_eq!(ranked[2].app_id, "runtime-heavy");
    }

    #[test]
    fn launch_count_is_once_per_session() {
        let db = Database::open_in_memory().unwrap();
        assert!(db
            .record_active_session("s1", "app", 1_000, now(10))
            .unwrap());
        assert!(!db
            .record_active_session("s1", "app", 1_000, now(11))
            .unwrap());
        let app = db.ranked_apps(1).unwrap().remove(0);
        assert_eq!(app.launch_count, 1);
        assert_eq!(app.process_runtime_ms, 2_000);
    }

    #[test]
    fn frozen_top_n_cannot_be_changed() {
        let db = Database::open_in_memory().unwrap();
        db.initialize_monitoring(true, now(1)).unwrap();
        let apps = vec![
            RankedApp {
                app_id: "one".into(),
                foreground_active_ms: 2,
                process_runtime_ms: 2,
                launch_count: 1,
                last_active_at: now(2),
            },
            RankedApp {
                app_id: "two".into(),
                foreground_active_ms: 1,
                process_runtime_ms: 1,
                launch_count: 1,
                last_active_at: now(1),
            },
        ];
        let run = db.create_run_with_frozen_apps(now(3), &apps).unwrap();
        let connection = db.raw_connection().unwrap();
        assert!(connection
            .execute(
                "INSERT INTO backup_apps (run_id, app_id, rank, status, attempts, updated_at)
                 VALUES (?1, 'late', 3, 'PENDING', 0, ?2)",
                params![run, format_time(now(3))],
            )
            .is_err());
        assert!(connection
            .execute(
                "UPDATE backup_apps SET rank = 3 WHERE run_id = ?1 AND app_id = 'one'",
                params![run],
            )
            .is_err());
        assert!(connection
            .execute(
                "DELETE FROM backup_apps WHERE run_id = ?1 AND app_id = 'one'",
                params![run],
            )
            .is_err());
    }
}
