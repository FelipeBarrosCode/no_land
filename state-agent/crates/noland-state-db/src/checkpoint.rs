//! Consistent learned-state snapshots and conservative checkpoint reference checks.

use std::collections::HashSet;

use chrono::Utc;
use noland_state_core::*;
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::StateDb;

/// Restart-stable learned-state revision, scoped to this database incarnation.
/// It is not an observation/capture generation or a SQLite change counter.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LearnedCheckpointGeneration {
    pub database_id: String,
    pub generation: i64,
}

impl StateDb {
    pub fn learned_checkpoint_generation(&self) -> Result<LearnedCheckpointGeneration> {
        let conn = self.lock()?;
        read_generation(&conn)
    }

    /// Capture only the durable learned state, under one SQLite read transaction.
    /// Observation timestamps, dirty flags and transfer progress are not checkpoint
    /// content and must not cause otherwise identical checkpoints to be published.
    pub fn learned_checkpoint(
        &self,
        instance_id: Uuid,
        image_id: &str,
    ) -> Result<LearnedStateCheckpoint> {
        Ok(self.learned_checkpoint_versioned(instance_id, image_id)?.1)
    }

    pub fn learned_checkpoint_versioned(
        &self,
        instance_id: Uuid,
        image_id: &str,
    ) -> Result<(LearnedCheckpointGeneration, LearnedStateCheckpoint)> {
        let mut conn = self.lock()?;
        let tx = conn.transaction().map_err(db_err)?;
        let generation = read_generation(&tx)?;
        let checkpoint = read_checkpoint(&tx, instance_id, image_id)?;
        tx.commit().map_err(db_err)?;
        Ok((generation, checkpoint))
    }

    /// An unchanged tick reads only the singleton revision. On a change, the
    /// returned revision and rows share a read transaction: callers must record
    /// THIS revision after publication, never a later independent revision read.
    pub fn learned_checkpoint_if_changed(
        &self,
        instance_id: Uuid,
        image_id: &str,
        covered: Option<&LearnedCheckpointGeneration>,
    ) -> Result<Option<(LearnedCheckpointGeneration, LearnedStateCheckpoint)>> {
        let mut conn = self.lock()?;
        let tx = conn.transaction().map_err(db_err)?;
        let generation = read_generation(&tx)?;
        if covered == Some(&generation) {
            tx.commit().map_err(db_err)?;
            return Ok(None);
        }
        let checkpoint = read_checkpoint(&tx, instance_id, image_id)?;
        tx.commit().map_err(db_err)?;
        Ok(Some((generation, checkpoint)))
    }

    /// Check *all* retained references, including old terminal operations and seals.
    /// Do not use the bounded recent-operations API for garbage collection.
    pub fn referenced_checkpoints(&self, candidates: &[Uuid]) -> Result<HashSet<Uuid>> {
        let conn = self.lock()?;
        let mut statement = conn
            .prepare(
                "SELECT EXISTS (
                SELECT 1 FROM checkpoints WHERE checkpoint_id=?1
                UNION ALL SELECT 1 FROM operations WHERE instr(detail_json,?1)>0
                UNION ALL SELECT 1 FROM seals WHERE instr(payload_json,?1)>0
                UNION ALL SELECT 1 FROM operation_runtime
                    WHERE instr(progress_json,?1)>0 OR instr(metrics_json,?1)>0
                UNION ALL SELECT 1 FROM sync_journal
                    WHERE instr(item_key,?1)>0
                UNION ALL SELECT 1 FROM sync_journal_items
                    WHERE instr(local_path,?1)>0 OR instr(remote_path,?1)>0
                       OR instr(item_key,?1)>0 OR instr(detail_json,?1)>0
            )",
            )
            .map_err(db_err)?;
        let mut referenced = HashSet::new();
        for id in candidates {
            if statement
                .query_row(params![id.to_string()], |row| row.get::<_, bool>(0))
                .map_err(db_err)?
            {
                referenced.insert(*id);
            }
        }
        Ok(referenced)
    }
}

fn read_generation(conn: &Connection) -> Result<LearnedCheckpointGeneration> {
    conn.query_row(
        "SELECT database_id, generation FROM learned_checkpoint_generation WHERE singleton=1",
        [],
        |row| {
            Ok(LearnedCheckpointGeneration {
                database_id: row.get(0)?,
                generation: row.get(1)?,
            })
        },
    )
    .map_err(db_err)
}

fn read_checkpoint(
    conn: &Connection,
    instance_id: Uuid,
    image_id: &str,
) -> Result<LearnedStateCheckpoint> {
    let mut checkpoint = LearnedStateCheckpoint {
        schema_version: 1,
        checkpoint_id: Uuid::new_v4(),
        instance_id,
        image_id: image_id.to_owned(),
        created_at: Utc::now(),
        apps: Vec::new(),
        associations: Vec::new(),
        known_roots: Vec::new(),
        latest_bundle_refs: Vec::new(),
    };
    let mut apps = conn
        .prepare(
            "SELECT app_id, display_name, canonical_executable, desktop_entry_id,
                steam_app_id, launcher_kind, aliases_json, identity_confidence, icon_path
         FROM apps ORDER BY app_id",
        )
        .map_err(db_err)?;
    let mut rows = apps.query([]).map_err(db_err)?;
    while let Some(row) = rows.next().map_err(db_err)? {
        checkpoint.apps.push(AppIdentity {
            app_id: AppId(row.get(0).map_err(db_err)?),
            display_name: row.get(1).map_err(db_err)?,
            canonical_executable: row
                .get::<_, Option<String>>(2)
                .map_err(db_err)?
                .map(Into::into),
            desktop_entry_id: row.get(3).map_err(db_err)?,
            steam_app_id: row.get(4).map_err(db_err)?,
            launcher: row
                .get::<_, Option<String>>(5)
                .map_err(db_err)?
                .and_then(|s| LauncherKind::parse(&s)),
            aliases: serde_json::from_str(&row.get::<_, String>(6).map_err(db_err)?)?,
            identity_confidence: row.get(7).map_err(db_err)?,
            icon_path: row
                .get::<_, Option<String>>(8)
                .map_err(db_err)?
                .map(Into::into),
        });
    }
    let mut associations = conn
        .prepare(
            "SELECT a.app_id, p.canonical_path, p.logical_root, p.relative_path,
                a.confidence, a.persistence_class, a.semantic_role, a.evidence_json
         FROM path_associations a JOIN paths p ON p.path_id=a.path_id
         JOIN apps ON apps.app_id=a.app_id ORDER BY a.app_id, p.canonical_path",
        )
        .map_err(db_err)?;
    let mut rows = associations.query([]).map_err(db_err)?;
    while let Some(row) = rows.next().map_err(db_err)? {
        // Only the kind survives into LearnedStateCheckpoint. Ignore evidence
        // timestamps/detail, which change frequently without changing learned state.
        let evidence: Vec<Evidence> =
            serde_json::from_str(&row.get::<_, String>(7).map_err(db_err)?)?;
        checkpoint.associations.push(CheckpointAssociation {
            app_id: AppId(row.get(0).map_err(db_err)?),
            canonical_path: row.get(1).map_err(db_err)?,
            logical_root: row.get(2).map_err(db_err)?,
            relative_path: row.get(3).map_err(db_err)?,
            confidence: row.get(4).map_err(db_err)?,
            persistence_class: PersistenceClass::parse(&row.get::<_, String>(5).map_err(db_err)?),
            semantic_role: SemanticRole::parse(&row.get::<_, String>(6).map_err(db_err)?),
            evidence: evidence.into_iter().map(|e| e.kind).collect(),
        });
    }
    let mut roots = conn.prepare(
        "SELECT app_id, kind, canonical_path FROM known_roots ORDER BY app_id, kind, canonical_path",
    ).map_err(db_err)?;
    let mut rows = roots.query([]).map_err(db_err)?;
    while let Some(row) = rows.next().map_err(db_err)? {
        checkpoint.known_roots.push(CheckpointRoot {
            app_id: AppId(row.get(0).map_err(db_err)?),
            kind: row.get(1).map_err(db_err)?,
            path: row.get(2).map_err(db_err)?,
        });
    }
    let mut latest = conn
        .prepare(
            "SELECT l.app_id, l.commit_id, l.bundle_id
             FROM learned_checkpoint_latest_bundles l JOIN apps ON apps.app_id=l.app_id
             ORDER BY l.app_id",
        )
        .map_err(db_err)?;
    let mut rows = latest.query([]).map_err(db_err)?;
    while let Some(row) = rows.next().map_err(db_err)? {
        let parse_id = |index| -> Result<Uuid> {
            Uuid::parse_str(&row.get::<_, String>(index).map_err(db_err)?)
                .map_err(|err| StateError::Database(err.to_string()))
        };
        checkpoint.latest_bundle_refs.push(CheckpointBundleRef {
            app_id: AppId(row.get(0).map_err(db_err)?),
            commit_id: parse_id(1)?,
            bundle_id: parse_id(2)?,
        });
    }
    Ok(checkpoint)
}

fn db_err(error: rusqlite::Error) -> StateError {
    StateError::Database(error.to_string())
}

#[cfg(test)]
#[path = "checkpoint_tests.rs"]
mod tests;
