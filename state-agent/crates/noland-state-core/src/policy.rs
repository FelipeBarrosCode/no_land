use std::path::Path;

use crate::classify::{BackupDecision, BackupMode, PersistenceClass, SemanticRole};
use crate::confidence::{association_strength, AssociationStrength};
use crate::evidence::PathAssociation;
use crate::paths::{
    is_tracking_excluded, looks_like_cache, looks_like_secret, looks_like_user_state,
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
