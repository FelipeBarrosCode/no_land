use chrono::{DateTime, TimeZone, Utc};
use noland_state_core::*;
use rusqlite::{params, OptionalExtension};
use uuid::Uuid;

use crate::StateDb;

impl StateDb {
    /// Idempotently appends a mutation. Reusing a `mutation_id` leaves the
    /// original record unchanged.
    pub fn append_app_mutation(&self, mutation: &AppMutationRecord) -> Result<()> {
        let provenance_json = mutation
            .provenance
            .as_ref()
            .map(serde_json::to_string)
            .transpose()?;
        self.lock()?
            .execute(
                r#"INSERT INTO app_mutation_journal (
                       mutation_id, app_id, path, previous_path, kind, observed_at_ms,
                       session_id, provenance_json, processed_at_ms
                   ) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9)
                   ON CONFLICT(mutation_id) DO NOTHING"#,
                params![
                    mutation.mutation_id.to_string(),
                    mutation.app_id.as_str(),
                    mutation.path,
                    mutation.previous_path,
                    mutation.kind.as_str(),
                    mutation.observed_at.timestamp_millis(),
                    mutation.session_id.map(|id| id.to_string()),
                    provenance_json,
                    mutation.processed_at.map(|at| at.timestamp_millis()),
                ],
            )
            .map_err(db_err)?;
        Ok(())
    }

    /// Returns unprocessed mutations oldest-first. A zero limit returns no rows;
    /// larger limits are capped to 10,000 to bound memory use.
    pub fn pending_app_mutations(
        &self,
        app_id: &AppId,
        limit: usize,
    ) -> Result<Vec<AppMutationRecord>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let conn = self.lock()?;
        let mut stmt = conn
            .prepare(
                r#"SELECT mutation_id, app_id, path, previous_path, kind, observed_at_ms,
                          session_id, provenance_json, processed_at_ms
                   FROM app_mutation_journal
                   WHERE app_id=?1 AND processed_at_ms IS NULL
                   ORDER BY observed_at_ms ASC, mutation_id ASC LIMIT ?2"#,
            )
            .map_err(db_err)?;
        let rows = stmt
            .query_map(
                params![app_id.as_str(), bounded_limit(limit)],
                row_to_app_mutation,
            )
            .map_err(db_err)?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(db_err)?;
        Ok(rows)
    }

    /// Marks the supplied mutations processed in one transaction. Unknown IDs
    /// are ignored; the return value is the number of rows changed.
    pub fn mark_app_mutations_processed(
        &self,
        mutation_ids: &[Uuid],
        processed_at: DateTime<Utc>,
    ) -> Result<usize> {
        let mut conn = self.lock()?;
        let tx = conn.transaction().map_err(db_err)?;
        let mut changed = 0;
        for mutation_id in mutation_ids {
            changed += tx
                .execute(
                    r#"UPDATE app_mutation_journal SET processed_at_ms=?2
                       WHERE mutation_id=?1 AND processed_at_ms IS NULL"#,
                    params![mutation_id.to_string(), processed_at.timestamp_millis()],
                )
                .map_err(db_err)?;
        }
        tx.commit().map_err(db_err)?;
        Ok(changed)
    }

    pub fn prune_processed_app_mutations(&self, before: DateTime<Utc>) -> Result<usize> {
        self.lock()?
            .execute(
                "DELETE FROM app_mutation_journal WHERE processed_at_ms IS NOT NULL AND processed_at_ms < ?1",
                params![before.timestamp_millis()],
            )
            .map_err(db_err)
    }

    /// Marks a root dirty, preserving its first timestamp and incrementing its
    /// mutation count. Reconciliation requirements are sticky until cleared.
    pub fn mark_dirty_root(
        &self,
        app_id: &AppId,
        canonical_root: &str,
        logical_root: Option<&str>,
        requires_reconciliation: bool,
    ) -> Result<()> {
        let now = Utc::now().timestamp_millis();
        self.lock()?
            .execute(
                r#"INSERT INTO dirty_roots (
                       app_id, canonical_root, logical_root, first_dirty_at_ms,
                       last_dirty_at_ms, mutation_count, requires_reconciliation
                   ) VALUES (?1,?2,?3,?4,?4,1,?5)
                   ON CONFLICT(app_id, canonical_root) DO UPDATE SET
                       logical_root=COALESCE(excluded.logical_root, dirty_roots.logical_root),
                       last_dirty_at_ms=excluded.last_dirty_at_ms,
                       mutation_count=dirty_roots.mutation_count+1,
                       requires_reconciliation=MAX(
                           dirty_roots.requires_reconciliation,
                           excluded.requires_reconciliation
                       )"#,
                params![
                    app_id.as_str(),
                    canonical_root,
                    logical_root,
                    now,
                    requires_reconciliation as i64,
                ],
            )
            .map_err(db_err)?;
        Ok(())
    }

    pub fn list_dirty_roots(&self, app_id: Option<&AppId>) -> Result<Vec<DirtyRootRecord>> {
        let conn = self.lock()?;
        if let Some(app_id) = app_id {
            let mut stmt = conn
                .prepare(
                    r#"SELECT app_id, canonical_root, logical_root, first_dirty_at_ms,
                              last_dirty_at_ms, mutation_count, requires_reconciliation
                       FROM dirty_roots WHERE app_id=?1
                       ORDER BY last_dirty_at_ms ASC, canonical_root ASC"#,
                )
                .map_err(db_err)?;
            let rows = stmt
                .query_map(params![app_id.as_str()], row_to_dirty_root)
                .map_err(db_err)?
                .collect::<std::result::Result<Vec<_>, _>>()
                .map_err(db_err)?;
            return Ok(rows);
        }

        let mut stmt = conn
            .prepare(
                r#"SELECT app_id, canonical_root, logical_root, first_dirty_at_ms,
                          last_dirty_at_ms, mutation_count, requires_reconciliation
                   FROM dirty_roots ORDER BY last_dirty_at_ms ASC, app_id ASC, canonical_root ASC"#,
            )
            .map_err(db_err)?;
        let rows = stmt
            .query_map([], row_to_dirty_root)
            .map_err(db_err)?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(db_err)?;
        Ok(rows)
    }

    pub fn clear_dirty_root(&self, app_id: &AppId, canonical_root: &str) -> Result<bool> {
        let changed = self
            .lock()?
            .execute(
                "DELETE FROM dirty_roots WHERE app_id=?1 AND canonical_root=?2",
                params![app_id.as_str(), canonical_root],
            )
            .map_err(db_err)?;
        Ok(changed != 0)
    }

    pub fn clear_dirty_roots(&self, app_id: &AppId) -> Result<usize> {
        self.lock()?
            .execute(
                "DELETE FROM dirty_roots WHERE app_id=?1",
                params![app_id.as_str()],
            )
            .map_err(db_err)
    }

    pub fn upsert_file_state(&self, state: &FileStateRecord) -> Result<()> {
        self.lock()?
            .execute(
                r#"INSERT INTO file_state_index (
                       app_id, logical_root, relative_path, canonical_path, file_type,
                       size, mtime_ns, inode, mount_id, mode, content_hash, trust_state,
                       last_seen_at_ms, last_hashed_at_ms
                   ) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14)
                   ON CONFLICT(app_id, logical_root, relative_path) DO UPDATE SET
                       canonical_path=excluded.canonical_path,
                       file_type=excluded.file_type,
                       size=excluded.size,
                       mtime_ns=excluded.mtime_ns,
                       inode=excluded.inode,
                       mount_id=excluded.mount_id,
                       mode=excluded.mode,
                       content_hash=excluded.content_hash,
                       trust_state=excluded.trust_state,
                       last_seen_at_ms=excluded.last_seen_at_ms,
                       last_hashed_at_ms=excluded.last_hashed_at_ms"#,
                params![
                    state.app_id.as_str(),
                    state.logical_root,
                    state.relative_path,
                    state.canonical_path,
                    state.file_type.as_str(),
                    to_i64(state.size, "file size")?,
                    state.mtime_ns,
                    opt_u64_to_i64(state.inode, "inode")?,
                    opt_u64_to_i64(state.mount_id, "mount id")?,
                    state.mode.map(i64::from),
                    state.content_hash,
                    state.trust.as_str(),
                    state.last_seen_at.timestamp_millis(),
                    state.last_hashed_at.map(|at| at.timestamp_millis()),
                ],
            )
            .map_err(db_err)?;
        Ok(())
    }

    pub fn get_file_state(
        &self,
        app_id: &AppId,
        logical_root: &str,
        relative_path: &str,
    ) -> Result<Option<FileStateRecord>> {
        self.lock()?
            .query_row(
                r#"SELECT app_id, logical_root, relative_path, canonical_path, file_type,
                          size, mtime_ns, inode, mount_id, mode, content_hash, trust_state,
                          last_seen_at_ms, last_hashed_at_ms
                   FROM file_state_index
                   WHERE app_id=?1 AND logical_root=?2 AND relative_path=?3"#,
                params![app_id.as_str(), logical_root, relative_path],
                row_to_file_state,
            )
            .optional()
            .map_err(db_err)
    }

    pub fn list_file_states(
        &self,
        app_id: &AppId,
        logical_root: Option<&str>,
    ) -> Result<Vec<FileStateRecord>> {
        let conn = self.lock()?;
        if let Some(logical_root) = logical_root {
            let mut stmt = conn
                .prepare(
                    r#"SELECT app_id, logical_root, relative_path, canonical_path, file_type,
                              size, mtime_ns, inode, mount_id, mode, content_hash, trust_state,
                              last_seen_at_ms, last_hashed_at_ms
                       FROM file_state_index WHERE app_id=?1 AND logical_root=?2
                       ORDER BY relative_path"#,
                )
                .map_err(db_err)?;
            let rows = stmt
                .query_map(params![app_id.as_str(), logical_root], row_to_file_state)
                .map_err(db_err)?
                .collect::<std::result::Result<Vec<_>, _>>()
                .map_err(db_err)?;
            return Ok(rows);
        }

        let mut stmt = conn
            .prepare(
                r#"SELECT app_id, logical_root, relative_path, canonical_path, file_type,
                          size, mtime_ns, inode, mount_id, mode, content_hash, trust_state,
                          last_seen_at_ms, last_hashed_at_ms
                   FROM file_state_index WHERE app_id=?1
                   ORDER BY logical_root, relative_path"#,
            )
            .map_err(db_err)?;
        let rows = stmt
            .query_map(params![app_id.as_str()], row_to_file_state)
            .map_err(db_err)?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(db_err)?;
        Ok(rows)
    }

    pub fn set_file_state_trust(
        &self,
        app_id: &AppId,
        logical_root: &str,
        relative_path: &str,
        trust: FileStateTrust,
    ) -> Result<bool> {
        let changed = self
            .lock()?
            .execute(
                r#"UPDATE file_state_index SET trust_state=?4
                   WHERE app_id=?1 AND logical_root=?2 AND relative_path=?3"#,
                params![app_id.as_str(), logical_root, relative_path, trust.as_str()],
            )
            .map_err(db_err)?;
        Ok(changed != 0)
    }

    pub fn remove_file_state(
        &self,
        app_id: &AppId,
        logical_root: &str,
        relative_path: &str,
    ) -> Result<bool> {
        let changed = self
            .lock()?
            .execute(
                "DELETE FROM file_state_index WHERE app_id=?1 AND logical_root=?2 AND relative_path=?3",
                params![app_id.as_str(), logical_root, relative_path],
            )
            .map_err(db_err)?;
        Ok(changed != 0)
    }

    pub fn upsert_local_cas_entry(&self, entry: &LocalCasEntry) -> Result<()> {
        self.lock()?
            .execute(
                r#"INSERT INTO local_cas_index (
                       object_kind, content_hash, local_path, size, created_at_ms,
                       verified_at_ms, last_accessed_at_ms
                   ) VALUES (?1,?2,?3,?4,?5,?6,?7)
                   ON CONFLICT(object_kind, content_hash) DO UPDATE SET
                       local_path=excluded.local_path,
                       size=excluded.size,
                       verified_at_ms=COALESCE(excluded.verified_at_ms, local_cas_index.verified_at_ms),
                       last_accessed_at_ms=excluded.last_accessed_at_ms"#,
                params![
                    entry.object_kind.as_str(),
                    entry.content_hash,
                    entry.local_path,
                    to_i64(entry.size, "CAS object size")?,
                    entry.created_at.timestamp_millis(),
                    entry.verified_at.map(|at| at.timestamp_millis()),
                    entry.last_accessed_at.timestamp_millis(),
                ],
            )
            .map_err(db_err)?;
        Ok(())
    }

    pub fn get_local_cas_entry(
        &self,
        object_kind: ContentObjectKind,
        content_hash: &str,
    ) -> Result<Option<LocalCasEntry>> {
        self.lock()?
            .query_row(
                r#"SELECT object_kind, content_hash, local_path, size, created_at_ms,
                          verified_at_ms, last_accessed_at_ms
                   FROM local_cas_index WHERE object_kind=?1 AND content_hash=?2"#,
                params![object_kind.as_str(), content_hash],
                row_to_local_cas,
            )
            .optional()
            .map_err(db_err)
    }

    pub fn touch_local_cas_entry(
        &self,
        object_kind: ContentObjectKind,
        content_hash: &str,
        accessed_at: DateTime<Utc>,
    ) -> Result<bool> {
        let changed = self
            .lock()?
            .execute(
                r#"UPDATE local_cas_index SET last_accessed_at_ms=?3
                   WHERE object_kind=?1 AND content_hash=?2"#,
                params![
                    object_kind.as_str(),
                    content_hash,
                    accessed_at.timestamp_millis()
                ],
            )
            .map_err(db_err)?;
        Ok(changed != 0)
    }

    pub fn remove_local_cas_entry(
        &self,
        object_kind: ContentObjectKind,
        content_hash: &str,
    ) -> Result<bool> {
        let changed = self
            .lock()?
            .execute(
                "DELETE FROM local_cas_index WHERE object_kind=?1 AND content_hash=?2",
                params![object_kind.as_str(), content_hash],
            )
            .map_err(db_err)?;
        Ok(changed != 0)
    }

    pub fn upsert_remote_content_entry(&self, entry: &RemoteContentEntry) -> Result<()> {
        self.lock()?
            .execute(
                r#"INSERT INTO remote_content_index (
                       storage_id, object_kind, content_hash, remote_path, size, etag,
                       state, observed_at_ms, expires_at_ms
                   ) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9)
                   ON CONFLICT(storage_id, object_kind, content_hash) DO UPDATE SET
                       remote_path=excluded.remote_path,
                       size=excluded.size,
                       etag=excluded.etag,
                       state=excluded.state,
                       observed_at_ms=excluded.observed_at_ms,
                       expires_at_ms=excluded.expires_at_ms"#,
                params![
                    entry.storage_id,
                    entry.object_kind.as_str(),
                    entry.content_hash,
                    entry.remote_path,
                    opt_u64_to_i64(entry.size, "remote object size")?,
                    entry.etag,
                    entry.state.as_str(),
                    entry.observed_at.timestamp_millis(),
                    entry.expires_at.map(|at| at.timestamp_millis()),
                ],
            )
            .map_err(db_err)?;
        Ok(())
    }

    pub fn get_remote_content_by_hash(
        &self,
        storage_id: &str,
        object_kind: ContentObjectKind,
        content_hash: &str,
    ) -> Result<Option<RemoteContentEntry>> {
        self.lock()?
            .query_row(
                r#"SELECT storage_id, object_kind, content_hash, remote_path, size, etag,
                          state, observed_at_ms, expires_at_ms
                   FROM remote_content_index
                   WHERE storage_id=?1 AND object_kind=?2 AND content_hash=?3"#,
                params![storage_id, object_kind.as_str(), content_hash],
                row_to_remote_content,
            )
            .optional()
            .map_err(db_err)
    }

    pub fn get_remote_content_by_path(
        &self,
        storage_id: &str,
        remote_path: &str,
    ) -> Result<Option<RemoteContentEntry>> {
        self.lock()?
            .query_row(
                r#"SELECT storage_id, object_kind, content_hash, remote_path, size, etag,
                          state, observed_at_ms, expires_at_ms
                   FROM remote_content_index
                   WHERE storage_id=?1 AND remote_path=?2
                   ORDER BY observed_at_ms DESC LIMIT 1"#,
                params![storage_id, remote_path],
                row_to_remote_content,
            )
            .optional()
            .map_err(db_err)
    }

    pub fn remove_remote_content_entry(
        &self,
        storage_id: &str,
        object_kind: ContentObjectKind,
        content_hash: &str,
    ) -> Result<bool> {
        let changed = self
            .lock()?
            .execute(
                r#"DELETE FROM remote_content_index
                   WHERE storage_id=?1 AND object_kind=?2 AND content_hash=?3"#,
                params![storage_id, object_kind.as_str(), content_hash],
            )
            .map_err(db_err)?;
        Ok(changed != 0)
    }

    /// Upserts the complete journal entry without implicit state transitions.
    pub fn upsert_sync_journal_entry(&self, entry: &SyncJournalEntry) -> Result<()> {
        self.lock()?
            .execute(
                r#"INSERT INTO sync_journal_items (
                       operation_id, item_key, item_kind, direction, state, local_path,
                       remote_path, content_hash, size, attempts, bytes_transferred,
                       last_error, created_at_ms, updated_at_ms, started_at_ms,
                       completed_at_ms, next_retry_at_ms, detail_json
                   ) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18)
                   ON CONFLICT(operation_id, item_key) DO UPDATE SET
                       item_kind=excluded.item_kind,
                       direction=excluded.direction,
                       state=excluded.state,
                       local_path=excluded.local_path,
                       remote_path=excluded.remote_path,
                       content_hash=excluded.content_hash,
                       size=excluded.size,
                       attempts=excluded.attempts,
                       bytes_transferred=excluded.bytes_transferred,
                       last_error=excluded.last_error,
                       updated_at_ms=excluded.updated_at_ms,
                       started_at_ms=excluded.started_at_ms,
                       completed_at_ms=excluded.completed_at_ms,
                       next_retry_at_ms=excluded.next_retry_at_ms,
                       detail_json=excluded.detail_json"#,
                params![
                    entry.operation_id.to_string(),
                    entry.item_key,
                    entry.item_kind.as_str(),
                    entry.direction.as_str(),
                    entry.state.as_str(),
                    entry.local_path,
                    entry.remote_path,
                    entry.content_hash,
                    opt_u64_to_i64(entry.size, "sync item size")?,
                    i64::from(entry.attempts),
                    to_i64(entry.bytes_transferred, "transferred byte count")?,
                    entry.last_error,
                    entry.created_at.timestamp_millis(),
                    entry.updated_at.timestamp_millis(),
                    entry.started_at.map(|at| at.timestamp_millis()),
                    entry.completed_at.map(|at| at.timestamp_millis()),
                    entry.next_retry_at.map(|at| at.timestamp_millis()),
                    entry.detail_json.to_string(),
                ],
            )
            .map_err(db_err)?;
        Ok(())
    }

    pub fn get_sync_journal_entry(
        &self,
        operation_id: Uuid,
        item_key: &str,
    ) -> Result<Option<SyncJournalEntry>> {
        self.lock()?
            .query_row(
                &sync_journal_select("WHERE operation_id=?1 AND item_key=?2"),
                params![operation_id.to_string(), item_key],
                row_to_sync_journal_entry,
            )
            .optional()
            .map_err(db_err)
    }

    pub fn list_sync_journal_entries(
        &self,
        operation_id: Uuid,
        state: Option<SyncJournalState>,
    ) -> Result<Vec<SyncJournalEntry>> {
        let conn = self.lock()?;
        if let Some(state) = state {
            let mut stmt = conn
                .prepare(&sync_journal_select(
                    "WHERE operation_id=?1 AND state=?2 ORDER BY created_at_ms, item_key",
                ))
                .map_err(db_err)?;
            let rows = stmt
                .query_map(
                    params![operation_id.to_string(), state.as_str()],
                    row_to_sync_journal_entry,
                )
                .map_err(db_err)?
                .collect::<std::result::Result<Vec<_>, _>>()
                .map_err(db_err)?;
            return Ok(rows);
        }

        let mut stmt = conn
            .prepare(&sync_journal_select(
                "WHERE operation_id=?1 ORDER BY created_at_ms, item_key",
            ))
            .map_err(db_err)?;
        let rows = stmt
            .query_map(params![operation_id.to_string()], row_to_sync_journal_entry)
            .map_err(db_err)?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(db_err)?;
        Ok(rows)
    }

    /// Applies a journal state transition. Attempts increment once when entering
    /// `IN_PROGRESS`; terminal states stamp `completed_at`.
    pub fn set_sync_journal_state(
        &self,
        operation_id: Uuid,
        item_key: &str,
        state: SyncJournalState,
        last_error: Option<&str>,
        next_retry_at: Option<DateTime<Utc>>,
    ) -> Result<bool> {
        let now = Utc::now().timestamp_millis();
        let changed = self
            .lock()?
            .execute(
                r#"UPDATE sync_journal_items SET
                       state=?3,
                       attempts=attempts + CASE
                           WHEN ?3='IN_PROGRESS' AND state<>'IN_PROGRESS' THEN 1 ELSE 0 END,
                       last_error=?4,
                       updated_at_ms=?5,
                       started_at_ms=CASE
                           WHEN ?3='IN_PROGRESS' THEN COALESCE(started_at_ms, ?5)
                           ELSE started_at_ms END,
                       completed_at_ms=CASE
                           WHEN ?6=1 THEN ?5 ELSE NULL END,
                       next_retry_at_ms=?7
                   WHERE operation_id=?1 AND item_key=?2"#,
                params![
                    operation_id.to_string(),
                    item_key,
                    state.as_str(),
                    last_error,
                    now,
                    state.is_terminal() as i64,
                    next_retry_at.map(|at| at.timestamp_millis()),
                ],
            )
            .map_err(db_err)?;
        Ok(changed != 0)
    }

    /// Stores monotonic byte progress for an existing journal item.
    pub fn update_sync_journal_progress(
        &self,
        operation_id: Uuid,
        item_key: &str,
        bytes_transferred: u64,
    ) -> Result<bool> {
        let changed = self
            .lock()?
            .execute(
                r#"UPDATE sync_journal_items
                   SET bytes_transferred=MAX(bytes_transferred, ?3), updated_at_ms=?4
                   WHERE operation_id=?1 AND item_key=?2"#,
                params![
                    operation_id.to_string(),
                    item_key,
                    to_i64(bytes_transferred, "transferred byte count")?,
                    Utc::now().timestamp_millis(),
                ],
            )
            .map_err(db_err)?;
        Ok(changed != 0)
    }

    pub fn sync_journal_summary(&self, operation_id: Uuid) -> Result<SyncJournalSummary> {
        self.lock()?
            .query_row(
                r#"SELECT
                       COUNT(*),
                       COALESCE(SUM(state='PENDING'), 0),
                       COALESCE(SUM(state='IN_PROGRESS'), 0),
                       COALESCE(SUM(state='RETRY_SCHEDULED'), 0),
                       COALESCE(SUM(state='COMPLETED'), 0),
                       COALESCE(SUM(state='FAILED'), 0),
                       COALESCE(SUM(state='SKIPPED'), 0),
                       COALESCE(SUM(size), 0),
                       COALESCE(SUM(bytes_transferred), 0)
                   FROM sync_journal_items WHERE operation_id=?1"#,
                params![operation_id.to_string()],
                |row| {
                    Ok(SyncJournalSummary {
                        total_items: nonnegative(row.get(0)?),
                        pending_items: nonnegative(row.get(1)?),
                        in_progress_items: nonnegative(row.get(2)?),
                        retry_scheduled_items: nonnegative(row.get(3)?),
                        completed_items: nonnegative(row.get(4)?),
                        failed_items: nonnegative(row.get(5)?),
                        skipped_items: nonnegative(row.get(6)?),
                        total_bytes: nonnegative(row.get(7)?),
                        bytes_transferred: nonnegative(row.get(8)?),
                    })
                },
            )
            .map_err(db_err)
    }

    /// Lists operations newest-updated first. Limits are capped to 10,000.
    pub fn recent_operations(&self, limit: usize) -> Result<Vec<OperationRecord>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let conn = self.lock()?;
        let mut stmt = conn
            .prepare(
                r#"SELECT operation_id, kind, app_id, state, created_at, updated_at,
                          last_error, detail_json
                   FROM operations ORDER BY updated_at DESC, created_at DESC LIMIT ?1"#,
            )
            .map_err(db_err)?;
        let rows = stmt
            .query_map(params![bounded_limit(limit)], row_to_operation_record)
            .map_err(db_err)?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(db_err)?;
        Ok(rows)
    }

    pub fn recent_operations_for_app(
        &self,
        app_id: &AppId,
        limit: usize,
    ) -> Result<Vec<OperationRecord>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let conn = self.lock()?;
        let mut stmt = conn
            .prepare(
                r#"SELECT operation_id, kind, app_id, state, created_at, updated_at,
                          last_error, detail_json
                   FROM operations WHERE app_id=?1
                   ORDER BY updated_at DESC, created_at DESC LIMIT ?2"#,
            )
            .map_err(db_err)?;
        let rows = stmt
            .query_map(
                params![app_id.as_str(), bounded_limit(limit)],
                row_to_operation_record,
            )
            .map_err(db_err)?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(db_err)?;
        Ok(rows)
    }

    /// Sets or clears generic progress for an existing operation.
    pub fn set_operation_progress(
        &self,
        operation_id: Uuid,
        progress: Option<&OperationProgress>,
    ) -> Result<()> {
        let progress_json = progress.map(serde_json::to_string).transpose()?;
        self.lock()?
            .execute(
                r#"INSERT INTO operation_runtime (operation_id, progress_json, metrics_json, updated_at_ms)
                   VALUES (?1,?2,NULL,?3)
                   ON CONFLICT(operation_id) DO UPDATE SET
                       progress_json=excluded.progress_json,
                       updated_at_ms=excluded.updated_at_ms"#,
                params![
                    operation_id.to_string(),
                    progress_json,
                    Utc::now().timestamp_millis()
                ],
            )
            .map_err(db_err)?;
        Ok(())
    }

    pub fn get_operation_progress(&self, operation_id: Uuid) -> Result<Option<OperationProgress>> {
        let json: Option<String> = self
            .lock()?
            .query_row(
                "SELECT progress_json FROM operation_runtime WHERE operation_id=?1",
                params![operation_id.to_string()],
                |row| row.get(0),
            )
            .optional()
            .map_err(db_err)?
            .flatten();
        json.map(|value| serde_json::from_str(&value))
            .transpose()
            .map_err(Into::into)
    }

    /// Replaces the operation's metrics snapshot while preserving progress.
    pub fn set_operation_metrics(
        &self,
        operation_id: Uuid,
        metrics: &OperationMetrics,
    ) -> Result<()> {
        self.lock()?
            .execute(
                r#"INSERT INTO operation_runtime (operation_id, progress_json, metrics_json, updated_at_ms)
                   VALUES (?1,NULL,?2,?3)
                   ON CONFLICT(operation_id) DO UPDATE SET
                       metrics_json=excluded.metrics_json,
                       updated_at_ms=excluded.updated_at_ms"#,
                params![
                    operation_id.to_string(),
                    serde_json::to_string(metrics)?,
                    Utc::now().timestamp_millis()
                ],
            )
            .map_err(db_err)?;
        Ok(())
    }

    pub fn get_operation_metrics(&self, operation_id: Uuid) -> Result<Option<OperationMetrics>> {
        let json: Option<String> = self
            .lock()?
            .query_row(
                "SELECT metrics_json FROM operation_runtime WHERE operation_id=?1",
                params![operation_id.to_string()],
                |row| row.get(0),
            )
            .optional()
            .map_err(db_err)?
            .flatten();
        json.map(|value| serde_json::from_str(&value))
            .transpose()
            .map_err(Into::into)
    }
}

fn row_to_app_mutation(row: &rusqlite::Row<'_>) -> rusqlite::Result<AppMutationRecord> {
    let provenance_json: Option<String> = row.get(7)?;
    Ok(AppMutationRecord {
        mutation_id: parse_uuid(row.get(0)?),
        app_id: AppId(row.get(1)?),
        path: row.get(2)?,
        previous_path: row.get(3)?,
        kind: AppMutationKind::parse(&row.get::<_, String>(4)?),
        observed_at: millis_to_dt(row.get(5)?),
        session_id: row.get::<_, Option<String>>(6)?.map(parse_uuid),
        provenance: provenance_json.and_then(|json| serde_json::from_str(&json).ok()),
        processed_at: row.get::<_, Option<i64>>(8)?.map(millis_to_dt),
    })
}

fn row_to_dirty_root(row: &rusqlite::Row<'_>) -> rusqlite::Result<DirtyRootRecord> {
    Ok(DirtyRootRecord {
        app_id: AppId(row.get(0)?),
        canonical_root: row.get(1)?,
        logical_root: row.get(2)?,
        first_dirty_at: millis_to_dt(row.get(3)?),
        last_dirty_at: millis_to_dt(row.get(4)?),
        mutation_count: nonnegative(row.get(5)?),
        requires_reconciliation: row.get::<_, i64>(6)? != 0,
    })
}

fn row_to_file_state(row: &rusqlite::Row<'_>) -> rusqlite::Result<FileStateRecord> {
    Ok(FileStateRecord {
        app_id: AppId(row.get(0)?),
        logical_root: row.get(1)?,
        relative_path: row.get(2)?,
        canonical_path: row.get(3)?,
        file_type: FileType::parse(&row.get::<_, String>(4)?),
        size: nonnegative(row.get(5)?),
        mtime_ns: row.get(6)?,
        inode: row.get::<_, Option<i64>>(7)?.map(nonnegative),
        mount_id: row.get::<_, Option<i64>>(8)?.map(nonnegative),
        mode: row
            .get::<_, Option<i64>>(9)?
            .map(|value| value.max(0) as u32),
        content_hash: row.get(10)?,
        trust: FileStateTrust::parse(&row.get::<_, String>(11)?),
        last_seen_at: millis_to_dt(row.get(12)?),
        last_hashed_at: row.get::<_, Option<i64>>(13)?.map(millis_to_dt),
    })
}

fn row_to_local_cas(row: &rusqlite::Row<'_>) -> rusqlite::Result<LocalCasEntry> {
    Ok(LocalCasEntry {
        object_kind: ContentObjectKind::parse(&row.get::<_, String>(0)?),
        content_hash: row.get(1)?,
        local_path: row.get(2)?,
        size: nonnegative(row.get(3)?),
        created_at: millis_to_dt(row.get(4)?),
        verified_at: row.get::<_, Option<i64>>(5)?.map(millis_to_dt),
        last_accessed_at: millis_to_dt(row.get(6)?),
    })
}

fn row_to_remote_content(row: &rusqlite::Row<'_>) -> rusqlite::Result<RemoteContentEntry> {
    Ok(RemoteContentEntry {
        storage_id: row.get(0)?,
        object_kind: ContentObjectKind::parse(&row.get::<_, String>(1)?),
        content_hash: row.get(2)?,
        remote_path: row.get(3)?,
        size: row.get::<_, Option<i64>>(4)?.map(nonnegative),
        etag: row.get(5)?,
        state: RemoteContentState::parse(&row.get::<_, String>(6)?),
        observed_at: millis_to_dt(row.get(7)?),
        expires_at: row.get::<_, Option<i64>>(8)?.map(millis_to_dt),
    })
}

fn row_to_sync_journal_entry(row: &rusqlite::Row<'_>) -> rusqlite::Result<SyncJournalEntry> {
    let detail_json: String = row.get(17)?;
    Ok(SyncJournalEntry {
        operation_id: parse_uuid(row.get(0)?),
        item_key: row.get(1)?,
        item_kind: ContentObjectKind::parse(&row.get::<_, String>(2)?),
        direction: SyncDirection::parse(&row.get::<_, String>(3)?),
        state: SyncJournalState::parse(&row.get::<_, String>(4)?),
        local_path: row.get(5)?,
        remote_path: row.get(6)?,
        content_hash: row.get(7)?,
        size: row.get::<_, Option<i64>>(8)?.map(nonnegative),
        attempts: row.get::<_, i64>(9)?.max(0) as u32,
        bytes_transferred: nonnegative(row.get(10)?),
        last_error: row.get(11)?,
        created_at: millis_to_dt(row.get(12)?),
        updated_at: millis_to_dt(row.get(13)?),
        started_at: row.get::<_, Option<i64>>(14)?.map(millis_to_dt),
        completed_at: row.get::<_, Option<i64>>(15)?.map(millis_to_dt),
        next_retry_at: row.get::<_, Option<i64>>(16)?.map(millis_to_dt),
        detail_json: serde_json::from_str(&detail_json).unwrap_or_else(|_| serde_json::json!({})),
    })
}

fn row_to_operation_record(row: &rusqlite::Row<'_>) -> rusqlite::Result<OperationRecord> {
    let detail_json: String = row.get(7)?;
    Ok(OperationRecord {
        operation_id: parse_uuid(row.get(0)?),
        kind: row.get(1)?,
        app_id: row.get::<_, Option<String>>(2)?.map(AppId),
        state: row.get(3)?,
        created_at: seconds_to_dt(row.get(4)?),
        updated_at: seconds_to_dt(row.get(5)?),
        last_error: row.get(6)?,
        detail_json: serde_json::from_str(&detail_json).unwrap_or_else(|_| serde_json::json!({})),
    })
}

fn sync_journal_select(suffix: &str) -> String {
    format!(
        r#"SELECT operation_id, item_key, item_kind, direction, state, local_path,
                  remote_path, content_hash, size, attempts, bytes_transferred,
                  last_error, created_at_ms, updated_at_ms, started_at_ms,
                  completed_at_ms, next_retry_at_ms, detail_json
           FROM sync_journal_items {suffix}"#
    )
}

fn bounded_limit(limit: usize) -> i64 {
    limit.min(10_000) as i64
}

fn to_i64(value: u64, field: &str) -> Result<i64> {
    i64::try_from(value).map_err(|_| StateError::Invalid(format!("{field} exceeds SQLite range")))
}

fn opt_u64_to_i64(value: Option<u64>, field: &str) -> Result<Option<i64>> {
    value.map(|value| to_i64(value, field)).transpose()
}

fn nonnegative(value: i64) -> u64 {
    value.max(0) as u64
}

fn parse_uuid(value: String) -> Uuid {
    Uuid::parse_str(&value).unwrap_or_else(|_| Uuid::nil())
}

fn millis_to_dt(millis: i64) -> DateTime<Utc> {
    Utc.timestamp_millis_opt(millis)
        .single()
        .unwrap_or_else(Utc::now)
}

fn seconds_to_dt(seconds: i64) -> DateTime<Utc> {
    Utc.timestamp_opt(seconds, 0)
        .single()
        .unwrap_or_else(Utc::now)
}

fn db_err(error: rusqlite::Error) -> StateError {
    StateError::Database(error.to_string())
}

#[cfg(test)]
mod tests {
    use chrono::Duration;

    use super::*;
    use crate::schema::MIGRATION_V1;

    fn test_db_with_app() -> (StateDb, AppId) {
        let db = StateDb::open_in_memory().unwrap();
        let app = AppIdentity::new(AppId::steam(42), "Test Game");
        db.upsert_app(&app).unwrap();
        (db, app.app_id)
    }

    #[test]
    fn mutation_journal_is_idempotent_and_dirty_roots_are_sticky() {
        let (db, app_id) = test_db_with_app();
        let session_id = Uuid::new_v4();
        let mut provenance = Evidence::new(EvidenceKind::DirectCgroupWrite);
        provenance.session_id = Some(session_id);
        let mut mutation = AppMutationRecord::new(
            app_id.clone(),
            "/home/user/.config/game/save.dat",
            AppMutationKind::Modify,
        );
        mutation.session_id = Some(session_id);
        mutation.provenance = Some(provenance);

        db.append_app_mutation(&mutation).unwrap();
        db.append_app_mutation(&mutation).unwrap();
        let pending = db.pending_app_mutations(&app_id, 10).unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].session_id, Some(session_id));
        assert_eq!(
            pending[0].provenance.as_ref().map(|item| item.kind),
            Some(EvidenceKind::DirectCgroupWrite)
        );

        db.mark_dirty_root(
            &app_id,
            "/home/user/.config/game",
            Some("$XDG_CONFIG_HOME"),
            false,
        )
        .unwrap();
        db.mark_dirty_root(&app_id, "/home/user/.config/game", None, true)
            .unwrap();
        let roots = db.list_dirty_roots(Some(&app_id)).unwrap();
        assert_eq!(roots.len(), 1);
        assert_eq!(roots[0].mutation_count, 2);
        assert_eq!(roots[0].logical_root.as_deref(), Some("$XDG_CONFIG_HOME"));
        assert!(roots[0].requires_reconciliation);

        assert_eq!(
            db.mark_app_mutations_processed(&[mutation.mutation_id], Utc::now())
                .unwrap(),
            1
        );
        assert!(db.pending_app_mutations(&app_id, 10).unwrap().is_empty());
        assert_eq!(
            db.mark_app_mutations_processed(&[mutation.mutation_id], Utc::now())
                .unwrap(),
            0
        );
        assert_eq!(
            db.prune_processed_app_mutations(Utc::now() + Duration::seconds(1))
                .unwrap(),
            1
        );
        assert!(db
            .clear_dirty_root(&app_id, "/home/user/.config/game")
            .unwrap());
    }

    #[test]
    fn file_state_and_content_indexes_roundtrip() {
        let (db, app_id) = test_db_with_app();
        let now = Utc::now();
        let file_state = FileStateRecord {
            app_id: app_id.clone(),
            logical_root: "$XDG_CONFIG_HOME".into(),
            relative_path: "game/save.dat".into(),
            canonical_path: Some("/home/user/.config/game/save.dat".into()),
            file_type: FileType::File,
            size: 128,
            mtime_ns: 99,
            inode: Some(7),
            mount_id: Some(8),
            mode: Some(0o600),
            content_hash: Some("file-hash".into()),
            trust: FileStateTrust::Trusted,
            last_seen_at: now,
            last_hashed_at: Some(now),
        };
        db.upsert_file_state(&file_state).unwrap();
        let stored = db
            .get_file_state(&app_id, "$XDG_CONFIG_HOME", "game/save.dat")
            .unwrap()
            .unwrap();
        assert!(stored.fast_identity_matches(128, 99, Some(7), Some(8)));
        assert!(db
            .set_file_state_trust(
                &app_id,
                "$XDG_CONFIG_HOME",
                "game/save.dat",
                FileStateTrust::Dirty,
            )
            .unwrap());
        assert_eq!(
            db.list_file_states(&app_id, Some("$XDG_CONFIG_HOME"))
                .unwrap()[0]
                .trust,
            FileStateTrust::Dirty
        );

        let local = LocalCasEntry {
            object_kind: ContentObjectKind::Chunk,
            content_hash: "chunk-hash".into(),
            local_path: "/var/lib/noland/cas/chunk-hash".into(),
            size: 64,
            created_at: now,
            verified_at: Some(now),
            last_accessed_at: now,
        };
        db.upsert_local_cas_entry(&local).unwrap();
        assert_eq!(
            db.get_local_cas_entry(ContentObjectKind::Chunk, "chunk-hash")
                .unwrap()
                .unwrap()
                .size,
            64
        );

        let remote = RemoteContentEntry {
            storage_id: "shared-storage".into(),
            object_kind: ContentObjectKind::Chunk,
            content_hash: "chunk-hash".into(),
            remote_path: "cas/chunks/chunk-hash".into(),
            size: Some(64),
            etag: Some("etag".into()),
            state: RemoteContentState::Present,
            observed_at: now,
            expires_at: Some(now + Duration::minutes(5)),
        };
        db.upsert_remote_content_entry(&remote).unwrap();
        let stored_remote = db
            .get_remote_content_by_hash("shared-storage", ContentObjectKind::Chunk, "chunk-hash")
            .unwrap()
            .unwrap();
        assert_eq!(stored_remote.state, RemoteContentState::Present);
        assert!(stored_remote.is_fresh_at(now));
        assert_eq!(
            db.get_remote_content_by_path("shared-storage", "cas/chunks/chunk-hash")
                .unwrap()
                .unwrap()
                .content_hash,
            "chunk-hash"
        );
    }

    #[test]
    fn sync_journal_transitions_track_attempts_and_monotonic_progress() {
        let db = StateDb::open_in_memory().unwrap();
        let operation_id = Uuid::new_v4();
        let mut entry = SyncJournalEntry::pending(
            operation_id,
            "chunk:one",
            ContentObjectKind::Chunk,
            SyncDirection::Upload,
        );
        entry.size = Some(10);
        db.upsert_sync_journal_entry(&entry).unwrap();

        assert!(db
            .set_sync_journal_state(
                operation_id,
                "chunk:one",
                SyncJournalState::InProgress,
                None,
                None,
            )
            .unwrap());
        db.set_sync_journal_state(
            operation_id,
            "chunk:one",
            SyncJournalState::InProgress,
            None,
            None,
        )
        .unwrap();
        db.update_sync_journal_progress(operation_id, "chunk:one", 7)
            .unwrap();
        db.update_sync_journal_progress(operation_id, "chunk:one", 3)
            .unwrap();
        db.set_sync_journal_state(
            operation_id,
            "chunk:one",
            SyncJournalState::Completed,
            None,
            None,
        )
        .unwrap();

        let stored = db
            .get_sync_journal_entry(operation_id, "chunk:one")
            .unwrap()
            .unwrap();
        assert_eq!(stored.attempts, 1);
        assert_eq!(stored.bytes_transferred, 7);
        assert!(stored.started_at.is_some());
        assert!(stored.completed_at.is_some());
        let summary = db.sync_journal_summary(operation_id).unwrap();
        assert_eq!(summary.total_items, 1);
        assert_eq!(summary.completed_items, 1);
        assert_eq!(summary.total_bytes, 10);
        assert_eq!(summary.bytes_transferred, 7);
    }

    #[test]
    fn recent_operations_and_runtime_snapshots_roundtrip() {
        let (db, app_id) = test_db_with_app();
        let now = Utc::now();
        let older = OperationRecord {
            operation_id: Uuid::new_v4(),
            kind: "backup".into(),
            app_id: Some(app_id.clone()),
            state: "COMPLETED".into(),
            created_at: now - Duration::seconds(10),
            updated_at: now - Duration::seconds(5),
            last_error: None,
            detail_json: serde_json::json!({}),
        };
        let newer = OperationRecord {
            operation_id: Uuid::new_v4(),
            kind: "restore".into(),
            app_id: Some(app_id.clone()),
            state: "DOWNLOADING".into(),
            created_at: now - Duration::seconds(4),
            updated_at: now,
            last_error: None,
            detail_json: serde_json::json!({}),
        };
        db.upsert_operation(&older).unwrap();
        db.upsert_operation(&newer).unwrap();

        let recent = db.recent_operations(1).unwrap();
        assert_eq!(recent[0].operation_id, newer.operation_id);
        assert_eq!(db.recent_operations_for_app(&app_id, 10).unwrap().len(), 2);

        let mut progress = OperationProgress::new("upload", 4);
        progress.total_units = Some(10);
        progress.unit = Some("chunks".into());
        db.set_operation_progress(newer.operation_id, Some(&progress))
            .unwrap();
        let metrics = OperationMetrics {
            bytes_uploaded: 1024,
            ..OperationMetrics::default()
        };
        db.set_operation_metrics(newer.operation_id, &metrics)
            .unwrap();
        assert_eq!(
            db.get_operation_progress(newer.operation_id)
                .unwrap()
                .unwrap()
                .fraction(),
            Some(0.4)
        );
        assert_eq!(
            db.get_operation_metrics(newer.operation_id)
                .unwrap()
                .unwrap()
                .bytes_uploaded,
            1024
        );

        db.set_operation_progress(newer.operation_id, None).unwrap();
        assert!(db
            .get_operation_progress(newer.operation_id)
            .unwrap()
            .is_none());
        assert!(db
            .get_operation_metrics(newer.operation_id)
            .unwrap()
            .is_some());
    }

    #[test]
    fn version_one_database_upgrades_without_altering_existing_tables() {
        let path = std::env::temp_dir().join(format!("noland-state-db-{}.sqlite", Uuid::new_v4()));
        {
            let conn = rusqlite::Connection::open(&path).unwrap();
            conn.execute_batch(MIGRATION_V1).unwrap();
            conn.execute(
                "INSERT INTO schema_migrations(version, applied_at) VALUES (1, 0)",
                [],
            )
            .unwrap();
        }

        let db = StateDb::open(&path).unwrap();
        let app = AppIdentity::new(AppId::steam(7), "Upgrade Test");
        db.upsert_app(&app).unwrap();
        db.mark_dirty_root(&app.app_id, "/tmp/root", None, false)
            .unwrap();
        assert_eq!(db.list_dirty_roots(Some(&app.app_id)).unwrap().len(), 1);
        drop(db);
        std::fs::remove_file(&path).unwrap();
        let wal_path = path.with_extension("sqlite-wal");
        let shm_path = path.with_extension("sqlite-shm");
        let _ = std::fs::remove_file(wal_path);
        let _ = std::fs::remove_file(shm_path);
    }
}
