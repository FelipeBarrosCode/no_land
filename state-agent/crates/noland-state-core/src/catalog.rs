use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::classify::BackupMode;
use crate::identity::AppId;
use crate::operations::SealAppCommit;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CatalogDocument {
    pub schema_version: u32,
    pub catalog_commit_id: Uuid,
    pub created_at: DateTime<Utc>,
    pub heads: Vec<AppId>,
    pub apps: Vec<CatalogApp>,
    pub instances: Vec<CatalogInstance>,
}

impl CatalogDocument {
    pub fn empty() -> Self {
        Self {
            schema_version: 1,
            catalog_commit_id: Uuid::new_v4(),
            created_at: Utc::now(),
            heads: Vec::new(),
            apps: Vec::new(),
            instances: Vec::new(),
        }
    }

    pub fn app_mut(&mut self, app_id: &AppId) -> Option<&mut CatalogApp> {
        self.apps.iter_mut().find(|app| &app.app_id == app_id)
    }

    pub fn upsert_bundle(&mut self, app_id: AppId, display_name: String, bundle: CatalogBundle) {
        if let Some(existing) = self.app_mut(&app_id) {
            existing.display_name = display_name;
            existing.latest_bundle_id = bundle.bundle_id;
            if !existing
                .bundles
                .iter()
                .any(|b| b.commit_id == bundle.commit_id)
            {
                existing.bundles.push(bundle);
            }
        } else {
            self.apps.push(CatalogApp {
                latest_bundle_id: bundle.bundle_id,
                app_id: app_id.clone(),
                display_name,
                bundles: vec![bundle],
            });
            if !self.heads.iter().any(|id| id == &app_id) {
                self.heads.push(app_id);
            }
        }
    }

    /// Divergent heads are retained; never silently overwrite another branch.
    pub fn divergent_heads(&self, app_id: &AppId) -> Vec<Uuid> {
        let Some(app) = self.apps.iter().find(|a| &a.app_id == app_id) else {
            return Vec::new();
        };
        let children: std::collections::HashSet<Uuid> = app
            .bundles
            .iter()
            .filter_map(|b| b.parent_bundle_id)
            .collect();
        app.bundles
            .iter()
            .filter(|b| !children.contains(&b.bundle_id))
            .map(|b| b.bundle_id)
            .collect()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CatalogApp {
    pub app_id: AppId,
    pub display_name: String,
    pub latest_bundle_id: Uuid,
    pub bundles: Vec<CatalogBundle>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CatalogBundle {
    pub bundle_id: Uuid,
    pub commit_id: Uuid,
    pub parent_bundle_id: Option<Uuid>,
    pub captured_at: DateTime<Utc>,
    pub source_instance_id: Uuid,
    pub mode: BackupMode,
    pub logical_size: u64,
    pub stored_incremental_size: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CatalogInstance {
    pub instance_id: Uuid,
    pub image_id: String,
    pub last_seal: Option<SealAppCommit>,
}

pub const COMMITTED_MARKER: &str = "COMMITTED";
pub const LATEST_POINTER: &str = "LATEST";

pub fn bundle_dir(app_id: &AppId, bundle_id: Uuid) -> String {
    format!("bundles/{}/{bundle_id}", app_id.storage_safe())
}

pub fn pack_key(pack_id: &str) -> String {
    let prefix: String = pack_id.chars().take(2).collect();
    format!("packs/{prefix}/{pack_id}.pack")
}

pub fn catalog_commit_key(catalog_commit_id: Uuid) -> String {
    format!("catalog/commits/{catalog_commit_id}.enc")
}

pub fn catalog_latest_key() -> String {
    "catalog/LATEST".into()
}

pub fn checkpoint_dir(instance_id: Uuid, checkpoint_id: Uuid) -> String {
    format!("checkpoints/{instance_id}/{checkpoint_id}")
}

pub fn seal_dir(instance_id: Uuid, seal_id: Uuid) -> String {
    format!("instances/{instance_id}/seals/{seal_id}")
}
