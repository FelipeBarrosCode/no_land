use crate::{
    AdapterCredential, AdapterError, AdapterInput, ProviderKind, RcloneProviderAdapter,
    RcloneRemoteConfig, Result,
};

pub fn adapter_for(kind: ProviderKind) -> Box<dyn RcloneProviderAdapter> {
    Box::new(Dispatcher(kind))
}

pub struct Dispatcher(pub ProviderKind);

impl RcloneProviderAdapter for Dispatcher {
    fn backend_type(&self) -> &'static str {
        match self.0 {
            ProviderKind::AmazonS3
            | ProviderKind::CloudflareR2
            | ProviderKind::Wasabi
            | ProviderKind::DigitalOceanSpaces
            | ProviderKind::GenericS3 => "s3",
            ProviderKind::BackblazeB2 => "b2",
            ProviderKind::GoogleDrive => "drive",
            ProviderKind::GoogleCloudStorage => "google cloud storage",
            ProviderKind::MicrosoftOneDrive => "onedrive",
            ProviderKind::Dropbox => "dropbox",
            ProviderKind::Box => "box",
            ProviderKind::AzureBlob => "azureblob",
            ProviderKind::Sftp => "sftp",
            ProviderKind::Webdav => "webdav",
            ProviderKind::Local => "local",
        }
    }

    fn create_config(&self, input: &AdapterInput) -> Result<RcloneRemoteConfig> {
        let name = input.remote_name.clone();
        let fields = &input.fields;
        match (self.0, &input.credentials) {
            (
                ProviderKind::BackblazeB2,
                AdapterCredential::BackblazeB2 {
                    key_id,
                    application_key,
                },
            ) => Ok(RcloneRemoteConfig::new(name, "b2")
                .entry("account", key_id)
                .entry("key", application_key)),
            (
                ProviderKind::AmazonS3,
                AdapterCredential::S3 {
                    access_key_id,
                    secret_access_key,
                    session_token,
                },
            ) => {
                let mut cfg = RcloneRemoteConfig::new(name, "s3")
                    .entry("provider", "AWS")
                    .entry("access_key_id", access_key_id)
                    .entry("secret_access_key", secret_access_key)
                    .entry(
                        "region",
                        fields
                            .get("region")
                            .cloned()
                            .unwrap_or_else(|| "us-east-1".into()),
                    );
                if let Some(token) = session_token.as_ref().filter(|v| !v.is_empty()) {
                    cfg = cfg.entry("session_token", token);
                }
                Ok(cfg)
            }
            (
                ProviderKind::CloudflareR2,
                AdapterCredential::S3 {
                    access_key_id,
                    secret_access_key,
                    ..
                },
            ) => {
                let account_id = fields.get("account_id").cloned().ok_or_else(|| {
                    AdapterError::Invalid("Cloudflare R2 account_id is missing.".into())
                })?;
                Ok(RcloneRemoteConfig::new(name, "s3")
                    .entry("provider", "Cloudflare")
                    .entry("access_key_id", access_key_id)
                    .entry("secret_access_key", secret_access_key)
                    .entry("region", "auto")
                    .entry(
                        "endpoint",
                        format!("https://{account_id}.r2.cloudflarestorage.com"),
                    ))
            }
            (
                ProviderKind::Wasabi,
                AdapterCredential::S3 {
                    access_key_id,
                    secret_access_key,
                    ..
                },
            ) => {
                let mut cfg = RcloneRemoteConfig::new(name, "s3")
                    .entry("provider", "Wasabi")
                    .entry("access_key_id", access_key_id)
                    .entry("secret_access_key", secret_access_key)
                    .entry(
                        "region",
                        fields
                            .get("region")
                            .cloned()
                            .unwrap_or_else(|| "us-east-1".into()),
                    );
                if let Some(endpoint) = fields.get("endpoint").filter(|v| !v.trim().is_empty()) {
                    cfg = cfg.entry("endpoint", endpoint);
                }
                Ok(cfg)
            }
            (
                ProviderKind::DigitalOceanSpaces,
                AdapterCredential::S3 {
                    access_key_id,
                    secret_access_key,
                    ..
                },
            ) => {
                let region = fields.get("region").cloned().ok_or_else(|| {
                    AdapterError::Invalid("DigitalOcean Spaces region is missing.".into())
                })?;
                Ok(RcloneRemoteConfig::new(name, "s3")
                    .entry("provider", "DigitalOcean")
                    .entry("access_key_id", access_key_id)
                    .entry("secret_access_key", secret_access_key)
                    .entry("region", &region)
                    .entry("endpoint", format!("{region}.digitaloceanspaces.com")))
            }
            (
                ProviderKind::GenericS3,
                AdapterCredential::S3 {
                    access_key_id,
                    secret_access_key,
                    ..
                },
            ) => {
                let mut cfg = RcloneRemoteConfig::new(name, "s3")
                    .entry("provider", "Other")
                    .entry("access_key_id", access_key_id)
                    .entry("secret_access_key", secret_access_key)
                    .entry(
                        "region",
                        fields
                            .get("region")
                            .cloned()
                            .unwrap_or_else(|| "auto".into()),
                    );
                if let Some(endpoint) = fields.get("endpoint") {
                    cfg = cfg.entry("endpoint", endpoint);
                }
                if let Some(v) = fields.get("force_path_style") {
                    cfg = cfg.entry(
                        "force_path_style",
                        if v == "true" || v == "1" {
                            "true"
                        } else {
                            "false"
                        },
                    );
                }
                Ok(cfg)
            }
            (ProviderKind::GoogleDrive, AdapterCredential::OAuth2 { .. }) => {
                oauth_config(name, "drive", input)
            }
            (ProviderKind::MicrosoftOneDrive, AdapterCredential::OAuth2 { .. }) => {
                oauth_config(name, "onedrive", input)
            }
            (ProviderKind::Dropbox, AdapterCredential::OAuth2 { .. }) => {
                oauth_config(name, "dropbox", input)
            }
            (ProviderKind::Box, AdapterCredential::OAuth2 { .. }) => {
                oauth_config(name, "box", input)
            }
            (ProviderKind::GoogleCloudStorage, AdapterCredential::ServiceAccount { json }) => {
                Ok(RcloneRemoteConfig::new(name, "google cloud storage")
                    .entry("service_account_credentials", json))
            }
            (
                ProviderKind::AzureBlob,
                AdapterCredential::UsernamePassword { username, password },
            ) => Ok(RcloneRemoteConfig::new(name, "azureblob")
                .entry("account", username)
                .entry("key", password)),
            (ProviderKind::Sftp, AdapterCredential::UsernamePassword { username, password }) => {
                let host = fields
                    .get("host")
                    .cloned()
                    .ok_or_else(|| AdapterError::Invalid("SFTP host is missing.".into()))?;
                let mut cfg = RcloneRemoteConfig::new(name, "sftp")
                    .entry("host", host)
                    .entry("user", username)
                    .entry("pass", password);
                if let Some(port) = fields.get("port").filter(|v| !v.trim().is_empty()) {
                    cfg = cfg.entry("port", port);
                }
                Ok(cfg)
            }
            (ProviderKind::Webdav, AdapterCredential::UsernamePassword { username, password }) => {
                let url = fields
                    .get("url")
                    .cloned()
                    .ok_or_else(|| AdapterError::Invalid("WebDAV URL is missing.".into()))?;
                Ok(RcloneRemoteConfig::new(name, "webdav")
                    .entry("url", url)
                    .entry(
                        "vendor",
                        fields
                            .get("vendor")
                            .cloned()
                            .unwrap_or_else(|| "other".into()),
                    )
                    .entry("user", username)
                    .entry("pass", password))
            }
            (ProviderKind::Local, AdapterCredential::LocalPath { path }) => {
                Ok(RcloneRemoteConfig::new(name, "local").entry("nounc", path))
            }
            _ => Err(AdapterError::Invalid(format!(
                "Provider {} is not compatible with the stored credential format.",
                self.0.label()
            ))),
        }
    }
}

fn oauth_config(name: String, backend: &str, input: &AdapterInput) -> Result<RcloneRemoteConfig> {
    let AdapterCredential::OAuth2 {
        access_token,
        refresh_token,
        expires_at,
    } = &input.credentials
    else {
        return Err(AdapterError::Invalid("OAuth credentials required".into()));
    };
    let mut cfg = RcloneRemoteConfig::new(name, backend);
    if let Some(client_id) = input
        .fields
        .get("client_id")
        .filter(|v| !v.trim().is_empty())
    {
        cfg = cfg.entry("client_id", client_id);
    }
    if let Some(client_secret) = input
        .fields
        .get("client_secret")
        .filter(|v| !v.trim().is_empty())
    {
        cfg = cfg.entry("client_secret", client_secret);
    }
    cfg = cfg.entry(
        "token",
        oauth_token_json(access_token, refresh_token.as_deref(), *expires_at),
    );
    Ok(cfg)
}

fn oauth_token_json(access_token: &str, refresh_token: Option<&str>, expires_at: i64) -> String {
    let expiry = chrono::DateTime::<chrono::Utc>::from_timestamp(expires_at, 0)
        .unwrap_or_else(chrono::Utc::now)
        .to_rfc3339();
    let mut value = serde_json::json!({
        "access_token": access_token,
        "expiry": expiry,
    });
    if let Some(refresh) = refresh_token {
        value["refresh_token"] = serde_json::Value::String(refresh.to_string());
    }
    value.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{session_from_input, TokenMode};
    use std::collections::BTreeMap;

    fn b2_input() -> AdapterInput {
        AdapterInput {
            provider: ProviderKind::BackblazeB2,
            remote_name: "noland_b2".into(),
            credentials: AdapterCredential::BackblazeB2 {
                key_id: "kid".into(),
                application_key: "secret".into(),
            },
            fields: BTreeMap::from([("bucket".into(), "noland-backups".into())]),
            bucket: Some("noland-backups".into()),
            prefix: Some("repos".into()),
        }
    }

    #[test]
    fn b2_config_and_bucket_root() {
        let session = session_from_input(&b2_input(), "op-1", TokenMode::Ephemeral).unwrap();
        assert!(session.config_ini.contains("type = b2"));
        assert!(session.config_ini.contains("account = kid"));
        assert_eq!(session.backend_type, "b2");
        assert_eq!(session.root, "noland-backups/repos");
        assert_eq!(session.provider, "backblaze_b2");
    }

    #[test]
    fn r2_uses_cloudflare_endpoint() {
        let input = AdapterInput {
            provider: ProviderKind::CloudflareR2,
            remote_name: "noland_r2".into(),
            credentials: AdapterCredential::S3 {
                access_key_id: "ak".into(),
                secret_access_key: "sk".into(),
                session_token: None,
            },
            fields: BTreeMap::from([
                ("account_id".into(), "acct".into()),
                ("bucket".into(), "b".into()),
            ]),
            bucket: Some("b".into()),
            prefix: None,
        };
        let session = session_from_input(&input, "op", TokenMode::Ephemeral).unwrap();
        assert!(session.config_ini.contains("type = s3"));
        assert!(session.config_ini.contains("provider = Cloudflare"));
        assert!(session
            .config_ini
            .contains("endpoint = https://acct.r2.cloudflarestorage.com"));
        assert_eq!(session.root, "b");
    }

    #[test]
    fn drive_ephemeral_omits_refresh_token() {
        let input = AdapterInput {
            provider: ProviderKind::GoogleDrive,
            remote_name: "noland_drive".into(),
            credentials: AdapterCredential::OAuth2 {
                access_token: "ya29.access".into(),
                refresh_token: Some("1//refresh".into()),
                expires_at: 1_700_000_000,
            },
            fields: BTreeMap::from([("folder".into(), "Noland Shared Storage".into())]),
            bucket: None,
            prefix: None,
        };
        let ephemeral = session_from_input(&input, "op", TokenMode::Ephemeral).unwrap();
        assert!(ephemeral.config_ini.contains("type = drive"));
        assert!(ephemeral.config_ini.contains("ya29.access"));
        assert!(
            !ephemeral.config_ini.contains("1//refresh"),
            "ephemeral session must not contain the refresh token"
        );
        assert_eq!(ephemeral.root, "Noland Shared Storage");

        let operation = session_from_input(&input, "op", TokenMode::Operation).unwrap();
        assert!(operation.config_ini.contains("1//refresh"));
        assert!(TokenMode::Operation.is_ephemeral());

        let durable = session_from_input(&input, "op", TokenMode::Durable).unwrap();
        assert!(durable.config_ini.contains("1//refresh"));
        assert!(!TokenMode::Durable.is_ephemeral());
    }

    #[test]
    fn local_backend_for_tests() {
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
        let session = session_from_input(&input, "op", TokenMode::Ephemeral).unwrap();
        assert!(session.config_ini.contains("type = local"));
        assert_eq!(session.backend_type, "local");
    }
}
