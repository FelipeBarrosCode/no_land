//! Display-only browsing. Freeze selection before any await, coalesce the
//! complete connection/list path, and never turn cached rows into authority
//! for a transfer or deletion.

use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{Arc, Mutex, OnceLock, Weak},
    time::{Duration, Instant},
};

use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::sync::RwLock;
use tracing::Instrument;

use crate::{
    errors::{AppError, AppResult, FrontendError},
    models::app_state::{
        PersistedAppState, SharedStorageBrowseFreshness, SharedStorageBrowseSnapshot,
        SharedStorageObjectEntry, SshState,
    },
    services::{app_context::AppContext, remote_exec::RemoteExec, vast_api::VastApiClient},
};

#[derive(Clone, Copy)]
pub(super) enum BrowseFailure {
    HelperMissing,
    AgentUnavailable,
    AgentIncompatible,
    AgentUpdating,
    ProviderUnavailable,
    ProfileChanged,
    RefreshTimeout,
    HelperInstallFailed,
}

impl BrowseFailure {
    fn code(self) -> &'static str {
        match self {
            Self::HelperMissing => "storage_helper_missing",
            Self::AgentUnavailable => "storage_agent_unavailable",
            Self::AgentIncompatible => "storage_agent_incompatible",
            Self::AgentUpdating => "storage_agent_updating",
            Self::ProviderUnavailable => "storage_provider_unavailable",
            Self::ProfileChanged => "storage_profile_changed",
            Self::RefreshTimeout => "storage_refresh_timeout",
            Self::HelperInstallFailed => "storage_helper_install_failed",
        }
    }

    fn message(self) -> &'static str {
        match self {
            Self::HelperMissing => "The Cloud Storage transfer helper (rclone) is missing on this PC. Explicitly prepare the helper, then refresh. No agent was restarted and no transfer was started.",
            Self::AgentUnavailable => "The storage agent is unavailable. Reconnect this PC before refreshing. Browsing did not install or restart anything.",
            Self::AgentIncompatible => "The running storage agent does not support this browse protocol. Explicitly update the agent before refreshing; browsing will not deploy or restart it.",
            Self::AgentUpdating => "An explicit storage agent update is in progress. Wait for it to finish before refreshing.",
            Self::ProviderUnavailable => "Cloud Storage metadata could not be refreshed. Check the PC connection and provider authorization, then retry.",
            Self::ProfileChanged => "The storage account, profile or repository changed. Refresh the options before continuing.",
            Self::RefreshTimeout => "The storage option refresh timed out. Reconnect this PC or provider, then retry.",
            Self::HelperInstallFailed => "The verified Cloud Storage helper could not be prepared. Reconnect this PC and retry when its storage operations are idle.",
        }
    }
}

pub(super) fn failure(reason: BrowseFailure) -> AppError {
    // Internal tags survive the display-only singleflight's string transport.
    // They carry no provider output and never authorize setup or any mutation.
    AppError::State(format!("[{}] {}", reason.code(), reason.message()))
}

pub(crate) fn frontend_error(error: AppError) -> FrontendError {
    let text = error.to_string();
    let reason = [
        BrowseFailure::HelperMissing,
        BrowseFailure::AgentUnavailable,
        BrowseFailure::AgentIncompatible,
        BrowseFailure::AgentUpdating,
        BrowseFailure::ProviderUnavailable,
        BrowseFailure::ProfileChanged,
        BrowseFailure::RefreshTimeout,
        BrowseFailure::HelperInstallFailed,
    ]
    .into_iter()
    .find(|reason| text.contains(&format!("[{}]", reason.code())))
    .or_else(|| {
        text.contains(SELECTION_CHANGED)
            .then_some(BrowseFailure::ProfileChanged)
    })
    .or_else(|| {
        text.contains("Storage option refresh timed out.")
            .then_some(BrowseFailure::RefreshTimeout)
    })
    .unwrap_or(BrowseFailure::ProviderUnavailable);
    FrontendError {
        code: reason.code().into(),
        message: reason.message().into(),
        details: None,
        retryable: !matches!(reason, BrowseFailure::AgentIncompatible),
    }
}

use super::{
    agent_handoff::{catalog_to_entries, parse_agent_catalog},
    browse_cache::{BrowseCache, SELECTION_CHANGED},
    provider_profiles::repository_key_from_state,
    rclone_adapter::mint_ephemeral_session,
    shared_storage_manager::{ActiveSharedStorageProfile, SharedStorageManager},
};

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum BrowseTarget {
    Cloud,
    RemotePc,
}

type Registry = Mutex<HashMap<usize, (Weak<RwLock<PersistedAppState>>, Arc<BrowseCache>)>>;

fn cache_for(context: &AppContext) -> Arc<BrowseCache> {
    static CACHES: OnceLock<Registry> = OnceLock::new();
    let mut caches = CACHES
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    caches.retain(|_, (context, _)| context.strong_count() > 0);
    let id = Arc::as_ptr(&context.state) as usize;
    caches
        .entry(id)
        .or_insert_with(|| (Arc::downgrade(&context.state), BrowseCache::new()))
        .1
        .clone()
}

pub(crate) fn invalidate_context(context: &AppContext) {
    cache_for(context).invalidate();
}

/// Invalidate both at mutation admission and when its monitor returns, including
/// cancellation/timeout/unknown execution. No cached result authorizes a start.
pub(crate) struct BrowseMutationGuard<'a>(&'a AppContext);
impl<'a> BrowseMutationGuard<'a> {
    pub fn new(context: &'a AppContext) -> Self {
        invalidate_context(context);
        Self(context)
    }
}
impl Drop for BrowseMutationGuard<'_> {
    fn drop(&mut self) {
        invalidate_context(self.0);
    }
}

fn selection_binding(cache: &BrowseCache, state: &PersistedAppState) -> String {
    let mut value = json!({
        "profiles": state.shared_storage_profiles,
        "credentials": state.shared_storage_credentials.profiles,
        "repositoryKey": state.shared_storage_credentials.repository_key_hex,
        "settings": state.shared_storage.settings,
        "account": {
            "vastApiKey": state.credentials.vast_api_key,
            "username": state.credentials.app_username,
        },
        "ssh": state.ssh,
    });
    value.sort_all_objects();
    cache.binding(&value)
}

fn scope_binding(
    context: &AppContext,
    cache: &BrowseCache,
    state: &PersistedAppState,
    target: BrowseTarget,
    instance_id: u64,
) -> (String, u64) {
    let selection = selection_binding(cache, state);
    let revision = cache.observe_selection(selection.clone());
    let server = state
        .provisioned_servers
        .iter()
        .find(|server| server.instance_id == instance_id);
    let current = (state.instance.instance_id == Some(instance_id)).then_some(&state.instance);
    let scope = cache.binding(&json!({
        "selection": selection,
        "revision": revision,
        "target": target,
        "instanceId": instance_id,
        "machineId": server.and_then(|server| server.machine_id.as_deref())
            .or_else(|| current.and_then(|instance| instance.machine_id.as_deref())),
        "provider": context.config.vast_base_url,
        "targetUser": context.config.audio_target_user,
    }));
    (scope, revision)
}

struct FrozenBrowse {
    target: BrowseTarget,
    instance_id: u64,
    api_key: String,
    ssh: SshState,
    active: Option<ActiveSharedStorageProfile>,
    master: Option<noland_crypto::MasterKey>,
}

impl FrozenBrowse {
    fn capture(
        state: &PersistedAppState,
        instance_id: u64,
        target: BrowseTarget,
    ) -> AppResult<Self> {
        let (active, master) = if target == BrowseTarget::Cloud {
            let active = SharedStorageManager::profile_from_state(state, None)?;
            let mut bytes = repository_key_from_state(state, &active.profile.id)?;
            let key = noland_crypto::MasterKey::from_bytes(bytes);
            bytes.fill(0);
            (Some(active), Some(key))
        } else {
            (None, None)
        };
        Ok(Self {
            target,
            instance_id,
            api_key: state.credentials.vast_api_key.clone(),
            ssh: state.ssh.clone(),
            active,
            master,
        })
    }
}

/// `refresh=false`: in-memory only, including scope resolution. `refresh=true`:
/// await a shared fresh read; failed refreshes leave the last snapshot intact.
/// UI code should get the current scope first and compare any later response.
pub async fn get_snapshot(
    context: &AppContext,
    instance_id: u64,
    target: BrowseTarget,
    refresh: bool,
) -> AppResult<SharedStorageBrowseSnapshot> {
    let started = Instant::now();
    let cache = cache_for(context);
    let (scope, revision, frozen) = {
        let state = context.state.read().await;
        let (scope, revision) = scope_binding(context, &cache, &state, target, instance_id);
        let frozen = refresh
            .then(|| FrozenBrowse::capture(&state, instance_id, target))
            .transpose()?;
        (scope, revision, frozen)
    };
    if !refresh {
        let snapshot = cache.peek(&scope);
        tracing::info!(
            event = "shared_storage_browse_request",
            instance_id,
            target = ?target,
            stage = "cache_only",
            hit = snapshot.observed_at.is_some(),
            latency_ms = started.elapsed().as_millis() as u64,
            network_requests = 0u64,
            "Returned private last-known storage options"
        );
        return Ok(snapshot);
    }

    let frozen = frozen.expect("fresh request captures inputs");
    let owned_context = context.clone();
    let owned_scope = scope.clone();
    let owned_cache = cache.clone();
    let outcome = cache.refresh(scope.clone(), revision, move |request_id| async move {
        let started = Instant::now();
        tracing::info!(
            event = "shared_storage_browse_request",
            request_id = %request_id,
            instance_id,
            target = ?target,
            stage = "refresh_start",
            "Refreshing storage options without deployment"
        );
        let result = match tokio::time::timeout(
            Duration::from_secs(90),
            load_frozen(&owned_context, &frozen, &request_id)
                .instrument(tracing::info_span!("shared_storage_browse", request_id = %request_id)),
        ).await {
            Ok(result) => result.map_err(|error| error.to_string()),
            Err(_) => Err("Storage option refresh timed out. Reconnect this PC or provider and retry; last-known options have not been replaced.".to_string()),
        };
        let state = owned_context.state.read().await;
        let (current, current_revision) = scope_binding(&owned_context, &owned_cache, &state, target, instance_id);
        if current != owned_scope || current_revision != revision {
            return Err(SELECTION_CHANGED.into());
        }
        tracing::info!(
            event = "shared_storage_browse_request",
            request_id = %request_id,
            instance_id,
            target = ?target,
            stage = "refresh_complete",
            success = result.is_ok(),
            entry_count = result.as_ref().map(Vec::len).unwrap_or(0),
            latency_ms = started.elapsed().as_millis() as u64,
            "Storage option refresh completed"
        );
        result
    }).await.map_err(AppError::State)?;

    // Cover selection changes after the loader completed but before this waiter
    // resumed. No await or network after this final in-memory binding check.
    let state = context.state.read().await;
    let (current, _) = scope_binding(context, &cache, &state, target, instance_id);
    if current != scope || !cache.is_current(revision) {
        return Err(AppError::State(SELECTION_CHANGED.into()));
    }
    Ok(outcome)
}

/// Compatibility Vec callers share the same complete load and fresh cache.
/// They never silently receive unavailable/cross-profile cached options.
pub async fn list(
    context: &AppContext,
    instance_id: u64,
    target: BrowseTarget,
) -> AppResult<Vec<SharedStorageObjectEntry>> {
    let cached = get_snapshot(context, instance_id, target, false).await?;
    if cached.freshness == SharedStorageBrowseFreshness::Fresh {
        return Ok(cached.entries);
    }
    Ok(get_snapshot(context, instance_id, target, true)
        .await?
        .entries)
}

/// Explicit human-clicked prerequisite preparation. No catalog browse invokes
/// this function. The existing verified installer requires an idle agent and
/// changes only rclone, never an agent, service, repository or game file.
pub async fn prepare_helper(context: &AppContext, instance_id: u64) -> AppResult<()> {
    let _action = crate::services::instance_lifecycle::InstanceLifecycleService::begin_action(
        context,
        instance_id,
        "storage helper preparation",
    )
    .await?;
    let _invalidate = BrowseMutationGuard::new(context);
    let cache = cache_for(context);
    let (scope, revision, frozen) = {
        let state = context.state.read().await;
        let (scope, revision) =
            scope_binding(context, &cache, &state, BrowseTarget::RemotePc, instance_id);
        (
            scope,
            revision,
            FrozenBrowse::capture(&state, instance_id, BrowseTarget::RemotePc)?,
        )
    };
    let remote = resolve_frozen_remote(context, &frozen).await?;
    let configuration = context.state.read().await;
    let (current, _) = scope_binding(
        context,
        &cache,
        &configuration,
        BrowseTarget::RemotePc,
        instance_id,
    );
    if current != scope || !cache.is_current(revision) {
        return Err(failure(BrowseFailure::ProfileChanged));
    }
    // The human-authorized target/SSH identity is now frozen. This installer
    // never reads repository credentials or follows a later profile switch, so
    // do not hold the global settings lock through a potentially slow download.
    drop(configuration);
    SharedStorageManager::ensure_rclone_installed(&remote)
        .await
        .map_err(|_| failure(BrowseFailure::HelperInstallFailed))?;
    Ok(())
}

async fn load_frozen(
    context: &AppContext,
    frozen: &FrozenBrowse,
    request_id: &str,
) -> AppResult<Vec<SharedStorageObjectEntry>> {
    // Existing shared core handles authenticated catalog generations, deletion
    // receipts and immutable metadata. No provider-specific API or new transport.
    if frozen.target == BrowseTarget::Cloud && local_rclone_available() {
        return load_local_metadata(frozen, request_id).await;
    }
    let started = Instant::now();
    let remote = resolve_frozen_remote(context, frozen).await?;
    tracing::info!(
        event = "shared_storage_browse_stage",
        request_id,
        stage = "remote_connection",
        latency_ms = started.elapsed().as_millis() as u64,
        provider_requests = 1u64,
        "Resolved a frozen-account browse connection"
    );
    let started = Instant::now();
    let result = match frozen.target {
        BrowseTarget::Cloud => {
            let active = frozen.active.as_ref().expect("cloud profile");
            let key_hex = BrowseKeyHex(
                hex::encode(frozen.master.as_ref().expect("cloud key").0.as_slice()).into_bytes(),
            );
            let catalog = SharedStorageManager::browse_agent_catalog(
                &remote,
                active,
                std::str::from_utf8(&key_hex.0).expect("hex encoding"),
            )
            .await;
            let catalog = catalog?;
            parse_agent_catalog(&catalog)?;
            Ok(catalog_to_entries(&catalog))
        }
        BrowseTarget::RemotePc => {
            SharedStorageManager::list_agent_apps(
                context,
                &remote,
                &context.config.audio_target_user,
            )
            .await
        }
    };
    tracing::info!(
        event = "shared_storage_browse_stage",
        request_id,
        stage = "agent_list",
        success = result.is_ok(),
        latency_ms = started.elapsed().as_millis() as u64,
        rpc_request_budget = 2u64,
        deployment_requests = 0u64,
        "Read-only storage listing returned"
    );
    result
}

struct BrowseKeyHex(Vec<u8>);
impl Drop for BrowseKeyHex {
    fn drop(&mut self) {
        self.0.fill(0);
    }
}

async fn resolve_frozen_remote(
    context: &AppContext,
    frozen: &FrozenBrowse,
) -> AppResult<RemoteExec> {
    if frozen.api_key.trim().is_empty() {
        return Err(AppError::InvalidInput(
            "Vast API key is missing. Reconnect the account before refreshing remote PC options."
                .into(),
        ));
    }
    if frozen.ssh.private_key_path.trim().is_empty() {
        return Err(AppError::InvalidInput(
            "SSH identity is unavailable. Connect this PC before refreshing its storage options."
                .into(),
        ));
    }
    let vast = VastApiClient::new(
        context.http_client.clone(),
        context.config.vast_base_url.clone(),
        frozen.api_key.clone(),
    );
    let instance = vast.get_instance(frozen.instance_id).await.map_err(|_| AppError::State(
        "This PC is unavailable for storage browsing. Start or reconnect it and refresh; last-known options have not been replaced.".into()
    ))?;
    let host = if instance.public_ip.trim().is_empty() {
        instance.ssh_host
    } else {
        instance.public_ip
    };
    if host.trim().is_empty() || instance.ssh_port == 0 {
        return Err(AppError::State(
            "This PC has no available SSH endpoint. Start or reconnect it and refresh the options."
                .into(),
        ));
    }
    Ok(RemoteExec {
        ssh_host: host,
        ssh_port: instance.ssh_port,
        ssh_user: if frozen.ssh.ssh_username.trim().is_empty() {
            context.config.audio_target_user.clone()
        } else {
            frozen.ssh.ssh_username.clone()
        },
        private_key_path: frozen.ssh.private_key_path.clone(),
        ssh_password: frozen.ssh.ssh_password.clone(),
    })
}

fn local_rclone_available() -> bool {
    // RcloneStorage currently resolves `rclone` on PATH. Do not mutate global
    // PATH, install a tool, or claim a bundled/override binary is usable when the
    // shared provider cannot yet select that executable.
    let name = if cfg!(windows) {
        "rclone.exe"
    } else {
        "rclone"
    };
    std::env::var_os("PATH").is_some_and(|path| {
        std::env::split_paths(&path)
            .any(|directory| directory.is_absolute() && directory.join(name).is_file())
    })
}

struct PrivateRunRoot(PathBuf);
impl PrivateRunRoot {
    fn create() -> AppResult<Self> {
        let path =
            std::env::temp_dir().join(format!("noland-desktop-browse-{}", uuid::Uuid::new_v4()));
        let mut builder = std::fs::DirBuilder::new();
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        builder.create(&path)?;
        Ok(Self(path))
    }
}
impl Drop for PrivateRunRoot {
    fn drop(&mut self) {
        // Only this randomly named, exclusively-created private directory.
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

async fn load_local_metadata(
    frozen: &FrozenBrowse,
    request_id: &str,
) -> AppResult<Vec<SharedStorageObjectEntry>> {
    use noland_storage::SharedStorageProvider;
    let started = Instant::now();
    let active = frozen.active.as_ref().expect("cloud profile");
    let master = frozen.master.as_ref().expect("cloud key");
    let session = mint_ephemeral_session(
        &active.profile.provider,
        &active.credentials,
        &active.provider_fields,
        active.profile.bucket.as_deref(),
        active.profile.prefix.as_deref(),
        &active.remote_name,
        &uuid::Uuid::new_v4().to_string(),
    )?;
    let root = PrivateRunRoot::create()?;
    let (config, _session_guard) =
        noland_storage::write_guarded_ephemeral_session(&root.0, &session).map_err(|_| {
            AppError::State("Could not prepare a private local browse session.".into())
        })?;
    let storage = noland_storage::RcloneStorage::try_from_session(&session, &config)
        .map_err(|_| AppError::State("The connected provider cannot create a metadata browse session. Reconnect it and retry.".into()))?;
    let entries = load_provider_metadata(&storage, master).await?;
    let metrics = storage.operation_metrics();
    tracing::info!(
        event = "shared_storage_browse_stage",
        request_id,
        stage = "local_metadata",
        latency_ms = started.elapsed().as_millis() as u64,
        rclone_invocations = metrics.rclone_invocations,
        remote_stat_calls = metrics.remote_stat_calls,
        remote_list_calls = metrics.remote_list_calls,
        remote_download_calls = metrics.remote_download_calls,
        bytes_downloaded = metrics.bytes_downloaded,
        deployment_requests = 0u64,
        "Authenticated provider-neutral metadata browse completed"
    );
    Ok(entries)
}

async fn load_provider_metadata(
    storage: &dyn noland_storage::SharedStorageProvider,
    master: &noland_crypto::MasterKey,
) -> AppResult<Vec<SharedStorageObjectEntry>> {
    let (catalog, pending) = tokio::try_join!(
        async {
            noland_storage::load_catalog(storage, master).await
                .map_err(|_| AppError::State("Authenticated cloud metadata could not be read. Check provider authorization, repository key and local rclone; last-known options have not been replaced.".into()))
        },
        async {
            noland_storage::pending_deletions(storage, master).await
                .map_err(|_| AppError::State("Pending cloud deletion metadata could not be authenticated. Refresh again before selecting cloud options.".into()))
        },
    )?;
    let mut catalog = serde_json::to_value(catalog)?;
    catalog["pending_deletions"] = json!(pending
        .iter()
        .map(|plan| json!({
            "plan_id": plan.plan_id,
            "names": plan.targets.iter().map(|target| target.name.clone()).collect::<Vec<_>>(),
        }))
        .collect::<Vec<_>>());
    Ok(catalog_to_entries(&catalog))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{
        app_state::SharedStorageProfileSecret,
        application_bundle::{ProfileReference, StorageCredential, StorageProvider},
    };

    #[test]
    fn browse_failures_expose_only_allowlisted_codes_and_safe_messages() {
        for reason in [
            BrowseFailure::HelperMissing,
            BrowseFailure::AgentUnavailable,
            BrowseFailure::AgentIncompatible,
            BrowseFailure::AgentUpdating,
            BrowseFailure::ProviderUnavailable,
            BrowseFailure::ProfileChanged,
            BrowseFailure::RefreshTimeout,
            BrowseFailure::HelperInstallFailed,
        ] {
            let original = failure(reason);
            let transported =
                AppError::State(format!("nested: {original}; bearer=must-not-escape"));
            let error = frontend_error(transported);
            assert_eq!(error.code, reason.code());
            assert_eq!(error.message, reason.message());
            assert!(error.details.is_none());
            assert!(!serde_json::to_string(&error)
                .unwrap()
                .contains("must-not-escape"));
        }
        let error = frontend_error(AppError::Command(
            "provider rejected https://host/?token=must-not-escape".into(),
        ));
        assert_eq!(error.code, "storage_provider_unavailable");
        assert!(error.details.is_none());
        assert!(!serde_json::to_string(&error)
            .unwrap()
            .contains("must-not-escape"));
        assert_eq!(
            frontend_error(AppError::State(SELECTION_CHANGED.into())).code,
            "storage_profile_changed"
        );
    }

    #[tokio::test]
    async fn explicit_helper_preparation_needs_connection_inputs_but_not_repository_secrets() {
        let mut state = PersistedAppState::default();
        state.credentials.vast_api_key = "fixture-account".into();
        // No SSH identity: reject locally without a provider request, tool
        // installation, a state-file read or creating a repository key.
        let context = context(state);
        let error = prepare_helper(&context, 9_999_981).await.unwrap_err();
        assert!(error.to_string().contains("SSH identity is unavailable"));
        assert!(context
            .state
            .read()
            .await
            .shared_storage_credentials
            .repository_key_hex
            .is_empty());
    }

    struct NoDiskStore(PathBuf);
    #[async_trait::async_trait]
    impl crate::services::state_store::StateStore for NoDiskStore {
        async fn load_state(&self) -> AppResult<PersistedAppState> {
            panic!("browse snapshots must not read a state file");
        }
        async fn save_state(&self, _: &PersistedAppState) -> AppResult<()> {
            panic!("browse snapshots must not persist state");
        }
        fn path(&self) -> &std::path::Path {
            &self.0
        }
    }

    fn context(state: PersistedAppState) -> AppContext {
        AppContext::new(
            crate::services::app_config::AppConfig::default(),
            Arc::new(NoDiskStore(
                std::env::temp_dir().join("desktop-browse-no-disk.fixture"),
            )),
            state,
        )
    }

    fn state() -> PersistedAppState {
        let mut state = PersistedAppState::default();
        state.credentials.vast_api_key = "account-a".into();
        state.shared_storage_profiles.push(ProfileReference {
            id: "profile-a".into(),
            display_name: "Provider".into(),
            provider_label: "SFTP".into(),
            provider: Some(StorageProvider::Sftp),
            bucket: None,
            prefix: None,
            active: true,
        });
        state.shared_storage_credentials.repository_key_hex = hex::encode([7; 32]);
        state.shared_storage_credentials.profiles.insert(
            "profile-a".into(),
            SharedStorageProfileSecret {
                credentials: StorageCredential::UsernamePassword {
                    username: "account-a".into(),
                    password: "not-retained-in-cache".into(),
                },
                provider_fields: HashMap::from([
                    ("host".into(), "sftp.invalid".into()),
                    ("remote_path".into(), "/repo".into()),
                ]),
                repository_key_hex: String::new(),
            },
        );
        state
    }

    #[test]
    fn desktop_browse_private_bindings_change_with_account_profile_root_and_key() {
        let cache = BrowseCache::new();
        let original = state();
        let binding = selection_binding(&cache, &original);
        let mut other = original.clone();
        other.credentials.vast_api_key = "account-b".into();
        assert_ne!(selection_binding(&cache, &other), binding);
        let mut other = original.clone();
        other.shared_storage_profiles[0].id = "profile-b".into();
        assert_ne!(selection_binding(&cache, &other), binding);
        let mut other = original.clone();
        // Absolute and relative SFTP roots are distinct repositories.
        other
            .shared_storage_credentials
            .profiles
            .get_mut("profile-a")
            .unwrap()
            .provider_fields
            .insert("remote_path".into(), "repo".into());
        assert_ne!(selection_binding(&cache, &other), binding);
        let mut other = original.clone();
        other.shared_storage_credentials.repository_key_hex = hex::encode([8; 32]);
        assert_ne!(selection_binding(&cache, &other), binding);
        let mut other = original.clone();
        other
            .shared_storage_credentials
            .profiles
            .get_mut("profile-a")
            .unwrap()
            .credentials = StorageCredential::UsernamePassword {
            username: "account-b".into(),
            password: "different".into(),
        };
        assert_ne!(selection_binding(&cache, &other), binding);
        assert_eq!(selection_binding(&cache, &original), binding);
    }

    #[test]
    fn desktop_browse_freezes_session_and_key_from_one_in_memory_selection() {
        let mut state = state();
        let frozen = FrozenBrowse::capture(&state, 9, BrowseTarget::Cloud).unwrap();
        state.shared_storage_profiles[0].prefix = Some("different".into());
        state.shared_storage_credentials.repository_key_hex = hex::encode([8; 32]);
        state.credentials.vast_api_key = "account-b".into();
        assert_eq!(frozen.api_key, "account-a");
        assert_eq!(frozen.active.unwrap().profile.prefix, None);
        assert_eq!(frozen.master.unwrap().0, [7; 32]);
    }

    #[test]
    fn desktop_browse_missing_profile_is_not_a_successful_empty_cloud_refresh() {
        assert!(
            FrozenBrowse::capture(&PersistedAppState::default(), 9, BrowseTarget::Cloud).is_err()
        );
        assert!(
            FrozenBrowse::capture(&PersistedAppState::default(), 9, BrowseTarget::RemotePc).is_ok()
        );
    }

    #[tokio::test]
    async fn desktop_browse_cache_only_works_offline_without_profile_keys_or_disk_access() {
        let context = context(PersistedAppState::default());
        for target in [BrowseTarget::Cloud, BrowseTarget::RemotePc] {
            let first = get_snapshot(&context, 9, target, false).await.unwrap();
            assert_eq!(first.freshness, SharedStorageBrowseFreshness::Unavailable);
            assert!(first.entries.is_empty());
            assert!(first.observed_at.is_none());
            let again = get_snapshot(&context, 9, target, false).await.unwrap();
            assert_eq!(first.scope, again.scope);
        }
        let cloud = get_snapshot(&context, 9, BrowseTarget::Cloud, false)
            .await
            .unwrap();
        let pc = get_snapshot(&context, 9, BrowseTarget::RemotePc, false)
            .await
            .unwrap();
        assert_ne!(cloud.scope, pc.scope);
        assert!(get_snapshot(&context, 9, BrowseTarget::Cloud, true)
            .await
            .is_err());
        // No account/SSH identity means this fails before any network request.
        assert!(get_snapshot(&context, 9, BrowseTarget::RemotePc, true)
            .await
            .is_err());
    }

    #[tokio::test]
    async fn desktop_browse_snapshot_rejects_cached_rows_after_settings_profile_account_or_key_changes(
    ) {
        let context = context(state());
        let cache = cache_for(&context);
        for change in 0..4 {
            let before = get_snapshot(&context, 9, BrowseTarget::Cloud, false)
                .await
                .unwrap();
            let revision = {
                let state = context.state.read().await;
                scope_binding(&context, &cache, &state, BrowseTarget::Cloud, 9).1
            };
            cache
                .refresh(before.scope.clone(), revision, |_| async {
                    Ok(vec![SharedStorageObjectEntry {
                        path: "/catalog/private".into(),
                        name: "private".into(),
                        parent_path: "/catalog".into(),
                        is_dir: true,
                    }])
                })
                .await
                .unwrap();
            assert!(get_snapshot(&context, 9, BrowseTarget::Cloud, false)
                .await
                .unwrap()
                .observed_at
                .is_some());
            {
                let mut state = context.state.write().await;
                match change {
                    0 => state.shared_storage.settings.destination_prefix = "different".into(),
                    1 => state.shared_storage_profiles[0].active = false,
                    2 => state.credentials.vast_api_key = "other-account".into(),
                    _ => state.shared_storage_credentials.repository_key_hex = hex::encode([9; 32]),
                }
            }
            let after = get_snapshot(&context, 9, BrowseTarget::Cloud, false)
                .await
                .unwrap();
            assert_ne!(before.scope, after.scope);
            assert!(after.entries.is_empty());
            assert_eq!(after.freshness, SharedStorageBrowseFreshness::Unavailable);
        }
    }

    #[tokio::test]
    async fn desktop_browse_native_refresh_error_preserves_visible_cached_snapshot() {
        let context = context(PersistedAppState::default());
        let cache = cache_for(&context);
        let before = get_snapshot(&context, 9, BrowseTarget::RemotePc, false)
            .await
            .unwrap();
        let revision = {
            let state = context.state.read().await;
            scope_binding(&context, &cache, &state, BrowseTarget::RemotePc, 9).1
        };
        let observed = cache
            .refresh(before.scope.clone(), revision, |_| async {
                Ok(vec![SharedStorageObjectEntry {
                    path: "/apps/private".into(),
                    name: "last known".into(),
                    parent_path: "/apps".into(),
                    is_dir: true,
                }])
            })
            .await
            .unwrap();
        assert!(get_snapshot(&context, 9, BrowseTarget::RemotePc, true)
            .await
            .is_err());
        let after = get_snapshot(&context, 9, BrowseTarget::RemotePc, false)
            .await
            .unwrap();
        assert_eq!(observed.entries, after.entries);
        assert_eq!(observed.observed_at, after.observed_at);
        assert_eq!(after.freshness, SharedStorageBrowseFreshness::Cached);
    }

    #[test]
    fn desktop_browse_private_run_directory_is_exclusive_and_cleaned_on_drop() {
        let root = PrivateRunRoot::create().unwrap();
        let path = root.0.clone();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o700
            );
        }
        std::fs::write(path.join("owned-fixture"), b"fixture").unwrap();
        drop(root);
        assert!(!path.exists());
    }

    #[tokio::test]
    async fn desktop_browse_shared_core_authenticates_catalog_and_applies_current_deletion_receipts(
    ) {
        use noland_state_core::{
            AppId, BackupMode, CatalogBundle, CatalogDocument, COMMITTED_MARKER,
        };
        let root = PrivateRunRoot::create().unwrap();
        let master = noland_crypto::MasterKey::from_bytes([7; 32]);
        let wrong_key = noland_crypto::MasterKey::from_bytes([8; 32]);
        let mut catalog = CatalogDocument::empty();
        let bundle_id = uuid::Uuid::new_v4();
        let commit_id = uuid::Uuid::new_v4();
        catalog.upsert_bundle(
            AppId("example".into()),
            "Example".into(),
            CatalogBundle {
                bundle_id,
                commit_id,
                parent_bundle_id: None,
                captured_at: chrono::Utc::now(),
                source_instance_id: uuid::Uuid::new_v4(),
                mode: BackupMode::CompleteApplication,
                logical_size: 1,
                stored_incremental_size: 1,
            },
        );
        let keys = noland_crypto::derive_keys(&master);
        let sealed = noland_storage::seal_metadata(&keys.catalog, b"catalog", &catalog).unwrap();
        let commits = root.0.join("catalog/commits");
        std::fs::create_dir_all(&commits).unwrap();
        std::fs::write(
            commits.join(format!("{}.enc", catalog.catalog_commit_id)),
            sealed.envelope,
        )
        .unwrap();
        std::fs::write(
            commits.join(format!("{}.{COMMITTED_MARKER}", catalog.catalog_commit_id)),
            COMMITTED_MARKER,
        )
        .unwrap();
        std::fs::write(
            root.0.join("catalog/LATEST"),
            catalog.catalog_commit_id.to_string(),
        )
        .unwrap();
        let storage = noland_storage::LocalStorage::new(&root.0);
        let entries = load_provider_metadata(&storage, &master).await.unwrap();
        let selected = format!("/catalog/example/{bundle_id}");
        assert!(entries.iter().any(|entry| entry.path == selected));
        assert!(
            load_provider_metadata(&storage, &wrong_key).await.is_err(),
            "a wrong key must never become a successful empty catalog"
        );

        let deletion = noland_storage::BundleDeletion {
            app_id: AppId("example".into()),
            bundle_id,
            commit_id,
        };
        let sealed =
            noland_storage::seal_metadata(&keys.catalog, b"bundle-deletion-v1", &deletion).unwrap();
        let deletions = root.0.join("deletions/bundles");
        std::fs::create_dir_all(&deletions).unwrap();
        std::fs::write(deletions.join(format!("{bundle_id}.enc")), sealed.envelope).unwrap();
        let entries = load_provider_metadata(&storage, &master).await.unwrap();
        assert!(!entries.iter().any(|entry| entry.path == selected));
    }
}
