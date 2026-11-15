//! Cache identity is a repository namespace, not an rclone profile label or a
//! credential hash. Where the handoff lacks a stable account ID, an explicitly
//! private exact-config binding is conservative (rotation misses, never aliases).
use std::collections::BTreeMap;

use noland_crypto::MasterKey;
use noland_rclone_adapter::{EphemeralRcloneSession, ProviderRootIdentity};

pub(crate) fn binding(
    identity: Option<ProviderRootIdentity>,
    master: &MasterKey,
) -> Option<String> {
    let identity = identity?;
    // Hash the complete identity, not ProviderRootIdentity's short FNV cache key.
    let mut hash = blake3::Hasher::new_keyed(&master.0);
    hash.update(b"noland-repository-cache-v2\0");
    hash.update(&serde_json::to_vec(&identity).ok()?);
    Some(hash.finalize().to_hex().to_string())
}

/// Preserve streaming's requirement for a provider identity without pretending
/// a profile label identifies an OAuth account. Only a keyed digest is persisted.
pub(crate) fn bound_session_identity(
    session: &EphemeralRcloneSession,
    master: &MasterKey,
) -> ProviderRootIdentity {
    if let Some(identity) = session_identity(session) {
        return identity;
    }
    let mut hash = blake3::Hasher::new_keyed(&master.0);
    hash.update(b"noland-private-session-config-v1\0");
    hash.update(&serde_json::to_vec(&session.storage_identity()).expect("identity serializes"));
    hash.update(&[0]);
    hash.update(session.config_ini.as_bytes());
    ProviderRootIdentity::new(
        &session.provider,
        &session.backend_type,
        format!("private-config-v1-{}", hash.finalize().to_hex()),
        &session.root,
    )
}

pub(crate) fn session_identity(session: &EphemeralRcloneSession) -> Option<ProviderRootIdentity> {
    let mut fields = BTreeMap::new();
    let mut selected = false;
    let mut sections = 0;
    for line in session.config_ini.lines().map(str::trim) {
        if line.is_empty() || line.starts_with(['#', ';']) {
            continue;
        }
        if line.starts_with('[') {
            let name = line.strip_prefix('[')?.strip_suffix(']')?;
            selected = name == session.remote_name;
            sections += 1;
            continue;
        }
        if !selected {
            return None;
        }
        let (key, value) = line.split_once('=')?;
        if fields.insert(key.trim(), value.trim()).is_some() {
            return None;
        }
    }
    // Aliases, config inheritance and duplicate sections are not guessed.
    if sections != 1 || fields.get("type").copied()? != session.backend_type {
        return None;
    }
    let get = |name| fields.get(name).copied().filter(|value| !value.is_empty());
    let mut namespace = BTreeMap::<&str, String>::new();
    let allowed: &[&str] = match session.backend_type.as_str() {
        "local" if std::path::Path::new(&session.root).is_absolute() => &["type", "nounc"],
        "sftp" => {
            namespace.insert("host", get("host")?.to_ascii_lowercase());
            namespace.insert("user", get("user")?.into());
            namespace.insert(
                "port",
                get("port").unwrap_or("22").parse::<u16>().ok()?.to_string(),
            );
            &[
                "type",
                "host",
                "user",
                "port",
                "pass",
                "key_pem",
                "key_file_pass",
                "host_keys",
            ]
        }
        "webdav" => {
            let url = get("url")?;
            // Embedded credentials/signed query tokens cannot become stable
            // namespace fields; these configurations require the private binding.
            if !url.starts_with("https://") && !url.starts_with("http://")
                || url.contains(['@', '?', '#'])
            {
                return None;
            }
            namespace.insert("url", url.trim_end_matches('/').into());
            namespace.insert("user", get("user")?.into());
            namespace.insert("vendor", get("vendor").unwrap_or("other").into());
            &[
                "type",
                "url",
                "user",
                "pass",
                "vendor",
                "nextcloud_chunk_size",
            ]
        }
        "azureblob" => {
            namespace.insert("account", get("account")?.into());
            if let Some(endpoint) = get("endpoint") {
                if endpoint.contains(['@', '?', '#']) {
                    return None;
                }
                namespace.insert("endpoint", endpoint.trim_end_matches('/').into());
            }
            &["type", "account", "key", "endpoint"]
        }
        "onedrive" => {
            namespace.insert("drive_id", get("drive_id")?.into());
            namespace.insert("drive_type", get("drive_type")?.into());
            &[
                "type",
                "drive_id",
                "drive_type",
                "token",
                "client_id",
                "client_secret",
            ]
        }
        "s3" if get("provider") == Some("Cloudflare") => {
            // R2's endpoint contains the stable account ID, unlike access keys.
            let endpoint = get("endpoint")?;
            let host = endpoint.strip_prefix("https://")?.trim_end_matches('/');
            let account = host.strip_suffix(".r2.cloudflarestorage.com")?;
            if account.is_empty() || !account.bytes().all(|b| b.is_ascii_alphanumeric()) {
                return None;
            }
            namespace.insert("endpoint", host.to_ascii_lowercase());
            &[
                "type",
                "provider",
                "endpoint",
                "region",
                "access_key_id",
                "secret_access_key",
                "session_token",
            ]
        }
        // Drive/Dropbox/Box tokens, B2 key IDs and S3 access keys do not provide a
        // stable account identity. Do not infer it from a profile name or token.
        _ => return None,
    };
    if fields.keys().any(|key| !allowed.contains(key)) {
        return None;
    }
    let identity = session.storage_identity();
    let fingerprint = blake3::hash(&serde_json::to_vec(&(identity, namespace)).ok()?)
        .to_hex()
        .to_string();
    Some(ProviderRootIdentity::new(
        &session.provider,
        &session.backend_type,
        format!("namespace-v2-{fingerprint}"),
        &session.root,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session(backend: &str, entries: &str) -> EphemeralRcloneSession {
        EphemeralRcloneSession {
            operation_id: "temporary".into(),
            provider: backend.into(),
            backend_type: backend.into(),
            remote_name: "profile".into(),
            root: "/repo".into(),
            config_ini: format!("[profile]\ntype = {backend}\n{entries}"),
            expires_at_unix: 0,
            upload_concurrency: None,
        }
    }

    #[test]
    fn account_endpoint_root_and_key_changes_isolate_cache_but_credentials_do_not() {
        let key = MasterKey::generate();
        let original = session("sftp", "host = server.test\nuser = alice\npass = first\n");
        let scope = binding(session_identity(&original), &key).unwrap();
        for changed in [
            session("sftp", "host = other.test\nuser = alice\npass = first\n"),
            session("sftp", "host = server.test\nuser = bob\npass = first\n"),
            session("sftp", "host = server.test\nuser = alice\nport = 2222\n"),
        ] {
            assert_ne!(
                Some(scope.clone()),
                binding(session_identity(&changed), &key)
            );
        }
        let mut changed = original.clone();
        changed.root = "repo".into();
        assert_ne!(
            Some(scope.clone()),
            binding(session_identity(&changed), &key)
        );
        assert_ne!(
            Some(scope.clone()),
            binding(session_identity(&original), &MasterKey::generate())
        );
        changed = original.clone();
        changed.config_ini = changed.config_ini.replace("first", "new-password");
        changed.operation_id = "new-credentials".into();
        changed.expires_at_unix = 123;
        assert_eq!(Some(scope), binding(session_identity(&changed), &key));
    }

    #[test]
    fn unidentified_accounts_and_ambiguous_config_do_not_get_stable_namespace_scope() {
        for session in [
            session("drive", "token = access-token\n"),
            session("s3", "provider = AWS\naccess_key_id = key\n"),
            session("sftp", "host = server\nuser = a\nuser = b\n"),
            session(
                "sftp",
                "host = server\nuser = a\nunknown_namespace_option = value\n",
            ),
            session("webdav", "url = https://host/repo?token=secret\nuser = a\n"),
            session("local", "[another]\ntype = local\n"),
        ] {
            assert!(session_identity(&session).is_none());
        }
    }

    #[test]
    fn private_config_fallback_never_aliases_accounts_or_persists_tokens() {
        let key = MasterKey::generate();
        let original = session("drive", "token = token-for-alice\n");
        let mut changed = original.clone();
        changed.config_ini = changed.config_ini.replace("alice", "bob");
        let private = bound_session_identity(&original, &key);
        assert_ne!(private, bound_session_identity(&changed, &key));
        assert_ne!(
            private,
            bound_session_identity(&original, &MasterKey::generate())
        );
        assert!(!serde_json::to_string(&private)
            .unwrap()
            .contains("token-for-"));
        changed = original.clone();
        changed.operation_id = "fresh-session-id".into();
        changed.expires_at_unix = 456;
        assert_eq!(private, bound_session_identity(&changed, &key));
    }
}
