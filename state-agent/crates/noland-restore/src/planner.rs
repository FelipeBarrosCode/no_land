use noland_state_core::{
    BundleManifest, ManifestFile, PersistenceClass, RestoreMode, SemanticRole,
};

/// Deterministic restore tiers, ordered from launch-blocking to deferrable work.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum RestorePriority {
    Prerequisite,
    Critical,
    Soon,
    Background,
}

/// A caller-visible restore milestone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RestoreTarget {
    /// Restore prerequisite and critical files, after which the application can launch.
    ReadyToLaunch,
    /// Restore every file selected by the restore mode.
    Complete,
}

/// The milestone reached after prerequisite and critical tiers are applied.
pub const READY_TO_LAUNCH: RestoreTarget = RestoreTarget::ReadyToLaunch;

impl RestoreTarget {
    pub(crate) fn includes(self, priority: RestorePriority) -> bool {
        match self {
            Self::ReadyToLaunch => priority <= RestorePriority::Critical,
            Self::Complete => true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RestorePlanEntry {
    pub manifest_index: usize,
    pub priority: RestorePriority,
}

#[derive(Debug, Clone, PartialEq, Eq)]
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
    let mut entries = manifest
        .files
        .iter()
        .enumerate()
        .filter(|(_, file)| super::include_file(mode, file))
        .map(|(manifest_index, file)| RestorePlanEntry {
            manifest_index,
            priority: restore_priority(file),
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
