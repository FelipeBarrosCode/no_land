//! Human-approved metadata-only recovery. Never installs tools, deploys/restarts
//! agents, submits a transfer, clears deletion plans or reads credential files.
mod drive;

use super::{
    agent_runtime::{call_agent_raw, repository_admission_idle},
    provider_profiles::shared_profile_manager,
    rclone_adapter::mint_ephemeral_session,
    shared_storage_manager::SharedStorageManager,
};
use crate::{
    errors::{AppError, AppResult},
    models::{
        app_state::PersistedAppState,
        application_bundle::{StorageCredential, StorageProvider},
    },
    services::{
        app_context::AppContext, instance_lifecycle::InstanceLifecycleService,
        remote_exec::RemoteExec, vast_api::VastApiClient,
    },
};
use chrono::Utc;
use drive::DriveMetadata;
use noland_crypto::MasterKey;
use noland_storage::{
    repository_recovery::{
        recover_repository_activity_preserving, resume_repository_activity,
        validate_resume_snapshot, RecoveryAuthorization, RecoveryCheckpoint, RecoveryMetadataStore,
        RECOVERY_OWNER_PREFIX,
    },
    RepositoryAccess, RepositoryActivity,
};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::{HashMap, HashSet},
    sync::OnceLock,
    time::Duration,
};
use tauri::Manager;
use tokio::sync::Mutex;
use uuid::Uuid;

const PREVIEW_TTL: i64 = 300;
const MAX_PREVIEWS: usize = 16;

#[derive(Clone)]
struct Preview {
    instance_id: u64,
    binding: String,
    state_binding: String,
    profile_id: String,
    root_id: String,
    activity: RepositoryActivity,
    retained_activities: Vec<RepositoryActivity>,
    created_at: i64,
    recovery_id: Uuid,
    resume: Option<Resume>,
}

#[derive(Clone)]
struct Resume {
    failed_job_id: String,
    checkpoint: RecoveryCheckpoint,
}

fn verified_readback_failure(job: &noland_mcp::core::Job, instance_id: u64) -> AppResult<Uuid> {
    if job.tool != "storage_lock_recover"
        || job.instance_id != Some(instance_id)
        || job.status != "failed"
        || job.finished_at.is_none()
        || job.active()
    {
        return Err(AppError::State(
            "The original recovery execution is not verified stopped.".into(),
        ));
    }
    let error = job.error.as_deref().unwrap_or_default();
    let prefix = "State error: Lock recovery did not confirm completion: integrity: ";
    let suffix = " Do not automatically retry; inspect the repository and recovery receipt ";
    let id = error
        .strip_prefix(prefix)
        .and_then(|message| {
            ["activity changed while authenticating.", "new recovery metadata did not stabilize."]
                .iter()
                .find_map(|reason| message.strip_prefix(reason))
        })
        .and_then(|message| message.strip_prefix(suffix))
        .and_then(|message| message.strip_suffix('.'))
        .and_then(|id| Uuid::parse_str(id).ok())
        .ok_or_else(|| {
            AppError::State("Only an exactly correlated terminal metadata-readback failure can resume here; unknown or unsettled provider mutations are rejected.".into())
        })?;
    Ok(id)
}

async fn stopped_owner(app: &tauri::AppHandle, job_id: &str, instance_id: u64) -> AppResult<Uuid> {
    // The local core's terminal record proves the original execution returned.
    // Its error suffix only correlates identity; it never grants permission.
    // Authenticated receipts, fresh liveness and native human approval remain
    // mandatory. Cancelled/interrupted jobs and network timeouts are rejected.
    let result = app
        .state::<crate::mcp::McpManager>()
        .core
        .call(
            "job_get",
            json!({"job_id":job_id}).as_object().unwrap().clone(),
        )
        .await
        .map_err(AppError::State)?;
    let job: noland_mcp::core::Job = serde_json::from_value(result)?;
    verified_readback_failure(&job, instance_id)
}

static PREVIEWS: OnceLock<Mutex<HashMap<Uuid, Preview>>> = OnceLock::new();
fn previews() -> &'static Mutex<HashMap<Uuid, Preview>> {
    PREVIEWS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn storage_error(error: impl std::fmt::Display) -> AppError {
    AppError::State(error.to_string())
}

fn select_recovery_claim(
    mut activities: Vec<RepositoryActivity>,
    claim_id: Uuid,
) -> AppResult<(RepositoryActivity, Vec<RepositoryActivity>)> {
    if activities.len() > 1024 {
        return Err(AppError::State("Activity inventory exceeds ordinary admission capacity; inspect the original recovery execution before continuing.".into()));
    }
    let mut ids = HashSet::new();
    for activity in &activities {
        if activity.claim_id.is_nil()
            || activity.operation_id.is_nil()
            || activity.owner.trim().is_empty()
            || !ids.insert(activity.claim_id)
            || activity.instance_id.is_some_and(|id| id.is_nil())
            || (activity.access == RepositoryAccess::RestoreRead && activity.instance_id.is_none())
        {
            return Err(AppError::State(
                "Invalid or ambiguous authenticated activity snapshot; recovery stopped.".into(),
            ));
        }
        if activity.owner.starts_with(RECOVERY_OWNER_PREFIX) {
            return Err(AppError::State("An unresolved recovery execution exists; resume its original authenticated plan before preparing another recovery.".into()));
        }
    }
    let index = activities
        .iter()
        .position(|activity| activity.claim_id == claim_id)
        .ok_or_else(|| {
            AppError::State("The selected claim changed or is unavailable; inspect again.".into())
        })?;
    let target = activities.remove(index);
    activities.sort_by_key(|activity| activity.claim_id);
    Ok((target, activities))
}

fn check_preview(preview: &Preview, instance_id: u64, now: i64) -> AppResult<()> {
    if preview.instance_id != instance_id
        || now < preview.created_at
        || now.saturating_sub(preview.created_at) > PREVIEW_TTL
    {
        return Err(AppError::InvalidInput(
            "Lock recovery preview expired or belongs to another PC; inspect again.".into(),
        ));
    }
    Ok(())
}

fn state_binding(state: &PersistedAppState) -> AppResult<String> {
    // Private digest, never returned to callers. The matching native read
    // guard freezes account/profile/key configuration throughout mutation.
    let mut value = json!({
        "profiles":state.shared_storage_profiles,
        "credentials":state.shared_storage_credentials,
        "provider_account":state.credentials.vast_api_key,
    });
    value.sort_all_objects();
    Ok(hex::encode(Sha256::digest(serde_json::to_vec(&value)?)))
}

async fn connect(context: &AppContext) -> AppResult<(DriveMetadata, String, String, String)> {
    let initial_state_binding = state_binding(&context.load_state().await)?;
    let active = SharedStorageManager::resolve_active_profile(context).await?;
    if active.profile.provider != StorageProvider::GoogleDrive {
        return Err(AppError::InvalidInput(
            "Authenticated lock recovery currently supports Google Drive only; other providers remain unchanged.".into(),
        ));
    }
    let StorageCredential::OAuth2 { access_token, .. } = &active.credentials else {
        return Err(AppError::State(
            "Google Drive authorization is unavailable.".into(),
        ));
    };
    let key = shared_profile_manager()
        .retrieve_repository_key(context, &active.profile)
        .await?;
    // Use exactly the same repository-root resolution as ordinary transfers.
    let session = mint_ephemeral_session(
        &active.profile.provider,
        &active.credentials,
        &active.provider_fields,
        active.profile.bucket.as_deref(),
        active.profile.prefix.as_deref(),
        &active.remote_name,
        &Uuid::new_v4().to_string(),
    )?;
    let drive = DriveMetadata::connect(
        access_token.clone(),
        MasterKey::from_bytes(key),
        &session.root,
    )
    .await
    .map_err(storage_error)?;
    let state = context.load_state().await;
    let current_state_binding = state_binding(&state)?;
    if current_state_binding != initial_state_binding {
        return Err(AppError::State(
            "Storage/account settings changed or authorization refreshed during inspection; inspect again before recovery.".into(),
        ));
    }
    if state.credentials.vast_api_key.trim().is_empty() {
        return Err(AppError::State(
            "Provider inventory authorization is unavailable.".into(),
        ));
    }
    let binding = hex::encode(Sha256::digest(serde_json::to_vec(&json!({
        "profile_id":active.profile.id,
        "repository_root_id":drive.root_id,
        "repository_key":hex::encode(Sha256::digest(key)),
        "provider_account":hex::encode(Sha256::digest(state.credentials.vast_api_key.as_bytes())),
        "state_binding":current_state_binding,
    }))?));
    Ok((drive, binding, current_state_binding, active.profile.id))
}

async fn observation(
    context: &AppContext,
    remote: &RemoteExec,
    instance_id: u64,
    confirmed: bool,
) -> AppResult<RecoveryAuthorization> {
    let checked_at = Utc::now().timestamp();
    let state = context.load_state().await;
    let key = state.credentials.vast_api_key.clone();
    if key.trim().is_empty() {
        return Err(AppError::State(
            "Complete provider inventory cannot be verified.".into(),
        ));
    }
    drop(state);
    let vast = VastApiClient::new(
        context.http_client.clone(),
        context.config.vast_base_url.clone(),
        key,
    );
    // NOT get_rented_instances/MCP instances_list: those hide stopped and
    // unregistered PCs and therefore cannot prove that foreign writers stopped.
    let (instances, health, local_admission_unlocked) =
        tokio::time::timeout(Duration::from_secs(30), async {
            tokio::try_join!(
                vast.list_instances(),
                call_agent_raw(remote, "GetHealth", json!({})),
                repository_admission_idle(remote),
            )
        })
        .await
        .map_err(|_| {
            AppError::Timeout(
                "Lock recovery liveness checks timed out; no recovery was started.".into(),
            )
        })??;
    let mut ids = instances
        .iter()
        .map(|instance| instance.id)
        .collect::<Vec<_>>();
    ids.sort_unstable();
    let active_operations = health
        .get("active_operations")
        .and_then(Value::as_u64)
        .ok_or_else(|| {
            AppError::State("Agent did not provide an active-operation count.".into())
        })?;
    Ok(RecoveryAuthorization {
        instance_id,
        all_owned_instance_ids: ids,
        active_operations,
        local_admission_unlocked,
        checked_at,
        no_other_writers_confirmed: confirmed,
    })
}

pub async fn inspect(
    context: &AppContext,
    remote: &RemoteExec,
    instance_id: u64,
) -> AppResult<Value> {
    let (drive, _, _, _) = connect(context).await?;
    let (activities, pending) = tokio::try_join!(drive.activities(), drive.has_pending_deletion(),)
        .map_err(storage_error)?;
    let proof = observation(context, remote, instance_id, false).await?;
    let mut checked_proof = proof.clone();
    // Capability indication only, not authorization. The execution path still
    // requires an explicit argument AND native MCP approval before mutations.
    checked_proof.no_other_writers_confirmed = true;
    let can_prepare = checked_proof.validate(Utc::now().timestamp()).is_ok()
        && !pending
        && activities.first().is_some_and(|activity| {
            select_recovery_claim(activities.clone(), activity.claim_id).is_ok()
        });
    Ok(json!({
        "instance_id":instance_id,
        "activities":activities,
        "pending_deletion_metadata":pending,
        "active_operations":proof.active_operations,
        "local_admission_unlocked":proof.local_admission_unlocked,
        "owned_provider_instance_count":proof.all_owned_instance_ids.len(),
        "sole_owned_instance_is_target":proof.all_owned_instance_ids==[instance_id],
        "can_prepare_recovery":can_prepare,
        "ownership_note":"Legacy owner hashes do not identify a PC. Recovery requires every other provider instance to be absent, an idle current agent, and human confirmation that no other clients or external writers are active.",
        "repository_changes_made":false,
    }))
}

pub async fn prepare(
    app: &tauri::AppHandle,
    context: &AppContext,
    remote: &RemoteExec,
    instance_id: u64,
    claim_id: &str,
    failed_job_id: Option<&str>,
) -> AppResult<Value> {
    let claim_id = Uuid::parse_str(claim_id)
        .map_err(|_| AppError::InvalidInput("Invalid repository claim ID.".into()))?;
    let (drive, binding, state_binding, profile_id) = connect(context).await?;
    let activities = drive.activities().await.map_err(storage_error)?;
    if drive.has_pending_deletion().await.map_err(storage_error)? {
        return Err(AppError::State(
            "Resolve existing deletion metadata before lock recovery.".into(),
        ));
    }
    let mut proof = observation(context, remote, instance_id, false).await?;
    proof.no_other_writers_confirmed = true; // validates readiness, not human authorization
    proof
        .validate(Utc::now().timestamp())
        .map_err(storage_error)?;
    let (activity, retained_activities, recovery_id, resume) = if let Some(job_id) = failed_job_id {
        let recovery_id = stopped_owner(app, job_id, instance_id).await?;
        let checkpoint = drive.checkpoint(recovery_id).await.map_err(storage_error)?;
        if checkpoint.prepared.target.claim_id != claim_id {
            return Err(AppError::State(
                "Original prepared recovery targets another claim.".into(),
            ));
        }
        let retained = activities
            .iter()
            .filter(|activity| {
                activity.claim_id != checkpoint.prepared.target.claim_id
                    && activity.claim_id != recovery_id
            })
            .cloned()
            .collect();
        validate_resume_snapshot(&checkpoint, &proof, true, activities).map_err(storage_error)?;
        (
            checkpoint.prepared.target.clone(),
            retained,
            recovery_id,
            Some(Resume {
                failed_job_id: job_id.into(),
                checkpoint,
            }),
        )
    } else {
        let (activity, retained) = select_recovery_claim(activities, claim_id)?;
        (activity, retained, Uuid::new_v4(), None)
    };
    let id = Uuid::new_v4();
    let preview = Preview {
        instance_id,
        binding,
        state_binding,
        profile_id,
        root_id: drive.root_id.clone(),
        activity,
        retained_activities,
        created_at: Utc::now().timestamp(),
        recovery_id,
        resume,
    };
    let result = public_preview(id, &preview);
    let mut cache = previews().lock().await;
    let now = Utc::now().timestamp();
    cache.retain(|_, value| check_preview(value, value.instance_id, now).is_ok());
    if cache.len() >= MAX_PREVIEWS {
        return Err(AppError::State(
            "Too many recovery previews; wait for older previews to expire.".into(),
        ));
    }
    cache.insert(id, preview);
    Ok(result)
}

fn public_preview(id: Uuid, preview: &Preview) -> Value {
    json!({
        "preview_id":id,
        "instance_id":preview.instance_id,
        "claim":preview.activity,
        "retained_claim_count":preview.retained_activities.len(),
        "profile_id":preview.profile_id,
        "repository_root_id":preview.root_id,
        "recovery_id":preview.recovery_id,
        "resume_from_job":preview.resume.as_ref().map(|resume|resume.failed_job_id.as_str()),
        "mode":if preview.resume.is_some() {"resume_original_plan"} else {"new_recovery"},
        "expires_at":preview.created_at + PREVIEW_TTL,
        "scope":if preview.resume.is_some() {
            "Resume ONLY this original authenticated recovery plan and recovery ID. Do not create another plan. Remove its target only if not already completed, then release only its existing fence. Retain every other frozen activity claim. Preserve all backups and local files; never start a transfer."
        } else {
            "Remove exactly this authenticated repository activity object. Retain every other frozen activity claim. Preserve all games, packs, bundles, manifests, deletion plans and local files. Do not start a transfer."
        },
        "required_confirmation":"I verified that all other PCs/clients and external writers using this repository have stopped and their prior provider requests have settled. This legacy owner hash cannot be mapped automatically to a PC.",
        "safety":"All owned provider instances, including stopped/unregistered ones, and current active operations are rechecked. Changed claims, profiles/accounts or pending deletions block recovery. An uncertain attempt is never automatically retried.",
    })
}

pub async fn approval_preview(instance_id: u64, preview_id: &str) -> AppResult<Value> {
    let id = Uuid::parse_str(preview_id)
        .map_err(|_| AppError::InvalidInput("Invalid recovery preview ID.".into()))?;
    let cache = previews().lock().await;
    let preview = cache.get(&id).ok_or_else(|| {
        AppError::State("Recovery preview is unavailable or already consumed.".into())
    })?;
    check_preview(preview, instance_id, Utc::now().timestamp())?;
    Ok(public_preview(id, preview))
}

pub async fn recover(
    app: &tauri::AppHandle,
    context: &AppContext,
    remote: &RemoteExec,
    instance_id: u64,
    preview_id: &str,
    confirm_no_other_writers: bool,
) -> AppResult<Value> {
    if !confirm_no_other_writers {
        return Err(AppError::InvalidInput(
            "Explicit stopped-writer confirmation is required.".into(),
        ));
    }
    let _action =
        InstanceLifecycleService::begin_action(context, instance_id, "storage lock recovery")
            .await?;
    let id = Uuid::parse_str(preview_id)
        .map_err(|_| AppError::InvalidInput("Invalid recovery preview ID.".into()))?;
    // Consume before any cloud mutation. A lost response cannot replay a new
    // destructive attempt against an uncertain prior execution.
    let preview = previews().lock().await.remove(&id).ok_or_else(|| {
        AppError::State(
            "Preview unavailable or already attempted; inspect before continuing.".into(),
        )
    })?;
    check_preview(&preview, instance_id, Utc::now().timestamp())?;
    let (drive, binding, current_state_binding, _) = connect(context).await?;
    if preview.binding != binding {
        return Err(AppError::State("Storage profile, repository key/root or provider account changed; prepare a new preview.".into()));
    }
    let resume_checkpoint = if let Some(resume) = &preview.resume {
        let id = stopped_owner(app, &resume.failed_job_id, instance_id).await?;
        if id != preview.recovery_id {
            return Err(AppError::State(
                "Original recovery execution identity changed.".into(),
            ));
        }
        let current = drive.checkpoint(id).await.map_err(storage_error)?;
        if current != resume.checkpoint {
            return Err(AppError::State(
                "Original recovery receipts changed; inspect again.".into(),
            ));
        }
        Some(current)
    } else {
        None
    };
    let proof = observation(context, remote, instance_id, confirm_no_other_writers).await?;
    proof
        .validate(Utc::now().timestamp())
        .map_err(storage_error)?;
    let configuration_guard = context.state.read().await;
    if state_binding(&configuration_guard)? != current_state_binding
        || preview.state_binding != current_state_binding
    {
        return Err(AppError::State(
            "Storage/account settings changed after verification; inspect again before recovery."
                .into(),
        ));
    }
    let receipt = tokio::time::timeout(
        Duration::from_secs(180),
        async {
            if let Some(checkpoint) = resume_checkpoint {
                resume_repository_activity(&drive, checkpoint, proof, true).await
            } else {
                recover_repository_activity_preserving(
                    &drive, preview.recovery_id, preview.activity,
                    preview.retained_activities, proof,
                ).await
            }
        },
    ).await.map_err(|_| AppError::Timeout(format!(
        "Lock recovery outcome is uncertain; do not retry automatically. Inspect recovery receipt {}.",
        preview.recovery_id,
    )))?.map_err(|error| AppError::State(format!(
        "Lock recovery did not confirm completion: {error}. Do not automatically retry; inspect the repository and recovery receipt {}.",
        preview.recovery_id,
    )))?;
    drop(configuration_guard);
    Ok(json!({
        "cleared_claim_id":receipt.target.claim_id,
        "operation_id":receipt.target.operation_id,
        "recovery_id":receipt.recovery_id,
        "absence_verified":true,
        "backups_deleted":false,
        "transfer_started":false,
        "receipt":receipt,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn activity(owner: &str) -> RepositoryActivity {
        RepositoryActivity {
            claim_id: Uuid::new_v4(),
            operation_id: Uuid::new_v4(),
            owner: owner.into(),
            access: RepositoryAccess::RestoreRead,
            instance_id: Some(Uuid::new_v4()),
        }
    }

    #[test]
    fn one_recovery_selection_retains_other_frozen_claims_without_bulk_clearing() {
        let selected = activity("selected instance");
        let other = activity("retained instance");
        let (target, retained) =
            select_recovery_claim(vec![other.clone(), selected.clone()], selected.claim_id)
                .unwrap();
        assert_eq!(target, selected);
        assert_eq!(retained, vec![other]);
    }

    #[test]
    fn recovery_selection_rejects_ambiguous_scopes_and_another_recovery_fence() {
        let selected = activity("selected");
        assert!(
            select_recovery_claim(vec![selected.clone(), selected.clone()], selected.claim_id)
                .is_err()
        );
        let mut invalid = activity("malformed reader");
        invalid.instance_id = None;
        assert!(select_recovery_claim(vec![selected.clone(), invalid], selected.claim_id).is_err());
        let mut fence = activity(&format!("{RECOVERY_OWNER_PREFIX}{}", Uuid::new_v4()));
        fence.access = RepositoryAccess::Exclusive;
        assert!(select_recovery_claim(vec![selected.clone(), fence], selected.claim_id).is_err());
        assert!(select_recovery_claim(vec![selected], Uuid::new_v4()).is_err());
    }

    #[test]
    fn full_capacity_selection_leaves_exactly_one_slot_for_its_recovery_fence() {
        let claims = (0..1024).map(|_| activity("instance")).collect::<Vec<_>>();
        let id = claims[0].claim_id;
        let (_, retained) = select_recovery_claim(claims.clone(), id).unwrap();
        assert_eq!(retained.len(), 1023);
        let mut overflow = claims;
        overflow.push(activity("overflow"));
        assert!(select_recovery_claim(overflow, id).is_err());
    }

    #[test]
    fn resume_requires_our_terminal_failed_job_and_a_correlated_known_readback_failure() {
        let id = Uuid::new_v4();
        let job=noland_mcp::core::Job {
            id:Uuid::new_v4().to_string(),tool:"storage_lock_recover".into(),
            instance_id:Some(42),as_root:false,status:"failed".into(),
            started_at:"2026-11-15T13:35:51Z".into(),finished_at:Some("2026-11-15T13:36:04Z".into()),
            result:None,error:Some(format!("State error: Lock recovery did not confirm completion: integrity: activity changed while authenticating. Do not automatically retry; inspect the repository and recovery receipt {id}.")),
            request_id:None,argument_hash:None,
        };
        assert_eq!(verified_readback_failure(&job, 42).unwrap(), id);
        assert!(verified_readback_failure(&job, 43).is_err());
        let mut altered = job.clone();
        altered.status = "running".into();
        assert!(verified_readback_failure(&altered, 42).is_err());
        let mut altered = job.clone();
        altered.status = "cancelled".into();
        assert!(verified_readback_failure(&altered, 42).is_err());
        let mut altered = job.clone();
        altered.error = Some(format!("Network timeout; recovery receipt {id}."));
        assert!(verified_readback_failure(&altered, 42).is_err());
        let mut altered = job;
        altered.tool = "ssh_exec".into();
        assert!(verified_readback_failure(&altered, 42).is_err());
    }
    #[test]
    fn previews_are_short_lived_and_frozen_to_one_pc() {
        let preview = Preview {
            instance_id: 42,
            binding: "private binding".into(),
            state_binding: "private state binding".into(),
            profile_id: "profile".into(),
            root_id: "repository-folder".into(),
            activity: RepositoryActivity {
                claim_id: Uuid::new_v4(),
                operation_id: Uuid::new_v4(),
                owner: "owner".into(),
                instance_id: None,
                access: noland_storage::RepositoryAccess::Exclusive,
            },
            retained_activities: Vec::new(),
            created_at: 1000,
            recovery_id: Uuid::new_v4(),
            resume: None,
        };
        check_preview(&preview, 42, 1000).unwrap();
        check_preview(&preview, 42, 1300).unwrap();
        assert!(check_preview(&preview, 42, 1301).is_err());
        assert!(check_preview(&preview, 42, 999).is_err());
        assert!(check_preview(&preview, 43, 1000).is_err());
        let output = public_preview(Uuid::new_v4(), &preview).to_string();
        assert!(!output.contains("private binding"));
        assert!(output.contains("external writers"));
    }

    #[test]
    fn credential_account_and_repository_key_changes_invalidate_private_bindings() {
        let mut state = PersistedAppState::default();
        let original = state_binding(&state).unwrap();
        state.credentials.vast_api_key = "fixture account credential".into();
        let changed_account = state_binding(&state).unwrap();
        assert_ne!(original, changed_account);
        state.shared_storage_credentials.repository_key_hex = "fixture repository key".into();
        let changed_key = state_binding(&state).unwrap();
        assert_ne!(changed_account, changed_key);
        assert_eq!(changed_key, state_binding(&state.clone()).unwrap());
        assert!(!changed_key.contains("fixture"));
    }
}
