//! Small operation reads. Legacy rows are upgraded lazily, outside the DB lock.
use chrono::{TimeZone, Utc};
use noland_state_core::*;
use rusqlite::{params, OptionalExtension};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::StateDb;

fn db_err(error: rusqlite::Error) -> StateError {
    StateError::Database(error.to_string())
}

pub(crate) fn matching_details(detail: &Value) -> Value {
    fn fields(value: &Value) -> Value {
        let mut out = serde_json::Map::new();
        for key in [
            "app_id",
            "bundle_id",
            "mode",
            "performance_mode",
            "regenerable_caches",
            "client_operation_id",
        ] {
            if let Some(value) = value.get(key).and_then(Value::as_str) {
                out.insert(key.into(), json!(value));
            }
        }
        if let Some(identity) = value.get("client_storage_identity") {
            let keys = ["provider", "backend_type", "remote_name", "root"];
            if keys
                .iter()
                .all(|key| identity.get(key).is_some_and(Value::is_string))
            {
                out.insert(
                    "client_storage_identity".into(),
                    Value::Object(
                        keys.into_iter()
                            .map(|key| (key.into(), identity[key].clone()))
                            .collect(),
                    ),
                );
            }
        }
        Value::Object(out)
    }
    let mut out = fields(detail);
    out["retry"] = json!({"params": fields(&detail["retry"]["params"])});
    out
}

type Raw = (
    String,
    String,
    Option<String>,
    String,
    i64,
    i64,
    Option<String>,
    Option<String>,
    bool,
);

fn raw(row: &rusqlite::Row<'_>) -> rusqlite::Result<Raw> {
    Ok((
        row.get(0)?,
        row.get(1)?,
        row.get(2)?,
        row.get(3)?,
        row.get(4)?,
        row.get(5)?,
        row.get(6)?,
        row.get(7)?,
        row.get(8)?,
    ))
}

impl StateDb {
    pub fn projected_operation(&self, id: Uuid, matching: bool) -> Result<Option<OperationRecord>> {
        let sql = if matching {
            "SELECT o.operation_id, o.kind, o.app_id, o.state, o.created_at, o.updated_at, o.last_error,
             COALESCE(p.detail_json,o.detail_json), p.operation_id IS NULL FROM operations o
             LEFT JOIN operation_projections p ON p.operation_id=o.operation_id WHERE o.operation_id=?1"
        } else {
            "SELECT operation_id, kind, app_id, state, created_at, updated_at, last_error,
             NULL, false FROM operations WHERE operation_id=?1"
        };
        let record = self
            .lock()?
            .query_row(sql, [id.to_string()], raw)
            .optional()
            .map_err(db_err)?;
        record.map(|row| self.decode_projected(row)).transpose()
    }

    pub fn recent_matching_operations(
        &self,
        app: Option<&AppId>,
        limit: usize,
    ) -> Result<Vec<OperationRecord>> {
        let rows = {
            let conn = self.lock()?;
            let mut stmt = conn.prepare(
                "SELECT o.operation_id, o.kind, o.app_id, o.state, o.created_at, o.updated_at, o.last_error,
                 COALESCE(p.detail_json,o.detail_json), p.operation_id IS NULL FROM operations o
                 LEFT JOIN operation_projections p ON p.operation_id=o.operation_id
                 WHERE (?1 IS NULL OR o.app_id=?1)
                 ORDER BY o.updated_at DESC, o.created_at DESC LIMIT ?2"
            ).map_err(db_err)?;
            let rows = stmt
                .query_map(
                    params![app.map(AppId::as_str), limit.min(10_000) as i64],
                    raw,
                )
                .map_err(db_err)?
                .collect::<std::result::Result<Vec<_>, _>>()
                .map_err(db_err)?;
            rows
        };
        rows.into_iter()
            .map(|row| self.decode_projected(row))
            .collect()
    }

    fn decode_projected(&self, row: Raw) -> Result<OperationRecord> {
        let (id, kind, app, state, created, updated, error, detail, legacy) = row;
        let detail_json = match &detail {
            Some(raw) => {
                let value = serde_json::from_str(raw).unwrap_or_else(|_| json!({}));
                if legacy {
                    matching_details(&value)
                } else {
                    value
                }
            }
            None => json!({}),
        };
        if legacy {
            // Do not cache a stale projection if a writer advanced this row
            // while its (potentially large) legacy JSON was parsed above.
            self.lock()?
                .execute(
                    "INSERT OR IGNORE INTO operation_projections(operation_id, detail_json)
                 SELECT operation_id, ?2 FROM operations WHERE operation_id=?1 AND detail_json=?3",
                    params![id, detail_json.to_string(), detail],
                )
                .map_err(db_err)?;
        }
        Ok(OperationRecord {
            operation_id: Uuid::parse_str(&id).unwrap_or_else(|_| Uuid::nil()),
            kind,
            app_id: app.map(AppId),
            state,
            created_at: Utc.timestamp_opt(created, 0).single().unwrap_or_default(),
            updated_at: Utc.timestamp_opt(updated, 0).single().unwrap_or_default(),
            last_error: error,
            detail_json,
        })
    }
}

#[cfg(test)]
#[path = "projection_tests.rs"]
mod tests;
