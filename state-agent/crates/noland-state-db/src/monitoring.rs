//! Read-only committed operation snapshots for monitoring, never execution authority.
//!
//! Do not call the lazy, write-back matching projection here. One deferred read
//! transaction pins the operation header, progress and pack totals to the same
//! WAL commit, even if the writer commits again between the SELECTs. Operation
//! results/retry JSON and diagnostic metrics are never read, even for legacy rows.

use chrono::{TimeZone, Utc};
use noland_state_core::*;
use rusqlite::{params, Connection, OptionalExtension};
use uuid::Uuid;

use crate::StateDb;

// Match the existing native client's 4 MiB response bound rather than imposing
// a smaller display-only limit. The client still owns its complete-frame limit;
// this bounds the stored progress JSON read without fetching oversized payloads.
const MAX_PROGRESS_BYTES: i64 = 4 * 1024 * 1024;

#[derive(Debug, Clone)]
pub struct OperationMonitoringSnapshot {
    /// Intentionally excludes result/retry JSON. Never use this for admission,
    /// matching, retry, cancellation or commit verification.
    pub operation: OperationRecord,
    pub progress: Option<OperationProgress>,
    pub pack_transfer: SyncJournalSummary,
}

struct RawSnapshot {
    kind: String,
    app: Option<String>,
    state: String,
    created: i64,
    updated: i64,
    error: Option<String>,
    progress: Option<String>,
    progress_bytes: i64,
}

impl StateDb {
    /// A display-only view of the last committed state, not execution authority.
    /// Database/JSON/size errors propagate: unavailable status is not empty or
    /// zero progress. In-memory fixtures retain their existing single connection.
    pub fn operation_monitoring_snapshot(
        &self,
        id: Uuid,
    ) -> Result<Option<OperationMonitoringSnapshot>> {
        let mut conn = self.browse_lock()?;
        let tx = conn.transaction().map_err(db_err)?;
        let snapshot = read_snapshot(&tx, id, || {})?;
        tx.commit().map_err(db_err)?;
        Ok(snapshot)
    }
}

fn read_snapshot(
    conn: &Connection,
    id: Uuid,
    after_runtime: impl FnOnce(),
) -> Result<Option<OperationMonitoringSnapshot>> {
    let raw = conn
        .query_row(
            "SELECT o.kind, o.app_id, o.state, o.created_at, o.updated_at, o.last_error,
             CASE WHEN length(CAST(r.progress_json AS BLOB)) <= ?2 THEN r.progress_json END,
             COALESCE(length(CAST(r.progress_json AS BLOB)), 0)
             FROM operations o LEFT JOIN operation_runtime r ON r.operation_id=o.operation_id
             WHERE o.operation_id=?1",
            params![id.to_string(), MAX_PROGRESS_BYTES],
            |row| {
                Ok(RawSnapshot {
                    kind: row.get(0)?,
                    app: row.get(1)?,
                    state: row.get(2)?,
                    created: row.get(3)?,
                    updated: row.get(4)?,
                    error: row.get(5)?,
                    progress: row.get(6)?,
                    progress_bytes: row.get(7)?,
                })
            },
        )
        .optional()
        .map_err(db_err)?;
    let Some(raw) = raw else {
        return Ok(None);
    };
    if raw.progress_bytes > MAX_PROGRESS_BYTES {
        return Err(StateError::Database(format!(
            "operation {id} monitoring progress exceeds {MAX_PROGRESS_BYTES} bytes"
        )));
    }
    let progress = raw
        .progress
        .as_deref()
        .map(|value| decode_progress(value, id))
        .transpose()?;
    after_runtime();
    let pack_transfer = pack_summary(conn, id)?;
    let timestamp = |value| {
        Utc.timestamp_opt(value, 0).single().ok_or_else(|| {
            StateError::Database(format!("invalid monitoring timestamp for operation {id}"))
        })
    };
    Ok(Some(OperationMonitoringSnapshot {
        operation: OperationRecord {
            operation_id: id,
            kind: raw.kind,
            app_id: raw.app.map(AppId),
            state: raw.state,
            created_at: timestamp(raw.created)?,
            updated_at: timestamp(raw.updated)?,
            last_error: raw.error,
            detail_json: serde_json::json!({}),
        },
        progress,
        pack_transfer,
    }))
}

fn decode_progress(raw: &str, id: Uuid) -> Result<OperationProgress> {
    let value: serde_json::Value = serde_json::from_str(raw)?;
    // Most progress records rely on their operation_runtime primary key. If an
    // envelope additionally declares its own identity, do not silently discard
    // a contradictory ID during typed deserialization and expose foreign work.
    // Nested repository/child identities are separate business data, not these
    // progress-envelope identities.
    for envelope in [&value, &value["detail_json"]] {
        if let Some(embedded) = envelope
            .get("operation_id")
            .filter(|value| !value.is_null())
        {
            if embedded.as_str().and_then(|raw| Uuid::parse_str(raw).ok()) != Some(id) {
                return Err(StateError::Database(format!(
                    "operation {id} monitoring progress identity mismatch"
                )));
            }
        }
    }
    serde_json::from_value(value).map_err(Into::into)
}

fn pack_summary(conn: &Connection, id: Uuid) -> Result<SyncJournalSummary> {
    conn.query_row(
        "SELECT COUNT(*),
         COALESCE(SUM(state='PENDING'), 0), COALESCE(SUM(state='IN_PROGRESS'), 0),
         COALESCE(SUM(state='RETRY_SCHEDULED'), 0), COALESCE(SUM(state='COMPLETED'), 0),
         COALESCE(SUM(state='FAILED'), 0), COALESCE(SUM(state='SKIPPED'), 0),
         COALESCE(SUM(size), 0),
         COALESCE(SUM(CASE WHEN state='COMPLETED' THEN size ELSE 0 END), 0),
         COALESCE(SUM(bytes_transferred), 0)
         FROM sync_journal_items WHERE operation_id=?1 AND item_kind='PACK'",
        [id.to_string()],
        |row| {
            let count = |index| row.get::<_, i64>(index).map(|value| value.max(0) as u64);
            Ok(SyncJournalSummary {
                total_items: count(0)?,
                pending_items: count(1)?,
                in_progress_items: count(2)?,
                retry_scheduled_items: count(3)?,
                completed_items: count(4)?,
                failed_items: count(5)?,
                skipped_items: count(6)?,
                total_bytes: count(7)?,
                completed_bytes: count(8)?,
                bytes_transferred: count(9)?,
            })
        },
    )
    .map_err(db_err)
}

fn db_err(error: rusqlite::Error) -> StateError {
    StateError::Database(error.to_string())
}

#[cfg(test)]
#[path = "monitoring_tests.rs"]
mod tests;
