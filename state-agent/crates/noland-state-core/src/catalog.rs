use std::path::PathBuf;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::classify::BackupMode;
use crate::identity::{AppId, LauncherKind};
use crate::manifest::ManifestApp;
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

    pub fn refresh_bundle_heads(&mut self) {
        for app in &mut self.apps {
            app.refresh_bundle_heads();
        }
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
            existing.refresh_bundle_heads();
        } else {
            let latest_complete_bundle_id =
                (bundle.mode == BackupMode::CompleteApplication).then_some(bundle.bundle_id);
            let latest_personal_state_bundle_id =
                (bundle.mode == BackupMode::PersonalState).then_some(bundle.bundle_id);
            self.apps.push(CatalogApp {
                latest_bundle_id: bundle.bundle_id,
                latest_complete_bundle_id,
                latest_personal_state_bundle_id,
                app_id: app_id.clone(),
                display_name,
                aliases: Vec::new(),
                desktop_entry_id: None,
                steam_app_id: None,
                launcher: None,
                canonical_executable: None,
                icon_path: None,
                bundles: vec![bundle],
            });
            if !self.heads.iter().any(|id| id == &app_id) {
                self.heads.push(app_id);
            }
        }
    }

    pub fn upsert_bundle_from_manifest(&mut self, app: &ManifestApp, bundle: CatalogBundle) {
        self.upsert_bundle(app.app_id.clone(), app.display_name.clone(), bundle);
        if let Some(existing) = self.app_mut(&app.app_id) {
            for alias in &app.aliases {
                if !existing.aliases.iter().any(|item| item == alias) {
                    existing.aliases.push(alias.clone());
                }
            }
            if app.desktop_entry_id.is_some() {
                existing.desktop_entry_id.clone_from(&app.desktop_entry_id);
            }
            if app.steam_app_id.is_some() {
                existing.steam_app_id = app.steam_app_id;
            }
            if app.launcher.is_some() {
                existing.launcher = app.launcher;
            }
            if app.canonical_executable.is_some() {
                existing
                    .canonical_executable
                    .clone_from(&app.canonical_executable);
            }
            if app.icon_path.is_some() {
                existing.icon_path.clone_from(&app.icon_path);
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
    #[serde(default)]
    pub aliases: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub desktop_entry_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub steam_app_id: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub launcher: Option<LauncherKind>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub canonical_executable: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon_path: Option<PathBuf>,
    pub latest_bundle_id: Uuid,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latest_complete_bundle_id: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latest_personal_state_bundle_id: Option<Uuid>,
    pub bundles: Vec<CatalogBundle>,
}

impl CatalogApp {
    pub fn refresh_bundle_heads(&mut self) {
        self.latest_complete_bundle_id =
            latest_bundle_for_mode(&self.bundles, BackupMode::CompleteApplication);
        self.latest_personal_state_bundle_id =
            latest_bundle_for_mode(&self.bundles, BackupMode::PersonalState);
    }

    pub fn restorable_bundle_id(&self) -> Option<Uuid> {
        self.latest_complete_bundle_id
            .or_else(|| latest_bundle_for_mode(&self.bundles, BackupMode::CompleteApplication))
    }
}

fn latest_bundle_for_mode(bundles: &[CatalogBundle], mode: BackupMode) -> Option<Uuid> {
    bundles
        .iter()
        .filter(|bundle| bundle.mode == mode)
        .max_by_key(|bundle| bundle.captured_at)
        .map(|bundle| bundle.bundle_id)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn old_catalog_json_uses_launch_metadata_defaults() {
        let catalog: CatalogDocument = serde_json::from_value(serde_json::json!({
            "schema_version": 1,
            "catalog_commit_id": Uuid::nil(),
            "created_at": Utc::now(),
            "heads": ["desktop:game"],
            "apps": [{
                "app_id": "desktop:game",
                "display_name": "Game",
                "latest_bundle_id": Uuid::nil(),
                "bundles": []
            }],
            "instances": []
        }))
        .unwrap();

        let app = &catalog.apps[0];
        assert!(app.aliases.is_empty());
        assert!(app.desktop_entry_id.is_none());
        assert!(app.steam_app_id.is_none());
        assert!(app.launcher.is_none());
        assert!(app.canonical_executable.is_none());
        assert!(app.icon_path.is_none());
        assert!(app.latest_complete_bundle_id.is_none());
        assert!(app.latest_personal_state_bundle_id.is_none());
    }

    #[test]
    fn complete_and_personal_bundle_heads_are_tracked_separately() {
        let app_id = AppId("desktop:game".into());
        let mut catalog = CatalogDocument::empty();
        let complete_id = Uuid::new_v4();
        let personal_id = Uuid::new_v4();
        let now = Utc::now();
        let bundle = |bundle_id, mode, captured_at| CatalogBundle {
            bundle_id,
            commit_id: Uuid::new_v4(),
            parent_bundle_id: None,
            captured_at,
            source_instance_id: Uuid::new_v4(),
            mode,
            logical_size: 1,
            stored_incremental_size: 1,
        };

        catalog.upsert_bundle(
            app_id.clone(),
            "Game".into(),
            bundle(complete_id, BackupMode::CompleteApplication, now),
        );
        catalog.upsert_bundle(
            app_id.clone(),
            "Game".into(),
            bundle(
                personal_id,
                BackupMode::PersonalState,
                now + chrono::Duration::seconds(1),
            ),
        );

        let app = catalog.app_mut(&app_id).unwrap();
        assert_eq!(app.latest_bundle_id, personal_id);
        assert_eq!(app.restorable_bundle_id(), Some(complete_id));
        assert_eq!(app.latest_personal_state_bundle_id, Some(personal_id));
    }
}
