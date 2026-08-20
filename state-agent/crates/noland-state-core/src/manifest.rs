use std::path::PathBuf;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::classify::{BackupMode, ConsistencyKind, PersistenceClass, SemanticRole};
use crate::constants;
use crate::identity::{AppId, AppIdentity, LauncherKind};
use crate::logical_path::LogicalRoot;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BundleManifest {
    pub schema_version: u32,
    pub bundle_id: Uuid,
    pub commit_id: Uuid,
    pub parent_bundle_id: Option<Uuid>,
    pub app: ManifestApp,
    pub source: ManifestSource,
    pub mode: BackupMode,
    pub consistency: ConsistencyKind,
    pub chunking: ChunkingMeta,
    pub hash: HashMeta,
    #[serde(default)]
    pub base: serde_json::Value,
    #[serde(default)]
    pub files: Vec<ManifestFile>,
    #[serde(default)]
    pub tombstones: Vec<ManifestTombstone>,
    #[serde(default)]
    pub environment: serde_json::Value,
    #[serde(default)]
    pub relationships: Vec<ManifestRelationship>,
    #[serde(default)]
    pub restore_plan: serde_json::Value,
    #[serde(default)]
    pub learned_state_ref: Option<String>,
}

impl BundleManifest {
    pub fn new(app: ManifestApp, source: ManifestSource, mode: BackupMode) -> Self {
        Self {
            schema_version: constants::MANIFEST_SCHEMA_VERSION,
            bundle_id: Uuid::new_v4(),
            commit_id: Uuid::new_v4(),
            parent_bundle_id: None,
            app,
            source,
            mode,
            consistency: ConsistencyKind::Snapshot,
            chunking: ChunkingMeta::fastcdc_v1(),
            hash: HashMeta {
                algorithm: constants::HASH_ALGORITHM.into(),
            },
            base: serde_json::json!({}),
            files: Vec::new(),
            tombstones: Vec::new(),
            environment: serde_json::json!({}),
            relationships: Vec::new(),
            restore_plan: serde_json::json!({}),
            learned_state_ref: None,
        }
    }

    pub fn logical_size(&self) -> u64 {
        self.files.iter().map(|f| f.size).sum()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ManifestApp {
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
}

impl From<&AppIdentity> for ManifestApp {
    fn from(identity: &AppIdentity) -> Self {
        Self {
            app_id: identity.app_id.clone(),
            display_name: identity.display_name.clone(),
            aliases: identity.aliases.clone(),
            desktop_entry_id: identity.desktop_entry_id.clone(),
            steam_app_id: identity.steam_app_id,
            launcher: identity.launcher,
            canonical_executable: identity.canonical_executable.clone(),
            icon_path: identity.icon_path.clone(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ManifestSource {
    pub instance_id: Uuid,
    pub image_id: String,
    pub captured_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChunkingMeta {
    pub algorithm: String,
    pub min: u64,
    pub avg: u64,
    pub max: u64,
}

impl ChunkingMeta {
    pub fn fastcdc_v1() -> Self {
        Self {
            algorithm: constants::CHUNK_ALGORITHM.into(),
            min: constants::FASTCDC_MIN,
            avg: constants::FASTCDC_AVG,
            max: constants::FASTCDC_MAX,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HashMeta {
    pub algorithm: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ManifestFile {
    pub logical_root: String,
    pub relative_path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_path_hint: Option<String>,
    pub file_type: String,
    pub size: u64,
    pub file_hash: String,
    #[serde(default)]
    pub chunks: Vec<ChunkRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mtime_ns: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uid: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gid: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub symlink_target: Option<String>,
    pub persistence_class: PersistenceClass,
    pub semantic_role: SemanticRole,
    pub association_confidence: f32,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub shared_app_ids: Vec<AppId>,
}

impl ManifestFile {
    pub fn logical_root_parsed(&self) -> Option<LogicalRoot> {
        LogicalRoot::parse(&self.logical_root)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChunkRef {
    pub hash: String,
    pub size: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ManifestTombstone {
    pub logical_root: String,
    pub relative_path: String,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ManifestRelationship {
    pub relation: String,
    pub from: String,
    pub to: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LearnedStateCheckpoint {
    pub schema_version: u32,
    pub checkpoint_id: Uuid,
    pub instance_id: Uuid,
    pub image_id: String,
    pub created_at: DateTime<Utc>,
    pub apps: Vec<crate::identity::AppIdentity>,
    pub associations: Vec<CheckpointAssociation>,
    pub known_roots: Vec<CheckpointRoot>,
    pub latest_bundle_refs: Vec<CheckpointBundleRef>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CheckpointAssociation {
    pub app_id: AppId,
    pub canonical_path: String,
    pub logical_root: Option<String>,
    pub relative_path: Option<String>,
    pub confidence: f32,
    pub persistence_class: PersistenceClass,
    pub semantic_role: SemanticRole,
    pub evidence: Vec<crate::evidence::EvidenceKind>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CheckpointRoot {
    pub app_id: AppId,
    pub kind: String,
    pub path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CheckpointBundleRef {
    pub app_id: AppId,
    pub bundle_id: Uuid,
    pub commit_id: Uuid,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn old_manifest_app_json_uses_launch_metadata_defaults() {
        let app: ManifestApp = serde_json::from_value(serde_json::json!({
            "app_id": "desktop:game",
            "display_name": "Game",
            "aliases": []
        }))
        .unwrap();

        assert_eq!(app.app_id, AppId::desktop("game"));
        assert!(app.desktop_entry_id.is_none());
        assert!(app.steam_app_id.is_none());
        assert!(app.launcher.is_none());
        assert!(app.canonical_executable.is_none());
        assert!(app.icon_path.is_none());
    }
}
