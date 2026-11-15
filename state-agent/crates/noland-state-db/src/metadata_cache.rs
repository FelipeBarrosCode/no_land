//! Bounded, loss-tolerant metadata hints. Authentication belongs to the caller.
use chrono::Utc;
use noland_state_core::{Result, StateError};
use rusqlite::{params, OptionalExtension};

use crate::StateDb;

const MAX_ENTRY_BYTES: usize = 8 * 1024 * 1024;
const MAX_CACHE_BYTES: i64 = 64 * 1024 * 1024;
const MAX_CACHE_ENTRIES: i64 = 128;

pub struct CachedMetadata {
    pub payload: Vec<u8>,
    pub receipt: String,
}

fn db_err(error: rusqlite::Error) -> StateError {
    StateError::Database(error.to_string())
}

impl StateDb {
    /// Size filtering happens in SQLite, before allocating the cached payload.
    pub fn cached_metadata(&self, binding: &str, path: &str) -> Result<Option<CachedMetadata>> {
        self.lock()?
            .query_row(
                "SELECT payload, receipt FROM immutable_metadata_cache
                 WHERE storage_binding=?1 AND remote_path=?2 AND length(payload)<=?3",
                params![binding, path, MAX_ENTRY_BYTES as i64],
                |row| {
                    Ok(CachedMetadata {
                        payload: row.get(0)?,
                        receipt: row.get(1)?,
                    })
                },
            )
            .optional()
            .map_err(db_err)
    }

    /// Call only after authenticating an immutable object. Never stores absence.
    pub fn cache_authenticated_metadata(
        &self,
        binding: &str,
        path: &str,
        payload: &[u8],
        receipt: &str,
    ) -> Result<()> {
        if payload.is_empty() || payload.len() > MAX_ENTRY_BYTES {
            return Ok(());
        }
        let mut conn = self.lock()?;
        let tx = conn.transaction().map_err(db_err)?;
        tx.execute(
            "INSERT INTO immutable_metadata_cache
             (storage_binding, remote_path, payload, receipt, observed_at_ms) VALUES (?1,?2,?3,?4,?5)
             ON CONFLICT(storage_binding,remote_path) DO UPDATE SET
             payload=excluded.payload, receipt=excluded.receipt, observed_at_ms=excluded.observed_at_ms",
            params![binding, path, payload, receipt, Utc::now().timestamp_millis()],
        ).map_err(db_err)?;
        // Cumulative newest-first bounds cap both rows and bytes, globally. This
        // does not touch remote_content_index.etag (an authenticated index receipt).
        tx.execute(
            "DELETE FROM immutable_metadata_cache WHERE rowid IN (
               SELECT rowid FROM (
                 SELECT rowid,
                   row_number() OVER (ORDER BY observed_at_ms DESC, rowid DESC) AS n,
                   sum(length(payload)) OVER (ORDER BY observed_at_ms DESC, rowid DESC) AS bytes
                 FROM immutable_metadata_cache
               ) WHERE n>?1 OR bytes>?2
             )",
            params![MAX_CACHE_ENTRIES, MAX_CACHE_BYTES],
        )
        .map_err(db_err)?;
        tx.commit().map_err(db_err)
    }

    pub fn invalidate_cached_metadata(&self, binding: &str) -> Result<()> {
        self.lock()?
            .execute(
                "DELETE FROM immutable_metadata_cache WHERE storage_binding=?1",
                [binding],
            )
            .map_err(db_err)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metadata_hints_are_scoped_bounded_and_disposable() {
        let db = StateDb::open_in_memory().unwrap();
        db.lock()
            .unwrap()
            .execute(
                "INSERT INTO remote_content_index
             (storage_id, object_kind, content_hash, remote_path, etag, state, observed_at_ms)
             VALUES ('one','chunk','hash','packs/pack','authenticated-index-receipt','present',0)",
                [],
            )
            .unwrap();
        for i in 0..140 {
            db.cache_authenticated_metadata("one", &format!("path-{i}"), b"encrypted", "mac")
                .unwrap();
        }
        assert!(db.cached_metadata("one", "path-0").unwrap().is_none());
        assert!(db.cached_metadata("one", "path-139").unwrap().is_some());
        assert!(db
            .cached_metadata("other-key-or-account", "path-139")
            .unwrap()
            .is_none());
        db.cache_authenticated_metadata("two", "path", b"ciphertext", "mac")
            .unwrap();
        db.invalidate_cached_metadata("one").unwrap();
        assert!(db.cached_metadata("one", "path-139").unwrap().is_none());
        assert!(db.cached_metadata("two", "path").unwrap().is_some());
        db.cache_authenticated_metadata("two", "empty", b"", "mac")
            .unwrap();
        assert!(db.cached_metadata("two", "empty").unwrap().is_none());
        let receipt: String = db
            .lock()
            .unwrap()
            .query_row(
                "SELECT etag FROM remote_content_index WHERE storage_id='one'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            receipt, "authenticated-index-receipt",
            "metadata eviction/invalidation must not touch chunk receipts"
        );
    }
}
