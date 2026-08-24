use chrono::Utc;
use noland_state_core::*;
use uuid::Uuid;

use crate::StateAgent;

pub fn write_local_checkpoint(agent: &StateAgent) -> Result<LearnedStateCheckpoint> {
    let apps = agent.db.list_apps()?;
    let mut associations = Vec::new();
    for app in &apps {
        for (record, assoc) in agent.db.associations_for_app(&app.app_id)? {
            associations.push(CheckpointAssociation {
                app_id: assoc.app_id,
                canonical_path: record.canonical_path,
                logical_root: record.logical_root,
                relative_path: record.relative_path,
                confidence: assoc.confidence,
                persistence_class: assoc.persistence_class,
                semantic_role: assoc.semantic_role,
                evidence: assoc.evidence.into_iter().map(|e| e.kind).collect(),
            });
        }
    }
    let known_roots = agent
        .db
        .known_roots(None)?
        .into_iter()
        .map(|(app_id, kind, path)| CheckpointRoot { app_id, kind, path })
        .collect();
    let mut latest_bundle_refs = Vec::new();
    for app in &apps {
        if let Some((commit_id, bundle_id, _)) = agent.db.latest_commit(&app.app_id)? {
            latest_bundle_refs.push(CheckpointBundleRef {
                app_id: app.app_id.clone(),
                bundle_id,
                commit_id,
            });
        }
    }
    let checkpoint = LearnedStateCheckpoint {
        schema_version: 1,
        checkpoint_id: Uuid::new_v4(),
        instance_id: agent.config.instance_id,
        image_id: agent.config.image_id.clone(),
        created_at: Utc::now(),
        apps,
        associations,
        known_roots,
        latest_bundle_refs,
    };
    let path = agent
        .config
        .paths
        .checkpoints
        .join(format!("{}.json", checkpoint.checkpoint_id));
    std::fs::write(path, serde_json::to_vec_pretty(&checkpoint)?)?;
    Ok(checkpoint)
}

pub fn import_checkpoint(agent: &StateAgent, checkpoint: &LearnedStateCheckpoint) -> Result<()> {
    for app in &checkpoint.apps {
        agent.db.upsert_app(app)?;
    }
    for root in &checkpoint.known_roots {
        agent
            .db
            .add_known_root(&root.app_id, &root.kind, &root.path)?;
    }
    for assoc in &checkpoint.associations {
        let path_id = agent.db.upsert_path(&assoc.canonical_path)?;
        agent.db.upsert_association(&PathAssociation {
            app_id: assoc.app_id.clone(),
            path_id,
            confidence: assoc.confidence,
            evidence: assoc
                .evidence
                .iter()
                .copied()
                .map(Evidence::new)
                .collect(),
            persistence_class: assoc.persistence_class,
            semantic_role: assoc.semantic_role,
            first_seen_at: checkpoint.created_at,
            last_seen_at: checkpoint.created_at,
        })?;
    }
    Ok(())
}

pub fn maybe_checkpoint(agent: &StateAgent) -> Result<()> {
    if agent.db.list_dirty_apps()?.is_empty() && agent.db.list_apps()?.is_empty() {
        return Ok(());
    }
    write_local_checkpoint(agent)?;
    Ok(())
}
