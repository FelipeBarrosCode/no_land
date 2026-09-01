use std::collections::BTreeMap;

use noland_state_core::{
    BundleManifest, ManifestFile, PersistenceClass, RestoreMode, SemanticRole,
};
use serde::{Deserialize, Serialize};

/// Deterministic restore tiers, ordered from launch-blocking to deferrable work.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RestorePriority {
    Prerequisite,
    Critical,
    Soon,
    Background,
}

/// A caller-visible restore milestone.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum RestoreTarget {
    /// Restore prerequisite and critical files, after which the application can launch.
    ReadyToLaunch,
    /// Restore launch-critical files plus the soon-needed prefetch tier.
    Soon,
    /// Restore every file selected by the restore mode.
    Complete,
}

/// The milestone reached after prerequisite and critical tiers are applied.
pub const READY_TO_LAUNCH: RestoreTarget = RestoreTarget::ReadyToLaunch;

impl RestoreTarget {
    pub(crate) fn includes(self, priority: RestorePriority) -> bool {
        match self {
            Self::ReadyToLaunch => priority <= RestorePriority::Critical,
            Self::Soon => priority <= RestorePriority::Soon,
            Self::Complete => true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RestorePlanEntry {
    pub manifest_index: usize,
    pub priority: RestorePriority,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RestorePriorityPlan {
    pub entries: Vec<RestorePlanEntry>,
}

impl RestorePriorityPlan {
    pub fn entries_for(&self, target: RestoreTarget) -> impl Iterator<Item = &RestorePlanEntry> {
        self.entries
            .iter()
            .filter(move |entry| target.includes(entry.priority))
    }
}

/// Builds a stable plan from manifest classification and semantic role.
///
/// Ties are sorted by logical root, relative path, file type, and original manifest position.
pub fn plan_restore_priorities(
    manifest: &BundleManifest,
    mode: RestoreMode,
) -> RestorePriorityPlan {
    let durable = durable_priorities(manifest);
    let mut entries = manifest
        .files
        .iter()
        .enumerate()
        .filter(|(_, file)| super::include_file(mode, file))
        .map(|(manifest_index, file)| RestorePlanEntry {
            manifest_index,
            priority: durable
                .get(&(file.logical_root.clone(), file.relative_path.clone()))
                .copied()
                .unwrap_or_else(|| restore_priority(file)),
        })
        .collect::<Vec<_>>();

    entries.sort_by(|left, right| {
        let left_file = &manifest.files[left.manifest_index];
        let right_file = &manifest.files[right.manifest_index];
        left.priority
            .cmp(&right.priority)
            .then_with(|| left_file.logical_root.cmp(&right_file.logical_root))
            .then_with(|| left_file.relative_path.cmp(&right_file.relative_path))
            .then_with(|| left_file.file_type.cmp(&right_file.file_type))
            .then_with(|| left.manifest_index.cmp(&right.manifest_index))
    });

    RestorePriorityPlan { entries }
}

/// Persists deterministic launch-priority and prefetch hints without changing
/// the manifest's backward-compatible file representation.
pub fn embed_restore_plan(manifest: &mut BundleManifest, mode: RestoreMode) {
    let plan = plan_restore_priorities(manifest, mode);
    let files = plan
        .entries
        .iter()
        .map(|entry| {
            let file = &manifest.files[entry.manifest_index];
            serde_json::json!({
                "logical_root": file.logical_root,
                "relative_path": file.relative_path,
                "priority": entry.priority,
            })
        })
        .collect::<Vec<_>>();
    let startup_prefetch = plan
        .entries_for(RestoreTarget::ReadyToLaunch)
        .map(|entry| {
            let file = &manifest.files[entry.manifest_index];
            serde_json::json!({
                "logical_root": file.logical_root,
                "relative_path": file.relative_path,
            })
        })
        .collect::<Vec<_>>();
    let soon_prefetch = plan
        .entries
        .iter()
        .filter(|entry| entry.priority == RestorePriority::Soon)
        .map(|entry| {
            let file = &manifest.files[entry.manifest_index];
            serde_json::json!({
                "logical_root": file.logical_root,
                "relative_path": file.relative_path,
            })
        })
        .collect::<Vec<_>>();
    manifest.restore_plan = serde_json::json!({
        "version": 1,
        "source": "classification_v1",
        "files": files,
        "startup_prefetch": startup_prefetch,
        "soon_prefetch": soon_prefetch,
    });
}

fn durable_priorities(manifest: &BundleManifest) -> BTreeMap<(String, String), RestorePriority> {
    manifest
        .restore_plan
        .get("files")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|value| {
            let logical_root = value.get("logical_root")?.as_str()?.to_string();
            let relative_path = value.get("relative_path")?.as_str()?.to_string();
            let priority = serde_json::from_value(value.get("priority")?.clone()).ok()?;
            Some(((logical_root, relative_path), priority))
        })
        .collect()
}

pub fn restore_priority(file: &ManifestFile) -> RestorePriority {
    match file.semantic_role {
        SemanticRole::AppContent | SemanticRole::SharedRuntime => RestorePriority::Prerequisite,
        SemanticRole::UserState | SemanticRole::Secret => RestorePriority::Critical,
        SemanticRole::Cache | SemanticRole::Temp | SemanticRole::Os => RestorePriority::Background,
        SemanticRole::Unknown => match file.persistence_class {
            PersistenceClass::ReconstructableApp => RestorePriority::Prerequisite,
            PersistenceClass::PersistentState => RestorePriority::Critical,
            PersistenceClass::SharedState | PersistenceClass::Unknown => RestorePriority::Soon,
            PersistenceClass::BaseImage | PersistenceClass::Ephemeral => {
                RestorePriority::Background
            }
        },
    }
}
