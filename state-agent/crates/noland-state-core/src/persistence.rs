use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{AppId, Evidence, FileType};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum AppMutationKind {
    Create,
    Modify,
    Delete,
    Rename,
    Metadata,
}

impl AppMutationKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Create => "CREATE",
            Self::Modify => "MODIFY",
            Self::Delete => "DELETE",
            Self::Rename => "RENAME",
            Self::Metadata => "METADATA",
        }
    }

    pub fn parse(raw: &str) -> Self {
        match raw {
            "CREATE" => Self::Create,
            "DELETE" => Self::Delete,
            "RENAME" => Self::Rename,
            "METADATA" => Self::Metadata,
            _ => Self::Modify,
        }
    }
}

/// Durable, idempotently appendable mutation observation.
///
/// `previous_path` is populated for renames. `session_id` identifies the
/// originating app session when known, while `provenance` can retain the
/// richer attribution evidence independently.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppMutationRecord {
    pub mutation_id: Uuid,
    pub app_id: AppId,
    pub path: String,
    pub previous_path: Option<String>,
    pub kind: AppMutationKind,
    pub observed_at: DateTime<Utc>,
    pub session_id: Option<Uuid>,
    pub provenance: Option<Evidence>,
    pub processed_at: Option<DateTime<Utc>>,
}

impl AppMutationRecord {
    pub fn new(app_id: AppId, path: impl Into<String>, kind: AppMutationKind) -> Self {
        Self {
            mutation_id: Uuid::new_v4(),
            app_id,
            path: path.into(),
            previous_path: None,
            kind,
            observed_at: Utc::now(),
            session_id: None,
            provenance: None,
            processed_at: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DirtyRootRecord {
    pub app_id: AppId,
    pub canonical_root: String,
    pub logical_root: Option<String>,
    pub first_dirty_at: DateTime<Utc>,
    pub last_dirty_at: DateTime<Utc>,
    pub mutation_count: u64,
    pub requires_reconciliation: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum FileStateTrust {
    /// The content hash is reusable only while the stored fast identity matches.
    Trusted,
    /// Metadata is known, but content must be verified before hash reuse.
    VerifyRequired,
    /// A mutation invalidated the stored identity and content hash.
    Dirty,
    /// The path was observed absent and may produce a tombstone.
    Missing,
}

impl FileStateTrust {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Trusted => "TRUSTED",
            Self::VerifyRequired => "VERIFY_REQUIRED",
            Self::Dirty => "DIRTY",
            Self::Missing => "MISSING",
        }
    }

    pub fn parse(raw: &str) -> Self {
        match raw {
            "TRUSTED" => Self::Trusted,
            "DIRTY" => Self::Dirty,
            "MISSING" => Self::Missing,
            _ => Self::VerifyRequired,
        }
    }
}

/// Persistent fast-identity and content-hash cache keyed by portable app path.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileStateRecord {
    pub app_id: AppId,
    pub logical_root: String,
    pub relative_path: String,
    pub canonical_path: Option<String>,
    pub file_type: FileType,
    pub size: u64,
    pub mtime_ns: i64,
    pub inode: Option<u64>,
    pub mount_id: Option<u64>,
    pub mode: Option<u32>,
    pub content_hash: Option<String>,
    pub trust: FileStateTrust,
    pub last_seen_at: DateTime<Utc>,
    pub last_hashed_at: Option<DateTime<Utc>>,
}

impl FileStateRecord {
    pub fn fast_identity_matches(
        &self,
        size: u64,
        mtime_ns: i64,
        inode: Option<u64>,
        mount_id: Option<u64>,
    ) -> bool {
        self.trust == FileStateTrust::Trusted
            && self.content_hash.is_some()
            && self.size == size
            && self.mtime_ns == mtime_ns
            && self.inode == inode
            && self.mount_id == mount_id
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ContentObjectKind {
    File,
    Chunk,
    Pack,
    Manifest,
    Other,
}

impl ContentObjectKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::File => "FILE",
            Self::Chunk => "CHUNK",
            Self::Pack => "PACK",
            Self::Manifest => "MANIFEST",
            Self::Other => "OTHER",
        }
    }

    pub fn parse(raw: &str) -> Self {
        match raw {
            "FILE" => Self::File,
            "CHUNK" => Self::Chunk,
            "PACK" => Self::Pack,
            "MANIFEST" => Self::Manifest,
            _ => Self::Other,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LocalCasEntry {
    pub object_kind: ContentObjectKind,
    pub content_hash: String,
    pub local_path: String,
    pub size: u64,
    pub created_at: DateTime<Utc>,
    pub verified_at: Option<DateTime<Utc>>,
    pub last_accessed_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum RemoteContentState {
    Present,
    Missing,
    Unknown,
}

impl RemoteContentState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Present => "PRESENT",
            Self::Missing => "MISSING",
            Self::Unknown => "UNKNOWN",
        }
    }

    pub fn parse(raw: &str) -> Self {
        match raw {
            "PRESENT" => Self::Present,
            "MISSING" => Self::Missing,
            _ => Self::Unknown,
        }
    }
}

/// Cached observation for one content-addressed object in one remote namespace.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RemoteContentEntry {
    pub storage_id: String,
    pub object_kind: ContentObjectKind,
    pub content_hash: String,
    pub remote_path: String,
    pub size: Option<u64>,
    pub etag: Option<String>,
    pub state: RemoteContentState,
    pub observed_at: DateTime<Utc>,
    pub expires_at: Option<DateTime<Utc>>,
}

impl RemoteContentEntry {
    pub fn is_fresh_at(&self, now: DateTime<Utc>) -> bool {
        self.expires_at.is_none_or(|expires_at| expires_at > now)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum SyncDirection {
    Upload,
    Download,
}

impl SyncDirection {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Upload => "UPLOAD",
            Self::Download => "DOWNLOAD",
        }
    }

    pub fn parse(raw: &str) -> Self {
        match raw {
            "DOWNLOAD" => Self::Download,
            _ => Self::Upload,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum SyncJournalState {
    Pending,
    InProgress,
    RetryScheduled,
    Completed,
    Failed,
    Skipped,
}

impl SyncJournalState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "PENDING",
            Self::InProgress => "IN_PROGRESS",
            Self::RetryScheduled => "RETRY_SCHEDULED",
            Self::Completed => "COMPLETED",
            Self::Failed => "FAILED",
            Self::Skipped => "SKIPPED",
        }
    }

    pub fn parse(raw: &str) -> Self {
        match raw {
            "IN_PROGRESS" => Self::InProgress,
            "RETRY_SCHEDULED" => Self::RetryScheduled,
            "COMPLETED" => Self::Completed,
            "FAILED" => Self::Failed,
            "SKIPPED" => Self::Skipped,
            _ => Self::Pending,
        }
    }

    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Failed | Self::Skipped)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncJournalEntry {
    pub operation_id: Uuid,
    pub item_key: String,
    pub item_kind: ContentObjectKind,
    pub direction: SyncDirection,
    pub state: SyncJournalState,
    pub local_path: Option<String>,
    pub remote_path: Option<String>,
    pub content_hash: Option<String>,
    pub size: Option<u64>,
    pub attempts: u32,
    pub bytes_transferred: u64,
    pub last_error: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub started_at: Option<DateTime<Utc>>,
    pub completed_at: Option<DateTime<Utc>>,
    pub next_retry_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub detail_json: serde_json::Value,
}

impl SyncJournalEntry {
    pub fn pending(
        operation_id: Uuid,
        item_key: impl Into<String>,
        item_kind: ContentObjectKind,
        direction: SyncDirection,
    ) -> Self {
        let now = Utc::now();
        Self {
            operation_id,
            item_key: item_key.into(),
            item_kind,
            direction,
            state: SyncJournalState::Pending,
            local_path: None,
            remote_path: None,
            content_hash: None,
            size: None,
            attempts: 0,
            bytes_transferred: 0,
            last_error: None,
            created_at: now,
            updated_at: now,
            started_at: None,
            completed_at: None,
            next_retry_at: None,
            detail_json: serde_json::json!({}),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyncJournalSummary {
    pub total_items: u64,
    pub pending_items: u64,
    pub in_progress_items: u64,
    pub retry_scheduled_items: u64,
    pub completed_items: u64,
    pub failed_items: u64,
    pub skipped_items: u64,
    pub total_bytes: u64,
    pub bytes_transferred: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trusted_file_state_requires_an_exact_fast_identity() {
        let state = FileStateRecord {
            app_id: AppId::steam(42),
            logical_root: "$HOME".into(),
            relative_path: "save.dat".into(),
            canonical_path: Some("/home/user/save.dat".into()),
            file_type: FileType::File,
            size: 12,
            mtime_ns: 34,
            inode: Some(56),
            mount_id: Some(78),
            mode: Some(0o600),
            content_hash: Some("hash".into()),
            trust: FileStateTrust::Trusted,
            last_seen_at: Utc::now(),
            last_hashed_at: Some(Utc::now()),
        };

        assert!(state.fast_identity_matches(12, 34, Some(56), Some(78)));
        assert!(!state.fast_identity_matches(13, 34, Some(56), Some(78)));

        let mut dirty = state;
        dirty.trust = FileStateTrust::Dirty;
        assert!(!dirty.fast_identity_matches(12, 34, Some(56), Some(78)));
    }
}
