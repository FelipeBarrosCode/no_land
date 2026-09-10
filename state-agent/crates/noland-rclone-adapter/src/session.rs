use crate::{adapter_for, AdapterCredential, AdapterInput, EphemeralRcloneSession, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenMode {
    /// Include refresh tokens in a persistent desktop-local rclone config.
    Durable,
    /// Include refresh capability in a guarded, operation-scoped remote config.
    /// The state agent must remove the config when the operation finishes.
    Operation,
    /// Access token only, suitable only for short-lived read-only uses.
    Ephemeral,
}

pub fn session_from_input(
    input: &AdapterInput,
    operation_id: impl Into<String>,
    mode: TokenMode,
) -> Result<EphemeralRcloneSession> {
    let mut input = input.clone();
    if mode == TokenMode::Ephemeral {
        strip_refresh_token(&mut input);
    }
    let adapter = adapter_for(input.provider);
    let config = adapter.create_config(&input)?;
    let root = adapter.storage_root(&input)?;
    let expires_at_unix = match &input.credentials {
        AdapterCredential::OAuth2 { expires_at, .. } => *expires_at,
        _ => 0,
    };
    let upload_concurrency = input
        .fields
        .get("upload_concurrency")
        .map(|raw| {
            raw.parse::<usize>().map_err(|_| {
                crate::AdapterError::Invalid(format!(
                    "invalid upload_concurrency `{raw}` for {}",
                    input.provider.label()
                ))
            })
        })
        .transpose()?;
    Ok(EphemeralRcloneSession {
        operation_id: operation_id.into(),
        provider: input.provider.as_str().into(),
        backend_type: adapter.backend_type().into(),
        remote_name: root.remote_name,
        root: root.root,
        config_ini: config.to_ini_string(),
        expires_at_unix,
        upload_concurrency,
    })
}

fn strip_refresh_token(input: &mut AdapterInput) {
    if let AdapterCredential::OAuth2 { refresh_token, .. } = &mut input.credentials {
        *refresh_token = None;
    }
}

impl TokenMode {
    pub fn is_ephemeral(self) -> bool {
        self != TokenMode::Durable
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use crate::ProviderKind;

    fn drive_input(upload_concurrency: Option<&str>) -> AdapterInput {
        let mut fields = BTreeMap::from([("folder".into(), "Noland Shared Storage".into())]);
        if let Some(value) = upload_concurrency {
            fields.insert("upload_concurrency".into(), value.into());
        }
        AdapterInput {
            provider: ProviderKind::GoogleDrive,
            remote_name: "noland_drive".into(),
            credentials: AdapterCredential::OAuth2 {
                access_token: "access".into(),
                refresh_token: None,
                expires_at: 1_800_000_000,
            },
            fields,
            bucket: None,
            prefix: None,
        }
    }

    #[test]
    fn session_carries_explicit_provider_upload_concurrency() {
        let session =
            session_from_input(&drive_input(Some("2")), "op", TokenMode::Operation).unwrap();
        assert_eq!(session.upload_concurrency, Some(2));
    }

    #[test]
    fn session_rejects_non_numeric_upload_concurrency() {
        assert!(
            session_from_input(&drive_input(Some("many")), "op", TokenMode::Operation).is_err()
        );
    }
}
