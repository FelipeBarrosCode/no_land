use std::path::Path;

use crate::classify::{BackupDecision, BackupMode, PersistenceClass, SemanticRole};
use crate::confidence::{association_strength, AssociationStrength};
use crate::evidence::PathAssociation;
use crate::paths::{
    is_regenerable_game_cache, is_tracking_excluded, looks_like_cache, looks_like_secret,
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
    let in_known_app_root = ctx.association.evidence.iter().any(|evidence| {
        matches!(
            evidence.kind,
            crate::evidence::EvidenceKind::KnownAppRoot
                | crate::evidence::EvidenceKind::ExplicitUserBinding
        )
    });
    if is_tracking_excluded(ctx.path, in_known_app_root, None) {
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

    if ctx.association.persistence_class == PersistenceClass::ReconstructableApp
        && (is_regenerable_game_cache(ctx.path)
            || ctx.association.semantic_role == SemanticRole::Cache)
    {
        return match ctx.mode {
            BackupMode::PersonalState if looks_like_cache(ctx.path) => BackupDecision::Exclude,
            BackupMode::PersonalState => BackupDecision::MetadataOnly,
            BackupMode::CompleteApplication | BackupMode::Custom => {
                BackupDecision::IncludeAsBaseOrOverlay
            }
        };
    }
    if ctx.mode == BackupMode::CompleteApplication
        && in_known_app_root
        && !crate::paths::looks_like_lock_or_socket(ctx.path)
    {
        return match ctx.association.persistence_class {
            PersistenceClass::ReconstructableApp | PersistenceClass::Unknown => {
                BackupDecision::IncludeAsBaseOrOverlay
            }
            PersistenceClass::SharedState => BackupDecision::IncludeSharedReference,
            _ => BackupDecision::Include,
        };
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
            } else if matches!(
                ctx.mode,
                BackupMode::CompleteApplication | BackupMode::Custom
            ) && ctx
                .association
                .evidence
                .iter()
                .any(|evidence| evidence.kind == crate::evidence::EvidenceKind::ReadOnlyDependency)
            {
                BackupDecision::IncludeAsBaseOrOverlay
            } else {
                BackupDecision::DeferAndReconcile
            }
        }
    }
}

/// Restore uses explicit binding to retain the authenticated bundle's scope.
/// That ownership evidence must not turn its app-content entries into saves.
pub fn is_restored_app_content(association: &PathAssociation) -> bool {
    association.persistence_class == PersistenceClass::ReconstructableApp
        && matches!(
            association.semantic_role,
            SemanticRole::AppContent | SemanticRole::Cache
        )
        && association.evidence.iter().any(|evidence| {
            evidence.kind == crate::evidence::EvidenceKind::RestoredFromCommittedBundle
        })
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
    if class == PersistenceClass::ReconstructableApp && is_regenerable_game_cache(path) {
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

#[cfg(test)]
mod tests {
    use chrono::Utc;

    use super::*;
    use crate::evidence::{Evidence, EvidenceKind};
    use crate::identity::AppId;

    fn external_dependency() -> PathAssociation {
        PathAssociation {
            app_id: AppId::desktop("portable-app"),
            path_id: 1,
            confidence: crate::confidence::CONF_DEPENDENCY,
            evidence: vec![Evidence::new(EvidenceKind::ReadOnlyDependency)],
            persistence_class: PersistenceClass::Unknown,
            semantic_role: SemanticRole::Unknown,
            first_seen_at: Utc::now(),
            last_seen_at: Utc::now(),
        }
    }

    #[test]
    fn regenerable_game_caches_are_app_content_with_default_complete_inclusion() {
        let path = Path::new("/home/user/Games/Hookshot/hookshot_programs.bin");
        let mut association = external_dependency();
        association.persistence_class = PersistenceClass::ReconstructableApp;
        association.semantic_role = infer_semantic_role(path, association.persistence_class);
        assert_eq!(association.semantic_role, SemanticRole::Cache);
        for (mode, expected) in [
            (BackupMode::PersonalState, BackupDecision::MetadataOnly),
            (
                BackupMode::CompleteApplication,
                BackupDecision::IncludeAsBaseOrOverlay,
            ),
            (BackupMode::Custom, BackupDecision::IncludeAsBaseOrOverlay),
        ] {
            assert_eq!(
                decide_path(PathDecisionContext {
                    path,
                    association: &association,
                    mode,
                    matches_image_baseline: false,
                    reliable_reconstruction: true,
                    policy_override: None,
                }),
                expected
            );
        }
        assert_eq!(
            decide_path(PathDecisionContext {
                path,
                association: &association,
                mode: BackupMode::CompleteApplication,
                matches_image_baseline: false,
                reliable_reconstruction: false,
                policy_override: Some(PathPolicy::Exclude),
            }),
            BackupDecision::Exclude
        );
    }

    #[test]
    fn complete_application_includes_unknown_external_dependencies() {
        let association = external_dependency();
        let decide = |mode| {
            decide_path(PathDecisionContext {
                path: Path::new("/home/gamer/Downloads/portable-app.AppImage"),
                association: &association,
                mode,
                matches_image_baseline: false,
                reliable_reconstruction: false,
                policy_override: None,
            })
        };

        assert_eq!(
            decide(BackupMode::PersonalState),
            BackupDecision::DeferAndReconcile
        );
        assert_eq!(
            decide(BackupMode::CompleteApplication),
            BackupDecision::IncludeAsBaseOrOverlay
        );
    }

    #[test]
    fn complete_application_still_excludes_cache_dependencies() {
        let association = external_dependency();
        assert_eq!(
            decide_path(PathDecisionContext {
                path: Path::new("/home/gamer/.cache/portable-app/index"),
                association: &association,
                mode: BackupMode::CompleteApplication,
                matches_image_baseline: false,
                reliable_reconstruction: false,
                policy_override: None,
            }),
            BackupDecision::Exclude
        );
    }

    #[test]
    fn base_system_paths_require_explicit_known_root_evidence() {
        let association = external_dependency();
        assert_eq!(
            decide_path(PathDecisionContext {
                path: Path::new("/etc/example-app/settings.toml"),
                association: &association,
                mode: BackupMode::CompleteApplication,
                matches_image_baseline: false,
                reliable_reconstruction: false,
                policy_override: Some(PathPolicy::ForcePersistent),
            }),
            BackupDecision::Exclude
        );

        let mut known_root = association;
        known_root
            .evidence
            .push(Evidence::new(EvidenceKind::KnownAppRoot));
        known_root.persistence_class = PersistenceClass::PersistentState;
        assert_eq!(
            decide_path(PathDecisionContext {
                path: Path::new("/etc/example-app/settings.toml"),
                association: &known_root,
                mode: BackupMode::PersonalState,
                matches_image_baseline: false,
                reliable_reconstruction: false,
                policy_override: None,
            }),
            BackupDecision::Include
        );
    }

    #[test]
    fn known_roots_cannot_override_noland_or_volatile_exclusions() {
        let mut association = external_dependency();
        association
            .evidence
            .push(Evidence::new(EvidenceKind::KnownAppRoot));
        association.persistence_class = PersistenceClass::PersistentState;
        for path in [
            "/opt/noland/state-agent/config.toml",
            "/etc/sunshine/sunshine.conf",
            "/run/example-app/state",
        ] {
            assert_eq!(
                decide_path(PathDecisionContext {
                    path: Path::new(path),
                    association: &association,
                    mode: BackupMode::CompleteApplication,
                    matches_image_baseline: false,
                    reliable_reconstruction: false,
                    policy_override: Some(PathPolicy::ForcePersistent),
                }),
                BackupDecision::Exclude,
                "{path}"
            );
        }
    }
}
