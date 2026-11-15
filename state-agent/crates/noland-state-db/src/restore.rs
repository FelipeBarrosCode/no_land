//! Bounded set-based restore publication, within one atomic write transaction.

use std::collections::HashSet;
use std::fmt::Write;

use chrono::Utc;
use noland_state_core::*;
use rusqlite::{params, params_from_iter, types::ValueRef, Connection, OptionalExtension};

use crate::{persistence::bump_capture_generation, RestoredPathAssociationInput, StateDb};

// Keep even the widest statement below SQLite's historical 999-variable limit.
const BATCH: usize = 64;

pub(crate) fn commit_restored_paths(
    db: &StateDb,
    app_id: &AppId,
    entries: &[RestoredPathAssociationInput],
    states: &[FileStateRecord],
    observed_generation: Option<i64>,
    still_verified: &mut impl FnMut(&FileStateRecord) -> bool,
) -> Result<usize> {
    let paths: HashSet<_> = entries.iter().map(|e| e.canonical_path.as_str()).collect();
    for state in states {
        if state.app_id != *app_id
            || state.trust != FileStateTrust::Trusted
            || state.file_type != FileType::File
            || state.content_hash.is_none()
            || !state
                .canonical_path
                .as_deref()
                .is_some_and(|p| paths.contains(p))
        {
            return Err(StateError::Invalid("invalid verified restore state".into()));
        }
    }
    let restored = [
        Evidence::new(EvidenceKind::RestoredFromCommittedBundle),
        Evidence::new(EvidenceKind::ExplicitUserBinding)
            .with_detail("Included by the selected authenticated restore bundle"),
    ];
    // Most restores introduce new associations. Serialize their shared evidence
    // once, and borrow all input strings rather than cloning records per file.
    let default_evidence = serde_json::to_string(&restored)?;
    let now = Utc::now();
    let mut conn = db.lock()?;
    let tx = conn
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .map_err(db_err)?;
    let generation: i64 = tx
        .query_row(
            "SELECT generation FROM capture_generations WHERE app_id=?1",
            [app_id.as_str()],
            |row| row.get(0),
        )
        .optional()
        .map_err(db_err)?
        .unwrap_or(0);
    for batch in entries.chunks(BATCH) {
        associations(
            &tx,
            app_id,
            batch,
            &restored,
            &default_evidence,
            now.timestamp(),
        )?;
    }
    let mut trusted = 0;
    for batch in states.chunks(BATCH) {
        trusted += file_states(
            &tx,
            app_id,
            batch,
            observed_generation == Some(generation),
            still_verified,
            now.timestamp(),
            now.timestamp_millis(),
        )?;
    }
    if trusted < states.len() {
        tx.execute(
            "INSERT INTO dirty_apps (app_id, first_dirty_at, last_dirty_at, requires_reconciliation)
             VALUES (?1,?2,?2,1) ON CONFLICT(app_id) DO UPDATE SET
             last_dirty_at=excluded.last_dirty_at, requires_reconciliation=1",
            params![app_id.as_str(), now.timestamp()],
        ).map_err(db_err)?;
    }
    // Captures that began before this publication may not acknowledge it.
    bump_capture_generation(&tx, app_id)?;
    tx.commit().map_err(db_err)?;
    Ok(trusted)
}

fn associations(
    conn: &Connection,
    app_id: &AppId,
    entries: &[RestoredPathAssociationInput],
    restored: &[Evidence],
    default_evidence: &str,
    now: i64,
) -> Result<()> {
    let mut evidence: Vec<Option<String>> = (0..entries.len()).map(|_| None).collect();
    let mut values = vec![text(app_id.as_str())];
    values.extend(entries.iter().map(|e| text(&e.canonical_path)));
    let sql = prior_evidence_sql(entries.len());
    let mut stmt = conn.prepare_cached(&sql).map_err(db_err)?;
    let mut rows = stmt
        .query(params_from_iter(values.iter().copied().map(SqlValue)))
        .map_err(db_err)?;
    while let Some(row) = rows.next().map_err(db_err)? {
        let ordinal = ordinal(row)?;
        let raw: String = row.get(1).map_err(db_err)?;
        let mut previous: Vec<Evidence> = serde_json::from_str(&raw)?;
        let mut changed = false;
        for item in restored {
            if !previous.iter().any(|e| e.kind == item.kind) {
                previous.push(item.clone());
                changed = true;
            }
        }
        evidence[ordinal] = Some(if changed {
            serde_json::to_string(&previous)?
        } else {
            raw
        });
    }
    drop(rows);
    drop(stmt);
    let sql = format!(
        "WITH input(ordinal, canonical_path) AS (VALUES {})
         INSERT INTO paths (canonical_path) SELECT canonical_path FROM input WHERE true
         ON CONFLICT(canonical_path) DO NOTHING",
        placeholders(entries.len(), 1, 1),
    );
    conn.prepare_cached(&sql)
        .map_err(db_err)?
        .execute(params_from_iter(entries.iter().map(|e| &e.canonical_path)))
        .map_err(db_err)?;
    let mut values = vec![
        text(app_id.as_str()),
        ValueRef::Real(CONF_EXPLICIT as f64),
        ValueRef::Integer(now),
    ];
    for (entry, evidence) in entries.iter().zip(&evidence) {
        values.extend([
            text(&entry.canonical_path),
            text(entry.persistence_class.as_str()),
            text(entry.semantic_role.as_str()),
            text(evidence.as_deref().unwrap_or(default_evidence)),
        ]);
    }
    let sql = format!(
        "WITH input(ordinal, canonical_path, persistence_class, semantic_role, evidence_json) AS (VALUES {})
         INSERT INTO path_associations (path_id, app_id, confidence, persistence_class,
             semantic_role, evidence_json, first_seen_at, last_seen_at)
         SELECT p.path_id, ?1, ?2, i.persistence_class, i.semantic_role, i.evidence_json, ?3, ?3
         FROM input i JOIN paths p ON p.canonical_path=i.canonical_path WHERE true ORDER BY i.ordinal
         ON CONFLICT(path_id, app_id) DO UPDATE SET confidence=excluded.confidence,
             persistence_class=excluded.persistence_class, semantic_role=excluded.semantic_role,
             evidence_json=excluded.evidence_json, last_seen_at=excluded.last_seen_at",
        placeholders(entries.len(), 4, 4),
    );
    conn.prepare_cached(&sql)
        .map_err(db_err)?
        .execute(params_from_iter(values.iter().copied().map(SqlValue)))
        .map_err(db_err)?;
    Ok(())
}

fn prior_evidence_sql(rows: usize) -> String {
    // SQLite may otherwise start with idx_path_assoc_app and scan this input
    // once for EVERY association belonging to the app. CROSS JOIN fixes the
    // loop order: at most `rows` unique canonical-path and association probes,
    // including when sqlite_stat1 is absent on a newly created runtime DB.
    format!(
        "WITH input(ordinal, canonical_path) AS (VALUES {})
         SELECT i.ordinal, a.evidence_json FROM input i
         CROSS JOIN paths p ON p.canonical_path=i.canonical_path
         CROSS JOIN path_associations a ON a.path_id=p.path_id AND a.app_id=?1",
        placeholders(rows, 1, 2),
    )
}

fn file_states(
    conn: &Connection,
    app_id: &AppId,
    states: &[FileStateRecord],
    generation_matches: bool,
    still_verified: &mut impl FnMut(&FileStateRecord) -> bool,
    now: i64,
    now_ms: i64,
) -> Result<usize> {
    let mut blocked = vec![false; states.len()];
    let mut values = vec![text(app_id.as_str())];
    for state in states {
        values.extend([text(&state.logical_root), text(&state.relative_path)]);
    }
    let sql = format!(
        "WITH input(ordinal, logical_root, relative_path) AS (VALUES {})
         SELECT i.ordinal FROM input i JOIN file_state_index s
         ON s.app_id=?1 AND s.logical_root=i.logical_root AND s.relative_path=i.relative_path
         WHERE s.trust_state IN ('DIRTY','MISSING')",
        placeholders(states.len(), 2, 2),
    );
    let mut stmt = conn.prepare_cached(&sql).map_err(db_err)?;
    let mut rows = stmt
        .query(params_from_iter(values.iter().copied().map(SqlValue)))
        .map_err(db_err)?;
    while let Some(row) = rows.next().map_err(db_err)? {
        blocked[ordinal(row)?] = true;
    }
    drop(rows);
    drop(stmt);
    values.clear();
    values.extend([
        text(app_id.as_str()),
        ValueRef::Integer(now),
        ValueRef::Integer(now_ms),
    ]);
    let mut trusted = 0;
    for (state, blocked) in states.iter().zip(blocked) {
        let verified = generation_matches && !blocked && still_verified(state);
        trusted += usize::from(verified);
        let (size, inode, mount_id) = if verified {
            (
                integer(state.size)?,
                state.inode.map(integer).transpose()?,
                state.mount_id.map(integer).transpose()?,
            )
        } else {
            (0, None, None)
        };
        values.extend([
            text(
                state
                    .canonical_path
                    .as_deref()
                    .expect("validated canonical path"),
            ),
            text(&state.logical_root),
            text(&state.relative_path),
            text(state.file_type.as_str()),
            ValueRef::Integer(size),
            ValueRef::Integer(if verified { state.mtime_ns } else { 0 }),
            optional_integer(inode),
            optional_integer(mount_id),
            optional_integer(if verified {
                state.mode.map(i64::from)
            } else {
                None
            }),
            if verified {
                text(state.content_hash.as_deref().expect("validated hash"))
            } else {
                ValueRef::Null
            },
            ValueRef::Integer(if verified {
                state.last_seen_at.timestamp_millis()
            } else {
                now_ms
            }),
            optional_integer(if verified {
                state.last_hashed_at.map(|at| at.timestamp_millis())
            } else {
                None
            }),
            ValueRef::Integer(i64::from(verified)),
        ]);
    }
    let input = format!(
        "WITH input(ordinal, canonical_path, logical_root, relative_path, file_type, size,
             mtime_ns, inode, mount_id, mode, content_hash, seen, hashed, verified) AS (VALUES {})",
        placeholders(states.len(), 13, 4),
    );
    // WHERE true disambiguates SQLite's SELECT ... ON CONFLICT grammar. Ordering
    // preserves sequential semantics even for duplicate portable keys in input.
    let sql = format!("{input}
        INSERT INTO file_state_index (app_id, logical_root, relative_path, canonical_path,
            file_type, size, mtime_ns, inode, mount_id, mode, content_hash, trust_state,
            last_seen_at_ms, last_hashed_at_ms)
        SELECT ?1, logical_root, relative_path, canonical_path, file_type, size, mtime_ns,
            inode, mount_id, mode, content_hash,
            CASE WHEN verified THEN 'TRUSTED' ELSE 'VERIFY_REQUIRED' END, seen, hashed
        FROM input WHERE true ORDER BY ordinal
        ON CONFLICT(app_id, logical_root, relative_path) DO UPDATE SET
            canonical_path=CASE WHEN excluded.trust_state='TRUSTED' THEN excluded.canonical_path ELSE file_state_index.canonical_path END,
            file_type=CASE WHEN excluded.trust_state='TRUSTED' THEN excluded.file_type ELSE file_state_index.file_type END,
            size=CASE WHEN excluded.trust_state='TRUSTED' THEN excluded.size ELSE file_state_index.size END,
            mtime_ns=CASE WHEN excluded.trust_state='TRUSTED' THEN excluded.mtime_ns ELSE file_state_index.mtime_ns END,
            inode=CASE WHEN excluded.trust_state='TRUSTED' THEN excluded.inode ELSE file_state_index.inode END,
            mount_id=CASE WHEN excluded.trust_state='TRUSTED' THEN excluded.mount_id ELSE file_state_index.mount_id END,
            mode=CASE WHEN excluded.trust_state='TRUSTED' THEN excluded.mode ELSE file_state_index.mode END,
            content_hash=CASE WHEN excluded.trust_state='TRUSTED' THEN excluded.content_hash ELSE file_state_index.content_hash END,
            last_seen_at_ms=CASE WHEN excluded.trust_state='TRUSTED' THEN excluded.last_seen_at_ms ELSE file_state_index.last_seen_at_ms END,
            last_hashed_at_ms=CASE WHEN excluded.trust_state='TRUSTED' THEN excluded.last_hashed_at_ms ELSE file_state_index.last_hashed_at_ms END,
            trust_state=CASE WHEN file_state_index.trust_state IN ('DIRTY','MISSING') THEN file_state_index.trust_state ELSE excluded.trust_state END");
    conn.prepare_cached(&sql)
        .map_err(db_err)?
        .execute(params_from_iter(values.iter().copied().map(SqlValue)))
        .map_err(db_err)?;
    let sql = format!(
        "{input}
        UPDATE paths SET logical_root=i.logical_root, relative_path=i.relative_path,
            file_type=i.file_type, size=i.size, mtime_ns=i.mtime_ns, inode=i.inode,
            mount_id=i.mount_id, mode=i.mode, content_hash=i.content_hash, last_scanned_at=?2
        FROM input i WHERE paths.canonical_path=i.canonical_path AND i.verified=1
            AND i.ordinal=(SELECT MAX(last.ordinal) FROM input last
                WHERE last.canonical_path=i.canonical_path AND last.verified=1)"
    );
    conn.prepare_cached(&sql)
        .map_err(db_err)?
        .execute(params_from_iter(values.iter().copied().map(SqlValue)))
        .map_err(db_err)?;
    Ok(trusted)
}

/// Each row begins with a literal ordinal; the other columns are bound values.
fn placeholders(rows: usize, columns: usize, first: usize) -> String {
    let mut sql = String::new();
    let mut parameter = first;
    for row in 0..rows {
        if row != 0 {
            sql.push(',');
        }
        write!(&mut sql, "({row}").expect("write to String");
        for _ in 0..columns {
            write!(&mut sql, ",?{parameter}").expect("write to String");
            parameter += 1;
        }
        sql.push(')');
    }
    sql
}

struct SqlValue<'a>(ValueRef<'a>);

impl rusqlite::ToSql for SqlValue<'_> {
    fn to_sql(&self) -> rusqlite::Result<rusqlite::types::ToSqlOutput<'_>> {
        Ok(rusqlite::types::ToSqlOutput::Borrowed(self.0))
    }
}

fn ordinal(row: &rusqlite::Row<'_>) -> Result<usize> {
    usize::try_from(row.get::<_, i64>(0).map_err(db_err)?)
        .map_err(|_| StateError::Database("invalid restore batch ordinal".into()))
}

fn text(value: &str) -> ValueRef<'_> {
    ValueRef::Text(value.as_bytes())
}
fn optional_integer(value: Option<i64>) -> ValueRef<'static> {
    value.map_or(ValueRef::Null, ValueRef::Integer)
}
fn integer(value: u64) -> Result<i64> {
    i64::try_from(value)
        .map_err(|_| StateError::Invalid("restore identity exceeds SQLite integer range".into()))
}
fn db_err(error: rusqlite::Error) -> StateError {
    StateError::Database(error.to_string())
}

#[cfg(test)]
#[path = "restore_tests.rs"]
mod tests;
