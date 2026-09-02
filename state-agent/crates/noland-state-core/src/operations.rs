use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::classify::{BackupMode, RestoreMode};
use crate::identity::AppId;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum BackupState {
    Queued,
    Discovering,
    Reconciling,
    Snapshotting,
    Hashing,
    Packing,
    Uploading,
    Committing,
    Checkpointing,
    Completed,
    Failed,
    Cancelled,
}

impl BackupState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "QUEUED",
            Self::Discovering => "DISCOVERING",
            Self::Reconciling => "RECONCILING",
            Self::Snapshotting => "SNAPSHOTTING",
            Self::Hashing => "HASHING",
            Self::Packing => "PACKING",
            Self::Uploading => "UPLOADING",
            Self::Committing => "COMMITTING",
            Self::Checkpointing => "CHECKPOINTING",
            Self::Completed => "COMPLETED",
            Self::Failed => "FAILED",
            Self::Cancelled => "CANCELLED",
        }
    }

    pub fn parse(raw: &str) -> Self {
        match raw {
            "DISCOVERING" => Self::Discovering,
            "RECONCILING" => Self::Reconciling,
            "SNAPSHOTTING" => Self::Snapshotting,
            "HASHING" => Self::Hashing,
            "PACKING" => Self::Packing,
            "UPLOADING" => Self::Uploading,
            "COMMITTING" => Self::Committing,
            "CHECKPOINTING" => Self::Checkpointing,
            "COMPLETED" => Self::Completed,
            "FAILED" => Self::Failed,
            "CANCELLED" => Self::Cancelled,
            _ => Self::Queued,
        }
    }

    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Failed | Self::Cancelled)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum RestoreState {
    Queued,
    FetchingManifest,
    CheckingPrerequisites,
    Downloading,
    Verifying,
    Materializing,
    WaitingForQuiesce,
    CreatingRollbackPoint,
    Applying,
    Validating,
    Completed,
    RollingBack,
    RolledBack,
    Failed,
    Cancelled,
}

impl RestoreState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "QUEUED",
            Self::FetchingManifest => "FETCHING_MANIFEST",
            Self::CheckingPrerequisites => "CHECKING_PREREQUISITES",
            Self::Downloading => "DOWNLOADING",
            Self::Verifying => "VERIFYING",
            Self::Materializing => "MATERIALIZING",
            Self::WaitingForQuiesce => "WAITING_FOR_QUIESCE",
            Self::CreatingRollbackPoint => "CREATING_ROLLBACK_POINT",
            Self::Applying => "APPLYING",
            Self::Validating => "VALIDATING",
            Self::Completed => "COMPLETED",
            Self::RollingBack => "ROLLING_BACK",
            Self::RolledBack => "ROLLED_BACK",
            Self::Failed => "FAILED",
            Self::Cancelled => "CANCELLED",
        }
    }

    pub fn parse(raw: &str) -> Self {
        match raw {
            "FETCHING_MANIFEST" => Self::FetchingManifest,
            "CHECKING_PREREQUISITES" => Self::CheckingPrerequisites,
            "DOWNLOADING" => Self::Downloading,
            "VERIFYING" => Self::Verifying,
            "MATERIALIZING" => Self::Materializing,
            "WAITING_FOR_QUIESCE" => Self::WaitingForQuiesce,
            "CREATING_ROLLBACK_POINT" => Self::CreatingRollbackPoint,
            "APPLYING" => Self::Applying,
            "VALIDATING" => Self::Validating,
            "COMPLETED" => Self::Completed,
            "ROLLING_BACK" => Self::RollingBack,
            "ROLLED_BACK" => Self::RolledBack,
            "FAILED" => Self::Failed,
            "CANCELLED" => Self::Cancelled,
            _ => Self::Queued,
        }
    }

    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Completed | Self::RolledBack | Self::Failed | Self::Cancelled
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum SealState {
    Requested,
    Reconciling,
    BackingUpDirtyApps,
    Checkpointing,
    UploadingSeal,
    CommittingSeal,
    Sealed,
    Failed,
}

impl SealState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Requested => "REQUESTED",
            Self::Reconciling => "RECONCILING",
            Self::BackingUpDirtyApps => "BACKING_UP_DIRTY_APPS",
            Self::Checkpointing => "CHECKPOINTING",
            Self::UploadingSeal => "UPLOADING_SEAL",
            Self::CommittingSeal => "COMMITTING_SEAL",
            Self::Sealed => "SEALED",
            Self::Failed => "FAILED",
        }
    }

    pub fn parse(raw: &str) -> Self {
        match raw {
            "RECONCILING" => Self::Reconciling,
            "BACKING_UP_DIRTY_APPS" => Self::BackingUpDirtyApps,
            "CHECKPOINTING" => Self::Checkpointing,
            "UPLOADING_SEAL" => Self::UploadingSeal,
            "COMMITTING_SEAL" => Self::CommittingSeal,
            "SEALED" => Self::Sealed,
            "FAILED" => Self::Failed,
            _ => Self::Requested,
        }
    }

    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Sealed | Self::Failed)
    }

    pub fn allows_automatic_delete(self) -> bool {
        self == Self::Sealed
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeletionKind {
    Safe,
    Force,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct OperationMetrics {
    pub total_duration_ms: u64,
    pub discovery_duration_ms: u64,
    pub reconciliation_duration_ms: u64,
    pub snapshot_duration_ms: u64,
    pub planning_duration_ms: u64,
    pub hashing_duration_ms: u64,
    pub chunking_duration_ms: u64,
    pub packing_duration_ms: u64,
    pub upload_duration_ms: u64,
    pub download_duration_ms: u64,
    pub manifest_duration_ms: u64,
    pub commit_duration_ms: u64,
    pub checkpoint_duration_ms: u64,
    pub restore_materialize_duration_ms: u64,
    pub restore_apply_duration_ms: u64,
    pub validation_duration_ms: u64,

    pub num_candidate_paths: u64,
    pub num_dirty_paths: u64,
    pub num_dirty_roots: u64,
    pub num_files_scanned: u64,
    pub num_files_skipped_fast_identity: u64,
    pub num_files_rehashed: u64,
    pub num_files_uploaded: u64,
    pub num_files_downloaded: u64,
    pub num_files_reused_local: u64,

    pub bytes_scanned: u64,
    pub bytes_hashed: u64,
    pub bytes_chunked: u64,
    pub bytes_packed: u64,
    pub bytes_uploaded: u64,
    pub bytes_downloaded: u64,
    pub bytes_reused_local: u64,

    pub num_chunks_created: u64,
    pub num_chunks_reused: u64,
    pub num_small_files_packed: u64,

    pub num_rclone_invocations: u64,
    pub num_remote_stat_calls: u64,
    pub num_remote_list_calls: u64,
    pub num_remote_mkdir_calls: u64,
    pub num_remote_upload_calls: u64,
    pub num_remote_download_calls: u64,
    pub num_manifest_writes: u64,

    pub num_local_cas_hits: u64,
    pub num_remote_index_hits: u64,
    pub num_remote_unknown_objects: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OperationProgress {
    pub phase: String,
    pub completed_units: u64,
    pub total_units: Option<u64>,
    pub unit: Option<String>,
    pub message: Option<String>,
    #[serde(default)]
    pub detail_json: serde_json::Value,
    pub updated_at: DateTime<Utc>,
}

impl OperationProgress {
    pub fn new(phase: impl Into<String>, completed_units: u64) -> Self {
        Self {
            phase: phase.into(),
            completed_units,
            total_units: None,
            unit: None,
            message: None,
            detail_json: serde_json::json!({}),
            updated_at: Utc::now(),
        }
    }

    pub fn fraction(&self) -> Option<f64> {
        self.total_units
            .filter(|total| *total > 0)
            .map(|total| (self.completed_units as f64 / total as f64).clamp(0.0, 1.0))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OperationRecord {
    pub operation_id: Uuid,
    pub kind: String,
    pub app_id: Option<AppId>,
    pub state: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub last_error: Option<String>,
    pub detail_json: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BackupRequest {
    pub app_id: AppId,
    pub mode: BackupMode,
    pub force: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RestoreRequest {
    pub app_id: AppId,
    pub bundle_id: Uuid,
    pub commit_id: Option<Uuid>,
    pub mode: RestoreMode,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SealRecord {
    pub seal_id: Uuid,
    pub instance_id: Uuid,
    pub image_id: String,
    pub sealed_at: DateTime<Utc>,
    pub app_bundle_commits: Vec<SealAppCommit>,
    pub checkpoint_id: Option<Uuid>,
    pub state: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SealAppCommit {
    pub app_id: AppId,
    pub bundle_id: Uuid,
    pub commit_id: Uuid,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum CommitVisibility {
    Pending,
    Uploading,
    Committed,
    Failed,
}

impl CommitVisibility {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "PENDING",
            Self::Uploading => "UPLOADING",
            Self::Committed => "COMMITTED",
            Self::Failed => "FAILED",
        }
    }

    pub fn parse(raw: &str) -> Self {
        match raw {
            "UPLOADING" => Self::Uploading,
            "COMMITTED" => Self::Committed,
            "FAILED" => Self::Failed,
            _ => Self::Pending,
        }
    }

    pub fn is_visible(self) -> bool {
        self == Self::Committed
    }
}
