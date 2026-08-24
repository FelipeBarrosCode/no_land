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
