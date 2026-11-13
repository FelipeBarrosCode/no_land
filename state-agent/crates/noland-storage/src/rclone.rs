use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use crate::stream::{self, PeerRates, StreamPolicy, StreamReadFailure, StreamTransferDiagnostics};
use async_trait::async_trait;
use bytes::Bytes;
use noland_rclone_adapter::{
    classify_remote_error, EphemeralRcloneSession, ProviderCapabilities, ProviderKind,
    ProviderRootIdentity, RcloneRoot, RemoteErrorClass, TransferProfile, TransferTuning,
};
use noland_state_core::{Result, StateError};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, BufReader};
use tokio::process::Command;
use tokio::sync::OnceCell;

pub(crate) mod upload;

use crate::{
    forbid_rclone_sync, transfer::validate_remote_key, AdaptiveConcurrency, DownloadObserver,
    DownloadStreamConsumer, Health, ImmutableUpload, ImmutableUploadCompletion,
    ImmutableUploadObserver, MetadataBatch, MetadataWrite, RemoteEntry, RemoteKey, RemoteMeta,
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

/// Tokio's kill_on_drop prevents detached work, but reaping normally happens
/// later. Keep staging/lease cleanup behind a bounded kill-and-reap on cancelled
/// futures as well. The runtime reaper remains the fallback for an unkillable OS
/// process; no unbounded wait is performed on an executor thread.
struct ReapedChild(tokio::process::Child);

impl std::ops::Deref for ReapedChild {
    type Target = tokio::process::Child;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl std::ops::DerefMut for ReapedChild {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl Drop for ReapedChild {
    fn drop(&mut self) {
        if self.0.try_wait().ok().flatten().is_some() {
            return;
        }
        let _ = self.0.start_kill();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(1);
        while std::time::Instant::now() < deadline {
            match self.0.try_wait() {
                Ok(Some(_)) | Err(_) => return,
                Ok(None) => std::thread::sleep(std::time::Duration::from_millis(2)),
            }
        }
    }
}

async fn read_error_tail(
    mut stream: impl tokio::io::AsyncRead + Unpin,
) -> std::io::Result<Vec<u8>> {
    let mut tail = Vec::new();
    let mut buffer = [0u8; 4096];
    loop {
        let count = stream.read(&mut buffer).await?;
        if count == 0 {
            return Ok(tail);
        }
        tail.extend_from_slice(&buffer[..count]);
        if tail.len() > 16 * 1024 {
            tail.drain(..tail.len() - 16 * 1024);
        }
    }
}

async fn drain_stream_errors(
    stderr: impl tokio::io::AsyncRead + Unpin,
    done: tokio::sync::oneshot::Receiver<std::time::Instant>,
    exit_timeout: std::time::Duration,
) -> (std::io::Result<Vec<u8>>, u64) {
    let tail = read_error_tail(stderr);
    tokio::pin!(tail);
    tokio::select! {
        result = &mut tail => (result, 0),
        ended = done => {
            let ended = ended.unwrap_or_else(|_| std::time::Instant::now());
            let result = tokio::time::timeout(exit_timeout, &mut tail).await
                .unwrap_or_else(|_| Err(std::io::Error::new(std::io::ErrorKind::TimedOut, "rclone stderr did not close after the stream ended")));
            (result, stream::micros(ended.elapsed()))
        }
    }
}

async fn kill_and_reap_for_retry(child: &mut ReapedChild) -> Result<()> {
    let _ = child.start_kill();
    tokio::time::timeout(std::time::Duration::from_secs(5), child.wait())
        .await
        .map_err(|_| {
            StateError::Storage("rclone did not exit; refusing an overlapping retry".into())
        })??;
    Ok(())
}

async fn reaped_output(command: &mut Command) -> std::io::Result<std::process::Output> {
    let mut child = ReapedChild(command.kill_on_drop(true).spawn()?);
    let mut stdout = child.stdout.take().expect("piped rclone stdout");
    let stderr = child.stderr.take().expect("piped rclone stderr");
    let output = async {
        let mut bytes = Vec::new();
        stdout.read_to_end(&mut bytes).await?;
        Ok::<_, std::io::Error>(bytes)
    };
    let (stdout, stderr, status) = tokio::try_join!(output, read_error_tail(stderr), child.wait())?;
    Ok(std::process::Output {
        status,
        stdout,
        stderr,
    })
}

/// Builds safe rclone argument lists from provider-neutral operations and a
/// provider-specific transfer profile.
#[derive(Debug, Clone)]
pub struct RcloneCommandBuilder {
    provider: ProviderKind,
    profile: TransferProfile,
}

impl RcloneCommandBuilder {
    pub fn new(
        provider: ProviderKind,
        backend_type: impl Into<String>,
        profile: TransferProfile,
    ) -> Result<Self> {
        let backend_type = backend_type.into();
        noland_rclone_adapter::validate_provider_backend(provider, &backend_type)
            .map_err(|error| StateError::Invalid(error.to_string()))?;
        if let Some(arg) = profile
            .rclone_backend_args
            .iter()
            .find(|arg| !noland_rclone_adapter::provider_arg_is_allowed(provider, arg))
        {
            return Err(StateError::Invalid(format!(
                "Provider-specific rclone option {arg} cannot be used with {}",
                provider.label(),
            )));
        }
        Ok(Self { provider, profile })
    }

    pub fn upload_copyto_args(
        &self,
        local: String,
        destination: String,
        immutable: bool,
    ) -> Result<Vec<String>> {
        let mut args = vec!["copyto".into()];
        if immutable {
            args.push("--immutable".into());
        }
        self.push_backend_options(&mut args);
        args.extend([local, destination]);
        Ok(args)
    }

    pub fn download_copyto_args(
        &self,
        tuning: &TransferTuning,
        source: String,
        destination: String,
    ) -> Vec<String> {
        let mut args = vec![
            "copyto".into(),
            "--stats".into(),
            "500ms".into(),
            "--stats-one-line".into(),
            "--use-json-log".into(),
            "--multi-thread-streams".into(),
            tuning.rclone_download_streams.max(1).to_string(),
        ];
        self.push_backend_options(&mut args);
        args.extend([source, destination]);
        args
    }

    pub fn download_stream_args(&self, source: String) -> Vec<String> {
        // cat produces an ordered stream; copyto's multistream output may have holes.
        // Parallelism is across packs, with OS pipe backpressure instead of disk copies.
        let mut args = vec!["cat".into()];
        self.push_backend_options(&mut args);
        args.push(source);
        args
    }

    pub fn upload_copy_args(
        &self,
        tuning: &TransferTuning,
        source: String,
        destination: String,
        immutable: bool,
    ) -> Result<Vec<String>> {
        let mut args = vec!["copy".into()];
        if immutable {
            args.push("--immutable".into());
        }
        args.push("--no-traverse".into());
        self.push_backend_options(&mut args);
        args.extend([
            "--transfers".into(),
            self.upload_transfers(tuning).to_string(),
            "--checkers".into(),
            tuning.rclone_checkers.max(1).to_string(),
            "--stats".into(),
            "500ms".into(),
            "--stats-one-line".into(),
            "--use-json-log".into(),
            source,
            destination,
        ]);
        Ok(args)
    }

    pub fn global_args(&self) -> &[String] {
        &self.profile.rclone_global_args
    }

    pub fn profile(&self) -> &TransferProfile {
        &self.profile
    }

    fn upload_transfers(&self, tuning: &TransferTuning) -> usize {
        tuning
            .rclone_transfers
            .max(1)
            .min(tuning.max_parallel_uploads.max(1))
            .min(ProviderCapabilities::for_provider(self.provider).max_upload_concurrency)
    }

    fn push_backend_options(&self, args: &mut Vec<String>) {
        args.extend(self.profile.rclone_backend_args.iter().cloned());
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
    provider_kind: ProviderKind,
    capabilities: ProviderCapabilities,
    command_builder: RcloneCommandBuilder,
    tuning: TransferTuning,
    retry_gate: Arc<SharedRetryGate>,
    upload_limiter: Arc<AdaptiveConcurrency>,
    upload_peers: PeerRates,
    #[cfg(test)]
    upload_executable: Option<PathBuf>,
    download_limiter: Arc<AdaptiveConcurrency>,
    stream_limiter: Arc<AdaptiveConcurrency>,
    stream_peers: PeerRates,
    stream_policy: StreamPolicy,
    control_timeout: std::time::Duration,
    deletion_control: bool,
    #[cfg(test)]
    control_executable: Option<PathBuf>,
    #[cfg(test)]
    stream_executable: Option<PathBuf>,
    root_ready: OnceCell<()>,
    metrics: StorageCounters,
}

impl RcloneStorage {
    pub fn new(remote: impl Into<String>, root: impl Into<String>) -> Self {
        let tuning = TransferTuning::default();
        let provider_kind = ProviderKind::Local;
        let capabilities = ProviderCapabilities::for_provider(provider_kind);
        let profile = TransferProfile::for_provider(provider_kind, provider_kind.backend_type())
            .expect("the built-in local profile must be valid");
        let command_builder =
            RcloneCommandBuilder::new(provider_kind, provider_kind.backend_type(), profile.clone())
                .expect("the built-in local command profile must be valid");
        Self {
            remote: remote.into(),
            root: root.into(),
            extra_args: Vec::new(),
            backend_type: provider_kind.backend_type().into(),
            provider: provider_kind.as_str().into(),
            provider_kind,
            capabilities,
            command_builder,
            retry_gate: Arc::new(SharedRetryGate::new(tuning.clone())),
            upload_limiter: AdaptiveConcurrency::for_uploads(&tuning),
            upload_peers: PeerRates::default(),
            #[cfg(test)]
            upload_executable: None,
            download_limiter: AdaptiveConcurrency::for_downloads(&tuning),
            stream_limiter: AdaptiveConcurrency::for_streams(&tuning),
            stream_peers: PeerRates::default(),
            stream_policy: StreamPolicy::default(),
            control_timeout: std::time::Duration::from_secs(120),
            deletion_control: false,
            #[cfg(test)]
            control_executable: None,
            #[cfg(test)]
            stream_executable: None,
            tuning,
            root_ready: OnceCell::new(),
            metrics: StorageCounters::default(),
        }
    }

    pub fn try_from_session(session: &EphemeralRcloneSession, config_path: &Path) -> Result<Self> {
        let tuning = TransferTuning::default();
        let provider_kind = ProviderKind::parse(&session.provider).ok_or_else(|| {
            StateError::Invalid(format!("unknown storage provider `{}`", session.provider))
        })?;
        let configured_backend = noland_rclone_adapter::configured_backend_type(
            &session.config_ini,
            &session.remote_name,
        )
        .ok_or_else(|| {
            StateError::Invalid(format!(
                "rclone config has no backend type for remote `{}`",
                session.remote_name
            ))
        })?;
        if configured_backend != session.backend_type {
            return Err(StateError::Invalid(format!(
                "rclone session backend `{}` does not match configured backend `{configured_backend}`",
                session.backend_type
            )));
        }
        let profile = TransferProfile::for_provider(provider_kind, configured_backend)
            .map_err(|error| StateError::Invalid(error.to_string()))?;
        let capabilities = ProviderCapabilities::for_provider(provider_kind);
        let command_builder =
            RcloneCommandBuilder::new(provider_kind, &session.backend_type, profile.clone())?;
        let mut extra_args = vec!["--config".into(), config_path.display().to_string()];
        extra_args.extend(command_builder.global_args().iter().cloned());
        Ok(Self {
            remote: session.remote_name.clone(),
            root: session.root.clone(),
            backend_type: session.backend_type.clone(),
            provider: session.provider.clone(),
            provider_kind,
            capabilities,
            command_builder,
            extra_args,
            retry_gate: Arc::new(SharedRetryGate::new(tuning.clone())),
            upload_limiter: AdaptiveConcurrency::for_uploads(&tuning),
            upload_peers: PeerRates::default(),
            #[cfg(test)]
            upload_executable: None,
            download_limiter: AdaptiveConcurrency::for_downloads(&tuning),
            stream_limiter: AdaptiveConcurrency::for_streams(&tuning),
            stream_peers: PeerRates::default(),
            stream_policy: StreamPolicy::default(),
            control_timeout: std::time::Duration::from_secs(120),
            deletion_control: false,
            #[cfg(test)]
            control_executable: None,
            #[cfg(test)]
            stream_executable: None,
            tuning,
            root_ready: OnceCell::new(),
            metrics: StorageCounters::default(),
        })
    }

    pub fn with_transfer_tuning(mut self, tuning: TransferTuning) -> Self {
        let mut tuning = tuning.normalized();
        tuning.max_parallel_uploads = tuning
            .max_parallel_uploads
            .min(self.capabilities.max_upload_concurrency)
            .clamp(1, 32);
        self.retry_gate = Arc::new(SharedRetryGate::new(tuning.clone()));
        self.upload_limiter = AdaptiveConcurrency::for_uploads(&tuning);
        self.upload_peers = PeerRates::default();
        self.download_limiter = AdaptiveConcurrency::for_downloads(&tuning);
        self.stream_limiter = AdaptiveConcurrency::for_streams(&tuning);
        self.stream_peers = PeerRates::default();
        self.tuning = tuning;
        self
    }

    pub fn transfer_tuning(&self) -> &TransferTuning {
        &self.tuning
    }

    /// Scope finite command deadlines to manual maintenance, never to long pack
    /// transfers or application runtimes sharing this provider implementation.
    pub fn for_deletion(mut self) -> Self {
        self.deletion_control = true;
        self
    }

    pub fn provider_kind(&self) -> ProviderKind {
        self.provider_kind
    }

    fn deletion_args(&self, key: &RemoteKey) -> Result<Vec<String>> {
        crate::deletion::validate_deletable_key(key)?;
        let mut args = vec!["deletefile".into(), self.remote_path(key)];
        args.extend(
            self.permanent_delete_flags()
                .iter()
                .map(|flag| (*flag).into()),
        );
        Ok(args)
    }

    fn permanent_delete_flags(&self) -> &'static [&'static str] {
        match self.provider_kind() {
            ProviderKind::GoogleDrive => &["--drive-use-trash=false"],
            ProviderKind::BackblazeB2 => &["--b2-hard-delete"],
            ProviderKind::MicrosoftOneDrive => &["--onedrive-hard-delete"],
            _ => &[],
        }
    }

    pub fn capabilities(&self) -> &ProviderCapabilities {
        &self.capabilities
    }

    pub fn transfer_profile(&self) -> &TransferProfile {
        self.command_builder.profile()
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
        self.remote_root().key_path(key.as_str())
    }

    fn root_remote(&self) -> String {
        self.remote_root().remote_path()
    }

    fn remote_root(&self) -> RcloneRoot {
        RcloneRoot {
            remote_name: self.remote.clone(),
            root: self.root.clone(),
        }
    }

    fn record_remote_error(&self, class: RemoteErrorClass) {
        self.upload_limiter.record_error(class);
        self.download_limiter.record_error(class);
        self.stream_limiter.record_error(class);
    }

    async fn run(&self, args: Vec<String>) -> Result<String> {
        self.run_classified(args)
            .await
            .map_err(RcloneFailure::into_state_error)
    }

    async fn run_timed(&self, args: Vec<String>) -> Result<(String, std::time::Duration)> {
        self.run_classified_timed(args)
            .await
            .map_err(RcloneFailure::into_state_error)
    }

    async fn run_classified(
        &self,
        args: Vec<String>,
    ) -> std::result::Result<String, RcloneFailure> {
        self.run_classified_timed(args)
            .await
            .map(|(output, _)| output)
    }

    async fn run_classified_timed(
        &self,
        args: Vec<String>,
    ) -> std::result::Result<(String, std::time::Duration), RcloneFailure> {
        self.run_classified_with_deadline(
            args,
            self.deletion_control.then_some(self.control_timeout),
        )
        .await
    }

    async fn run_control(&self, args: Vec<String>) -> std::result::Result<String, RcloneFailure> {
        self.run_classified_with_deadline(args, Some(self.control_timeout))
            .await
            .map(|(output, _)| output)
    }

    fn command(&self, _control: bool) -> Command {
        #[cfg(test)]
        if _control {
            if let Some(executable) = &self.control_executable {
                return Command::new(executable);
            }
        }
        Command::new("rclone")
    }

    async fn run_classified_with_deadline(
        &self,
        args: Vec<String>,
        deadline: Option<std::time::Duration>,
    ) -> std::result::Result<(String, std::time::Duration), RcloneFailure> {
        forbid_rclone_sync(&args).map_err(|error| RcloneFailure {
            class: RemoteErrorClass::Permanent,
            message: error.to_string(),
        })?;
        if let Some(arg) =
            self.extra_args.iter().chain(args.iter()).find(|arg| {
                !noland_rclone_adapter::provider_arg_is_allowed(self.provider_kind, arg)
            })
        {
            return Err(RcloneFailure {
                class: RemoteErrorClass::Permanent,
                message: format!(
                    "Provider-specific rclone option {arg} cannot be used with {}",
                    self.provider_kind.label(),
                ),
            });
        }
        let max_attempts = self.tuning.max_attempts.max(1);
        for attempt in 1..=max_attempts {
            self.retry_gate.wait_for_turn().await;
            let started = std::time::Instant::now();
            self.metrics
                .rclone_invocations
                .fetch_add(1, Ordering::Relaxed);
            if args.first().map(String::as_str) == Some("mkdir") {
                self.metrics
                    .remote_mkdir_calls
                    .fetch_add(1, Ordering::Relaxed);
            }
            let mut cmd = self.command(deadline.is_some());
            cmd.args(&self.extra_args)
                .args(&args)
                .stdout(Stdio::piped())
                .stderr(Stdio::piped());
            let output = match deadline {
                Some(deadline) => match tokio::time::timeout(deadline, reaped_output(&mut cmd)).await {
                    Ok(output) => output,
                    // The child future is killed/reaped before this return. A
                    // provider may already have accepted the DELETE: do not
                    // overlap a retry or claim rollback of its remote effects.
                    Err(_) => return Err(RcloneFailure {
                        class: RemoteErrorClass::Retryable,
                        message: "deletion control request timed out; cloud outcome is unknown; resume only the same confirmed deletion plan".into(),
                    }),
                },
                None => reaped_output(&mut cmd).await,
            };
            let output = match output {
                Ok(output) => output,
                Err(error) => {
                    let failure = RcloneFailure {
                        class: classify_remote_error(None, &error.to_string()),
                        message: error.to_string(),
                    };
                    self.record_remote_error(failure.class);
                    if failure.class.is_retryable() && attempt < max_attempts {
                        self.retry_gate.wait_for_retry(attempt, failure.class).await;
                        continue;
                    }
                    return Err(failure);
                }
            };
            if output.status.success() {
                return Ok((
                    String::from_utf8_lossy(&output.stdout).into_owned(),
                    started.elapsed(),
                ));
            }
            let message = String::from_utf8_lossy(&output.stderr).trim().to_string();
            let failure = RcloneFailure {
                class: classify_remote_error(output.status.code(), &message),
                message,
            };
            self.record_remote_error(failure.class);
            if failure.class.is_retryable() && attempt < max_attempts {
                self.retry_gate.wait_for_retry(attempt, failure.class).await;
                continue;
            }
            return Err(failure);
        }
        unreachable!("rclone retry count is always at least one")
    }

    async fn stat_inner(&self, key: &RemoteKey, control: bool) -> Result<Option<RemoteMeta>> {
        validate_remote_key(key)?;
        self.metrics
            .remote_stat_calls
            .fetch_add(1, Ordering::Relaxed);
        let args = vec![
            "lsjson".into(),
            "--stat".into(),
            "--no-modtime".into(),
            "--no-mimetype".into(),
            self.remote_path(key),
        ];
        let result = if control {
            self.run_control(args).await
        } else {
            self.run_classified(args).await
        };
        match result {
            Ok(out) => {
                let object: ListedObject = serde_json::from_str(&out)
                    .map_err(|_| StateError::Integrity("cloud object stat is malformed".into()))?;
                if object.is_dir {
                    Ok(None)
                } else if object.size < 0 {
                    Err(StateError::Integrity(
                        "cloud object has an unknown size".into(),
                    ))
                } else {
                    Ok(Some(RemoteMeta {
                        key: key.clone(),
                        size: object.size as u64,
                    }))
                }
            }
            Err(failure) if failure.class == RemoteErrorClass::NotFound => Ok(None),
            Err(failure) => Err(failure.into_state_error()),
        }
    }

    async fn run_with_progress(
        &self,
        args: Vec<String>,
        mut on_progress: impl FnMut(u64) -> Result<()>,
    ) -> Result<std::time::Duration> {
        forbid_rclone_sync(&args)?;
        if let Some(arg) =
            self.extra_args.iter().chain(args.iter()).find(|arg| {
                !noland_rclone_adapter::provider_arg_is_allowed(self.provider_kind, arg)
            })
        {
            return Err(StateError::Invalid(format!(
                "Provider-specific rclone option {arg} cannot be used with {}",
                self.provider_kind.label(),
            )));
        }

        let max_attempts = self.tuning.max_attempts.max(1);
        for attempt in 1..=max_attempts {
            self.retry_gate.wait_for_turn().await;
            let started = std::time::Instant::now();
            self.metrics
                .rclone_invocations
                .fetch_add(1, Ordering::Relaxed);
            let mut cmd = Command::new("rclone");
            cmd.args(&self.extra_args)
                .args(&args)
                .stdout(Stdio::null())
                .stderr(Stdio::piped())
                .kill_on_drop(true);
            let mut child = match cmd.spawn() {
                Ok(child) => ReapedChild(child),
                Err(error) => {
                    let failure = RcloneFailure {
                        class: classify_remote_error(None, &error.to_string()),
                        message: error.to_string(),
                    };
                    self.record_remote_error(failure.class);
                    if failure.class.is_retryable() && attempt < max_attempts {
                        self.retry_gate.wait_for_retry(attempt, failure.class).await;
                        continue;
                    }
                    return Err(failure.into_state_error());
                }
            };
            let stderr = child.stderr.take().ok_or_else(|| {
                StateError::Storage("rclone progress stream was not available".into())
            })?;
            let mut lines = BufReader::new(stderr).lines();
            let mut error_output = String::new();
            while let Some(line) = lines.next_line().await? {
                if let Some(bytes) = parse_rclone_stats_bytes(&line) {
                    if let Err(error) = on_progress(bytes) {
                        let _ = child.kill().await;
                        return Err(error);
                    }
                } else {
                    if !error_output.is_empty() {
                        error_output.push('\n');
                    }
                    error_output.push_str(&line);
                    if error_output.len() > 16 * 1024 {
                        let mut start = error_output.len() - 16 * 1024;
                        while !error_output.is_char_boundary(start) {
                            start += 1;
                        }
                        error_output.drain(..start);
                    }
                }
            }
            let status = child.wait().await?;
            if status.success() {
                return Ok(started.elapsed());
            }
            let failure = RcloneFailure {
                class: classify_remote_error(status.code(), error_output.trim()),
                message: error_output.trim().to_string(),
            };
            self.record_remote_error(failure.class);
            if failure.class.is_retryable() && attempt < max_attempts {
                self.retry_gate.wait_for_retry(attempt, failure.class).await;
                continue;
            }
            return Err(failure.into_state_error());
        }
        unreachable!("rclone retry count is always at least one")
    }

    async fn copy_staged(
        &self,
        stage: &StagingTree,
        immutable: bool,
        observer: Option<&(dyn ImmutableUploadObserver + '_)>,
    ) -> Result<()> {
        let waiting = std::time::Instant::now();
        let mut permit = self
            .upload_limiter
            .acquire_slots(stage.files.min(self.tuning.rclone_transfers))
            .await?;
        let limiter_wait = waiting.elapsed();
        let mut current_tuning = self.tuning.clone();
        current_tuning.rclone_transfers = permit.slots();
        current_tuning.max_parallel_uploads = permit.slots();
        current_tuning.rclone_checkers = current_tuning.rclone_checkers.min(permit.slots());
        let mut args = self.command_builder.upload_copy_args(
            &current_tuning,
            stage.root.display().to_string(),
            self.root_remote(),
            immutable,
        )?;
        args.push("--no-traverse".into());
        let mut remaining = stage.files;
        let result = self
            .run_upload(
                args,
                &stage.objects,
                observer,
                |_, _| {
                    remaining = remaining.saturating_sub(1);
                    permit.retain_slots(remaining);
                    Ok(())
                },
                limiter_wait,
                upload::UploadPolicy::default(),
            )
            .await;
        drop(permit);
        result.map(|_| ())
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
            let mut stage = StagingTree::new("metadata")?;
            for entry in chunk {
                validate_remote_key(&entry.key)?;
                stage.write(&entry.key, &entry.bytes)?;
            }
            self.copy_staged(&stage, false, None).await?;
            for entry in chunk {
                output.push(RemoteMeta {
                    key: entry.key.clone(),
                    size: entry.bytes.len() as u64,
                });
            }
            offset = end;
        }
        Ok(output)
    }

    async fn upload_compared(
        &self,
        uploads: &[ImmutableUpload],
        comparison: crate::RemoteKnownComparison,
        observer: Option<&(dyn ImmutableUploadObserver + '_)>,
    ) -> Result<Vec<RemoteMeta>> {
        comparison.ensure_immutable()?;
        self.metrics
            .remote_upload_calls
            .fetch_add(uploads.len() as u64, Ordering::Relaxed);
        let tuning = self.tuning.normalized();
        let mut uploaded_by_key: HashMap<RemoteKey, RemoteMeta> = comparison
            .known
            .into_iter()
            .map(|meta| (meta.key.clone(), meta))
            .collect();
        if let Some(observer) = observer {
            let known = uploads
                .iter()
                .filter(|upload| uploaded_by_key.contains_key(&upload.key))
                .cloned()
                .collect::<Vec<_>>();
            if !known.is_empty() {
                observer.completed(&known, ImmutableUploadCompletion::ReusedRemote)?;
            }
        }
        let mut offset = 0;
        while offset < comparison.missing.len() {
            let mut end = offset;
            let mut bytes = 0u64;
            while end < comparison.missing.len() && end - offset < tuning.max_bulk_files {
                let size = std::fs::metadata(&comparison.missing[end].local)?.len();
                if size > tuning.max_bulk_bytes {
                    return Err(StateError::Invalid(
                        "immutable object exceeds transfer byte budget".into(),
                    ));
                }
                if bytes.saturating_add(size) > tuning.max_bulk_bytes {
                    break;
                }
                bytes += size;
                end += 1;
            }
            let mut stage = StagingTree::near("immutable", &comparison.missing[offset].local)?;
            for upload in &comparison.missing[offset..end] {
                stage.link_or_copy(&upload.local, &upload.key)?;
            }
            // Even keys absent from the inventory go through --immutable. The
            // listing is an optimization, never permission to overwrite a race.
            self.copy_staged(&stage, true, observer).await?;
            for upload in &comparison.missing[offset..end] {
                uploaded_by_key.insert(
                    upload.key.clone(),
                    RemoteMeta {
                        key: upload.key.clone(),
                        size: std::fs::metadata(&upload.local)?.len(),
                    },
                );
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
}

struct StagingTree {
    root: PathBuf,
    files: usize,
    bytes: u64,
    objects: Vec<ImmutableUpload>,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "PascalCase")]
struct ListedObject {
    path: String,
    size: i64,
    is_dir: bool,
}

fn parse_listing(prefix: &RemoteKey, output: &str) -> Result<Vec<RemoteEntry>> {
    let objects: Vec<ListedObject> = serde_json::from_str(output)
        .map_err(|_| StateError::Integrity("cloud inventory is malformed or incomplete".into()))?;
    let mut seen = std::collections::HashSet::new();
    objects
        .into_iter()
        .map(|object| {
            if object.path.split('/').any(|p| {
                p.is_empty() || matches!(p, "." | "..") || p.contains(['\\', '\0', '\n', '\r'])
            }) {
                return Err(StateError::UnsafePath(
                    "noncanonical cloud inventory path".into(),
                ));
            }
            crate::transfer::validate_remote_key(&RemoteKey::new(&object.path))?;
            let key = prefix.join(&object.path);
            if !seen.insert(key.clone()) || (!object.is_dir && object.size < 0) {
                return Err(StateError::Integrity(
                    "cloud inventory contains ambiguous objects or sizes".into(),
                ));
            }
            Ok(RemoteEntry {
                key,
                size: object.size.max(0) as u64,
                is_prefix: object.is_dir,
            })
        })
        .collect()
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
        Ok(Self {
            root,
            files: 0,
            bytes: 0,
            objects: Vec::new(),
        })
    }

    fn path_for(&self, key: &RemoteKey) -> Result<PathBuf> {
        validate_remote_key(key)?;
        Ok(self.root.join(key.as_str()))
    }

    fn link_or_copy(&mut self, source: &Path, key: &RemoteKey) -> Result<()> {
        let destination = self.path_for(key)?;
        if let Some(parent) = destination.parent() {
            std::fs::create_dir_all(parent)?;
        }
        if std::fs::hard_link(source, &destination).is_err() {
            std::fs::copy(source, &destination)?;
        }
        self.objects
            .push(ImmutableUpload::new(destination, key.clone()));
        self.files += 1;
        self.bytes = self.bytes.saturating_add(std::fs::metadata(source)?.len());
        Ok(())
    }

    fn write(&mut self, key: &RemoteKey, bytes: &[u8]) -> Result<()> {
        let destination = self.path_for(key)?;
        if let Some(parent) = destination.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&destination, bytes)?;
        self.objects
            .push(ImmutableUpload::new(destination, key.clone()));
        self.files += 1;
        self.bytes = self.bytes.saturating_add(bytes.len() as u64);
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
    async fn check_full_deletion_support(&self) -> Result<()> {
        if self.permanent_delete_flags().is_empty() {
            return Ok(());
        }
        let mut args = vec!["deletefile".into(), "--help".into()];
        args.extend(
            self.permanent_delete_flags()
                .iter()
                .map(|flag| (*flag).into()),
        );
        // Help parses flags without accessing any objects or cloud credentials.
        self.run(args).await.map_err(|_|StateError::Storage("installed rclone lacks the provider's permanent-delete options; update rclone before deleting (no deletion started)".into()))?;
        Ok(())
    }
    async fn remove_empty_bundle(
        &self,
        app: &noland_state_core::AppId,
        bundle: uuid::Uuid,
    ) -> Result<()> {
        let key = RemoteKey::new(noland_state_core::bundle_dir(app, bundle));
        crate::transfer::validate_remote_key(&key)?;
        let mut args = vec!["rmdir".into(), self.remote_path(&key)];
        args.extend(
            self.permanent_delete_flags()
                .iter()
                .map(|flag| (*flag).into()),
        );
        match self.run_classified(args).await {
            Ok(_) => {}
            Err(failure) if failure.class == RemoteErrorClass::NotFound => {}
            Err(failure) => return Err(failure.into_state_error()),
        }
        Ok(())
    }
    async fn delete_object(&self, key: &RemoteKey) -> Result<()> {
        crate::deletion::validate_deletable_key(key)?;
        if self.stat_for_deletion(key).await?.is_some() {
            // Trash still consumes Drive quota; delete this exact object permanently.
            self.run_control(self.deletion_args(key)?)
                .await
                .map_err(RcloneFailure::into_state_error)?;
        }
        if self.stat_for_deletion(key).await?.is_some() {
            return Err(StateError::Storage(
                "deleted object is still present".into(),
            ));
        }
        Ok(())
    }

    async fn delete_prevalidated(&self, object: &crate::PrevalidatedDeletion) -> Result<()> {
        match self.run_control(self.deletion_args(object.key())?).await {
            Ok(_) => Ok(()),
            Err(failure) if failure.class == RemoteErrorClass::NotFound => Ok(()),
            Err(failure) => Err(failure.into_state_error()),
        }
    }

    async fn stat_for_deletion(&self, key: &RemoteKey) -> Result<Option<RemoteMeta>> {
        self.stat_inner(key, true).await
    }

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
        self.stat_inner(key, false).await
    }

    async fn upload_immutable(&self, local: &Path, key: &RemoteKey) -> Result<RemoteMeta> {
        validate_remote_key(key)?;
        let size = std::fs::metadata(local)?.len();
        self.metrics
            .remote_upload_calls
            .fetch_add(1, Ordering::Relaxed);
        let mut stage = StagingTree::near("single", local)?;
        stage.link_or_copy(local, key)?;
        self.copy_staged(&stage, true, None).await?;
        Ok(RemoteMeta {
            key: key.clone(),
            size,
        })
    }

    async fn upload_immutable_bulk(&self, uploads: &[ImmutableUpload]) -> Result<Vec<RemoteMeta>> {
        self.upload_immutable_bulk_observed(uploads, None).await
    }

    async fn upload_immutable_bulk_observed(
        &self,
        uploads: &[ImmutableUpload],
        observer: Option<&(dyn ImmutableUploadObserver + '_)>,
    ) -> Result<Vec<RemoteMeta>> {
        if uploads.is_empty() {
            return Ok(Vec::new());
        }
        let comparison = crate::RemoteKnownComparison {
            missing: uploads.to_vec(),
            ..Default::default()
        };
        self.upload_compared(uploads, comparison, observer).await
    }

    async fn upload_immutable_bulk_known_observed(
        &self,
        uploads: &[ImmutableUpload],
        known: &crate::RemoteKnownSet,
        observer: Option<&(dyn ImmutableUploadObserver + '_)>,
    ) -> Result<Vec<RemoteMeta>> {
        self.upload_compared(uploads, known.compare(uploads)?, observer)
            .await
    }

    fn transfer_tuning(&self) -> TransferTuning {
        self.tuning.clone()
    }

    fn supports_parallel_upload_groups(&self) -> bool {
        true
    }

    fn supports_targeted_uploads(&self) -> bool {
        true
    }

    async fn download(&self, key: &RemoteKey, dest: &Path) -> Result<()> {
        self.download_observed(key, dest, None).await
    }

    async fn download_observed(
        &self,
        key: &RemoteKey,
        dest: &Path,
        observer: Option<&(dyn DownloadObserver + '_)>,
    ) -> Result<()> {
        let permit = self.download_limiter.acquire().await?;
        self.metrics
            .remote_download_calls
            .fetch_add(1, Ordering::Relaxed);
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let args = self.command_builder.download_copyto_args(
            &self.tuning,
            self.remote_path(key),
            dest.display().to_string(),
        );
        let result = match observer {
            Some(observer) => {
                observer.transfer_started()?;
                self.run_with_progress(args, |bytes| observer.transfer_progress(bytes))
                    .await
            }
            None => self.run_timed(args).await.map(|(_, elapsed)| elapsed),
        };
        if let Ok(elapsed) = &result {
            let bytes = std::fs::metadata(dest).map(|meta| meta.len()).unwrap_or(0);
            self.download_limiter.record_success(bytes, *elapsed);
        }
        drop(permit);
        result?;
        if let Ok(metadata) = std::fs::metadata(dest) {
            self.metrics
                .bytes_downloaded
                .fetch_add(metadata.len(), Ordering::Relaxed);
        }
        Ok(())
    }

    fn supports_streaming_downloads(&self) -> bool {
        true
    }

    async fn download_stream_observed(
        &self,
        key: &RemoteKey,
        _scratch: &Path,
        consumer: &dyn DownloadStreamConsumer,
        observer: Option<&(dyn DownloadObserver + '_)>,
    ) -> Result<()> {
        validate_remote_key(key)?;
        let waiting = std::time::Instant::now();
        let _permit = self.stream_limiter.acquire().await?;
        let mut diagnostics = StreamTransferDiagnostics {
            limiter_wait_us: stream::micros(waiting.elapsed()),
            ..StreamTransferDiagnostics::default()
        };
        let args = self
            .command_builder
            .download_stream_args(self.remote_path(key));
        forbid_rclone_sync(&args)?;
        if let Some(arg) =
            self.extra_args.iter().chain(args.iter()).find(|arg| {
                !noland_rclone_adapter::provider_arg_is_allowed(self.provider_kind, arg)
            })
        {
            return Err(StateError::Invalid(format!(
                "unsupported provider streaming option {arg}"
            )));
        }
        self.metrics
            .remote_download_calls
            .fetch_add(1, Ordering::Relaxed);
        for attempt in 1..=self.tuning.max_attempts.max(1) {
            let waiting = std::time::Instant::now();
            self.retry_gate.wait_for_turn().await;
            diagnostics.retry_wait_us = diagnostics
                .retry_wait_us
                .saturating_add(stream::micros(waiting.elapsed()));
            diagnostics.attempt = attempt;
            diagnostics.attempt_bytes = 0;
            diagnostics.phase = "receiving".into();
            let processing = std::time::Instant::now();
            consumer.restart().await?;
            diagnostics.consumer_us = diagnostics
                .consumer_us
                .saturating_add(stream::micros(processing.elapsed()));
            if let Some(observer) = observer {
                observer.transfer_started()?;
            }
            stream::observe(observer, &diagnostics)?;
            self.metrics
                .rclone_invocations
                .fetch_add(1, Ordering::Relaxed);
            #[cfg(not(test))]
            let mut cmd = Command::new("rclone");
            #[cfg(test)]
            let mut cmd = if let Some(script) = self.stream_executable.as_deref() {
                // Invoke the system shell directly. Executing a freshly written
                // script can trigger macOS validation delays unrelated to I/O.
                let mut command = Command::new("/bin/sh");
                command.arg(script);
                command
            } else {
                Command::new("rclone")
            };
            cmd.args(&self.extra_args)
                .args(&args)
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .kill_on_drop(true);
            let mut child = ReapedChild(cmd.spawn()?);
            let mut stdout = child
                .stdout
                .take()
                .ok_or_else(|| StateError::Storage("rclone stdout missing".into()))?;
            let stderr = child
                .stderr
                .take()
                .ok_or_else(|| StateError::Storage("rclone stderr missing".into()))?;
            let before_network = diagnostics.network_wait_us;
            let before_consumer = diagnostics.consumer_us;
            let allow_slow_restart =
                diagnostics.slow_restarts == 0 && attempt < self.tuning.max_attempts.max(1);
            let (done, ended) = tokio::sync::oneshot::channel();
            // No detached tasks. An unsuccessful read kills the child before
            // draining stderr; successful EOF bounds only exit housekeeping.
            let stream = async {
                let result = stream::read_attempt(
                    &mut stdout,
                    consumer,
                    observer,
                    &mut diagnostics,
                    &self.stream_peers,
                    self.stream_policy,
                    allow_slow_restart,
                    &self.metrics.bytes_downloaded,
                )
                .await;
                if result.is_err() {
                    let _ = child.start_kill();
                }
                let _ = done.send(std::time::Instant::now());
                result
            };
            let (read, (tail, exit_wait)) = tokio::join!(
                stream,
                drain_stream_errors(stderr, ended, self.stream_policy.exit_timeout)
            );
            diagnostics.process_exit_wait_us =
                diagnostics.process_exit_wait_us.saturating_add(exit_wait);
            let mut reason = None;
            let mut retry_class = RemoteErrorClass::Retryable;
            let bytes = match read {
                Ok(bytes) => bytes,
                Err(StreamReadFailure::Local(error)) => {
                    kill_and_reap_for_retry(&mut child).await?;
                    diagnostics.phase = "consumer_error".into();
                    let _ = stream::observe(observer, &diagnostics);
                    return Err(error);
                }
                Err(error) => {
                    match error {
                        StreamReadFailure::Idle => {
                            diagnostics.idle_restarts +=
                                u32::from(attempt < self.tuning.max_attempts.max(1))
                        }
                        StreamReadFailure::SlowPeer => diagnostics.slow_restarts += 1,
                        StreamReadFailure::Local(_) => unreachable!(),
                    }
                    reason = Some(error.message());
                    0
                }
            };
            let tail = match tail {
                Ok(tail) => tail,
                Err(_) => {
                    diagnostics.exit_restarts +=
                        u32::from(attempt < self.tuning.max_attempts.max(1));
                    reason.get_or_insert("rclone stderr did not finish after stream EOF");
                    Vec::new()
                }
            };
            if reason.is_none() {
                let waiting = std::time::Instant::now();
                let status =
                    tokio::time::timeout(self.stream_policy.exit_timeout, child.wait()).await;
                diagnostics.process_exit_wait_us = diagnostics
                    .process_exit_wait_us
                    .saturating_add(stream::micros(waiting.elapsed()));
                match status {
                    Ok(status) => {
                        let status = status?;
                        if status.success() {
                            let receive = std::time::Duration::from_micros(
                                diagnostics.network_wait_us.saturating_sub(before_network),
                            );
                            let consumed = std::time::Duration::from_micros(
                                diagnostics.consumer_us.saturating_sub(before_consumer),
                            );
                            let processing = std::time::Instant::now();
                            let finished = consumer.finish().await;
                            diagnostics.consumer_us = diagnostics
                                .consumer_us
                                .saturating_add(stream::micros(processing.elapsed()));
                            if let Err(error) = finished {
                                diagnostics.phase = "consumer_error".into();
                                let _ = stream::observe(observer, &diagnostics);
                                return Err(error);
                            }
                            // Disk/decoder backpressure is not provider congestion.
                            if consumed <= receive / 3 {
                                self.stream_limiter.record_success(bytes, receive);
                                self.stream_peers.record(bytes, receive, consumed);
                            }
                            diagnostics.phase = "completed".into();
                            stream::observe(observer, &diagnostics)?;
                            return Ok(());
                        }
                        let failure = RcloneFailure {
                            class: classify_remote_error(
                                status.code(),
                                &String::from_utf8_lossy(&tail),
                            ),
                            message: String::from_utf8_lossy(&tail).trim().to_string(),
                        };
                        self.record_remote_error(failure.class);
                        if !failure.class.is_retryable()
                            || attempt == self.tuning.max_attempts.max(1)
                        {
                            diagnostics.phase = "failed".into();
                            let _ = stream::observe(observer, &diagnostics);
                            return Err(failure.into_state_error());
                        }
                        retry_class = failure.class;
                        reason = Some("retryable provider failure");
                    }
                    Err(_) => {
                        diagnostics.exit_restarts +=
                            u32::from(attempt < self.tuning.max_attempts.max(1));
                        reason = Some("rclone did not exit after stream EOF");
                    }
                }
            }
            let reason = reason.expect("unsuccessful stream has a retry reason");
            // Never race two readers/decoders for the same pack, or abandon a
            // child using the ephemeral session while starting another attempt.
            kill_and_reap_for_retry(&mut child).await?;
            diagnostics.phase = if attempt < self.tuning.max_attempts.max(1) {
                "retrying"
            } else {
                "failed"
            }
            .into();
            stream::observe(observer, &diagnostics)?;
            if attempt == self.tuning.max_attempts.max(1) {
                return Err(StateError::Storage(reason.into()));
            }
            tracing::warn!(object = %key.as_str(), attempt, reason,
                network_wait_us = diagnostics.network_wait_us,
                consumer_us = diagnostics.consumer_us, "restarting stalled pack stream");
            if let Some(observer) = observer {
                observer.transfer_retrying(reason)?;
            }
            let waiting = std::time::Instant::now();
            self.retry_gate.wait_for_retry(attempt, retry_class).await;
            diagnostics.retry_wait_us = diagnostics
                .retry_wait_us
                .saturating_add(stream::micros(waiting.elapsed()));
        }
        unreachable!("rclone always makes at least one attempt")
    }

    async fn list_prefix(&self, prefix: &RemoteKey) -> Result<Vec<RemoteEntry>> {
        validate_remote_key(prefix)?;
        self.metrics
            .remote_list_calls
            .fetch_add(1, Ordering::Relaxed);
        let out = match self
            .run_classified(vec![
                "lsjson".into(),
                "-R".into(),
                "--no-modtime".into(),
                "--no-mimetype".into(),
                self.remote_path(prefix),
            ])
            .await
        {
            Ok(out) => out,
            Err(failure) if failure.class == RemoteErrorClass::NotFound => "[]".into(),
            Err(failure) => return Err(failure.into_state_error()),
        };
        parse_listing(prefix, &out)
    }

    async fn put_small_versioned(&self, bytes: Bytes, key: &RemoteKey) -> Result<RemoteMeta> {
        self.metrics
            .remote_upload_calls
            .fetch_add(1, Ordering::Relaxed);
        let mut stage = StagingTree::new("metadata-single")?;
        stage.write(key, &bytes)?;
        let tmp = stage.path_for(key)?;
        let result = async {
            let permit = self.upload_limiter.acquire().await?;
            let args = self.command_builder.upload_copyto_args(
                tmp.display().to_string(),
                self.remote_path(key),
                false,
            )?;
            let result = self.run_timed(args).await;
            if let Ok((_, elapsed)) = &result {
                self.upload_limiter
                    .record_success(bytes.len() as u64, *elapsed);
            }
            drop(permit);
            result
        }
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

    fn provider_kind(&self) -> Option<ProviderKind> {
        Some(self.provider_kind)
    }

    fn capabilities(&self) -> Option<&ProviderCapabilities> {
        Some(&self.capabilities)
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

fn parse_rclone_stats_bytes(line: &str) -> Option<u64> {
    let value = serde_json::from_str::<serde_json::Value>(line).ok()?;
    value
        .get("bytes")
        .and_then(serde_json::Value::as_u64)
        .or_else(|| value.get("stats")?.get("bytes")?.as_u64())
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

    #[cfg(unix)]
    #[tokio::test]
    async fn deletion_control_timeout_reaps_the_child_without_an_overlapping_retry() {
        use std::os::unix::fs::PermissionsExt;
        let root =
            std::env::temp_dir().join(format!("noland-deletion-timeout-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let script = root.join("slow-control");
        let pids = root.join("pids");
        std::fs::write(
            &script,
            format!("#!/bin/sh\nprintf '%s\\n' \"$$\" >> {pids:?}\nexec sleep 30\n"),
        )
        .unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        let mut storage = RcloneStorage::new("fixture", "").for_deletion();
        storage.control_executable = Some(script);
        // macOS may validate a cold script before it starts. Leave the same
        // startup allowance as the stream fixtures; the intentional 30s sleep
        // must be what exercises the timeout, not interpreter startup latency.
        storage.control_timeout = std::time::Duration::from_secs(2);
        let result = storage
            .stat_for_deletion(&RemoteKey::new("packs/fixture"))
            .await;
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("cloud outcome is unknown"));
        let observed = std::fs::read_to_string(pids).unwrap();
        assert_eq!(
            observed.lines().count(),
            1,
            "a timed-out delete must not immediately retry an uncertain provider request"
        );
        let status = std::process::Command::new("kill")
            .args(["-0", observed.trim()])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .unwrap();
        assert!(
            !status.success(),
            "the child must be reaped before returning"
        );
        assert!(
            !RcloneStorage::new("fixture", "").deletion_control,
            "long backup/restore transfers must not inherit maintenance deadlines"
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    #[ignore = "requires real rclone; fresh local fixture, no cloud credentials"]
    async fn real_rclone_prevalidated_delete_uses_one_command_and_tolerates_absence() {
        let root =
            std::env::temp_dir().join(format!("noland-exact-delete-{}", uuid::Uuid::new_v4()));
        let id = uuid::Uuid::new_v4().to_string();
        let key = RemoteKey::new(noland_state_core::pack_key(&id));
        let config = root.join("local.conf");
        std::fs::create_dir_all(root.join("cloud").join(key.as_str()).parent().unwrap()).unwrap();
        std::fs::write(&config, "[fixture]\ntype = local\n").unwrap();
        let object = root.join("cloud").join(key.as_str());
        std::fs::write(&object, b"fixture payload").unwrap();
        let keep = root.join("cloud/keep");
        std::fs::write(&keep, b"unrelated").unwrap();
        let mut storage = RcloneStorage::new("fixture", root.join("cloud").to_string_lossy());
        storage.extra_args = vec!["--config".into(), config.to_string_lossy().into()];
        let capability = crate::PrevalidatedDeletion::fixture(key.clone());
        storage.delete_prevalidated(&capability).await.unwrap();
        assert_eq!(storage.operation_metrics().rclone_invocations, 1);
        assert_eq!(storage.operation_metrics().remote_stat_calls, 0);
        assert!(!object.exists());
        storage.delete_prevalidated(&capability).await.unwrap();
        assert!(storage.stat_for_deletion(&key).await.unwrap().is_none());
        assert_eq!(std::fs::read(keep).unwrap(), b"unrelated");
        std::fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    struct StreamFixture {
        root: PathBuf,
        storage: RcloneStorage,
    }

    #[cfg(unix)]
    impl StreamFixture {
        fn new(first_attempt: &str) -> Self {
            use std::os::unix::fs::PermissionsExt;
            let root = std::env::temp_dir()
                .join(format!("noland-stream-process-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir_all(&root).unwrap();
            let script = root.join("rclone-fixture");
            let source = format!("#!/bin/sh\nset -eu\nprintf '%s\\n' \"$$\" >> {pids:?}\nif [ ! -f {once:?} ]; then\n  printf '%s' \"$$\" > {first:?}\n  : > {once:?}\n  {first_attempt}\nfi\nif kill -0 \"$(cat {first:?})\" 2>/dev/null; then exit 9; fi\nprintf abcdef\n",
                pids=root.join("pids"), once=root.join("once"), first=root.join("first-pid"));
            std::fs::write(&script, source).unwrap();
            std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
            let tuning = TransferTuning {
                max_attempts: 2,
                initial_backoff_ms: 1,
                max_backoff_ms: 1,
                ..TransferTuning::default()
            };
            let mut storage = RcloneStorage::new("fixture", "").with_transfer_tuning(tuning);
            storage.stream_executable = Some(script);
            // Leave room for macOS executable validation and process startup;
            // only the intentional sleep in the fixture should trigger a retry.
            storage.stream_policy = StreamPolicy {
                idle_timeout: std::time::Duration::from_secs(2),
                exit_timeout: std::time::Duration::from_millis(500),
                ..StreamPolicy::default()
            };
            Self { root, storage }
        }
    }

    #[cfg(unix)]
    impl Drop for StreamFixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    #[derive(Default)]
    struct StreamBuffer {
        bytes: std::sync::Mutex<Vec<u8>>,
        restarts: AtomicU64,
        reject: bool,
    }

    #[async_trait]
    impl DownloadStreamConsumer for StreamBuffer {
        async fn restart(&self) -> Result<()> {
            self.restarts.fetch_add(1, Ordering::Relaxed);
            self.bytes.lock().unwrap().clear();
            Ok(())
        }
        async fn write(&self, bytes: &[u8]) -> Result<()> {
            if self.reject {
                return Err(StateError::Integrity("fixture corruption".into()));
            }
            self.bytes.lock().unwrap().extend_from_slice(bytes);
            Ok(())
        }
        async fn finish(&self) -> Result<()> {
            Ok(())
        }
    }

    #[derive(Default)]
    struct StreamObserver(std::sync::Mutex<Vec<StreamTransferDiagnostics>>);
    impl DownloadObserver for StreamObserver {
        fn transfer_progress_with_diagnostics(
            &self,
            _: u64,
            metrics: &StreamTransferDiagnostics,
        ) -> Result<()> {
            self.0.lock().unwrap().push(metrics.clone());
            Ok(())
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn stalled_pack_is_killed_and_reaped_before_retry_without_losing_wire_accounting() {
        let fixture = StreamFixture::new("printf abc; exec sleep 60");
        let target = StreamBuffer::default();
        let observer = StreamObserver::default();
        fixture
            .storage
            .download_stream_observed(
                &RemoteKey::new("packs/test"),
                &fixture.root.join("unused"),
                &target,
                Some(&observer),
            )
            .await
            .unwrap();
        assert_eq!(&*target.bytes.lock().unwrap(), b"abcdef");
        assert_eq!(target.restarts.load(Ordering::Relaxed), 2);
        assert_eq!(fixture.storage.operation_metrics().bytes_downloaded, 9);
        let metrics = observer.0.lock().unwrap().last().unwrap().clone();
        assert_eq!(metrics.attempt, 2);
        assert_eq!(metrics.idle_restarts, 1);
        assert_eq!(metrics.received_bytes, 9);
        assert_eq!(metrics.attempt_bytes, 6);
        assert_eq!(metrics.phase, "completed");
        assert_eq!(
            std::fs::read_to_string(fixture.root.join("pids"))
                .unwrap()
                .lines()
                .count(),
            2
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn an_integrity_error_never_restarts_the_process_or_weakens_verification() {
        let fixture = StreamFixture::new("printf abc; exec sleep 60");
        let target = StreamBuffer {
            reject: true,
            ..StreamBuffer::default()
        };
        let result = fixture
            .storage
            .download_stream_observed(
                &RemoteKey::new("packs/test"),
                &fixture.root.join("unused"),
                &target,
                None,
            )
            .await;
        assert!(matches!(result, Err(StateError::Integrity(_))));
        assert_eq!(target.restarts.load(Ordering::Relaxed), 1);
        assert_eq!(
            std::fs::read_to_string(fixture.root.join("pids"))
                .unwrap()
                .lines()
                .count(),
            1
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn eof_without_process_exit_cannot_hold_a_pack_slot_indefinitely() {
        let fixture = StreamFixture::new("printf abc; exec 1>&-; exec sleep 60");
        let observer = StreamObserver::default();
        fixture
            .storage
            .download_stream_observed(
                &RemoteKey::new("packs/test"),
                &fixture.root.join("unused"),
                &StreamBuffer::default(),
                Some(&observer),
            )
            .await
            .unwrap();
        let metrics = observer.0.lock().unwrap().last().unwrap().clone();
        assert_eq!(metrics.attempt, 2);
        assert_eq!(metrics.exit_restarts, 1);
        assert!(metrics.process_exit_wait_us > 0);
    }

    #[test]
    fn storage_paths_preserve_absolute_and_relative_remote_roots() {
        for (root, expected_root, expected_key) in [
            ("", "remote:", "remote:packs/a"),
            ("/", "remote:/", "remote:/packs/a"),
            ("/srv/path", "remote:/srv/path", "remote:/srv/path/packs/a"),
            ("srv/path", "remote:srv/path", "remote:srv/path/packs/a"),
            ("/srv/path/", "remote:/srv/path", "remote:/srv/path/packs/a"),
        ] {
            let storage = RcloneStorage::new("remote", root);
            assert_eq!(storage.root_remote(), expected_root);
            assert_eq!(
                storage.remote_path(&RemoteKey::new("packs/a")),
                expected_key
            );
        }
    }

    #[test]
    fn rejects_sync() {
        assert!(assert_copy_only(&["sync".into(), "src".into(), "dst".into()]).is_err());
        assert!(assert_copy_only(&["copy".into(), "src".into(), "dst".into()]).is_ok());
    }

    #[test]
    fn destructive_inventory_rejects_truncation_duplicates_and_unsafe_or_unknown_objects() {
        let prefix = RemoteKey::new("bundles");
        for malformed in [
            "",
            "[",
            r#"[{"Path":"a","Size":10,"IsDir":false},"#,
            r#"[{"Path":"a","Size":10,"IsDir":false},{"Path":"a","Size":10,"IsDir":false}]"#,
            r#"[{"Path":"../escape","Size":10,"IsDir":false}]"#,
            r#"[{"Path":"app//id","Size":10,"IsDir":false}]"#,
            r#"[{"Path":"app","Size":-1,"IsDir":false}]"#,
        ] {
            assert!(parse_listing(&prefix, malformed).is_err(), "{malformed}");
        }
        let entries=parse_listing(&prefix,r#"[{"Path":"app","Size":-1,"IsDir":true},{"Path":"app/data;ok","Size":7,"IsDir":false}]"#).unwrap();
        assert_eq!(entries[1].size, 7);
        assert_eq!(entries[1].key.as_str(), "bundles/app/data;ok");
    }

    #[test]
    fn full_delete_is_exact_and_drive_does_not_retain_payload_in_trash() {
        let key = RemoteKey::new(noland_state_core::pack_key(
            &uuid::Uuid::new_v4().to_string(),
        ));
        let mut storage = RcloneStorage::new("remote", "approved-root");
        for kind in [
            ProviderKind::Local,
            ProviderKind::GoogleDrive,
            ProviderKind::BackblazeB2,
            ProviderKind::MicrosoftOneDrive,
        ] {
            storage.provider_kind = kind;
            let args = storage.deletion_args(&key).unwrap();
            assert_eq!(args[0], "deletefile");
            assert_eq!(args[1], storage.remote_path(&key));
            assert_eq!(
                args.iter().any(|a| a == "--drive-use-trash=false"),
                kind == ProviderKind::GoogleDrive
            );
            assert_eq!(
                args.iter().any(|a| a == "--b2-hard-delete"),
                kind == ProviderKind::BackblazeB2
            );
            assert_eq!(
                args.iter().any(|a| a == "--onedrive-hard-delete"),
                kind == ProviderKind::MicrosoftOneDrive
            );
            assert!(!args
                .iter()
                .any(|a| matches!(a.as_str(), "purge" | "sync" | "delete")));
        }
        assert!(storage.deletion_args(&RemoteKey::new("packs")).is_err());
    }

    #[tokio::test]
    #[ignore = "requires rclone on PATH; only parses help flags with an empty config"]
    async fn real_rclone_permanent_flags_without_remote_io() {
        let mut storage = RcloneStorage::new("unused", "unused");
        storage.extra_args = vec!["--config".into(), "/dev/null".into()];
        for kind in [
            ProviderKind::GoogleDrive,
            ProviderKind::BackblazeB2,
            ProviderKind::MicrosoftOneDrive,
        ] {
            storage.provider_kind = kind;
            storage.check_full_deletion_support().await.unwrap();
        }
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
        let profile = TransferProfile::for_provider(ProviderKind::Local, "local").unwrap();
        let builder = RcloneCommandBuilder::new(ProviderKind::Local, "local", profile).unwrap();
        let args = builder
            .upload_copy_args(
                &TransferTuning::default(),
                "/tmp/stage".into(),
                "remote:root".into(),
                true,
            )
            .unwrap();
        assert_eq!(args.first().map(String::as_str), Some("copy"));
        assert!(args.iter().any(|arg| arg == "--immutable"));
        assert!(args.iter().any(|arg| arg == "--no-traverse"));
        assert!(args.windows(2).any(|pair| pair == ["--stats", "500ms"]));
        assert!(args.iter().any(|arg| arg == "--stats-one-line"));
        assert!(args.iter().any(|arg| arg == "--use-json-log"));
        assert!(assert_copy_only(&args).is_ok());
        assert!(!args.iter().any(|arg| arg.starts_with("--drive-")));
    }

    #[test]
    fn download_copy_args_emit_machine_readable_progress() {
        let profile = TransferProfile::for_provider(ProviderKind::Local, "local").unwrap();
        let builder = RcloneCommandBuilder::new(ProviderKind::Local, "local", profile).unwrap();
        let args = builder.download_copyto_args(
            &TransferTuning::throughput(),
            "remote:pack".into(),
            "/tmp/pack".into(),
        );
        assert_eq!(args.first().map(String::as_str), Some("copyto"));
        assert!(args.windows(2).any(|pair| pair == ["--stats", "500ms"]));
        assert!(args.iter().any(|arg| arg == "--stats-one-line"));
        assert!(args.iter().any(|arg| arg == "--use-json-log"));
        assert!(args
            .windows(2)
            .any(|pair| pair == ["--multi-thread-streams", "4"]));
    }

    #[test]
    fn backend_optimizations_apply_to_uploads_and_downloads() {
        let profile = TransferProfile::for_provider(ProviderKind::Dropbox, "dropbox").unwrap();
        let builder = RcloneCommandBuilder::new(ProviderKind::Dropbox, "dropbox", profile).unwrap();
        let upload = builder
            .upload_copy_args(
                &TransferTuning::throughput(),
                "/tmp/stage".into(),
                "remote:root".into(),
                true,
            )
            .unwrap();
        let download = builder.download_copyto_args(
            &TransferTuning::throughput(),
            "remote:pack".into(),
            "/tmp/pack".into(),
        );
        for args in [&upload, &download] {
            assert!(args
                .windows(2)
                .any(|pair| pair == ["--dropbox-chunk-size", "64M"]));
            assert!(args
                .windows(2)
                .any(|pair| pair == ["--dropbox-batch-mode", "sync"]));
        }
    }

    #[test]
    fn drive_upload_args_are_scoped_and_concurrency_is_bounded() {
        let profile = TransferProfile::for_provider(ProviderKind::GoogleDrive, "drive").unwrap();
        let builder =
            RcloneCommandBuilder::new(ProviderKind::GoogleDrive, "drive", profile).unwrap();
        let args = builder
            .upload_copy_args(
                &TransferTuning::throughput(),
                "/tmp/stage".into(),
                "remote:root".into(),
                true,
            )
            .unwrap();
        assert!(args
            .windows(2)
            .any(|pair| pair == ["--drive-chunk-size", "64M"]));
        assert!(args
            .windows(2)
            .any(|pair| pair == ["--drive-upload-cutoff", "8M"]));
        assert!(args.windows(2).any(|pair| pair == ["--transfers", "4"]));
    }

    #[test]
    fn bulk_copy_never_exceeds_the_shared_transfer_budget() {
        let profile = TransferProfile::for_provider(ProviderKind::Local, "local").unwrap();
        let builder = RcloneCommandBuilder::new(ProviderKind::Local, "local", profile).unwrap();
        let args = builder
            .upload_copy_args(
                &TransferTuning::throughput(),
                "/tmp/stage".into(),
                "remote:root".into(),
                true,
            )
            .unwrap();
        assert!(args.windows(2).any(|pair| pair == ["--transfers", "4"]));
        assert!(args.windows(2).any(|pair| pair == ["--checkers", "8"]));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn cancelled_command_is_killed_and_reaped_before_its_owner_returns() {
        let mut command = Command::new("sh");
        command
            .args(["-c", "exec sleep 60"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        let child = ReapedChild(command.spawn().unwrap());
        let pid = child.id().unwrap();
        let future = async move {
            let _child = child;
            std::future::pending::<()>().await;
        };
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(10), future)
                .await
                .is_err()
        );
        let status = std::process::Command::new("kill")
            .args(["-0", &pid.to_string()])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .unwrap();
        assert!(
            !status.success(),
            "cancelled child is still running or unreaped"
        );
    }

    #[test]
    fn parses_rclone_one_line_json_stats() {
        assert_eq!(
            parse_rclone_stats_bytes(
                r#"{"bytes":123456,"checks":0,"deletedDirs":0,"elapsedTime":0.5}"#,
            ),
            Some(123_456)
        );
        assert_eq!(parse_rclone_stats_bytes("ordinary rclone error"), None);
        assert_eq!(
            parse_rclone_stats_bytes(r#"{"stats":{"bytes":654321}}"#),
            Some(654_321)
        );
    }

    #[test]
    fn command_builder_rejects_provider_backend_mismatch() {
        let profile = TransferProfile::for_provider(ProviderKind::GoogleDrive, "drive").unwrap();
        assert!(RcloneCommandBuilder::new(ProviderKind::GoogleDrive, "s3", profile).is_err());

        let mut local = TransferProfile::for_provider(ProviderKind::Local, "local").unwrap();
        local.rclone_backend_args.push("--s3-chunk-size".into());
        assert!(RcloneCommandBuilder::new(ProviderKind::Local, "local", local).is_err());
    }

    #[test]
    fn session_rejects_metadata_that_disagrees_with_the_rclone_config() {
        let input = AdapterInput {
            provider: ProviderKind::GoogleDrive,
            remote_name: "noland_drive".into(),
            credentials: AdapterCredential::OAuth2 {
                access_token: "access".into(),
                refresh_token: None,
                expires_at: 1_800_000_000,
            },
            fields: BTreeMap::new(),
            bucket: None,
            prefix: None,
        };
        let mut session = session_from_input(&input, "op-drive", TokenMode::Operation).unwrap();
        session.config_ini = session.config_ini.replacen("type = drive", "type = s3", 1);
        assert!(RcloneStorage::try_from_session(
            &session,
            Path::new("/run/noland/storage/op-drive/rclone.conf"),
        )
        .is_err());
    }

    #[test]
    fn drive_session_ignores_legacy_manual_upload_concurrency() {
        let make_session = |concurrency: &str| {
            session_from_input(
                &AdapterInput {
                    provider: ProviderKind::GoogleDrive,
                    remote_name: "noland_drive".into(),
                    credentials: AdapterCredential::OAuth2 {
                        access_token: "access".into(),
                        refresh_token: None,
                        expires_at: 1_800_000_000,
                    },
                    fields: BTreeMap::from([("upload_concurrency".into(), concurrency.into())]),
                    bucket: None,
                    prefix: None,
                },
                "op-drive",
                TokenMode::Operation,
            )
            .unwrap()
        };
        let storage = RcloneStorage::try_from_session(
            &make_session("1"),
            Path::new("/run/noland/storage/op-drive/rclone.conf"),
        )
        .unwrap();
        assert_eq!(storage.transfer_profile().upload_concurrency, 4);
        assert!(RcloneStorage::try_from_session(
            &make_session("not-a-number"),
            Path::new("/run/noland/storage/op-drive/rclone.conf"),
        )
        .is_ok());
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
        let storage = RcloneStorage::try_from_session(
            &session,
            Path::new("/run/noland/storage/op-42/rclone.conf"),
        )
        .unwrap();
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
