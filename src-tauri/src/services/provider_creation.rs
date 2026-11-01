//! Recover a possibly accepted paid request before another is allowed. This is
//! provider safety state, independent of feedback enrollment or journal health.
use super::{app_context::AppContext, feedback::native_task, vast_api::VastApiClient};
use crate::{
    errors::{AppError, AppResult},
    models::{
        app_state::{OfferCandidate, ProviderCreationAttempt, ProvisionedServerState},
        vast::VastInstance,
    },
};
use sha2::{Digest, Sha256};
use uuid::Uuid;
fn provider_key_fingerprint(key: &str) -> String {
    hex::encode(Sha256::digest(key.trim().as_bytes()))
}

pub struct ProviderCreationService;
impl ProviderCreationService {
    pub async fn prepare(
        context: &AppContext,
        offer: &OfferCandidate,
        provider_key: &str,
    ) -> AppResult<ProviderCreationAttempt> {
        if context.load_state().await.credentials.vast_api_key != provider_key {
            return Err(AppError::Cancelled);
        }
        if context
            .load_state()
            .await
            .pending_provider_creation
            .is_some()
        {
            return Err(ambiguous());
        }
        let feedback_context = context.clone();
        let feedback_offer = offer.clone();
        let feedback_session_id = native_task(async move {
            feedback_context
                .feedback
                .begin_rental(&feedback_context, &feedback_offer)
                .await
        })
        .await
        .flatten();
        let request_id = Uuid::new_v4().to_string();
        let attempt = ProviderCreationAttempt {
            provider_label: format!("Noland Connect {request_id}"),
            request_id,
            credential_fingerprint: provider_key_fingerprint(provider_key),
            offer_id: offer.id,
            machine_id: offer.machine_id.clone(),
            requested_at: chrono::Utc::now().to_rfc3339(),
            confirmed_instance_id: None,
            feedback_session_id,
        };
        let saved = attempt.clone();
        // A paid request requires its own durable intent, not successful feedback persistence.
        context
            .update_state(|state| state.pending_provider_creation = Some(saved))
            .await?;
        Ok(attempt)
    }

    pub async fn accepted(context: &AppContext, feedback_session: Option<String>, instance: u64) {
        let _ = context
            .update_state(|state| {
                if let Some(attempt) = &mut state.pending_provider_creation {
                    attempt.confirmed_instance_id = Some(instance);
                }
            })
            .await;
        let context = context.clone();
        let _ = native_task(async move {
            context
                .feedback
                .confirm_rental(&context, feedback_session.as_deref(), instance)
                .await;
        })
        .await;
    }

    pub async fn rejected(context: &AppContext, attempt: &ProviderCreationAttempt) {
        let _ = context
            .update_state(|state| {
                if state
                    .pending_provider_creation
                    .as_ref()
                    .is_some_and(|current| current.request_id == attempt.request_id)
                {
                    state.pending_provider_creation = None;
                }
            })
            .await;
        let context = context.clone();
        let id = attempt.feedback_session_id.clone();
        let _ = native_task(async move {
            context
                .feedback
                .abandon_unconfirmed(&context, id.as_deref())
                .await;
        })
        .await;
    }

    pub async fn complete(context: &AppContext, instance: u64) -> AppResult<()> {
        context
            .update_state(|state| {
                if state
                    .pending_provider_creation
                    .as_ref()
                    .is_some_and(|attempt| attempt.confirmed_instance_id == Some(instance))
                {
                    state.pending_provider_creation = None;
                }
            })
            .await?;
        Ok(())
    }

    pub async fn recover(
        context: &AppContext,
        api: &VastApiClient,
    ) -> AppResult<Option<VastInstance>> {
        let Some(attempt) = context.load_state().await.pending_provider_creation else {
            return Ok(None);
        };
        if Uuid::parse_str(&attempt.request_id).is_err()
            || attempt.credential_fingerprint
                != provider_key_fingerprint(&context.load_state().await.credentials.vast_api_key)
        {
            return Err(ambiguous());
        }
        let instances = api.list_instances().await?;
        let matches: Vec<_> = instances
            .into_iter()
            .filter(|instance| {
                attempt.confirmed_instance_id == Some(instance.id)
                    || instance.label == attempt.provider_label
            })
            .collect();
        if matches.len() != 1 {
            return Err(ambiguous());
        }
        if attempt.credential_fingerprint
            != provider_key_fingerprint(&context.load_state().await.credentials.vast_api_key)
        {
            return Err(AppError::Cancelled);
        }
        let mut instance = matches.into_iter().next().ok_or_else(ambiguous)?;
        if instance.machine_id.is_none() {
            instance.machine_id = attempt.machine_id.clone();
        }
        let recovered = instance.clone();
        context
            .update_state(|state| {
                // Never copy machine/offer identity from whichever rental happened to be active before this one.
                state.instance = Default::default();
                state.instance.instance_id = Some(recovered.id);
                state.instance.machine_id = recovered.machine_id.clone();
                state.instance.offer_id = Some(attempt.offer_id);
                state.instance.status = recovered.status.clone();
                state.instance.ssh_host = recovered.ssh_host.clone();
                state.instance.ssh_port = recovered.ssh_port;
                state.instance.ssh_command = recovered.ssh_command.clone();
                let index = state
                    .provisioned_servers
                    .iter()
                    .position(|s| s.instance_id == recovered.id)
                    .unwrap_or_else(|| {
                        state
                            .provisioned_servers
                            .push(ProvisionedServerState::new(recovered.id));
                        state.provisioned_servers.len() - 1
                    });
                let server = &mut state.provisioned_servers[index];
                server.machine_id = recovered.machine_id.clone();
                server.offer_id = Some(attempt.offer_id);
                server.status = recovered.status.clone();
                server.ssh_host = recovered.ssh_host.clone();
                server.ssh_port = recovered.ssh_port;
                server.steps.instance_created = true;
                state.pending_provider_creation = None;
            })
            .await?;
        let feedback_context = context.clone();
        let feedback_id = attempt.feedback_session_id;
        let id = instance.id;
        let _ = native_task(async move {
            feedback_context
                .feedback
                .confirm_rental_with_recovery(&feedback_context, feedback_id.as_deref(), id, true)
                .await;
            feedback_context
                .feedback
                .attach_references(&feedback_context, id)
                .await;
        })
        .await;
        Ok(Some(instance))
    }

    /// Explicit local acknowledgement only, never a paid operation. The caller must
    /// first inspect the provider account. Absence is checked again, not inferred from an API error.
    pub async fn clear_unconfirmed(
        context: &AppContext,
        request_id: &str,
        confirmed_absent: bool,
    ) -> AppResult<()> {
        let snapshot = context.load_state().await;
        let attempt = snapshot
            .pending_provider_creation
            .ok_or_else(|| AppError::InvalidInput("No provider creation is pending".into()))?;
        if !confirmed_absent
            || attempt.credential_fingerprint
                != provider_key_fingerprint(&snapshot.credentials.vast_api_key)
            || attempt.request_id != request_id
            || attempt.confirmed_instance_id.is_some()
        {
            return Err(ambiguous());
        }
        let api = VastApiClient::new(
            context.http_client.clone(),
            context.config.vast_base_url.clone(),
            snapshot.credentials.vast_api_key,
        );
        if api
            .list_instances()
            .await?
            .iter()
            .any(|instance| instance.label == attempt.provider_label)
        {
            return Err(ambiguous());
        }
        if attempt.credential_fingerprint
            != provider_key_fingerprint(&context.load_state().await.credentials.vast_api_key)
        {
            return Err(AppError::Cancelled);
        }
        Self::rejected(context, &attempt).await;
        if context
            .load_state()
            .await
            .pending_provider_creation
            .is_some()
        {
            return Err(AppError::State("The provider request acknowledgement could not be saved; no new rental is authorized".into()));
        }
        Ok(())
    }
}
fn ambiguous() -> AppError {
    AppError::InvalidInput("A previous paid creation may have succeeded. No second request was sent. Refresh the provider account and retry to recover the labeled rental; if none exists, explicitly acknowledge its absence with provider_creation_clear_unconfirmed before trying again.".into())
}
