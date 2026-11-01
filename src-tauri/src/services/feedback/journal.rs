use std::{
    collections::{BTreeMap, HashSet},
    path::Path,
};

use chrono::Utc;
use noland_network_contracts::feedback::{
    valid_provider_id, JitterBucket, MAX_BUCKETS, METRIC_VERSION,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::{
    errors::{AppError, AppResult},
    models::app_state::OfferCandidate,
    utils::atomic_file::write_atomically,
};

pub const MAX_SESSIONS: usize = 100;
pub const MAX_BYTES: usize = 5 * 1024 * 1024;
pub const MAX_PAYLOAD_BYTES: usize = 64 * 1024;
const MAX_AGE_SECONDS: i64 = 30 * 24 * 60 * 60;
const EVENTS: [&str; 6] = [
    "rentalStarted",
    "ready",
    "paired",
    "setupSuccess",
    "firstStreamFrame",
    "destroyed",
];

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Lifecycle {
    pub rental_started_at: String,
    pub ready_at: Option<String>,
    pub paired_at: Option<String>,
    pub setup_success_at: Option<String>,
    pub first_stream_frame_at: Option<String>,
    pub destroyed_at: Option<String>,
    pub observation_complete: bool,
    pub last_event_sequence: u64,
    pub event_order: BTreeMap<String, Option<u64>>,
}

impl Lifecycle {
    pub fn milestone(&mut self, key: &str, at: &str) -> bool {
        if self.destroyed_at.is_some() {
            return false;
        }
        let target = match key {
            "ready" => &mut self.ready_at,
            "paired" => &mut self.paired_at,
            "setupSuccess" => &mut self.setup_success_at,
            "firstStreamFrame" => &mut self.first_stream_frame_at,
            "destroyed" => &mut self.destroyed_at,
            _ => return false,
        };
        if target.is_some() {
            return false;
        }
        *target = Some(at.into());
        self.last_event_sequence += 1;
        self.event_order
            .insert(key.into(), Some(self.last_event_sequence));
        true
    }

    pub fn ready_and_paired(&mut self, ready: bool, paired: bool, at: &str) -> bool {
        let mut changed = false;
        if ready {
            changed |= self.milestone("ready", at);
        }
        if paired {
            changed |= self.milestone("paired", at);
        }
        if self.ready_at.is_some() && self.paired_at.is_some() {
            changed |= self.milestone("setupSuccess", at);
        }
        changed
    }

    fn valid(&self) -> bool {
        let timestamps = [
            &self.rental_started_at,
            self.ready_at.as_ref().unwrap_or(&self.rental_started_at),
            self.paired_at.as_ref().unwrap_or(&self.rental_started_at),
            self.setup_success_at
                .as_ref()
                .unwrap_or(&self.rental_started_at),
            self.first_stream_frame_at
                .as_ref()
                .unwrap_or(&self.rental_started_at),
            self.destroyed_at
                .as_ref()
                .unwrap_or(&self.rental_started_at),
        ];
        if timestamps.iter().any(|at| !valid_time(at))
            || self.event_order.len() != EVENTS.len()
            || self.event_order.get("rentalStarted") != Some(&Some(1))
        {
            return false;
        }
        let present = [
            true,
            self.ready_at.is_some(),
            self.paired_at.is_some(),
            self.setup_success_at.is_some(),
            self.first_stream_frame_at.is_some(),
            self.destroyed_at.is_some(),
        ];
        let mut sequences = HashSet::new();
        for (key, present) in EVENTS.iter().zip(present) {
            let Some(sequence) = self.event_order.get(*key) else {
                return false;
            };
            if sequence.is_some() != present
                || sequence
                    .is_some_and(|n| n == 0 || n > self.last_event_sequence || !sequences.insert(n))
            {
                return false;
            }
        }
        if self.last_event_sequence != sequences.len() as u64 {
            return false;
        }
        if let Some(success) = self.event_order["setupSuccess"] {
            if self.event_order["ready"].is_none_or(|n| n >= success)
                || self.event_order["paired"].is_none_or(|n| n >= success)
            {
                return false;
            }
        }
        self.event_order["destroyed"].is_none_or(|n| n == self.last_event_sequence)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReviewDraft {
    pub revision: u64,
    pub score: Option<u8>,
    pub comment: Option<String>,
    pub acknowledged_revision: u64,
    pub rejected: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SessionContext {
    pub platform: String,
    pub app_version: String,
    pub collector_version: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HardwareSnapshot {
    pub gpu_name: Option<String>,
    pub rented_gpu_count: Option<u32>,
    pub country: Option<String>,
    pub region: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Journal {
    pub session_id: String,
    pub provider_machine_id: Option<String>,
    pub provider_instance_id: Option<String>,
    pub provider_offer_id: Option<String>,
    /// Local-only binding; never included in snapshot() or any feedback HTTP body.
    pub provider_credential_fingerprint: String,
    pub initial_consent_epoch: u64,
    pub consent_epoch: u64,
    pub revision: u64,
    pub acknowledged_revision: u64,
    pub lifecycle: Lifecycle,
    // Persist context rather than regenerating it at upload: retrying a revision
    // after an app update must have exactly the same payload/digest.
    pub context: SessionContext,
    pub hardware_snapshot: HardwareSnapshot,
    pub jitter_buckets: Vec<JitterBucket>,
    pub review: Option<ReviewDraft>,
    pub rejected: bool,
    pub external_disappearance: bool,
}

impl Journal {
    pub fn pending(offer: &OfferCandidate, epoch: u64, provider_key: &str) -> Self {
        let mut order = BTreeMap::new();
        for name in EVENTS {
            order.insert(name.into(), (name == "rentalStarted").then_some(1));
        }
        Self {
            session_id: Uuid::new_v4().to_string(),
            provider_machine_id: offer.machine_id.clone().filter(|id| valid_provider_id(id)),
            provider_instance_id: None,
            provider_offer_id: Some(offer.id.to_string()).filter(|id| valid_provider_id(id)),
            provider_credential_fingerprint: provider_key_fingerprint(provider_key),
            initial_consent_epoch: epoch,
            consent_epoch: epoch,
            revision: 1,
            acknowledged_revision: 0,
            lifecycle: Lifecycle {
                rental_started_at: Utc::now().to_rfc3339(),
                observation_complete: true,
                last_event_sequence: 1,
                event_order: order,
                ..Default::default()
            },
            context: SessionContext {
                platform: std::env::consts::OS.into(),
                app_version: env!("CARGO_PKG_VERSION").into(),
                collector_version: METRIC_VERSION.into(),
            },
            hardware_snapshot: HardwareSnapshot {
                gpu_name: label(&offer.gpu_name, 128),
                rented_gpu_count: (offer.gpu_count > 0).then_some(offer.gpu_count),
                country: country_code(&offer.country),
                region: label(&offer.region, 80),
            },
            jitter_buckets: Vec::new(),
            review: None,
            rejected: false,
            external_disappearance: false,
        }
    }

    pub fn confirm(&mut self, instance_id: u64, recovered: bool) -> bool {
        let id = instance_id.to_string();
        if !valid_provider_id(&id) || self.provider_instance_id.is_some() {
            return false;
        }
        self.provider_instance_id = Some(id);
        if recovered {
            self.lifecycle.observation_complete = false;
        } else {
            self.lifecycle.rental_started_at = Utc::now().to_rfc3339();
        }
        self.revision += 1;
        true
    }

    pub fn snapshot(&self) -> Option<Value> {
        let machine = self
            .provider_machine_id
            .as_deref()
            .filter(|id| valid_provider_id(id))?;
        let instance = self
            .provider_instance_id
            .as_deref()
            .filter(|id| valid_provider_id(id))?;
        let payload = json!({
            "schemaVersion": 1, "revision": self.revision, "consentEpoch": self.consent_epoch,
            "provider": "vast", "providerMachineId": machine, "providerInstanceId": instance,
            "providerOfferId": self.provider_offer_id, "lifecycle": self.lifecycle,
            "context": self.context, "hardwareSnapshot": self.hardware_snapshot, "jitterBuckets": self.jitter_buckets,
        });
        (serde_json::to_vec(&payload).ok()?.len() <= MAX_PAYLOAD_BYTES).then_some(payload)
    }

    pub fn review_eligible(&self) -> bool {
        self.provider_instance_id.is_some()
            && self.provider_machine_id.is_some()
            && self.lifecycle.first_stream_frame_at.is_some()
            && !self.external_disappearance
            && !self.rejected
    }

    pub fn mark_gap(&mut self) -> bool {
        if self.lifecycle.destroyed_at.is_some() || !self.lifecycle.observation_complete {
            return false;
        }
        self.lifecycle.observation_complete = false;
        self.revision += 1;
        true
    }

    fn valid(&self) -> bool {
        Uuid::parse_str(&self.session_id).is_ok()
            && self.provider_credential_fingerprint.len() == 64
            && self
                .provider_credential_fingerprint
                .bytes()
                .all(|b| b.is_ascii_hexdigit())
            && self
                .provider_machine_id
                .as_deref()
                .is_none_or(valid_provider_id)
            && self
                .provider_instance_id
                .as_deref()
                .is_none_or(valid_provider_id)
            && self
                .provider_offer_id
                .as_deref()
                .is_none_or(valid_provider_id)
            && self.initial_consent_epoch > 0
            && self.consent_epoch >= self.initial_consent_epoch
            && self.revision > 0
            && self.acknowledged_revision <= self.revision
            && self.lifecycle.valid()
            && self.jitter_buckets.len() <= MAX_BUCKETS
            && valid_buckets(&self.jitter_buckets)
            && matches!(
                self.context.platform.as_str(),
                "macos" | "windows" | "linux"
            )
            && label(&self.context.app_version, 128).as_ref() == Some(&self.context.app_version)
            && label(&self.context.collector_version, 128).as_ref()
                == Some(&self.context.collector_version)
            && self.review.as_ref().is_none_or(|r| {
                r.revision > 0
                    && r.acknowledged_revision <= r.revision
                    && r.score.is_none_or(|s| (1..=10).contains(&s))
                    && r.comment.as_deref().is_none_or(valid_comment)
            })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Outbox {
    pub schema_version: u32,
    pub installation_id: Option<String>,
    pub sessions: BTreeMap<String, Journal>,
}
impl Default for Outbox {
    fn default() -> Self {
        Self {
            schema_version: 1,
            installation_id: None,
            sessions: BTreeMap::new(),
        }
    }
}
impl Outbox {
    pub fn load(path: &Path) -> AppResult<Self> {
        if !path.exists() {
            return Ok(Self::default());
        }
        if std::fs::metadata(path)?.len() > MAX_BYTES as u64 {
            return Err(invalid_outbox());
        }
        let mut outbox: Self = serde_json::from_slice(&std::fs::read(path)?)?;
        let mut rentals = HashSet::new();
        if outbox.schema_version != 1
            || outbox.sessions.len() > MAX_SESSIONS
            || (!outbox.sessions.is_empty()
                && outbox
                    .installation_id
                    .as_ref()
                    .is_none_or(|id| Uuid::parse_str(id).is_err()))
            || outbox.sessions.iter().any(|(id, j)| {
                id != &j.session_id
                    || !j.valid()
                    || j.provider_instance_id
                        .as_ref()
                        .is_some_and(|id| !rentals.insert(id.clone()))
            })
        {
            return Err(invalid_outbox());
        }
        outbox.prune();
        Ok(outbox)
    }
    pub fn save(&self, path: &Path) -> AppResult<()> {
        let bytes = serde_json::to_vec(self)?;
        if self.sessions.len() > MAX_SESSIONS || bytes.len() > MAX_BYTES {
            return Err(AppError::State(
                "Feedback journal is full; no rental action was blocked".into(),
            ));
        }
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700))?;
            }
        }
        write_atomically(path, &bytes)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
        }
        Ok(())
    }
    pub fn prune(&mut self) -> bool {
        let before = self.sessions.len();
        let cutoff = Utc::now().timestamp() - MAX_AGE_SECONDS;
        self.sessions.retain(|_, j| {
            chrono::DateTime::parse_from_rfc3339(&j.lifecycle.rental_started_at)
                .is_ok_and(|at| at.timestamp() >= cutoff)
        });
        before != self.sessions.len()
    }
    pub fn make_room(&mut self) {
        self.prune();
        if self.sessions.len() < MAX_SESSIONS {
            return;
        }
        // Fully delivered terminal telemetry is disposable before any active rental,
        // draft, or unresolved lifecycle. Controls live separately in atomic app state.
        let oldest = self
            .sessions
            .iter()
            .filter(|(_, j)| {
                j.lifecycle.destroyed_at.is_some()
                    && j.revision == j.acknowledged_revision
                    && j.review
                        .as_ref()
                        .is_none_or(|r| r.revision == r.acknowledged_revision)
            })
            .min_by_key(|(_, j)| &j.lifecycle.rental_started_at)
            .map(|(id, _)| id.clone());
        if let Some(id) = oldest {
            self.sessions.remove(&id);
        }
    }
    pub fn pending_count(&self) -> usize {
        self.sessions
            .values()
            .filter(|j| {
                j.provider_instance_id.is_some()
                    && (j.revision > j.acknowledged_revision
                        || j.review
                            .as_ref()
                            .is_some_and(|r| r.revision > r.acknowledged_revision))
            })
            .count()
    }
}

pub fn provider_key_fingerprint(key: &str) -> String {
    hex::encode(Sha256::digest(key.trim().as_bytes()))
}

fn invalid_outbox() -> AppError {
    AppError::State(
        "Feedback journal is invalid, too large, or unsupported; collection is paused".into(),
    )
}
fn valid_time(at: &str) -> bool {
    at.len() <= 64 && chrono::DateTime::parse_from_rfc3339(at).is_ok()
}
pub fn valid_comment(comment: &str) -> bool {
    comment.encode_utf16().count() <= 2_000
        && !comment
            .chars()
            .any(|c| (c.is_control() && !matches!(c, '\n' | '\r' | '\t')))
}
fn label(value: &str, max: usize) -> Option<String> {
    let value: String = value
        .trim()
        .chars()
        .filter(|c| !c.is_control())
        .take(max)
        .collect();
    (!value.is_empty()).then_some(value)
}
fn country_code(value: &str) -> Option<String> {
    let country = value.trim().to_ascii_uppercase();
    (country.len() == 2 && country.bytes().all(|b| b.is_ascii_uppercase())).then_some(country)
}
fn valid_buckets(buckets: &[JitterBucket]) -> bool {
    let mut keys = HashSet::new();
    buckets.iter().all(|b| {
        let key = format!(
            "{}:{}:{:?}:{}:{}",
            b.metric_version,
            b.nominal_report_interval_ms,
            b.route_mode,
            b.completeness,
            b.threshold_version
        );
        keys.insert(key)
            && b.sample_count > 0
            && b.sample_count <= 10_000_000
            && [
                b.sum_ms,
                b.min_ms,
                b.max_ms,
                b.high_threshold_ms,
                b.severe_threshold_ms,
            ]
            .iter()
            .all(|v| v.is_finite() && *v >= 0.0)
            && b.min_ms <= b.max_ms
            && b.severe_sample_count <= b.high_sample_count
            && b.high_sample_count <= b.sample_count
            && b.severe_threshold_ms >= b.high_threshold_ms
            && valid_time(&b.first_observed_at)
            && valid_time(&b.last_observed_at)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn offer() -> OfferCandidate {
        serde_json::from_value(json!({"id": 3, "machineId": "9007199254740993", "hostId": 8,
            "hostLabel": "host", "locationLabel": "CA", "city": "", "region": "Quebec", "country": "CA",
            "latitude": 0, "longitude": 0, "reliability": 0.99, "gpuName": "RTX 3090", "gpuRamMb": 24576,
            "gpuCount": 1, "hourlyPrice": 0.2, "availableStorageGb": 100, "estimatedDistanceKm": 0, "score": 1})).unwrap()
    }
    #[test]
    fn confirmed_rental_keeps_lossless_machine_identity_and_stable_snapshot() {
        let mut j = Journal::pending(&offer(), 1, "provider-fixture");
        assert!(j.snapshot().is_none());
        assert!(j.confirm(9_007_199_254_740_995, false));
        assert!(!j.confirm(99, false));
        let snapshot = j.snapshot().unwrap();
        assert_eq!(snapshot["providerMachineId"], "9007199254740993");
        assert_eq!(snapshot["providerInstanceId"], "9007199254740995");
        assert!(!snapshot
            .to_string()
            .contains("providerCredentialFingerprint"));
        assert!(!snapshot.to_string().contains("provider-fixture"));
        let restored: Journal = serde_json::from_value(serde_json::to_value(&j).unwrap()).unwrap();
        assert_eq!(restored.snapshot(), Some(snapshot));
        assert!(restored.valid());
    }
    #[test]
    fn lifecycle_order_is_idempotent_and_wall_clock_independent() {
        let mut j = Journal::pending(&offer(), 1, "provider-fixture");
        j.confirm(4, false);
        assert!(j
            .lifecycle
            .ready_and_paired(true, false, "2026-10-26T14:00:00Z"));
        assert!(!j
            .lifecycle
            .ready_and_paired(true, false, "2026-10-26T14:01:00Z"));
        assert!(j
            .lifecycle
            .ready_and_paired(false, true, "2026-10-26T13:00:00Z"));
        assert!(!j.review_eligible());
        j.lifecycle
            .milestone("firstStreamFrame", "2026-10-26T13:01:00Z");
        assert!(j.review_eligible());
        assert!(j.mark_gap());
        assert!(j.lifecycle.setup_success_at.is_some());
        assert!(j.lifecycle.destroyed_at.is_none());
        j.lifecycle.milestone("destroyed", "2026-10-26T13:02:00Z");
        assert!(!j
            .lifecycle
            .ready_and_paired(true, true, "2026-10-26T15:00:00Z"));
        assert!(j.valid());
    }
    #[test]
    fn unknown_external_removal_never_fakes_a_failure_or_a_review() {
        let mut j = Journal::pending(&offer(), 1, "provider-fixture");
        j.confirm(4, true);
        j.external_disappearance = true;
        assert!(!j.lifecycle.observation_complete);
        assert!(!j.review_eligible());
        assert!(j.lifecycle.destroyed_at.is_none());
    }
    #[test]
    fn atomic_roundtrip_and_corrupt_spool_fail_closed() {
        let root = std::env::temp_dir().join(format!("noland-feedback-{}", Uuid::new_v4()));
        let path = root.join("outbox.json");
        let outbox = Outbox::default();
        outbox.save(&path).unwrap();
        assert_eq!(Outbox::load(&path).unwrap().schema_version, 1);
        std::fs::write(&path, b"{broken").unwrap();
        assert!(Outbox::load(&path).is_err());
        std::fs::remove_dir_all(root).unwrap();
    }
}
