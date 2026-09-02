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
    Ok(EphemeralRcloneSession {
        operation_id: operation_id.into(),
        provider: input.provider.as_str().into(),
        backend_type: adapter.backend_type().into(),
        remote_name: root.remote_name,
        root: root.root,
        config_ini: config.to_ini_string(),
        expires_at_unix,
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
