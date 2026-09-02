use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::classify::{PersistenceClass, SemanticRole};
use crate::identity::AppId;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub enum EvidenceKind {
    ExplicitUserBinding,
    RestoredFromCommittedBundle,
    DirectCgroupWrite,
    DirectCgroupCreate,
    DirectCgroupRename,
    DirectCgroupDelete,
    InstallerTransaction,
    KnownAppRoot,
    KnownUserStateRoot,
    SteamMetadata,
    ProtonPrefix,
    WinePrefix,
    DesktopEntry,
    PackageOwner,
    BaseImageMatch,
    RepeatedSessionUse,
    ReadOnlyDependency,
    SharedServiceMutation,
    ReconciliationDelta,
    NameHeuristic,
}

impl EvidenceKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ExplicitUserBinding => "ExplicitUserBinding",
            Self::RestoredFromCommittedBundle => "RestoredFromCommittedBundle",
            Self::DirectCgroupWrite => "DirectCgroupWrite",
            Self::DirectCgroupCreate => "DirectCgroupCreate",
            Self::DirectCgroupRename => "DirectCgroupRename",
            Self::DirectCgroupDelete => "DirectCgroupDelete",
            Self::InstallerTransaction => "InstallerTransaction",
            Self::KnownAppRoot => "KnownAppRoot",
            Self::KnownUserStateRoot => "KnownUserStateRoot",
            Self::SteamMetadata => "SteamMetadata",
            Self::ProtonPrefix => "ProtonPrefix",
            Self::WinePrefix => "WinePrefix",
            Self::DesktopEntry => "DesktopEntry",
            Self::PackageOwner => "PackageOwner",
            Self::BaseImageMatch => "BaseImageMatch",
            Self::RepeatedSessionUse => "RepeatedSessionUse",
            Self::ReadOnlyDependency => "ReadOnlyDependency",
            Self::SharedServiceMutation => "SharedServiceMutation",
            Self::ReconciliationDelta => "ReconciliationDelta",
            Self::NameHeuristic => "NameHeuristic",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        Some(match raw {
            "ExplicitUserBinding" => Self::ExplicitUserBinding,
            "RestoredFromCommittedBundle" => Self::RestoredFromCommittedBundle,
            "DirectCgroupWrite" => Self::DirectCgroupWrite,
            "DirectCgroupCreate" => Self::DirectCgroupCreate,
            "DirectCgroupRename" => Self::DirectCgroupRename,
            "DirectCgroupDelete" => Self::DirectCgroupDelete,
            "InstallerTransaction" => Self::InstallerTransaction,
            "KnownAppRoot" => Self::KnownAppRoot,
            "KnownUserStateRoot" => Self::KnownUserStateRoot,
            "SteamMetadata" => Self::SteamMetadata,
            "ProtonPrefix" => Self::ProtonPrefix,
            "WinePrefix" => Self::WinePrefix,
            "DesktopEntry" => Self::DesktopEntry,
            "PackageOwner" => Self::PackageOwner,
            "BaseImageMatch" => Self::BaseImageMatch,
            "RepeatedSessionUse" => Self::RepeatedSessionUse,
            "ReadOnlyDependency" => Self::ReadOnlyDependency,
            "SharedServiceMutation" => Self::SharedServiceMutation,
            "ReconciliationDelta" => Self::ReconciliationDelta,
            "NameHeuristic" => Self::NameHeuristic,
            _ => return None,
        })
    }

    pub fn is_mutation(self) -> bool {
        matches!(
            self,
            Self::DirectCgroupWrite
                | Self::DirectCgroupCreate
                | Self::DirectCgroupRename
                | Self::DirectCgroupDelete
                | Self::InstallerTransaction
                | Self::ReconciliationDelta
                | Self::SharedServiceMutation
        )
    }

    pub fn is_ownership_grade(self) -> bool {
        !matches!(
            self,
            Self::ReadOnlyDependency | Self::BaseImageMatch | Self::PackageOwner
        )
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Evidence {
    pub kind: EvidenceKind,
    pub observed_at: DateTime<Utc>,
    pub detail: Option<String>,
    pub session_id: Option<uuid::Uuid>,
}

impl Evidence {
    pub fn new(kind: EvidenceKind) -> Self {
        Self {
            kind,
            observed_at: Utc::now(),
            detail: None,
            session_id: None,
        }
    }

    pub fn with_detail(mut self, detail: impl Into<String>) -> Self {
        self.detail = Some(detail.into());
        self
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PathAssociation {
    pub app_id: AppId,
    pub path_id: i64,
    pub confidence: f32,
    pub evidence: Vec<Evidence>,
    pub persistence_class: PersistenceClass,
    pub semantic_role: SemanticRole,
    pub first_seen_at: DateTime<Utc>,
    pub last_seen_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PathRecord {
    pub path_id: i64,
    pub canonical_path: String,
    pub logical_root: Option<String>,
    pub relative_path: Option<String>,
    pub file_type: Option<String>,
    pub inode: Option<i64>,
    pub mount_id: Option<i64>,
    pub size: Option<i64>,
    pub mtime_ns: Option<i64>,
    pub mode: Option<i64>,
    pub uid: Option<i64>,
    pub gid: Option<i64>,
    pub content_hash: Option<String>,
    pub last_scanned_at: Option<i64>,
}

/// Inspectable, deterministic association score. Not a black-box classifier.
#[derive(Debug, Clone, Default)]
pub struct ScoreBreakdown {
    pub direct_process_evidence: f32,
    pub mutation_evidence: f32,
    pub known_root_evidence: f32,
    pub installer_evidence: f32,
    pub repeated_session_evidence: f32,
    pub user_state_prior: f32,
    pub restore_provenance: f32,
    pub base_image_penalty: f32,
    pub package_reconstructable_penalty: f32,
    pub ephemeral_pattern_penalty: f32,
    pub ambient_process_penalty: f32,
}

impl ScoreBreakdown {
    pub fn total(&self) -> f32 {
        let raw = self.direct_process_evidence
            + self.mutation_evidence
            + self.known_root_evidence
            + self.installer_evidence
            + self.repeated_session_evidence
            + self.user_state_prior
            + self.restore_provenance
            - self.base_image_penalty
            - self.package_reconstructable_penalty
            - self.ephemeral_pattern_penalty
            - self.ambient_process_penalty;
        raw.clamp(0.0, 1.0)
    }
}

pub fn score_evidence(evidence: &[Evidence]) -> (f32, ScoreBreakdown) {
    let mut breakdown = ScoreBreakdown::default();
    let mut kinds: Vec<EvidenceKind> = evidence.iter().map(|e| e.kind).collect();
    kinds.sort();
    kinds.dedup();

    if kinds.contains(&EvidenceKind::ExplicitUserBinding)
        || kinds.contains(&EvidenceKind::RestoredFromCommittedBundle)
    {
        breakdown.restore_provenance = 1.00;
        return (1.00, breakdown);
    }

    let mut mutation_in_root = false;
    let mut mutation_outside = false;
    for kind in &kinds {
        match kind {
            EvidenceKind::DirectCgroupWrite
            | EvidenceKind::DirectCgroupCreate
            | EvidenceKind::DirectCgroupRename
            | EvidenceKind::DirectCgroupDelete => {
                breakdown.direct_process_evidence = 0.55;
                breakdown.mutation_evidence = 0.25;
                if kinds.contains(&EvidenceKind::KnownAppRoot)
                    || kinds.contains(&EvidenceKind::KnownUserStateRoot)
                    || kinds.contains(&EvidenceKind::SteamMetadata)
                    || kinds.contains(&EvidenceKind::ProtonPrefix)
                    || kinds.contains(&EvidenceKind::WinePrefix)
                {
                    mutation_in_root = true;
                } else {
                    mutation_outside = true;
                }
            }
            EvidenceKind::InstallerTransaction => breakdown.installer_evidence = 0.20,
            EvidenceKind::KnownAppRoot | EvidenceKind::KnownUserStateRoot => {
                breakdown.known_root_evidence = breakdown.known_root_evidence.max(0.15);
            }
            EvidenceKind::SteamMetadata | EvidenceKind::DesktopEntry => {
                breakdown.known_root_evidence = breakdown.known_root_evidence.max(0.15);
            }
            EvidenceKind::ProtonPrefix | EvidenceKind::WinePrefix => {
                breakdown.known_root_evidence = breakdown.known_root_evidence.max(0.10);
            }
            EvidenceKind::RepeatedSessionUse => breakdown.repeated_session_evidence = 0.15,
            EvidenceKind::ReconciliationDelta => breakdown.mutation_evidence += 0.10,
            EvidenceKind::NameHeuristic => breakdown.user_state_prior += 0.05,
            EvidenceKind::ReadOnlyDependency => breakdown.direct_process_evidence += 0.05,
            EvidenceKind::SharedServiceMutation => breakdown.ambient_process_penalty += 0.05,
            EvidenceKind::BaseImageMatch => breakdown.base_image_penalty = 0.40,
            EvidenceKind::PackageOwner => breakdown.package_reconstructable_penalty = 0.20,
            EvidenceKind::ExplicitUserBinding | EvidenceKind::RestoredFromCommittedBundle => {}
        }
    }

    if kinds.contains(&EvidenceKind::KnownUserStateRoot) {
        breakdown.user_state_prior += 0.10;
    }

    if kinds.iter().all(|k| {
        matches!(
            k,
            EvidenceKind::ReadOnlyDependency | EvidenceKind::NameHeuristic
        )
    }) {
        return (crate::confidence::CONF_DEPENDENCY, breakdown);
    }

    if kinds.iter().all(|k| {
        matches!(
            k,
            EvidenceKind::NameHeuristic | EvidenceKind::SharedServiceMutation
        )
    }) {
        return (crate::confidence::CONF_AMBIENT, breakdown);
    }

    let computed = breakdown.total();
    let locked = if mutation_in_root {
        crate::confidence::CONF_DIRECT_IN_ROOT
    } else if mutation_outside {
        crate::confidence::CONF_DIRECT_OUTSIDE_ROOT
    } else if kinds.contains(&EvidenceKind::RepeatedSessionUse)
        && (kinds.contains(&EvidenceKind::KnownUserStateRoot)
            || kinds.contains(&EvidenceKind::ReconciliationDelta))
    {
        crate::confidence::CONF_REPEATED
    } else if kinds.contains(&EvidenceKind::ProtonPrefix)
        || kinds.contains(&EvidenceKind::WinePrefix)
        || kinds.contains(&EvidenceKind::NameHeuristic)
        || kinds.contains(&EvidenceKind::DesktopEntry)
    {
        crate::confidence::CONF_PLAUSIBLE
    } else if kinds.contains(&EvidenceKind::ReadOnlyDependency) {
        crate::confidence::CONF_DEPENDENCY
    } else {
        crate::confidence::CONF_AMBIENT
    };

    // Locked mapping is the source of truth; computed breakdown remains inspectable.
    let confidence = if computed >= locked {
        locked.max(computed.min(locked + 0.04))
    } else {
        locked
    };
    (confidence.clamp(0.0, 1.0), breakdown)
}
