use std::path::Path;

use crate::classify::{BackupDecision, BackupMode, PersistenceClass, SemanticRole};
use crate::confidence::{association_strength, AssociationStrength};
use crate::evidence::PathAssociation;
use crate::paths::{
    is_hard_volatile_root, is_noland_internal, looks_like_cache, looks_like_secret,
    looks_like_user_state,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PathPolicy {
    Include,
    Exclude,
    Cache,
    Secret,
    Shared,
    ForcePersistent,
    ForceReconstructable,
}

#[derive(Debug, Clone)]
pub struct PathDecisionContext<'a> {
    pub path: &'a Path,
    pub association: &'a PathAssociation,
    pub mode: BackupMode,
    pub matches_image_baseline: bool,
    pub reliable_reconstruction: bool,
    pub policy_override: Option<PathPolicy>,
}

pub fn decide_path(ctx: PathDecisionContext<'_>) -> BackupDecision {
    if is_noland_internal(ctx.path) {
        return BackupDecision::Exclude;
    }
    if is_hard_volatile_root(ctx.path) {
        return BackupDecision::Exclude;
    }
    if matches!(ctx.policy_override, Some(PathPolicy::Exclude)) {
        return BackupDecision::Exclude;
    }
    if matches!(ctx.policy_override, Some(PathPolicy::ForcePersistent)) {
        return BackupDecision::Include;
    }
    if ctx.association.semantic_role == SemanticRole::Secret
        || matches!(ctx.policy_override, Some(PathPolicy::Secret))
        || looks_like_secret(ctx.path)
    {
        return BackupDecision::EncryptedInclude;
    }

    match ctx.association.persistence_class {
        PersistenceClass::BaseImage => {
            if ctx.matches_image_baseline {
                BackupDecision::Exclude
            } else {
                BackupDecision::IncludeAsOverlay
            }
        }
        PersistenceClass::ReconstructableApp => match ctx.mode {
            BackupMode::PersonalState => BackupDecision::MetadataOnly,
            BackupMode::CompleteApplication | BackupMode::Custom => {
                if ctx.reliable_reconstruction
                    && !matches!(ctx.policy_override, Some(PathPolicy::ForceReconstructable))
                {
                    BackupDecision::MetadataOnly
                } else {
                    BackupDecision::IncludeAsBaseOrOverlay
                }
            }
        },
        PersistenceClass::PersistentState => BackupDecision::Include,
        PersistenceClass::SharedState => BackupDecision::IncludeSharedReference,
        PersistenceClass::Ephemeral => BackupDecision::Exclude,
        PersistenceClass::Unknown => {
            let strength = association_strength(ctx.association.confidence);
            if strength == AssociationStrength::StrongOwnership
                && (looks_like_user_state(ctx.path) || looks_like_secret(ctx.path))
            {
                BackupDecision::Include
            } else if looks_like_cache(ctx.path) {
                BackupDecision::Exclude
            } else {
                BackupDecision::DeferAndReconcile
            }
        }
    }
}

pub fn infer_semantic_role(path: &Path, class: PersistenceClass) -> SemanticRole {
    if looks_like_secret(path) {
        return SemanticRole::Secret;
    }
    if looks_like_cache(path) || class == PersistenceClass::Ephemeral {
        if path.to_string_lossy().contains("/tmp") {
            return SemanticRole::Temp;
        }
        return SemanticRole::Cache;
    }
    match class {
        PersistenceClass::BaseImage => SemanticRole::Os,
        PersistenceClass::ReconstructableApp => SemanticRole::AppContent,
        PersistenceClass::PersistentState => SemanticRole::UserState,
        PersistenceClass::SharedState => SemanticRole::SharedRuntime,
        PersistenceClass::Ephemeral => SemanticRole::Cache,
        PersistenceClass::Unknown => SemanticRole::Unknown,
    }
}
