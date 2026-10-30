//! Native-only optional feedback. No code in this module can rent, stop, or destroy a provider instance.
mod http;
mod journal;

use std::{
    collections::{BTreeMap, HashSet},
    future::Future,
    path::PathBuf,
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant},
};

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use chrono::Utc;
use noland_network_contracts::feedback::{
    valid_provider_id, JitterAccumulator, JitterObservation, MeasurementEvidence, RouteMode,
};
use reqwest::Method;
use serde_json::{json, Value};
use tauri::{AppHandle, Manager};
use tokio::sync::{watch, Mutex};
use uuid::Uuid;

use super::{app_context::AppContext, secure_store};
use crate::{
    errors::{AppError, AppResult},
    models::{
        app_state::{OfferCandidate, PersistedAppState, TransportKind},
        feedback::{EnrollmentRequest, FeedbackControl, FeedbackProgramState},
    },
};
use http::{Failure, FeedbackHttp};
use journal::{Journal, Outbox, ReviewDraft, MAX_SESSIONS};

const SECRET_SERVICE: &str = "noland-connect.feedback.desktop.v1";
const CHECKPOINT_INTERVAL: Duration = Duration::from_secs(30);
const UPLOAD_INTERVAL: Duration = Duration::from_secs(60);
const FRAME_FRESHNESS: Duration = Duration::from_secs(2);

struct ActiveStream {
    instance: u64,
    generation: u64,
    frames: u64,
    last_frame: Instant,
    started: Instant,
}

struct Runtime {
    initialized: bool,
    authorized: bool,
    storage_ok: bool,
    outbox: Outbox,
    accumulators: BTreeMap<String, JitterAccumulator>,
    streaming: Option<ActiveStream>,
    frame_cursor: Option<(u64, u64, u64)>,
    status: Option<String>,
    next_attempt: Instant,
    attempts: u32,
    control_blocked: bool,
    last_notices: Instant,
    last_upload: Instant,
    last_checkpoint: Instant,
}
impl Default for Runtime {
    fn default() -> Self {
        let now = Instant::now();
        Self {
            initialized: false,
            authorized: false,
            storage_ok: true,
            outbox: Outbox::default(),
            accumulators: BTreeMap::new(),
            streaming: None,
            frame_cursor: None,
            status: None,
            next_attempt: now,
            attempts: 0,
            control_blocked: false,
            last_notices: now.checked_sub(UPLOAD_INTERVAL).unwrap_or(now),
            last_upload: now.checked_sub(UPLOAD_INTERVAL).unwrap_or(now),
            last_checkpoint: now,
        }
    }
}

pub struct FeedbackProgramService {
    path: PathBuf,
    http: FeedbackHttp,
    runtime: Mutex<Runtime>,
    initialization: Mutex<()>,
    operation: Mutex<()>,
    reconciliation: Mutex<()>,
    stopped: AtomicBool,
    capture_allowed: std::sync::Arc<AtomicBool>,
    cancellation: watch::Sender<u64>,
}

impl FeedbackProgramService {
    pub fn new(state_path: &std::path::Path) -> Self {
        Self {
            path: state_path.with_file_name("feedback").join("outbox-v1.json"),
            http: FeedbackHttp::new(),
            runtime: Mutex::new(Runtime::default()),
            initialization: Mutex::new(()),
            operation: Mutex::new(()),
            reconciliation: Mutex::new(()),
            stopped: AtomicBool::new(true),
            capture_allowed: std::sync::Arc::new(AtomicBool::new(false)),
            cancellation: watch::channel(0).0,
        }
    }
    fn supported() -> bool {
        cfg!(any(
            target_os = "windows",
            target_os = "macos",
            target_os = "linux"
        ))
    }
    fn permits(&self, state: &FeedbackProgramState, rt: &Runtime) -> bool {
        !self.stopped.load(Ordering::SeqCst)
            && self.http.configured()
            && Self::supported()
            && state.enabled
            && state.enrolled()
            && !state.collection_paused
            && rt.authorized
            && rt.storage_ok
    }
    pub fn try_reconciliation(&self) -> Option<tokio::sync::MutexGuard<'_, ()>> {
        self.reconciliation.try_lock().ok()
    }

    pub fn frame_permission(&self) -> std::sync::Arc<AtomicBool> {
        self.capture_allowed.clone()
    }

    pub fn publish_local_permission(&self, state: &FeedbackProgramState) {
        self.capture_allowed.store(
            self.locally_running() && state.enabled && state.enrolled() && !state.collection_paused,
            Ordering::SeqCst,
        );
    }

    pub fn locally_running(&self) -> bool {
        !self.stopped.load(Ordering::SeqCst)
    }
    fn cancel_delivery(&self) {
        self.cancellation.send_modify(|n| *n = n.wrapping_add(1));
    }
    fn stop_now(&self) {
        self.stopped.store(true, Ordering::SeqCst);
        self.capture_allowed.store(false, Ordering::SeqCst);
        self.cancel_delivery();
    }

    async fn initialize(&self, context: &AppContext) -> AppResult<()> {
        let _initialization = self.initialization.lock().await;
        if self.runtime.lock().await.initialized {
            return Ok(());
        }
        let state = context.load_state().await.feedback_program;
        // Status while off is local-only and does not even open the contribution spool/keyring.
        if !state.enabled || !self.http.configured() || !Self::supported() {
            return Ok(());
        }
        if !state.enrolled() {
            self.stop_now();
            return Err(input("Feedback consent is incomplete or unsupported"));
        }
        let credential = secret_get(
            state
                .credential_ref
                .as_deref()
                .ok_or_else(|| input("Feedback credential reference is missing"))?,
        )
        .await?;
        let path = self.path.clone();
        let loaded = tokio::task::spawn_blocking(move || Outbox::load(&path))
            .await
            .map_err(|_| input("Feedback recovery task failed"))?;
        let mut rt = self.runtime.lock().await;
        let mut outbox = match loaded {
            Ok(outbox) => outbox,
            Err(error) => {
                rt.storage_ok = false;
                rt.initialized = true;
                self.stop_now();
                rt.status = Some(
                    "Feedback journal could not be recovered; rentals remain available".into(),
                );
                return Err(error);
            }
        };
        if outbox.installation_id != state.installation_id {
            outbox = Outbox::default();
        }
        outbox.installation_id = state.installation_id.clone();
        for journal in outbox.sessions.values_mut() {
            journal.mark_gap();
        }
        self.commit(&mut rt, outbox).await?;
        rt.authorized = credential.as_deref().is_some_and(valid_secret);
        rt.initialized = true;
        if rt.authorized {
            rt.status = None;
        }
        if !rt.authorized {
            rt.status = Some(
                "Protected feedback credentials are unavailable; unlock the credential store"
                    .into(),
            );
        }
        self.stopped.store(!rt.authorized, Ordering::SeqCst);
        self.publish_local_permission(&state);
        Ok(())
    }

    async fn commit(&self, rt: &mut Runtime, outbox: Outbox) -> AppResult<()> {
        let path = self.path.clone();
        let saved = outbox.clone();
        let result = tokio::task::spawn_blocking(move || saved.save(&path))
            .await
            .map_err(|_| input("Feedback checkpoint task failed"))
            .and_then(|result| result);
        if result.is_err() {
            rt.storage_ok = false;
            self.stop_now();
            for journal in rt.outbox.sessions.values_mut() {
                journal.mark_gap();
            }
            rt.accumulators.clear();
            rt.status = Some("Feedback could not be saved; collection is paused. Renting, streaming, and destruction are unaffected".into());
        }
        result?;
        rt.storage_ok = true;
        rt.accumulators
            .retain(|id, _| outbox.sessions.contains_key(id));
        rt.outbox = outbox;
        Ok(())
    }

    pub async fn status(&self, context: &AppContext) -> Value {
        if self.initialize(context).await.is_err() {
            self.stop_now();
            let mut rt = self.runtime.lock().await;
            if rt.status.is_none() {
                rt.status = Some(
                    "Feedback recovery or protected storage is unavailable; collection is disabled"
                        .into(),
                );
            }
        }
        let state = context.load_state().await.feedback_program;
        let rt = self.runtime.lock().await;
        let mut value = serde_json::to_value(&state).unwrap_or_else(|_| json!({"enabled": false}));
        value["enabled"] = json!(self.permits(&state, &rt));
        value["configured"] = json!(self.http.configured());
        value["supported"] = json!(Self::supported());
        value["pendingSessions"] = json!(rt.outbox.pending_count());
        value["statusMessage"] = json!(if !self.http.configured() {
            Some(
                "Feedback is unavailable: this build has no valid NOLAND_FEEDBACK_API_URL"
                    .to_string(),
            )
        } else if let Some(message) = &rt.status {
            Some(message.clone())
        } else if state.pending_control_action.is_some() {
            Some(
                "Collection follows the saved local choice; server synchronization is pending"
                    .into(),
            )
        } else if state.enrollment_status == "pending" {
            Some("Proof-bound enrollment recovery is pending; no rentals are being tracked".into())
        } else if state.collection_paused {
            Some("The feedback operator has paused collection".into())
        } else {
            None
        });
        value["jitterSupported"] = json!(Self::supported());
        value["jitterStatus"] = json!("Only fresh watermarked received-RTT reports during frame-active streaming after a full 60-second warmup qualify. Legacy agents and missing evidence produce N/A.");
        value
    }

    pub async fn notices(&self, context: &AppContext) -> AppResult<Value> {
        self.read_notices(context, true).await
    }

    async fn read_notices(&self, context: &AppContext, explicit: bool) -> AppResult<Value> {
        let mut cancelled = self.cancellation.subscribe();
        if !explicit {
            let state = context.load_state().await.feedback_program;
            let rt = self.runtime.lock().await;
            if !state.enabled
                || !state.enrolled()
                || !rt.authorized
                || self.stopped.load(Ordering::SeqCst)
            {
                return Err(input("Automatic feedback refresh is disabled"));
            }
        }
        let request = self.http.request(Method::GET, "/program", None, None, None);
        let document = if explicit {
            request.await.map_err(as_app_error)?
        } else {
            tokio::select! { biased;
                _ = cancelled.changed() => return Err(input("Automatic feedback refresh was cancelled")),
                result = request => result.map_err(as_app_error)?,
            }
        };
        validate_program(&document)?;
        let current = context.load_state().await.feedback_program;
        if current.enrolled() {
            let paused = document["collectionPaused"].as_bool() != Some(false)
                || document["available"].as_bool() != Some(true);
            let policies_match = document["privacyVersion"].as_str()
                == current.privacy_version.as_deref()
                && document["termsVersion"].as_str() == current.terms_version.as_deref();
            if paused || !policies_match {
                self.cancel_delivery();
                self.checkpoint(true).await;
            }
            context
                .update_state(|s| {
                    s.feedback_program.collection_paused = paused || !policies_match;
                })
                .await?;
            if !policies_match {
                self.runtime.lock().await.status = Some("Feedback policy versions have changed; this client cannot silently expand consent".into());
            }
        }
        Ok(document)
    }

    pub async fn enroll(
        &self,
        context: &AppContext,
        mut request: EnrollmentRequest,
    ) -> AppResult<Value> {
        let _operation = self.operation.lock().await;
        if request.privacy_version != "1" || request.terms_version != "1" {
            return Err(input("This client cannot accept an unsupported feedback policy version; update the app first"));
        }
        if !Self::supported() || !request.participation_accepted {
            return Err(input(
                "Explicit participation acceptance on a supported platform is required",
            ));
        }
        validate_profile(&mut request)?;
        let current = context.load_state().await.feedback_program;
        if current.pending_control_action.is_some() {
            return Err(input(
                "Finish the pending privacy control before enrolling again",
            ));
        }
        if current.enrolled() {
            return Err(input(
                "This installation is already enrolled; use the consent control to rejoin",
            ));
        }
        if current.enrollment_proof_ref.is_some() {
            let (_, saved) = pending_enrollment(&current).await?;
            if serde_json::to_value(&request)? != serde_json::to_value(&saved)? {
                return Err(input(
                    "Resume pending enrollment using the original accepted choices",
                ));
            }
        } else {
            if current
                .receipt_status
                .as_deref()
                .is_some_and(|status| status != "completed")
            {
                return Err(input(
                    "Wait for the existing deletion receipt to complete before enrolling again",
                ));
            }
            let program = self.notices(context).await?;
            if program["available"].as_bool() != Some(true)
                || program["collectionPaused"].as_bool() != Some(false)
            {
                return Err(input("Feedback enrollment is currently unavailable"));
            }
            if program["privacyVersion"].as_str() != Some(request.privacy_version.as_str())
                || program["termsVersion"].as_str() != Some(request.terms_version.as_str())
            {
                return Err(input(
                    "Read and accept the current privacy and terms versions before enrolling",
                ));
            }
            let proof = URL_SAFE_NO_PAD.encode(rand::random::<[u8; 32]>());
            let reference = new_reference();
            secret_put(
                &reference,
                &json!({"proof": proof, "request": request}).to_string(),
            )
            .await?;
            // All reference names, including response destinations, are durable before the first POST.
            let state = FeedbackProgramState {
                installation_id: Some(Uuid::new_v4().to_string()),
                enrollment_status: "pending".into(),
                credential_ref: Some(new_reference()),
                deletion_code_ref: Some(new_reference()),
                enrollment_request_id: Some(Uuid::new_v4().to_string()),
                enrollment_proof_ref: Some(reference.clone()),
                enrollment_started_at: Some(Utc::now().to_rfc3339()),
                ..Default::default()
            };
            if let Err(error) = context.update_state(|s| s.feedback_program = state).await {
                let _ = secret_delete(&reference).await;
                return Err(error);
            }
            if let Some(reference) = current.receipt_token_ref {
                let _ = secret_delete(&reference).await;
            }
        }
        if let Err(failure) = self.finish_enrollment(context).await {
            if !failure.retryable {
                self.runtime.lock().await.control_blocked = true;
            }
            self.defer(&failure).await;
            return Err(as_app_error(failure));
        }
        self.initialize(context).await?;
        Ok(self.status(context).await)
    }

    async fn finish_enrollment(&self, context: &AppContext) -> Result<(), Failure> {
        let generation = *self.cancellation.borrow();
        let state = context.load_state().await.feedback_program;
        let (proof, request) = pending_enrollment(&state).await.map_err(|_| Failure::local("Enrollment recovery proof is missing, expired, or locked; no identity can be recovered from installation ID alone"))?;
        let payload = json!({"installationId": state.installation_id, "requestId": state.enrollment_request_id, "recoveryProof": proof,
            "privacyVersion": request.privacy_version, "termsVersion": request.terms_version, "participationAccepted": true,
            "optionalContextEnabled": request.optional_context_enabled, "optionalProfile": request.optional_profile});
        let response = self
            .http
            .request(
                Method::POST,
                "/enrollments",
                None,
                Some(&payload),
                state.enrollment_request_id.as_deref(),
            )
            .await?;
        let user = required_uuid(&response, "userId").map_err(protocol_failure)?;
        let credential = required_secret(&response, "credential").map_err(protocol_failure)?;
        let code = required_secret(&response, "deletionCode").map_err(protocol_failure)?;
        let epoch = response["consentEpoch"]
            .as_u64()
            .filter(|n| *n > 0 && *n <= 9_007_199_254_740_991)
            .ok_or_else(|| Failure::local("Enrollment returned an invalid consent epoch"))?;
        if response["privacyVersion"].as_str() != Some(request.privacy_version.as_str())
            || response["termsVersion"].as_str() != Some(request.terms_version.as_str())
        {
            return Err(Failure::local("Enrollment acknowledged different policies"));
        }
        let at = response["consentedAt"]
            .as_str()
            .filter(|at| chrono::DateTime::parse_from_rfc3339(at).is_ok())
            .ok_or_else(|| Failure::local("Enrollment returned an invalid consent timestamp"))?
            .to_string();
        let credential_ref = state
            .credential_ref
            .as_deref()
            .ok_or_else(|| Failure::local("Enrollment credential destination is missing"))?;
        let deletion_ref = state
            .deletion_code_ref
            .as_deref()
            .ok_or_else(|| Failure::local("Enrollment deletion destination is missing"))?;
        secret_put_pair(credential_ref, &credential, deletion_ref, &code)
            .await
            .map_err(persistence_failure)?;
        let withdrawing =
            state.pending_control_action.is_some() || *self.cancellation.borrow() != generation;
        context
            .update_state(|s| {
                let feedback = &mut s.feedback_program;
                feedback.enabled = !withdrawing;
                feedback.enrollment_status = "enrolled".into();
                feedback.user_id = Some(user);
                feedback.consent_epoch = epoch;
                feedback.privacy_version = Some(request.privacy_version);
                feedback.terms_version = Some(request.terms_version);
                feedback.consented_at = Some(at);
                feedback.optional_context_enabled =
                    request.optional_context_enabled && !withdrawing;
                feedback.enrollment_request_id = None;
                feedback.enrollment_started_at = None;
                feedback.enrollment_proof_ref = None;
                if let Some(control) = &mut feedback.pending_control_action {
                    control.expected_epoch = epoch;
                }
            })
            .await
            .map_err(persistence_failure)?;
        if let Some(reference) = state.enrollment_proof_ref {
            let _ = secret_delete(&reference).await;
        }
        let mut rt = self.runtime.lock().await;
        rt.initialized = false;
        rt.control_blocked = false;
        rt.status = None;
        Ok(())
    }

    pub async fn consent(
        &self,
        context: &AppContext,
        enabled: bool,
        optional: bool,
    ) -> AppResult<Value> {
        if !enabled {
            self.stop_now();
        }
        self.cancel_delivery();
        let _operation = self.operation.lock().await;
        let state = context.load_state().await.feedback_program;
        if !state.enrolled() {
            return Err(input("Feedback is not enrolled on this installation"));
        }
        if let Some(control) = &state.pending_control_action {
            if control.kind == "consent"
                && control.enabled == enabled
                && control.optional_context_enabled == (optional && enabled)
                && self.runtime.lock().await.control_blocked
            {
                self.reset_retry().await;
                return Ok(self.status(context).await);
            }
            // A retry of the exact choice is harmless. A withdrawal may supersede an unsynchronized scope/rejoin.
            if control.kind == "delete" || enabled {
                return Err(input(
                    "Resolve the pending privacy control before rejoining or changing scope",
                ));
            }
            if !control.enabled {
                self.reset_retry().await;
                return Ok(self.status(context).await);
            }
            context
                .update_state(|s| {
                    s.feedback_program.enabled = false;
                    s.feedback_program.optional_context_enabled = false;
                    if let Some(control) = &mut s.feedback_program.pending_control_action {
                        control.withdraw_after_acknowledgement = true;
                    }
                })
                .await?;
            self.clear_outbox().await?;
            self.reset_retry().await;
            return Ok(self.status(context).await);
        }
        if enabled && !state.enabled {
            let document = self.notices(context).await?;
            if document["available"].as_bool() != Some(true)
                || document["collectionPaused"].as_bool() != Some(false)
                || document["privacyVersion"].as_str() != state.privacy_version.as_deref()
                || document["termsVersion"].as_str() != state.terms_version.as_deref()
            {
                return Err(input(
                    "Rejoining requires current accepted notices and an available program",
                ));
            }
        }
        let control = FeedbackControl {
            kind: "consent".into(),
            request_id: Uuid::new_v4().to_string(),
            expected_epoch: state.consent_epoch,
            enabled,
            optional_context_enabled: optional && enabled,
            withdraw_after_acknowledgement: false,
        };
        context
            .update_state(|s| {
                if !enabled {
                    s.feedback_program.enabled = false;
                }
                s.feedback_program.optional_context_enabled = optional && enabled;
                s.feedback_program.pending_control_action = Some(control);
            })
            .await?;
        if !enabled {
            self.clear_outbox().await?;
        }
        self.reset_retry().await;
        Ok(self.status(context).await)
    }

    pub async fn delete(&self, context: &AppContext) -> AppResult<Value> {
        self.stop_now();
        let _operation = self.operation.lock().await;
        let state = context.load_state().await.feedback_program;
        if state.receipt_id.is_some() && state.pending_control_action.is_none() {
            return Ok(self.status(context).await);
        }
        if state.credential_ref.is_none()
            && state.deletion_code_ref.is_none()
            && state.enrollment_proof_ref.is_none()
        {
            return Err(input(
                "No protected ownership credential is available for deletion",
            ));
        }
        let control = state
            .pending_control_action
            .filter(|c| c.kind == "delete")
            .unwrap_or(FeedbackControl {
                kind: "delete".into(),
                request_id: Uuid::new_v4().to_string(),
                expected_epoch: state.consent_epoch.max(1),
                enabled: false,
                optional_context_enabled: false,
                withdraw_after_acknowledgement: false,
            });
        context
            .update_state(|s| {
                s.feedback_program.enabled = false;
                s.feedback_program.optional_context_enabled = false;
                s.feedback_program.pending_control_action = Some(control);
                if s.feedback_program.receipt_token_ref.is_none() {
                    s.feedback_program.receipt_token_ref = Some(new_reference());
                }
            })
            .await?;
        self.clear_outbox().await?;
        self.reset_retry().await;
        Ok(self.status(context).await)
    }
    async fn reset_retry(&self) {
        let mut rt = self.runtime.lock().await;
        rt.next_attempt = Instant::now();
        rt.attempts = 0;
        rt.control_blocked = false;
        rt.status = None;
    }
    async fn clear_outbox(&self) -> AppResult<()> {
        let mut rt = self.runtime.lock().await;
        rt.accumulators.clear();
        rt.streaming = None;
        rt.frame_cursor = None;
        if self.path.exists() {
            self.commit(&mut rt, Outbox::default()).await?;
        } else {
            rt.outbox = Outbox::default();
        }
        Ok(())
    }

    async fn participant(
        &self,
        context: &AppContext,
        method: Method,
        path: &str,
        body: Option<&Value>,
        epoch: Option<u64>,
        review_revision: Option<u64>,
    ) -> Result<Value, Failure> {
        let mut cancel = self.cancellation.subscribe();
        let state = context.load_state().await.feedback_program;
        {
            let rt = self.runtime.lock().await;
            if !self.permits(&state, &rt)
                || state.pending_control_action.is_some()
                || rt.next_attempt > Instant::now()
                || epoch.is_some_and(|epoch| epoch != state.consent_epoch)
            {
                return Err(Failure::local("Feedback is disabled, in backoff, unavailable, or awaiting consent synchronization"));
            }
        }
        let token = secret_get(
            state
                .credential_ref
                .as_deref()
                .ok_or_else(|| Failure::local("Feedback credential is missing"))?,
        )
        .await
        .map_err(|_| Failure::local("Protected feedback credential is unavailable"))?
        .filter(|token| valid_secret(token))
        .ok_or_else(|| Failure::local("Feedback credential is missing"))?;
        // Check consent again after possibly slow keyring access, immediately before the request.
        let latest = context.load_state().await.feedback_program;
        if latest.consent_epoch != state.consent_epoch
            || latest.pending_control_action.is_some()
            || !self.permits(&latest, &self.runtime.lock().await)
            || cancel.has_changed().unwrap_or(true)
        {
            return Err(Failure::local(
                "Feedback request cancelled by a privacy choice",
            ));
        }
        let request = async {
            if let Some(revision) = review_revision {
                self.http.delete_review(path, &token, revision).await
            } else {
                self.http
                    .request(method, path, Some(&token), body, None)
                    .await
            }
        };
        let result = tokio::select! { biased;
            _ = cancel.changed() => Err(Failure::local("Feedback request cancelled by a privacy choice")),
            result = request => result,
        };
        if let Err(failure) = &result {
            if matches!(failure.status, Some(401 | 403)) {
                self.stop_now();
                let _ = context
                    .update_state(|s| s.feedback_program.enabled = false)
                    .await;
                let mut rt = self.runtime.lock().await;
                rt.authorized = false;
                rt.status = Some("Feedback authorization is no longer valid; contributions are suspended, but deletion remains available".into());
            }
            if failure.retryable {
                self.defer(failure).await;
            }
        }
        result
    }

    pub async fn machine_summaries(
        &self,
        context: &AppContext,
        mut ids: Vec<String>,
    ) -> AppResult<Value> {
        self.initialize(context).await?;
        ids.sort();
        ids.dedup();
        if ids.is_empty() || ids.len() > 100 || ids.iter().any(|id| !valid_provider_id(id)) {
            return Err(input(
                "Provide between 1 and 100 canonical decimal machine IDs",
            ));
        }
        self.participant(
            context,
            Method::POST,
            "/machines/summaries",
            Some(&json!({"provider": "vast", "machineIds": ids})),
            None,
            None,
        )
        .await
        .map_err(as_app_error)
    }
    pub async fn machine_reviews(
        &self,
        context: &AppContext,
        id: String,
        cursor: Option<String>,
        limit: Option<u32>,
    ) -> AppResult<Value> {
        self.initialize(context).await?;
        if !valid_provider_id(&id) || cursor.as_ref().is_some_and(|c| c.len() > 2_048) {
            return Err(input("Invalid feedback review page"));
        }
        let mut path = format!(
            "/machines/{id}/reviews?limit={}",
            limit.unwrap_or(20).clamp(1, 50)
        );
        if let Some(cursor) = cursor {
            path.push_str("&cursor=");
            path.push_str(&urlencoding::encode(&cursor));
        }
        self.participant(context, Method::GET, &path, None, None, None)
            .await
            .map_err(as_app_error)
    }
    pub async fn moderate(
        &self,
        context: &AppContext,
        id: String,
        reason: Option<String>,
    ) -> AppResult<Value> {
        self.initialize(context).await?;
        if Uuid::parse_str(&id).is_err() {
            return Err(input("Invalid review ID"));
        }
        let (action, body) = if let Some(reason) = reason {
            let reason = reason.trim();
            if reason.is_empty()
                || reason.chars().count() > 500
                || reason.chars().any(char::is_control)
            {
                return Err(input(
                    "Report reason must contain 1–500 plain-text characters",
                ));
            }
            ("reports", json!({"reason": reason}))
        } else {
            ("block-author", json!({}))
        };
        self.participant(
            context,
            Method::POST,
            &format!("/reviews/{id}/{action}"),
            Some(&body),
            None,
            None,
        )
        .await
        .map_err(as_app_error)
    }

    pub async fn begin_rental(
        &self,
        context: &AppContext,
        offer: &OfferCandidate,
    ) -> Option<String> {
        let snapshot = context.load_state().await;
        let state = snapshot.feedback_program;
        let mut rt = self.runtime.lock().await;
        if !self.permits(&state, &rt) || state.pending_control_action.is_some() {
            return None;
        }
        let mut next = rt.outbox.clone();
        next.make_room();
        if next.sessions.len() >= MAX_SESSIONS {
            rt.status = Some("Feedback journal is full; this rental will not contribute".into());
            return None;
        }
        let journal = Journal::pending(
            offer,
            state.consent_epoch,
            &snapshot.credentials.vast_api_key,
        );
        let id = journal.session_id.clone();
        next.installation_id = state.installation_id;
        next.sessions.insert(id.clone(), journal);
        self.commit(&mut rt, next).await.ok().map(|_| id)
    }

    pub async fn confirm_rental(
        &self,
        context: &AppContext,
        pending: Option<&str>,
        instance_id: u64,
    ) {
        self.confirm_rental_with_recovery(context, pending, instance_id, false)
            .await;
    }
    pub async fn confirm_rental_with_recovery(
        &self,
        context: &AppContext,
        pending: Option<&str>,
        instance_id: u64,
        recovered: bool,
    ) {
        let Some(pending) = pending else {
            return;
        };
        let state = context.load_state().await.feedback_program;
        let mut rt = self.runtime.lock().await;
        if !self.permits(&state, &rt) {
            return;
        }
        let id = instance_id.to_string();
        let mut next = rt.outbox.clone();
        if next
            .sessions
            .values()
            .any(|j| j.provider_instance_id.as_deref() == Some(id.as_str()))
        {
            return;
        }
        let Some(journal) = next
            .sessions
            .get_mut(pending)
            .filter(|j| j.consent_epoch == state.consent_epoch)
        else {
            return;
        };
        if journal.confirm(instance_id, recovered) {
            let _ = self.commit(&mut rt, next).await;
        }
    }
    pub async fn abandon_unconfirmed(&self, context: &AppContext, id: Option<&str>) {
        let Some(id) = id else {
            return;
        };
        let state = context.load_state().await.feedback_program;
        let mut rt = self.runtime.lock().await;
        if !self.permits(&state, &rt) {
            return;
        }
        let mut next = rt.outbox.clone();
        if next
            .sessions
            .get(id)
            .is_some_and(|j| j.provider_instance_id.is_none())
        {
            next.sessions.remove(id);
            let _ = self.commit(&mut rt, next).await;
        }
    }

    pub async fn confirmed_pairing(&self, context: &AppContext, instance: u64) {
        let state = context.load_state().await.feedback_program;
        let mut rt = self.runtime.lock().await;
        if !self.permits(&state, &rt) {
            return;
        }
        let mut next = rt.outbox.clone();
        if let Some(journal) = next.sessions.values_mut().find(|journal| {
            journal.provider_instance_id.as_deref() == Some(instance.to_string().as_str())
                && !journal.rejected
                && !journal.external_disappearance
        }) {
            if journal
                .lifecycle
                .ready_and_paired(false, true, &Utc::now().to_rfc3339())
            {
                journal.revision += 1;
                let _ = self.commit(&mut rt, next).await;
            }
        }
    }

    /// Observes committed native state, including automatic and manual pairing paths.
    pub async fn observe_state(&self, state: &PersistedAppState) {
        let mut rt = self.runtime.lock().await;
        if !self.permits(&state.feedback_program, &rt) {
            return;
        }
        let mut next = rt.outbox.clone();
        let mut changed = false;
        let at = Utc::now().to_rfc3339();
        for journal in next
            .sessions
            .values_mut()
            .filter(|j| j.lifecycle.destroyed_at.is_none() && !j.rejected)
        {
            let Some(id) = journal
                .provider_instance_id
                .as_deref()
                .and_then(|id| id.parse::<u64>().ok())
            else {
                continue;
            };
            let server = state
                .provisioned_servers
                .iter()
                .find(|s| s.instance_id == id);
            let current = state.instance.instance_id == Some(id);
            let machine = server.and_then(|s| s.machine_id.as_ref()).or_else(|| {
                if current {
                    state.instance.machine_id.as_ref()
                } else {
                    None
                }
            });
            let mut updated = false;
            if let Some(machine) = machine.filter(|id| valid_provider_id(id)) {
                if journal
                    .provider_machine_id
                    .as_ref()
                    .is_some_and(|old| old != machine)
                {
                    journal.rejected = true;
                    journal.lifecycle.observation_complete = false;
                    updated = true;
                    rt.status = Some(
                        "Conflicting provider machine identity was excluded from feedback".into(),
                    );
                } else if journal.provider_machine_id.is_none() {
                    journal.provider_machine_id = Some(machine.clone());
                    updated = true;
                }
            }
            if let Some(accumulator) = rt.accumulators.get_mut(&journal.session_id) {
                accumulator.route_changed(route_for(state, id), Instant::now());
            }
            let post = state.post_wireguard_setup.current_instance_id == Some(id);
            let ready = server.is_some_and(|s| s.steps.instance_ready)
                || (post && state.post_wireguard_setup.setup_complete);
            let paired = server
                .is_some_and(|s| s.steps.pairing_completed || s.embedded_moonlight_paired)
                || (post && state.post_wireguard_setup.paired);
            if !journal.external_disappearance && !journal.rejected {
                updated |= journal.lifecycle.ready_and_paired(ready, paired, &at);
            }
            if updated {
                journal.revision += 1;
                changed = true;
            }
        }
        if changed {
            let _ = self.commit(&mut rt, next).await;
        }
    }

    pub async fn attach_references(&self, context: &AppContext, instance_id: u64) {
        let id = instance_id.to_string();
        let reference = {
            let rt = self.runtime.lock().await;
            rt.outbox
                .sessions
                .values()
                .find(|j| j.provider_instance_id.as_deref() == Some(id.as_str()))
                .map(|j| j.session_id.clone())
        };
        let Some(reference) = reference else {
            return;
        };
        let _ = context
            .update_state(|s| {
                let journal_ref = format!("feedback/outbox-v1.json#{reference}");
                if s.instance.instance_id == Some(instance_id) {
                    s.instance.feedback_session_id = Some(reference.clone());
                    s.instance.feedback_journal_ref = Some(journal_ref.clone());
                }
                if let Some(server) = s
                    .provisioned_servers
                    .iter_mut()
                    .find(|server| server.instance_id == instance_id)
                {
                    server.feedback_session_id = Some(reference);
                    server.feedback_journal_ref = Some(journal_ref);
                }
            })
            .await;
    }
    pub async fn review_eligible(&self, context: &AppContext, instance: &str) -> bool {
        let state = context.load_state().await.feedback_program;
        let rt = self.runtime.lock().await;
        self.permits(&state, &rt)
            && valid_provider_id(instance)
            && rt.outbox.sessions.values().any(|j| {
                j.provider_instance_id.as_deref() == Some(instance)
                    && j.review_eligible()
                    && j.lifecycle.destroyed_at.is_none()
            })
    }
    pub async fn stage_review(
        &self,
        context: &AppContext,
        instance: String,
        score: Option<u8>,
        comment: Option<String>,
    ) -> AppResult<Value> {
        let comment = comment
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());
        if score.is_some_and(|s| !(1..=10).contains(&s))
            || comment
                .as_deref()
                .is_some_and(|s| !journal::valid_comment(s))
        {
            return Err(input(
                "Review requires an integer score from 1–10 and at most 2000 plain-text characters",
            ));
        }
        if !self.review_eligible(context, &instance).await {
            return Err(input(
                "This rental has no eligible enrolled first-frame observation",
            ));
        }
        let state = context.load_state().await.feedback_program;
        let mut rt = self.runtime.lock().await;
        if !self.permits(&state, &rt) {
            return Err(input("Feedback is disabled"));
        }
        let mut next = rt.outbox.clone();
        let journal = next
            .sessions
            .values_mut()
            .find(|j| {
                j.provider_instance_id.as_deref() == Some(instance.as_str())
                    && j.lifecycle.destroyed_at.is_none()
            })
            .ok_or_else(|| input("Active feedback rental journal is missing"))?;
        let revision = journal.review.as_ref().map(|r| r.revision + 1).unwrap_or(1);
        // Both nulls intentionally cancel any previous draft with an independently revisioned tombstone.
        journal.review = Some(ReviewDraft {
            revision,
            score,
            comment,
            acknowledged_revision: 0,
            rejected: false,
        });
        self.commit(&mut rt, next).await?;
        Ok(json!({"staged": true, "reviewRevision": revision}))
    }

    pub async fn observe_runtime(
        &self,
        context: &AppContext,
        instance: Option<u64>,
        generation: u64,
        streaming: bool,
        frames: u64,
    ) {
        let state = context.load_state().await.feedback_program;
        let mut rt = self.runtime.lock().await;
        if !self.permits(&state, &rt) {
            return;
        }
        let now = Instant::now();
        let counter_reset = instance.is_some_and(|instance| {
            rt.frame_cursor
                .is_some_and(|(old_instance, old_generation, old_frames)| {
                    old_instance == instance && old_generation == generation && frames < old_frames
                })
        });
        let advanced = instance.is_some_and(|instance| {
            frames > 0
                && rt
                    .frame_cursor
                    .is_none_or(|(old_instance, old_generation, old_frames)| {
                        old_instance != instance
                            || old_generation != generation
                            || frames != old_frames
                    })
        });
        if let Some(instance) = instance {
            rt.frame_cursor = Some((instance, generation, frames));
        }
        let same = rt.streaming.as_ref().is_some_and(|s| {
            Some(s.instance) == instance && s.generation == generation && streaming
        });
        let gap = rt
            .streaming
            .as_ref()
            .is_some_and(|s| now.saturating_duration_since(s.last_frame) > FRAME_FRESHNESS);
        if !same || gap || counter_reset {
            if let Some(next) = checkpoint_accumulators(&rt) {
                let _ = self.commit(&mut rt, next).await;
            }
            for accumulator in rt.accumulators.values_mut() {
                accumulator.stop();
            }
            rt.streaming =
                instance
                    .filter(|_| streaming && advanced)
                    .map(|instance| ActiveStream {
                        instance,
                        generation,
                        frames,
                        last_frame: now,
                        started: now,
                    });
        } else if let Some(active) = &mut rt.streaming {
            if frames > active.frames {
                active.frames = frames;
                active.last_frame = now;
            }
        }
        if let Some(instance) = instance.filter(|_| streaming && frames > 0) {
            let mut next = rt.outbox.clone();
            if let Some(journal) = next.sessions.values_mut().find(|j| {
                j.provider_instance_id.as_deref() == Some(instance.to_string().as_str())
                    && j.lifecycle.destroyed_at.is_none()
                    && !j.external_disappearance
                    && !j.rejected
            }) {
                if journal
                    .lifecycle
                    .milestone("firstStreamFrame", &Utc::now().to_rfc3339())
                {
                    journal.revision += 1;
                    let _ = self.commit(&mut rt, next).await;
                }
            }
        }
    }
    pub async fn can_measure(&self, context: &AppContext, instance: u64) -> bool {
        let state = context.load_state().await.feedback_program;
        let Ok(rt) = self.runtime.try_lock() else {
            return false;
        };
        self.permits(&state, &rt)
            && rt.outbox.sessions.values().any(|j| {
                j.provider_instance_id.as_deref() == Some(instance.to_string().as_str())
                    && j.lifecycle.destroyed_at.is_none()
                    && !j.rejected
                    && !j.external_disappearance
            })
    }
    pub async fn observe_jitter(
        &self,
        context: &AppContext,
        instance: u64,
        monitor: Uuid,
        evidence: &MeasurementEvidence,
        jitter: f64,
        measured_at: Instant,
        route: RouteMode,
    ) {
        let state = context.load_state().await.feedback_program;
        // Operational monitoring must never wait on feedback disk I/O.
        let Ok(mut rt) = self.runtime.try_lock() else {
            return;
        };
        if !self.permits(&state, &rt)
            || rt
                .streaming
                .as_ref()
                .is_none_or(|s| s.instance != instance || s.last_frame.elapsed() > FRAME_FRESHNESS)
        {
            return;
        }
        let Some(journal) = rt
            .outbox
            .sessions
            .values()
            .find(|j| {
                j.provider_instance_id.as_deref() == Some(instance.to_string().as_str())
                    && j.lifecycle.destroyed_at.is_none()
                    && j.lifecycle.first_stream_frame_at.is_some()
                    && !j.rejected
                    && !j.external_disappearance
            })
            .cloned()
        else {
            return;
        };
        let stream_started = rt
            .streaming
            .as_ref()
            .map(|s| s.started)
            .unwrap_or(measured_at);
        let accumulator = rt
            .accumulators
            .entry(journal.session_id)
            .or_insert_with(|| JitterAccumulator::restore(journal.jitter_buckets));
        accumulator.start(monitor, measured_at.max(stream_started), route);
        accumulator.observe(JitterObservation {
            evidence,
            jitter_ms: jitter,
            measured_at,
            received_at: Instant::now(),
            observed_at: Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
            route,
        });
    }
    pub async fn checkpoint(&self, stop: bool) {
        let mut rt = self.runtime.lock().await;
        if self.stopped.load(Ordering::SeqCst) || !rt.initialized {
            return;
        }
        if let Some(next) = checkpoint_accumulators(&rt) {
            let _ = self.commit(&mut rt, next).await;
        }
        if stop {
            for accumulator in rt.accumulators.values_mut() {
                accumulator.stop();
            }
            rt.streaming = None;
        }
        rt.last_checkpoint = Instant::now();
    }
    async fn observation_gap(&self) {
        self.checkpoint(true).await;
        let mut rt = self.runtime.lock().await;
        if self.stopped.load(Ordering::SeqCst) || !rt.initialized {
            return;
        }
        let mut next = rt.outbox.clone();
        let mut changed = false;
        for journal in next.sessions.values_mut() {
            changed |= journal.mark_gap();
        }
        if changed {
            let _ = self.commit(&mut rt, next).await;
        }
    }

    pub async fn confirmed_destroy(&self, context: &AppContext, instance: u64) {
        self.finalize(context, instance, false).await;
    }
    pub async fn confirmed_external_destroy(&self, context: &AppContext, instance: u64) {
        self.finalize(context, instance, true).await;
    }
    async fn finalize(&self, context: &AppContext, instance: u64, external: bool) {
        self.checkpoint(false).await;
        let state = context.load_state().await.feedback_program;
        let mut rt = self.runtime.lock().await;
        if !self.permits(&state, &rt) {
            return;
        }
        let mut next = rt.outbox.clone();
        if let Some(journal) = next
            .sessions
            .values_mut()
            .find(|j| j.provider_instance_id.as_deref() == Some(instance.to_string().as_str()))
        {
            if external {
                journal.mark_gap();
                journal.external_disappearance = true;
                journal.review = None;
            }
            if journal
                .lifecycle
                .milestone("destroyed", &Utc::now().to_rfc3339())
            {
                journal.revision += 1;
            }
            if let Some(accumulator) = rt.accumulators.get_mut(&journal.session_id) {
                accumulator.stop();
            }
            let _ = self.commit(&mut rt, next).await;
            rt.last_upload = Instant::now()
                .checked_sub(UPLOAD_INTERVAL)
                .unwrap_or_else(Instant::now);
        }
    }
    pub async fn reconcile_missing(&self, context: &AppContext, owned: &HashSet<u64>) {
        self.checkpoint(false).await;
        let snapshot = context.load_state().await;
        let fingerprint = journal::provider_key_fingerprint(&snapshot.credentials.vast_api_key);
        let state = snapshot.feedback_program;
        let mut rt = self.runtime.lock().await;
        if !self.permits(&state, &rt) {
            return;
        }
        let mut next = rt.outbox.clone();
        let mut changed = false;
        for j in next.sessions.values_mut() {
            let present = j
                .provider_instance_id
                .as_ref()
                .and_then(|id| id.parse::<u64>().ok())
                .is_some_and(|id| owned.contains(&id));
            if present && j.provider_credential_fingerprint != fingerprint {
                // Positive ownership evidence permits a key rotation; absence from
                // an unrelated account never proves this rental was destroyed.
                j.provider_credential_fingerprint = fingerprint.clone();
                changed = true;
            }
            if j.provider_credential_fingerprint != fingerprint {
                continue;
            }
            if j.external_disappearance
                && j.lifecycle.destroyed_at.is_none()
                && j.provider_instance_id
                    .as_ref()
                    .and_then(|id| id.parse::<u64>().ok())
                    .is_some_and(|id| owned.contains(&id))
            {
                j.external_disappearance = false;
                j.revision += 1;
                changed = true;
            }
            if j.provider_instance_id
                .as_ref()
                .and_then(|id| id.parse::<u64>().ok())
                .is_some_and(|id| !owned.contains(&id))
                && j.lifecycle.destroyed_at.is_none()
                && !j.external_disappearance
            {
                j.mark_gap();
                j.external_disappearance = true;
                j.review = None;
                j.revision += 1;
                changed = true;
                if let Some(accumulator) = rt.accumulators.get_mut(&j.session_id) {
                    accumulator.stop();
                }
            }
        }
        if changed {
            let _ = self.commit(&mut rt, next).await;
        }
    }
    pub async fn tracked_instances(&self, context: &AppContext) -> Vec<u64> {
        let snapshot = context.load_state().await;
        let fingerprint = journal::provider_key_fingerprint(&snapshot.credentials.vast_api_key);
        let state = snapshot.feedback_program;
        let rt = self.runtime.lock().await;
        if !self.permits(&state, &rt) {
            return Vec::new();
        }
        rt.outbox
            .sessions
            .values()
            .filter(|j| {
                j.lifecycle.destroyed_at.is_none()
                    && j.provider_credential_fingerprint == fingerprint
            })
            .filter_map(|j| j.provider_instance_id.as_ref()?.parse().ok())
            .collect()
    }

    async fn synchronize_control(
        &self,
        context: &AppContext,
        control: FeedbackControl,
    ) -> Result<(), Failure> {
        let generation = *self.cancellation.borrow();
        let state = context.load_state().await.feedback_program;
        if state.pending_control_action.as_ref() != Some(&control) {
            return Ok(());
        }
        if control.kind == "delete" {
            if state.enrollment_proof_ref.is_some() {
                let recovered_credential = match state.credential_ref.as_deref() {
                    Some(reference) => secret_get(reference).await.map_err(persistence_failure)?,
                    None => None,
                };
                if recovered_credential
                    .as_deref()
                    .is_none_or(|token| !valid_secret(token))
                {
                    self.finish_enrollment(context).await?;
                    return Ok(());
                }
                // A partial protected response may already hold the bearer even
                // after enrollment-proof expiry. That ownership still permits DELETE /me.
            }
            if state.receipt_id.is_none() {
                let code = match state.deletion_code_ref.as_deref() {
                    Some(reference) => secret_get(reference).await.map_err(persistence_failure)?,
                    None => None,
                };
                let response = if let Some(code) = code {
                    // Recovery deletion is retry-safe even after participant revocation or a lost DELETE response.
                    self.http
                        .request(
                            Method::POST,
                            "/deletions",
                            None,
                            Some(&json!({"deletionCode": code, "requestId": control.request_id})),
                            Some(&control.request_id),
                        )
                        .await?
                } else {
                    let token = secret_get(state.credential_ref.as_deref().ok_or_else(|| {
                        Failure::local("Deletion ownership credential is missing")
                    })?)
                    .await
                    .map_err(persistence_failure)?
                    .ok_or_else(|| Failure::local("Deletion ownership credential is missing"))?;
                    self.http
                        .request(
                            Method::DELETE,
                            "/me",
                            Some(&token),
                            None,
                            Some(&control.request_id),
                        )
                        .await?
                };
                let receipt = required_uuid(&response, "receiptId").map_err(protocol_failure)?;
                let token = required_secret(&response, "statusToken").map_err(protocol_failure)?;
                let status = deletion_status(&response)?;
                let reference = state
                    .receipt_token_ref
                    .clone()
                    .ok_or_else(|| Failure::local("Protected receipt destination is missing"))?;
                secret_put(&reference, &token)
                    .await
                    .map_err(persistence_failure)?;
                context
                    .update_state(|s| {
                        s.feedback_program.enabled = false;
                        s.feedback_program.enrollment_status = "deleted".into();
                        s.feedback_program.receipt_id = Some(receipt);
                        s.feedback_program.receipt_status = Some(status);
                        // Keep the control until protected cleanup also succeeds, across crashes/restarts.
                    })
                    .await
                    .map_err(persistence_failure)?;
            }
            self.cleanup_deleted(context).await?;
            return Ok(());
        }
        let token = secret_get(
            state
                .credential_ref
                .as_deref()
                .ok_or_else(|| Failure::local("Feedback credential is missing"))?,
        )
        .await
        .map_err(persistence_failure)?
        .ok_or_else(|| Failure::local("Feedback credential is missing"))?;
        let response = self.http.request(Method::PUT, "/me/consent", Some(&token), Some(&json!({"requestId": control.request_id,
            "expectedEpoch": control.expected_epoch, "enabled": control.enabled, "optionalContextEnabled": control.optional_context_enabled})), Some(&control.request_id)).await?;
        let epoch = response["consentEpoch"]
            .as_u64()
            .filter(|n| *n > control.expected_epoch && *n <= 9_007_199_254_740_991)
            .ok_or_else(|| Failure::local("Consent response has no advancing epoch"))?;
        if response["enabled"].as_bool() != Some(control.enabled)
            || response["optionalContextEnabled"].as_bool()
                != Some(control.optional_context_enabled)
        {
            return Err(Failure::local(
                "Consent response does not acknowledge the saved choice",
            ));
        }
        if state.enabled && control.enabled && !control.withdraw_after_acknowledgement {
            let mut rt = self.runtime.lock().await;
            let mut next = rt.outbox.clone();
            for j in next.sessions.values_mut() {
                if j.consent_epoch != epoch {
                    j.consent_epoch = epoch;
                    j.revision += 1;
                }
            }
            self.commit(&mut rt, next)
                .await
                .map_err(persistence_failure)?;
        } else {
            self.clear_outbox().await.map_err(persistence_failure)?;
        }
        let withdrawing = control.withdraw_after_acknowledgement
            || (*self.cancellation.borrow() != generation && control.enabled);
        context
            .update_state(|s| {
                s.feedback_program.consent_epoch = epoch;
                s.feedback_program.enabled = control.enabled && !withdrawing;
                s.feedback_program.optional_context_enabled =
                    control.optional_context_enabled && !withdrawing;
                s.feedback_program.pending_control_action = withdrawing.then(|| FeedbackControl {
                    kind: "consent".into(),
                    request_id: Uuid::new_v4().to_string(),
                    expected_epoch: epoch,
                    enabled: false,
                    optional_context_enabled: false,
                    withdraw_after_acknowledgement: false,
                });
            })
            .await
            .map_err(persistence_failure)?;
        let mut rt = self.runtime.lock().await;
        rt.authorized = true;
        rt.status = None;
        // Rejoining never repopulates old rentals. Only begin_rental can create a new journal.
        rt.initialized = true;
        self.stopped.store(
            !control.enabled || withdrawing || !rt.storage_ok,
            Ordering::SeqCst,
        );
        drop(rt);
        self.publish_local_permission(&context.load_state().await.feedback_program);
        Ok(())
    }
    async fn cleanup_deleted(&self, context: &AppContext) -> Result<(), Failure> {
        let state = context.load_state().await.feedback_program;
        self.clear_outbox().await.map_err(persistence_failure)?;
        for reference in [
            &state.credential_ref,
            &state.deletion_code_ref,
            &state.enrollment_proof_ref,
        ]
        .into_iter()
        .flatten()
        {
            secret_delete(reference)
                .await
                .map_err(persistence_failure)?;
        }
        context
            .update_state(|s| {
                let f = &mut s.feedback_program;
                f.credential_ref = None;
                f.deletion_code_ref = None;
                f.enrollment_proof_ref = None;
                f.enrollment_request_id = None;
                f.enrollment_started_at = None;
                f.user_id = None;
                f.installation_id = None;
                f.optional_context_enabled = false;
                f.pending_control_action = None;
                for server in &mut s.provisioned_servers {
                    server.feedback_session_id = None;
                    server.feedback_journal_ref = None;
                }
                s.instance.feedback_session_id = None;
                s.instance.feedback_journal_ref = None;
            })
            .await
            .map_err(persistence_failure)?;
        self.runtime.lock().await.authorized = false;
        Ok(())
    }
    async fn poll_receipt(&self, context: &AppContext) -> Result<(), Failure> {
        let state = context.load_state().await.feedback_program;
        let (Some(receipt), Some(reference)) = (&state.receipt_id, &state.receipt_token_ref) else {
            return Ok(());
        };
        if state.receipt_status.as_deref() == Some("completed") {
            return Ok(());
        }
        let token = secret_get(reference)
            .await
            .map_err(persistence_failure)?
            .ok_or_else(|| Failure::local("Protected deletion status capability is missing"))?;
        let response = self
            .http
            .request(
                Method::GET,
                &format!("/deletions/{receipt}"),
                Some(&token),
                None,
                None,
            )
            .await?;
        let status = deletion_status(&response)?;
        context
            .update_state(|s| s.feedback_program.receipt_status = Some(status))
            .await
            .map_err(persistence_failure)?;
        Ok(())
    }

    async fn deliver(&self, context: &AppContext) -> Result<(), Failure> {
        let state = context.load_state().await.feedback_program;
        if let Some(control) = state.pending_control_action {
            return self.synchronize_control(context, control).await;
        }
        if state.enrollment_status == "pending" && state.enrollment_proof_ref.is_some() {
            self.finish_enrollment(context).await?;
            self.initialize(context)
                .await
                .map_err(persistence_failure)?;
            return Ok(());
        }
        if state.receipt_id.is_some() {
            return self.poll_receipt(context).await;
        }
        if state.enabled && self.runtime.lock().await.last_notices.elapsed() >= UPLOAD_INTERVAL {
            // The previously accepted enrollment is the permission for this foreground/background refresh.
            // A timeout does not pause local measurement; only a valid explicit pause does.
            self.read_notices(context, false).await.map_err(|_| Failure { message: "Feedback availability refresh failed; local consented measurements remain queued".into(), status: None, retryable: true, retry_after: None })?;
            self.runtime.lock().await.last_notices = Instant::now();
        }
        let state = context.load_state().await.feedback_program;
        {
            let mut rt = self.runtime.lock().await;
            let mut next = rt.outbox.clone();
            if next.prune() {
                self.commit(&mut rt, next)
                    .await
                    .map_err(persistence_failure)?;
                rt.status = Some(
                    "Expired pending feedback was discarded without inventing outcomes".into(),
                );
            }
        }
        let jobs = {
            let rt = self.runtime.lock().await;
            if !self.permits(&state, &rt) || rt.last_upload.elapsed() < UPLOAD_INTERVAL {
                return Ok(());
            }
            rt.outbox
                .sessions
                .values()
                .filter(|j| !j.rejected && j.snapshot().is_some())
                .cloned()
                .collect::<Vec<_>>()
        };
        for j in jobs.into_iter().take(MAX_SESSIONS) {
            if j.revision > j.acknowledged_revision {
                let response = self
                    .participant(
                        context,
                        Method::PUT,
                        &format!("/sessions/{}", j.session_id),
                        j.snapshot().as_ref(),
                        Some(j.consent_epoch),
                        None,
                    )
                    .await;
                match response {
                    Ok(value) if value["revision"].as_u64() == Some(j.revision) => {
                        let mut rt = self.runtime.lock().await;
                        let mut next = rt.outbox.clone();
                        if let Some(current) = next.sessions.get_mut(&j.session_id) {
                            current.acknowledged_revision =
                                current.acknowledged_revision.max(j.revision);
                        }
                        self.commit(&mut rt, next)
                            .await
                            .map_err(persistence_failure)?;
                    }
                    Ok(_) => {
                        return Err(Failure::local(
                            "Feedback acknowledgement has the wrong revision",
                        ))
                    }
                    Err(failure) => {
                        self.reject_permanent(&j.session_id, false, &failure).await;
                        return Err(failure);
                    }
                }
            }
            if j.lifecycle.destroyed_at.is_some() && j.review_eligible() {
                if let Some(review) = j
                    .review
                    .filter(|r| !r.rejected && r.revision > r.acknowledged_revision)
                {
                    let path = format!("/sessions/{}/review", j.session_id);
                    let response = if review.score.is_none() && review.comment.is_none() {
                        self.participant(
                            context,
                            Method::DELETE,
                            &path,
                            None,
                            Some(j.consent_epoch),
                            Some(review.revision),
                        )
                        .await
                    } else {
                        self.participant(context, Method::PUT, &path, Some(&json!({"revision": review.revision, "score": review.score, "comment": review.comment})), Some(j.consent_epoch), None).await
                    };
                    match response {
                        Ok(value)
                            if value["reviewRevision"]
                                .as_u64()
                                .or_else(|| value["revision"].as_u64())
                                == Some(review.revision) =>
                        {
                            let mut rt = self.runtime.lock().await;
                            let mut next = rt.outbox.clone();
                            if let Some(current) = next
                                .sessions
                                .get_mut(&j.session_id)
                                .and_then(|j| j.review.as_mut())
                            {
                                if current.revision == review.revision {
                                    current.acknowledged_revision = review.revision;
                                }
                            }
                            self.commit(&mut rt, next)
                                .await
                                .map_err(persistence_failure)?;
                        }
                        Ok(_) => {
                            return Err(Failure::local(
                                "Review acknowledgement has the wrong revision",
                            ))
                        }
                        Err(failure) => {
                            self.reject_permanent(&j.session_id, true, &failure).await;
                            return Err(failure);
                        }
                    }
                }
            }
        }
        self.runtime.lock().await.last_upload = Instant::now();
        Ok(())
    }
    async fn reject_permanent(&self, session: &str, review: bool, failure: &Failure) {
        if failure.retryable
            || failure.status.is_none()
            || matches!(failure.status, Some(401 | 403))
        {
            return;
        }
        let mut rt = self.runtime.lock().await;
        let mut next = rt.outbox.clone();
        if let Some(j) = next.sessions.get_mut(session) {
            if review {
                if let Some(r) = &mut j.review {
                    r.rejected = true;
                }
            } else {
                j.rejected = true;
            }
        }
        let _ = self.commit(&mut rt, next).await;
    }
    async fn defer(&self, failure: &Failure) {
        let mut rt = self.runtime.lock().await;
        rt.status = Some(failure.message.clone());
        rt.attempts = rt.attempts.saturating_add(1);
        let delay = if failure.retryable {
            Duration::from_secs(
                (5_u64 * (1_u64 << rt.attempts.min(8))).min(900)
                    + u64::from(rand::random::<u8>() % 5),
            )
        } else {
            Duration::from_secs(900)
        };
        rt.next_attempt = Instant::now() + failure.retry_after.unwrap_or(delay).max(delay);
    }

    pub async fn foreground(&self, context: &AppContext) {
        if self.initialize(context).await.is_err() {
            self.stop_now();
        }
        let mut rt = self.runtime.lock().await;
        // Never bypass Retry-After just because a window regains focus.
        if rt.attempts == 0 {
            rt.next_attempt = Instant::now();
        }
    }
    pub fn start(app: AppHandle, context: AppContext) {
        let sampling_app = app.clone();
        let sampling_context = context.clone();
        tauri::async_runtime::spawn(async move {
            let mut tick = tokio::time::interval(Duration::from_millis(250));
            tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            let mut previous = (Instant::now(), Utc::now());
            loop {
                tick.tick().await;
                let now = (Instant::now(), Utc::now());
                let wall = now.1.signed_duration_since(previous.1).num_milliseconds();
                let mono = now.0.saturating_duration_since(previous.0).as_millis() as i64;
                if mono > 3_000 || (wall - mono).abs() > 3_000 {
                    sampling_context.feedback.observation_gap().await;
                }
                previous = now;
                sample_runtime(&sampling_app, &sampling_context).await;
                if sampling_context
                    .feedback
                    .runtime
                    .lock()
                    .await
                    .last_checkpoint
                    .elapsed()
                    >= CHECKPOINT_INTERVAL
                {
                    sampling_context.feedback.checkpoint(false).await;
                }
            }
        });
        tauri::async_runtime::spawn(async move {
            context.feedback.foreground(&context).await;
            let mut tick = tokio::time::interval(Duration::from_secs(1));
            tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                tick.tick().await;
                let service = &context.feedback;
                if service.runtime.lock().await.last_checkpoint.elapsed() >= CHECKPOINT_INTERVAL {
                    service.checkpoint(false).await;
                }
                let Some(_operation) = service.operation.try_lock().ok() else {
                    continue;
                };
                {
                    let rt = service.runtime.lock().await;
                    if rt.next_attempt > Instant::now() || rt.control_blocked {
                        continue;
                    }
                }
                let state = context.load_state().await.feedback_program;
                if !service.http.configured()
                    || !Self::supported()
                    || (!state.enabled
                        && state.pending_control_action.is_none()
                        && state.enrollment_proof_ref.is_none()
                        && (state.receipt_id.is_none()
                            || state.receipt_status.as_deref() == Some("completed")))
                {
                    continue;
                }
                match service.deliver(&context).await {
                    Ok(()) => {
                        let mut rt = service.runtime.lock().await;
                        rt.attempts = 0;
                        rt.next_attempt = Instant::now()
                            + if state.receipt_id.is_some() {
                                Duration::from_secs(60)
                            } else {
                                Duration::from_secs(1)
                            };
                    }
                    Err(failure) => {
                        if !failure.retryable
                            && (state.pending_control_action.is_some()
                                || state.enrollment_proof_ref.is_some())
                        {
                            service.runtime.lock().await.control_blocked = true;
                        }
                        service.defer(&failure).await;
                    }
                }
            }
        });
    }
}

fn checkpoint_accumulators(rt: &Runtime) -> Option<Outbox> {
    let mut next = rt.outbox.clone();
    let mut changed = false;
    for (id, accumulator) in &rt.accumulators {
        if let Some(j) = next.sessions.get_mut(id) {
            if j.jitter_buckets != accumulator.buckets() {
                j.jitter_buckets = accumulator.buckets().to_vec();
                j.revision += 1;
                changed = true;
            }
        }
    }
    changed.then_some(next)
}

/// Bounded waiting on native hooks, not cancellation of a half-written journal.
/// A slow disk task may finish later; the provider operation is never held hostage to it.
pub async fn native_task<T: Send + 'static>(
    future: impl Future<Output = T> + Send + 'static,
) -> Option<T> {
    let task = tokio::spawn(future);
    tokio::time::timeout(Duration::from_millis(500), task)
        .await
        .ok()
        .and_then(Result::ok)
}
pub async fn observe_persisted_state(context: AppContext, state: PersistedAppState) {
    if !context.feedback.locally_running() {
        return;
    }
    let _ = native_task(async move {
        context.feedback.observe_state(&state).await;
    })
    .await;
}
pub async fn sample_runtime(app: &AppHandle, context: &AppContext) {
    if !context.feedback.locally_running() {
        return;
    }
    let moonlight = app.state::<crate::moonlight::composition::MoonlightManager>();
    let stats = moonlight.runtime.latest_statistics();
    let instance = stats
        .active_host_id
        .as_deref()
        .and_then(|host| host.strip_prefix("instance-"))
        .and_then(|id| id.parse::<u64>().ok());
    let current = moonlight
        .active_stream_instance_id
        .lock()
        .ok()
        .and_then(|id| *id);
    let valid = stats.state == "streaming"
        && stats.sampled_at.elapsed() < FRAME_FRESHNESS
        && instance.is_some()
        && instance == current;
    context
        .feedback
        .observe_runtime(
            context,
            instance,
            stats.session_generation,
            valid,
            stats.feedback_video_frame_count,
        )
        .await;
}
pub async fn before_teardown(app: &AppHandle, context: &AppContext) {
    if !context.feedback.locally_running() {
        return;
    }
    let app = app.clone();
    let context = context.clone();
    let _ = native_task(async move {
        sample_runtime(&app, &context).await;
        context.feedback.checkpoint(true).await;
    })
    .await;
}
pub fn route_for(state: &PersistedAppState, instance: u64) -> RouteMode {
    match state
        .provisioned_servers
        .iter()
        .find(|s| s.instance_id == instance)
        .and_then(|s| s.network.active_transport)
    {
        Some(TransportKind::Direct) => RouteMode::Direct,
        Some(TransportKind::CloudflareTurn) => RouteMode::Relay,
        _ => RouteMode::Mixed,
    }
}

fn input(message: &str) -> AppError {
    AppError::Command(message.into())
}
fn as_app_error(failure: Failure) -> AppError {
    input(&failure.message)
}
fn protocol_failure(_: AppError) -> Failure {
    Failure::local("Feedback returned an incomplete or invalid acknowledgement")
}
fn persistence_failure(_: AppError) -> Failure {
    Failure { message: "Feedback protected or durable storage is unavailable; pending control is retained for recovery".into(), status: None, retryable: true, retry_after: None }
}
fn new_reference() -> String {
    format!("secure-store://feedback/{}", Uuid::new_v4())
}
async fn secret_get(reference: &str) -> AppResult<Option<String>> {
    if !crate::models::feedback::valid_reference(reference) {
        return Err(input("Invalid protected feedback reference"));
    }
    let reference = reference.to_string();
    tokio::task::spawn_blocking(move || secure_store::get(SECRET_SERVICE, &reference))
        .await
        .map_err(|_| input("Protected feedback store task failed"))?
}
async fn secret_put(reference: &str, value: &str) -> AppResult<()> {
    if !crate::models::feedback::valid_reference(reference) {
        return Err(input("Invalid protected feedback reference"));
    }
    let reference = reference.to_string();
    let value = value.to_string();
    tokio::task::spawn_blocking(move || secure_store::put(SECRET_SERVICE, &reference, &value))
        .await
        .map_err(|_| input("Protected feedback store task failed"))?
}
async fn secret_put_pair(a: &str, av: &str, b: &str, bv: &str) -> AppResult<()> {
    if !crate::models::feedback::valid_reference(a) || !crate::models::feedback::valid_reference(b)
    {
        return Err(input("Invalid protected feedback reference"));
    }
    let values = BTreeMap::from([
        (a.to_string(), Some(av.to_string())),
        (b.to_string(), Some(bv.to_string())),
    ]);
    tokio::task::spawn_blocking(move || secure_store::update(SECRET_SERVICE, &values))
        .await
        .map_err(|_| input("Protected feedback store task failed"))?
}
async fn secret_delete(reference: &str) -> AppResult<()> {
    if !crate::models::feedback::valid_reference(reference) {
        return Err(input("Invalid protected feedback reference"));
    }
    let reference = reference.to_string();
    tokio::task::spawn_blocking(move || secure_store::delete(SECRET_SERVICE, &reference))
        .await
        .map_err(|_| input("Protected feedback cleanup task failed"))?
}
async fn pending_enrollment(
    state: &FeedbackProgramState,
) -> AppResult<(String, EnrollmentRequest)> {
    let start = state
        .enrollment_started_at
        .as_deref()
        .and_then(|at| chrono::DateTime::parse_from_rfc3339(at).ok())
        .ok_or_else(|| input("Enrollment recovery timestamp is invalid"))?;
    let age = Utc::now().timestamp() - start.timestamp();
    if !(0..24 * 60 * 60).contains(&age) {
        return Err(input("Enrollment recovery expires after 24 hours"));
    }
    if state
        .enrollment_request_id
        .as_deref()
        .is_none_or(|id| Uuid::parse_str(id).is_err())
        || state
            .installation_id
            .as_deref()
            .is_none_or(|id| Uuid::parse_str(id).is_err())
    {
        return Err(input("Enrollment recovery identity is invalid"));
    }
    let protected = secret_get(
        state
            .enrollment_proof_ref
            .as_deref()
            .ok_or_else(|| input("Enrollment proof reference is missing"))?,
    )
    .await?
    .ok_or_else(|| input("Enrollment proof is missing"))?;
    let saved: Value =
        serde_json::from_str(&protected).map_err(|_| input("Enrollment proof is invalid"))?;
    let proof = saved["proof"]
        .as_str()
        .filter(|proof| {
            URL_SAFE_NO_PAD
                .decode(proof)
                .is_ok_and(|bytes| bytes.len() == 32)
        })
        .ok_or_else(|| input("Enrollment proof is invalid"))?
        .to_string();
    let request = serde_json::from_value(saved["request"].clone())
        .map_err(|_| input("Enrollment choices could not be recovered"))?;
    Ok((proof, request))
}
fn required_uuid(value: &Value, key: &str) -> AppResult<String> {
    value[key]
        .as_str()
        .filter(|v| Uuid::parse_str(v).is_ok())
        .map(str::to_string)
        .ok_or_else(|| input("Feedback returned an invalid identifier"))
}
fn valid_secret(value: &str) -> bool {
    value.len() >= 32
        && value.len() <= 2_048
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}
fn required_secret(value: &Value, key: &str) -> AppResult<String> {
    value[key]
        .as_str()
        .filter(|v| valid_secret(v))
        .map(str::to_string)
        .ok_or_else(|| input("Feedback returned an invalid protected credential"))
}
fn deletion_status(value: &Value) -> Result<String, Failure> {
    value["status"]
        .as_str()
        .filter(|v| matches!(*v, "pending" | "completed"))
        .map(str::to_string)
        .ok_or_else(|| Failure::local("Deletion returned an invalid status"))
}
fn validate_program(document: &Value) -> AppResult<()> {
    if document["schemaVersion"].as_u64() != Some(1)
        || !document["available"].is_boolean()
        || !document["collectionPaused"].is_boolean()
        || !document["metrics"].is_array()
        || ["privacyVersion", "termsVersion", "disclosure"]
            .iter()
            .any(|key| document[key].as_str().is_none_or(|s| s.is_empty()))
    {
        return Err(input("Feedback notices are incomplete or unsupported"));
    }
    if document["available"].as_bool() == Some(true)
        && ["privacyUrl", "termsUrl", "deletionUrl", "contact"]
            .iter()
            .any(|key| document[key].as_str().is_none_or(|s| s.is_empty()))
    {
        return Err(input(
            "Feedback publication/operator details are not configured",
        ));
    }
    Ok(())
}
fn validate_profile(request: &mut EnrollmentRequest) -> AppResult<()> {
    if !request.optional_context_enabled || request.optional_profile.is_null() {
        request.optional_profile = json!({});
    }
    let profile = request
        .optional_profile
        .as_object_mut()
        .ok_or_else(|| input("Optional profile must be an object"))?;
    for (key, value) in profile.iter_mut() {
        let max = match key.as_str() {
            "country" => 2,
            "region" => 80,
            "connectionCategory" | "bandwidthTier" => 32,
            _ => return Err(input("Unsupported optional profile field")),
        };
        if value.is_null() {
            continue;
        }
        let text = value
            .as_str()
            .ok_or_else(|| input("Optional profile values must be text or null"))?
            .trim();
        if text.chars().count() > max
            || text.chars().any(char::is_control)
            || (key == "country"
                && !text.is_empty()
                && (text.len() != 2 || !text.bytes().all(|b| b.is_ascii_uppercase())))
        {
            return Err(input("Invalid optional profile value"));
        }
        *value = if text.is_empty() {
            Value::Null
        } else {
            Value::String(text.to_string())
        };
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn default_is_off_and_an_enabled_bit_alone_never_authorizes() {
        let service = FeedbackProgramService::new(std::path::Path::new("state.json"));
        let mut state = FeedbackProgramState::default();
        state.enabled = true;
        assert!(!service.permits(&state, &Runtime::default()));
    }
    #[test]
    fn optional_profile_is_allowlisted_and_blank_is_valid() {
        let mut request = EnrollmentRequest {
            privacy_version: "1".into(),
            terms_version: "1".into(),
            participation_accepted: true,
            optional_context_enabled: true,
            optional_profile: json!({"country": "", "region": "", "connectionCategory": "fiber"}),
        };
        validate_profile(&mut request).unwrap();
        assert!(request.optional_profile["country"].is_null());
        request.optional_profile = json!({"ssid": "sensitive"});
        assert!(validate_profile(&mut request).is_err());
        request.optional_context_enabled = false;
        validate_profile(&mut request).unwrap();
        assert_eq!(request.optional_profile, json!({}));
    }
    #[test]
    fn public_notices_can_be_unavailable_without_invented_contact_or_domain() {
        let document = json!({"schemaVersion":1,"available":false,"collectionPaused":true,"privacyVersion":"1","termsVersion":"1","disclosure":"Not published","metrics":[],"contact":null});
        assert!(validate_program(&document).is_ok());
        let mut available = document;
        available["available"] = json!(true);
        assert!(validate_program(&available).is_err());
    }
}
