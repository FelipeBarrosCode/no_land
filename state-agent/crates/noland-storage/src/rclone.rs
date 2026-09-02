use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use bytes::Bytes;
use noland_rclone_adapter::{
    classify_remote_error, EphemeralRcloneSession, ProviderRootIdentity, RemoteErrorClass,
    TransferTuning,
};
use noland_state_core::{Result, StateError};
use tokio::process::Command;
use tokio::sync::OnceCell;

use crate::{
    compare_remote_known, forbid_rclone_sync, transfer::validate_remote_key, Health,
    ImmutableUpload, MetadataBatch, MetadataWrite, RemoteEntry, RemoteKey, RemoteMeta,
    SharedRetryGate, SharedStorageProvider, StorageOperationMetrics,
};

#[derive(Default)]
struct StorageCounters {
    rclone_invocations: AtomicU64,
    remote_stat_calls: AtomicU64,
    remote_list_calls: AtomicU64,
    remote_mkdir_calls: AtomicU64,
    remote_upload_calls: AtomicU64,
    remote_download_calls: AtomicU64,
    bytes_uploaded: AtomicU64,
    bytes_downloaded: AtomicU64,
}

impl StorageCounters {
    fn snapshot(&self) -> StorageOperationMetrics {
        StorageOperationMetrics {
            rclone_invocations: self.rclone_invocations.load(Ordering::Relaxed),
            remote_stat_calls: self.remote_stat_calls.load(Ordering::Relaxed),
            remote_list_calls: self.remote_list_calls.load(Ordering::Relaxed),
            remote_mkdir_calls: self.remote_mkdir_calls.load(Ordering::Relaxed),
            remote_upload_calls: self.remote_upload_calls.load(Ordering::Relaxed),
            remote_download_calls: self.remote_download_calls.load(Ordering::Relaxed),
            bytes_uploaded: self.bytes_uploaded.load(Ordering::Relaxed),
            bytes_downloaded: self.bytes_downloaded.load(Ordering::Relaxed),
        }
    }
}

struct RcloneFailure {
    class: RemoteErrorClass,
    message: String,
}

impl RcloneFailure {
    fn into_state_error(self) -> StateError {
        StateError::Storage(format!(
            "rclone failed ({:?}): {}",
            self.class, self.message
        ))
    }
}

/// rclone-backed provider. Backup transfers use `copy` / `copyto` only.
/// Provider-specific remotes are created by `noland-rclone-adapter`.
pub struct RcloneStorage {
    pub remote: String,
    pub root: String,
    pub extra_args: Vec<String>,
    pub backend_type: String,
    pub provider: String,
    tuning: TransferTuning,
    retry_gate: Arc<SharedRetryGate>,
    root_ready: OnceCell<()>,
    metrics: StorageCounters,
}

impl RcloneStorage {
    pub fn new(remote: impl Into<String>, root: impl Into<String>) -> Self {
        let tuning = TransferTuning::default();
        Self {
            remote: remote.into(),
            root: root.into(),
            extra_args: Vec::new(),
            backend_type: "unknown".into(),
            provider: "unknown".into(),
            retry_gate: Arc::new(SharedRetryGate::new(tuning.clone())),
            tuning,
            root_ready: OnceCell::new(),
            metrics: StorageCounters::default(),
        }
    }

    pub fn from_session(session: &EphemeralRcloneSession, config_path: &Path) -> Self {
        let tuning = TransferTuning::default();
        Self {
            remote: session.remote_name.clone(),
            root: session.root.clone(),
            backend_type: session.backend_type.clone(),
            provider: session.provider.clone(),
            extra_args: vec!["--config".into(), config_path.display().to_string()],
            retry_gate: Arc::new(SharedRetryGate::new(tuning.clone())),
            tuning,
            root_ready: OnceCell::new(),
            metrics: StorageCounters::default(),
        }
    }

    pub fn with_extra_args(mut self, args: Vec<String>) -> Self {
        self.extra_args.extend(args);
        self
    }

    pub fn with_transfer_tuning(mut self, tuning: TransferTuning) -> Self {
        let tuning = tuning.normalized();
        self.retry_gate = Arc::new(SharedRetryGate::new(tuning.clone()));
        self.tuning = tuning;
        self
    }

    pub fn transfer_tuning(&self) -> &TransferTuning {
        &self.tuning
    }

    pub fn root_cache_key(&self) -> String {
        self.identity().cache_key()
    }

    fn identity(&self) -> ProviderRootIdentity {
        ProviderRootIdentity::new(&self.provider, &self.backend_type, &self.remote, &self.root)
    }

    pub fn provider_label(&self) -> String {
        format!("rclone:{}", self.backend_type)
    }

    pub fn remote_path(&self, key: &RemoteKey) -> String {
        format!(
            "{}:{}/{}",
            self.remote,
            self.root.trim_end_matches('/'),
            key.as_str().trim_start_matches('/')
        )
    }

    fn root_remote(&self) -> String {
        if self.root.trim().is_empty() {
            format!("{}:", self.remote)
        } else {
            format!("{}:{}", self.remote, self.root.trim_end_matches('/'))
        }
    }

    async fn run(&self, args: Vec<String>) -> Result<String> {
        self.run_classified(args)
            .await
            .map_err(RcloneFailure::into_state_error)
    }

    async fn run_classified(
        &self,
        args: Vec<String>,
    ) -> std::result::Result<String, RcloneFailure> {
        forbid_rclone_sync(&args).map_err(|error| RcloneFailure {
            class: RemoteErrorClass::Permanent,
            message: error.to_string(),
        })?;
        let max_attempts = self.tuning.max_attempts.max(1);
        for attempt in 1..=max_attempts {
            self.retry_gate.wait_for_turn().await;
            self.metrics
                .rclone_invocations
                .fetch_add(1, Ordering::Relaxed);
            if args.first().map(String::as_str) == Some("mkdir") {
                self.metrics
                    .remote_mkdir_calls
                    .fetch_add(1, Ordering::Relaxed);
            }
            let mut cmd = Command::new("rclone");
            cmd.args(&self.extra_args)
                .args(&args)
                .stdout(Stdio::piped())
                .stderr(Stdio::piped());
            let output = match cmd.output().await {
                Ok(output) => output,
                Err(error) => {
                    let failure = RcloneFailure {
                        class: classify_remote_error(None, &error.to_string()),
                        message: error.to_string(),
                    };
                    if failure.class.is_retryable() && attempt < max_attempts {
                        self.retry_gate.wait_for_retry(attempt, failure.class).await;
                        continue;
                    }
                    return Err(failure);
                }
            };
            if output.status.success() {
                return Ok(String::from_utf8_lossy(&output.stdout).into_owned());
            }
            let message = String::from_utf8_lossy(&output.stderr).trim().to_string();
            let failure = RcloneFailure {
                class: classify_remote_error(output.status.code(), &message),
                message,
            };
            if failure.class.is_retryable() && attempt < max_attempts {
                self.retry_gate.wait_for_retry(attempt, failure.class).await;
                continue;
            }
            return Err(failure);
        }
        unreachable!("rclone retry count is always at least one")
    }

    async fn compare_bulk_known(
        &self,
        uploads: &[ImmutableUpload],
    ) -> Result<crate::RemoteKnownComparison> {
        let mut groups: HashMap<String, Vec<ImmutableUpload>> = HashMap::new();
        for upload in uploads {
            validate_remote_key(&upload.key)?;
            let prefix = upload
                .key
                .as_str()
                .split_once('/')
                .map(|(prefix, _)| prefix)
                .unwrap_or(upload.key.as_str());
            groups
                .entry(prefix.to_string())
                .or_default()
                .push(upload.clone());
        }

        let mut combined = crate::RemoteKnownComparison::default();
        for (prefix, candidates) in groups {
            if candidates.len() == 1 && candidates[0].key.as_str() == prefix {
                let candidate = &candidates[0];
                let local_size = std::fs::metadata(&candidate.local)?.len();
                match self.stat(&candidate.key).await? {
                    Some(remote) if remote.size == local_size => combined.known.push(remote),
                    Some(remote) => combined.size_conflicts.push(crate::RemoteSizeConflict {
                        key: candidate.key.clone(),
                        local_size,
                        remote_size: remote.size,
                    }),
                    None => combined.missing.push(candidate.clone()),
                }
            } else {
                let comparison =
                    compare_remote_known(self, &RemoteKey::new(prefix), &candidates).await?;
                combined.known.extend(comparison.known);
                combined.missing.extend(comparison.missing);
                combined.size_conflicts.extend(comparison.size_conflicts);
            }
        }
        Ok(combined)
    }

    async fn copy_staged(&self, stage: &StagingTree, immutable: bool) -> Result<()> {
        self.run(staged_copy_args(
            &self.tuning,
            stage.root.display().to_string(),
            self.root_remote(),
            immutable,
        ))
        .await
        .map(|_| ())
    }

    async fn put_metadata_entries(&self, entries: &[MetadataWrite]) -> Result<Vec<RemoteMeta>> {
        if entries.is_empty() {
            return Ok(Vec::new());
        }
        let mut output = Vec::with_capacity(entries.len());
        let tuning = self.tuning.normalized();
        let mut offset = 0;
        while offset < entries.len() {
            let end = (offset + tuning.max_bulk_files).min(entries.len());
            let chunk = &entries[offset..end];
            let stage = StagingTree::new("metadata")?;
            for entry in chunk {
                validate_remote_key(&entry.key)?;
                stage.write(&entry.key, &entry.bytes)?;
            }
            self.copy_staged(&stage, false).await?;
            for entry in chunk {
                self.metrics
                    .bytes_uploaded
                    .fetch_add(entry.bytes.len() as u64, Ordering::Relaxed);
                output.push(RemoteMeta {
                    key: entry.key.clone(),
                    size: entry.bytes.len() as u64,
                });
            }
            offset = end;
        }
        Ok(output)
    }
}

fn staged_copy_args(
    tuning: &TransferTuning,
    source: String,
    destination: String,
    immutable: bool,
) -> Vec<String> {
    let mut args = vec!["copy".into()];
    if immutable {
        args.push("--immutable".into());
    }
    args.extend([
        "--no-traverse".into(),
        "--transfers".into(),
        tuning.rclone_transfers.max(1).to_string(),
        "--checkers".into(),
        tuning.rclone_checkers.max(1).to_string(),
        source,
        destination,
    ]);
    args
}

struct StagingTree {
    root: PathBuf,
}

impl StagingTree {
    fn new(label: &str) -> Result<Self> {
        Self::create_under(&std::env::temp_dir(), label)
    }

    fn near(label: &str, source: &Path) -> Result<Self> {
        if let Some(parent) = source.parent() {
            if let Ok(stage) = Self::create_under(parent, label) {
                return Ok(stage);
            }
        }
        Self::new(label)
    }

    fn create_under(parent: &Path, label: &str) -> Result<Self> {
        let root = parent.join(format!(".noland-rclone-{label}-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root)?;
        Ok(Self { root })
    }

    fn path_for(&self, key: &RemoteKey) -> Result<PathBuf> {
        validate_remote_key(key)?;
        Ok(self.root.join(key.as_str()))
    }

    fn link_or_copy(&self, source: &Path, key: &RemoteKey) -> Result<()> {
        let destination = self.path_for(key)?;
        if let Some(parent) = destination.parent() {
            std::fs::create_dir_all(parent)?;
        }
        if std::fs::hard_link(source, &destination).is_err() {
            std::fs::copy(source, destination)?;
        }
        Ok(())
    }

    fn write(&self, key: &RemoteKey, bytes: &[u8]) -> Result<()> {
        let destination = self.path_for(key)?;
        if let Some(parent) = destination.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(destination, bytes)?;
        Ok(())
    }
}

impl Drop for StagingTree {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

#[async_trait]
impl SharedStorageProvider for RcloneStorage {
    async fn health_check(&self) -> Result<Health> {
        match self.run(vec!["lsd".into(), self.root_remote()]).await {
            Ok(detail) => Ok(Health {
                ok: true,
                provider: self.provider_label(),
                detail,
            }),
            Err(err) => Ok(Health {
                ok: false,
                provider: self.provider_label(),
                detail: err.to_string(),
            }),
        }
    }

    async fn ensure_root(&self) -> Result<()> {
        self.root_ready
            .get_or_try_init(|| async {
                self.run(vec!["mkdir".into(), self.root_remote()]).await?;
                for child in ["catalog", "bundles", "packs", "checkpoints", "instances"] {
                    let _ = self
                        .run(vec![
                            "mkdir".into(),
                            format!("{}/{}", self.root_remote(), child),
                        ])
                        .await;
                }
                Ok::<(), StateError>(())
            })
            .await?;
        Ok(())
    }

    async fn stat(&self, key: &RemoteKey) -> Result<Option<RemoteMeta>> {
        self.metrics
            .remote_stat_calls
            .fetch_add(1, Ordering::Relaxed);
        match self
            .run_classified(vec![
                "lsf".into(),
                "--format".into(),
                "s".into(),
                self.remote_path(key),
            ])
            .await
        {
            Ok(out) => {
                let size = out
                    .lines()
                    .next()
                    .and_then(|s| s.trim().parse().ok())
                    .unwrap_or(0);
                if out.trim().is_empty() {
                    Ok(None)
                } else {
                    Ok(Some(RemoteMeta {
                        key: key.clone(),
                        size,
                    }))
                }
            }
            Err(failure) if failure.class == RemoteErrorClass::NotFound => Ok(None),
            Err(failure) => Err(failure.into_state_error()),
        }
    }

    async fn upload_immutable(&self, local: &Path, key: &RemoteKey) -> Result<RemoteMeta> {
        let size = std::fs::metadata(local)?.len();
        self.metrics
            .remote_upload_calls
            .fetch_add(1, Ordering::Relaxed);
        self.run(vec![
            "copyto".into(),
            "--immutable".into(),
            local.display().to_string(),
            self.remote_path(key),
        ])
        .await?;
        self.metrics
            .bytes_uploaded
            .fetch_add(size, Ordering::Relaxed);
        Ok(RemoteMeta {
            key: key.clone(),
            size,
        })
    }

    async fn upload_immutable_bulk(&self, uploads: &[ImmutableUpload]) -> Result<Vec<RemoteMeta>> {
        if uploads.is_empty() {
            return Ok(Vec::new());
        }
        self.metrics
            .remote_upload_calls
            .fetch_add(uploads.len() as u64, Ordering::Relaxed);
        let comparison = self.compare_bulk_known(uploads).await?;
        if !comparison.size_conflicts.is_empty() {
            let keys = comparison
                .size_conflicts
                .iter()
                .map(|conflict| {
                    format!(
                        "{} (local {}, remote {})",
                        conflict.key.as_str(),
                        conflict.local_size,
                        conflict.remote_size
                    )
                })
                .collect::<Vec<_>>()
                .join(", ");
            return Err(StateError::Conflict(format!(
                "immutable remote objects differ: {keys}"
            )));
        }

        let tuning = self.tuning.normalized();
        let mut uploaded_by_key: HashMap<RemoteKey, RemoteMeta> = comparison
            .known
            .into_iter()
            .map(|meta| (meta.key.clone(), meta))
            .collect();
        let mut offset = 0;
        while offset < comparison.missing.len() {
            let mut end = offset;
            let mut bytes = 0u64;
            while end < comparison.missing.len() && end - offset < tuning.max_bulk_files {
                let size = std::fs::metadata(&comparison.missing[end].local)?.len();
                if end > offset && bytes.saturating_add(size) > tuning.max_bulk_bytes {
                    break;
                }
                bytes = bytes.saturating_add(size);
                end += 1;
            }
            let stage = StagingTree::near("immutable", &comparison.missing[offset].local)?;
            for upload in &comparison.missing[offset..end] {
                stage.link_or_copy(&upload.local, &upload.key)?;
            }
            self.copy_staged(&stage, true).await?;
            self.metrics
                .bytes_uploaded
                .fetch_add(bytes, Ordering::Relaxed);
            for upload in &comparison.missing[offset..end] {
                let meta = RemoteMeta {
                    key: upload.key.clone(),
                    size: std::fs::metadata(&upload.local)?.len(),
                };
                uploaded_by_key.insert(meta.key.clone(), meta);
            }
            offset = end;
        }

        uploads
            .iter()
            .map(|upload| {
                uploaded_by_key.remove(&upload.key).ok_or_else(|| {
                    StateError::Storage(format!(
                        "bulk upload produced no result for {}",
                        upload.key.as_str()
                    ))
                })
            })
            .collect()
    }

    async fn download(&self, key: &RemoteKey, dest: &Path) -> Result<()> {
        self.metrics
            .remote_download_calls
            .fetch_add(1, Ordering::Relaxed);
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)?;
        }
        self.run(vec![
            "copyto".into(),
            self.remote_path(key),
            dest.display().to_string(),
        ])
        .await?;
        if let Ok(metadata) = std::fs::metadata(dest) {
            self.metrics
                .bytes_downloaded
                .fetch_add(metadata.len(), Ordering::Relaxed);
        }
        Ok(())
    }

    async fn list_prefix(&self, prefix: &RemoteKey) -> Result<Vec<RemoteEntry>> {
        self.metrics
            .remote_list_calls
            .fetch_add(1, Ordering::Relaxed);
        let out = match self
            .run_classified(vec![
                "lsf".into(),
                "-R".into(),
                "--format".into(),
                "ps".into(),
                self.remote_path(prefix),
            ])
            .await
        {
            Ok(out) => out,
            Err(failure) if failure.class == RemoteErrorClass::NotFound => String::new(),
            Err(failure) => return Err(failure.into_state_error()),
        };
        let mut entries = Vec::new();
        for line in out.lines() {
            let mut parts = line.splitn(2, ';');
            let name = parts.next().unwrap_or_default();
            let size = parts.next().and_then(|s| s.parse().ok()).unwrap_or(0);
            if name.is_empty() {
                continue;
            }
            entries.push(RemoteEntry {
                key: prefix.join(name.trim_end_matches('/')),
                size,
                is_prefix: name.ends_with('/'),
            });
        }
        Ok(entries)
    }

    async fn put_small_versioned(&self, bytes: Bytes, key: &RemoteKey) -> Result<RemoteMeta> {
        self.metrics
            .remote_upload_calls
            .fetch_add(1, Ordering::Relaxed);
        let tmp = std::env::temp_dir().join(format!(
            "noland-rclone-{}-{}",
            uuid::Uuid::new_v4(),
            key.as_str().replace('/', "_")
        ));
        std::fs::write(&tmp, &bytes)?;
        let result = self
            .run(vec![
                "copyto".into(),
                tmp.display().to_string(),
                self.remote_path(key),
            ])
            .await
            .map(|_| {
                self.metrics
                    .bytes_uploaded
                    .fetch_add(bytes.len() as u64, Ordering::Relaxed);
                RemoteMeta {
                    key: key.clone(),
                    size: bytes.len() as u64,
                }
            });
        let _ = std::fs::remove_file(tmp);
        result
    }

    async fn put_metadata_batch(&self, batch: &MetadataBatch) -> Result<Vec<RemoteMeta>> {
        self.metrics
            .remote_upload_calls
            .fetch_add(batch.total_len() as u64, Ordering::Relaxed);
        let mut written = self.put_metadata_entries(batch.entries()).await?;
        if let Some(committed) = batch.committed() {
            // This separate invocation is the visibility boundary and must remain last.
            written.extend(
                self.put_metadata_entries(std::slice::from_ref(committed))
                    .await?,
            );
        }
        Ok(written)
    }

    fn storage_identity(&self) -> Option<ProviderRootIdentity> {
        Some(self.identity())
    }

    fn operation_metrics(&self) -> StorageOperationMetrics {
        self.metrics.snapshot()
    }
}

pub struct EphemeralSessionGuard {
    run_root: PathBuf,
    operation_id: String,
}

impl Drop for EphemeralSessionGuard {
    fn drop(&mut self) {
        let _ = shred_ephemeral_session(&self.run_root, &self.operation_id);
    }
}

pub fn write_guarded_ephemeral_session(
    run_root: &Path,
    session: &EphemeralRcloneSession,
) -> Result<(PathBuf, EphemeralSessionGuard)> {
    let config_path = write_ephemeral_session(run_root, session)?;
    Ok((
        config_path,
        EphemeralSessionGuard {
            run_root: run_root.to_path_buf(),
            operation_id: session.operation_id.clone(),
        },
    ))
}

pub fn write_ephemeral_session(
    run_root: &Path,
    session: &EphemeralRcloneSession,
) -> Result<PathBuf> {
    let dir = run_root.join("storage").join(&session.operation_id);
    std::fs::create_dir_all(&dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700));
    }
    let config_path = dir.join("rclone.conf");
    std::fs::write(&config_path, session.config_ini.as_bytes())?;
    let session_path = dir.join("session.json");
    std::fs::write(&session_path, serde_json::to_vec(session)?)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&config_path, std::fs::Permissions::from_mode(0o600));
        let _ = std::fs::set_permissions(&session_path, std::fs::Permissions::from_mode(0o600));
    }
    Ok(config_path)
}

pub fn shred_ephemeral_session(run_root: &Path, operation_id: &str) -> Result<()> {
    crate::shred_ephemeral_auth(run_root, operation_id)
}

pub fn shred_all_ephemeral_sessions(run_root: &Path) -> Result<()> {
    let storage_root = run_root.join("storage");
    if !storage_root.exists() {
        return Ok(());
    }
    for entry in std::fs::read_dir(&storage_root)? {
        let entry = entry?;
        if entry.path().is_dir() {
            let file_name = entry.file_name();
            if let Some(operation_id) = file_name.to_str() {
                shred_ephemeral_session(run_root, operation_id)?;
            }
        } else {
            let _ = std::fs::remove_file(entry.path());
        }
    }
    let _ = std::fs::remove_dir(&storage_root);
    Ok(())
}

#[cfg(test)]
fn assert_copy_only(command: &[String]) -> Result<()> {
    forbid_rclone_sync(command)
}

#[cfg(test)]
mod tests {
    use super::*;
    use noland_rclone_adapter::{
        session_from_input, AdapterCredential, AdapterInput, ProviderKind, TokenMode,
    };
    use std::collections::BTreeMap;

    #[test]
    fn rejects_sync() {
        assert!(assert_copy_only(&["sync".into(), "src".into(), "dst".into()]).is_err());
        assert!(assert_copy_only(&["copy".into(), "src".into(), "dst".into()]).is_ok());
    }

    #[test]
    fn guarded_session_is_shredded_on_drop() {
        let run_root =
            std::env::temp_dir().join(format!("noland-session-{}", uuid::Uuid::new_v4()));
        let input = AdapterInput {
            provider: ProviderKind::Local,
            remote_name: "noland_local".into(),
            credentials: AdapterCredential::LocalPath {
                path: "/tmp/cloud".into(),
            },
            fields: BTreeMap::new(),
            bucket: None,
            prefix: Some("Noland Shared Storage".into()),
        };
        let session = session_from_input(&input, "op-guard", TokenMode::Ephemeral).unwrap();
        let session_dir = run_root.join("storage/op-guard");

        {
            let (config, _guard) = write_guarded_ephemeral_session(&run_root, &session).unwrap();
            assert!(config.is_file());
            assert!(session_dir.is_dir());
        }

        assert!(!session_dir.exists());
        let _ = std::fs::remove_dir_all(run_root);
    }

    #[test]
    fn bulk_copy_args_are_provider_neutral_immutable_and_copy_only() {
        let args = staged_copy_args(
            &TransferTuning::default(),
            "/tmp/stage".into(),
            "remote:root".into(),
            true,
        );
        assert_eq!(args.first().map(String::as_str), Some("copy"));
        assert!(args.iter().any(|arg| arg == "--immutable"));
        assert!(args.iter().any(|arg| arg == "--no-traverse"));
        assert!(assert_copy_only(&args).is_ok());
        assert!(!args.iter().any(|arg| arg.contains("drive")));
    }

    #[test]
    fn from_session_uses_config_flag_and_generic_paths() {
        let input = AdapterInput {
            provider: ProviderKind::Local,
            remote_name: "noland_local".into(),
            credentials: AdapterCredential::LocalPath {
                path: "/tmp/cloud".into(),
            },
            fields: BTreeMap::new(),
            bucket: None,
            prefix: Some("Noland Shared Storage".into()),
        };
        let session = session_from_input(&input, "op-42", TokenMode::Ephemeral).unwrap();
        let storage = RcloneStorage::from_session(
            &session,
            Path::new("/run/noland/storage/op-42/rclone.conf"),
        );
        assert_eq!(storage.provider_label(), "rclone:local");
        assert_eq!(
            storage.remote_path(&RemoteKey::new("packs/ab/id.pack")),
            "noland_local:Noland Shared Storage/packs/ab/id.pack"
        );
        assert!(storage.extra_args.contains(&"--config".to_string()));
        assert_eq!(storage.root_cache_key(), session.root_cache_key());
        assert!(!session.config_ini.contains("sync"));
    }
}
