use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FeedbackProgramState {
    pub schema_version: u32,
    pub enabled: bool,
    pub enrollment_status: String,
    pub installation_id: Option<String>,
    pub user_id: Option<String>,
    pub credential_ref: Option<String>,
    pub deletion_code_ref: Option<String>,
    pub consent_epoch: u64,
    pub privacy_version: Option<String>,
    pub terms_version: Option<String>,
    pub consented_at: Option<String>,
    pub optional_context_enabled: bool,
    pub pending_control_action: Option<FeedbackControl>,
    #[serde(default)]
    pub enrollment_request_id: Option<String>,
    #[serde(default)]
    pub enrollment_proof_ref: Option<String>,
    #[serde(default)]
    pub enrollment_started_at: Option<String>,
    #[serde(default)]
    pub receipt_id: Option<String>,
    #[serde(default)]
    pub receipt_status: Option<String>,
    #[serde(default)]
    pub receipt_token_ref: Option<String>,
    #[serde(default)]
    pub collection_paused: bool,
}

impl Default for FeedbackProgramState {
    fn default() -> Self {
        Self {
            schema_version: 1,
            enabled: false,
            enrollment_status: "not_enrolled".into(),
            installation_id: None,
            user_id: None,
            credential_ref: None,
            deletion_code_ref: None,
            consent_epoch: 0,
            privacy_version: None,
            terms_version: None,
            consented_at: None,
            optional_context_enabled: false,
            pending_control_action: None,
            enrollment_request_id: None,
            enrollment_proof_ref: None,
            enrollment_started_at: None,
            receipt_id: None,
            receipt_status: None,
            receipt_token_ref: None,
            collection_paused: false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FeedbackControl {
    pub kind: String,
    pub request_id: String,
    pub expected_epoch: u64,
    pub enabled: bool,
    pub optional_context_enabled: bool,
    /// A local withdrawal cannot rewrite an in-flight idempotent scope-change payload.
    #[serde(default)]
    pub withdraw_after_acknowledgement: bool,
}

impl FeedbackProgramState {
    pub fn enrolled(&self) -> bool {
        self.schema_version == 1
            && self.enrollment_status == "enrolled"
            && self.consent_epoch > 0
            && self.consent_epoch <= 9_007_199_254_740_991
            && valid_uuid(self.installation_id.as_deref())
            && valid_uuid(self.user_id.as_deref())
            && self.credential_ref.as_deref().is_some_and(valid_reference)
            && self
                .deletion_code_ref
                .as_deref()
                .is_some_and(valid_reference)
            && self.privacy_version.as_deref() == Some("1")
            && self.terms_version.as_deref() == Some("1")
            && self
                .consented_at
                .as_ref()
                .is_some_and(|s| chrono::DateTime::parse_from_rfc3339(s).is_ok())
    }

    pub fn validated(self) -> Self {
        let refs = [
            &self.credential_ref,
            &self.deletion_code_ref,
            &self.enrollment_proof_ref,
            &self.receipt_token_ref,
        ];
        if self.schema_version != 1
            || !matches!(
                self.enrollment_status.as_str(),
                "not_enrolled" | "pending" | "enrolled" | "deleted"
            )
            || self
                .receipt_id
                .as_deref()
                .is_some_and(|id| Uuid::parse_str(id).is_err())
            || self
                .enrollment_request_id
                .as_deref()
                .is_some_and(|id| Uuid::parse_str(id).is_err())
            || self
                .installation_id
                .as_deref()
                .is_some_and(|id| Uuid::parse_str(id).is_err())
            || self
                .user_id
                .as_deref()
                .is_some_and(|id| Uuid::parse_str(id).is_err())
            || (self
                .receipt_status
                .as_deref()
                .is_some_and(|status| !matches!(status, "pending" | "completed")))
            || refs
                .iter()
                .any(|r| r.as_deref().is_some_and(|r| !valid_reference(r)))
            || (self.enabled && !self.enrolled())
            || self.pending_control_action.as_ref().is_some_and(|c| {
                !matches!(c.kind.as_str(), "consent" | "delete")
                    || Uuid::parse_str(&c.request_id).is_err()
                    || (c.expected_epoch == 0 && c.kind != "delete")
                    || (c.kind == "delete" && (c.enabled || c.optional_context_enabled))
            })
        {
            return Self::default();
        }
        self
    }
}

pub fn valid_reference(value: &str) -> bool {
    value
        .strip_prefix("secure-store://feedback/")
        .is_some_and(|suffix| Uuid::parse_str(suffix).is_ok())
}

fn valid_uuid(value: Option<&str>) -> bool {
    value.is_some_and(|s| Uuid::parse_str(s).is_ok())
}

pub fn deserialize_program<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<FeedbackProgramState, D::Error> {
    let raw = Value::deserialize(deserializer)?;
    Ok(serde_json::from_value::<FeedbackProgramState>(raw)
        .map(FeedbackProgramState::validated)
        .unwrap_or_default())
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EnrollmentRequest {
    pub privacy_version: String,
    pub terms_version: String,
    pub participation_accepted: bool,
    pub optional_context_enabled: bool,
    #[serde(default)]
    pub optional_profile: Value,
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn malformed_or_future_consent_cannot_enable_feedback() {
        for value in [
            serde_json::json!({"schemaVersion": 2, "enabled": true}),
            serde_json::json!({"enabled": true}),
            Value::Null,
        ] {
            let state = serde_json::from_value::<FeedbackProgramState>(value)
                .map(FeedbackProgramState::validated)
                .unwrap_or_default();
            assert!(!state.enabled);
            assert!(state.installation_id.is_none());
        }
    }
}
