//! Bridge from desktop Shared Storage profiles to the shared rclone adapter.

use std::collections::{BTreeMap, HashMap};

use noland_rclone_adapter::{
    session_from_input, AdapterCredential, AdapterInput, EphemeralRcloneSession, ProviderKind,
    TokenMode,
};

use crate::errors::{AppError, AppResult};
use crate::models::application_bundle::StorageProvider;
use crate::services::shared_storage::object_storage::StorageCredential;

pub fn mint_ephemeral_session(
    provider: &StorageProvider,
    credentials: &StorageCredential,
    fields: &HashMap<String, String>,
    bucket: Option<&str>,
    prefix: Option<&str>,
    remote_name: &str,
    operation_id: &str,
) -> AppResult<EphemeralRcloneSession> {
    let input = to_input(provider, credentials, fields, bucket, prefix, remote_name)?;
    session_from_input(&input, operation_id, TokenMode::Operation)
        .map_err(|e| AppError::InvalidInput(e.to_string()))
}

pub fn mint_config_ini(
    provider: &StorageProvider,
    credentials: &StorageCredential,
    fields: &HashMap<String, String>,
    bucket: Option<&str>,
    prefix: Option<&str>,
    remote_name: &str,
    mode: TokenMode,
) -> AppResult<String> {
    let input = to_input(provider, credentials, fields, bucket, prefix, remote_name)?;
    let session = session_from_input(&input, "desktop", mode)
        .map_err(|e| AppError::InvalidInput(e.to_string()))?;
    Ok(session.config_ini.clone())
}

pub fn storage_source(
    provider: &StorageProvider,
    credentials: &StorageCredential,
    fields: &HashMap<String, String>,
    bucket: Option<&str>,
    prefix: Option<&str>,
    remote_name: &str,
) -> AppResult<String> {
    let input = to_input(provider, credentials, fields, bucket, prefix, remote_name)?;
    let session = session_from_input(&input, "source", TokenMode::Ephemeral)
        .map_err(|e| AppError::InvalidInput(e.to_string()))?;
    if session.root.trim().is_empty() {
        Ok(format!("{}:", session.remote_name))
    } else {
        Ok(format!("{}:{}", session.remote_name, session.root))
    }
}

fn to_input(
    provider: &StorageProvider,
    credentials: &StorageCredential,
    fields: &HashMap<String, String>,
    bucket: Option<&str>,
    prefix: Option<&str>,
    remote_name: &str,
) -> AppResult<AdapterInput> {
    Ok(AdapterInput {
        provider: map_provider(provider)?,
        remote_name: remote_name.to_string(),
        credentials: map_credential(credentials)?,
        fields: fields
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect::<BTreeMap<_, _>>(),
        bucket: bucket.map(str::to_string),
        prefix: prefix.map(str::to_string),
    })
}

fn map_provider(provider: &StorageProvider) -> AppResult<ProviderKind> {
    Ok(match provider {
        StorageProvider::AmazonS3 => ProviderKind::AmazonS3,
        StorageProvider::BackblazeB2 => ProviderKind::BackblazeB2,
        StorageProvider::CloudflareR2 => ProviderKind::CloudflareR2,
        StorageProvider::Wasabi => ProviderKind::Wasabi,
        StorageProvider::DigitalOceanSpaces => ProviderKind::DigitalOceanSpaces,
        StorageProvider::GenericS3 => ProviderKind::GenericS3,
        StorageProvider::GoogleDrive => ProviderKind::GoogleDrive,
        StorageProvider::GoogleCloudStorage => ProviderKind::GoogleCloudStorage,
        StorageProvider::MicrosoftOneDrive => ProviderKind::MicrosoftOneDrive,
        StorageProvider::Dropbox => ProviderKind::Dropbox,
        StorageProvider::Box => ProviderKind::Box,
        StorageProvider::AzureBlob => ProviderKind::AzureBlob,
        StorageProvider::Sftp => ProviderKind::Sftp,
        StorageProvider::Webdav => ProviderKind::Webdav,
    })
}

fn map_credential(credential: &StorageCredential) -> AppResult<AdapterCredential> {
    Ok(match credential {
        StorageCredential::S3 {
            access_key_id,
            secret_access_key,
            session_token,
        } => AdapterCredential::S3 {
            access_key_id: access_key_id.clone(),
            secret_access_key: secret_access_key.clone(),
            session_token: session_token.clone(),
        },
        StorageCredential::BackblazeB2 {
            key_id,
            application_key,
        } => AdapterCredential::BackblazeB2 {
            key_id: key_id.clone(),
            application_key: application_key.clone(),
        },
        StorageCredential::OAuth2 {
            access_token,
            refresh_token,
            expires_at,
        } => AdapterCredential::OAuth2 {
            access_token: access_token.clone(),
            refresh_token: refresh_token.clone(),
            expires_at: *expires_at,
        },
        StorageCredential::UsernamePassword { username, password } => {
            AdapterCredential::UsernamePassword {
                username: username.clone(),
                password: password.clone(),
            }
        }
        StorageCredential::ServiceAccount { json } => {
            AdapterCredential::ServiceAccount { json: json.clone() }
        }
        StorageCredential::SshKey {
            username,
            private_key,
            passphrase,
        } => AdapterCredential::SshKey {
            username: username.clone(),
            private_key: private_key.clone(),
            passphrase: passphrase.clone(),
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ssh_key_profiles_mint_sessions_and_configs() {
        for passphrase in [None, Some(""), Some("test-passphrase")] {
            let credentials = StorageCredential::SshKey {
                username: "test-user".into(),
                private_key: "test key line one\r\ntest key line two\n".into(),
                passphrase: passphrase.map(str::to_string),
            };
            let fields = HashMap::from([
                ("host".into(), "sftp.example.test".into()),
                ("remote_path".into(), "/srv/backups/".into()),
            ]);
            let session = mint_ephemeral_session(
                &StorageProvider::Sftp,
                &credentials,
                &fields,
                None,
                None,
                "noland_sftp",
                "ssh-key-test",
            )
            .unwrap();
            assert_eq!(session.provider, "sftp");
            assert_eq!(session.root, "/srv/backups");
            assert_eq!(session.operation_id, "ssh-key-test");
            let config = mint_config_ini(
                &StorageProvider::Sftp,
                &credentials,
                &fields,
                None,
                None,
                "noland_sftp",
                TokenMode::Durable,
            )
            .unwrap();
            for config in [&session.config_ini, &config] {
                let entries: BTreeMap<_, _> = config
                    .lines()
                    .filter_map(|line| line.split_once(" = "))
                    .collect();
                assert_eq!(entries["type"], "sftp");
                assert_eq!(entries["host"], "sftp.example.test");
                assert_eq!(entries["user"], "test-user");
                assert_eq!(
                    entries["key_pem"],
                    "test key line one\\ntest key line two\\n"
                );
                assert!(!entries.contains_key("pass"));
                if let Some(passphrase) = passphrase.filter(|value| !value.is_empty()) {
                    assert_ne!(entries["key_file_pass"], passphrase);
                } else {
                    assert!(!entries.contains_key("key_file_pass"));
                }
            }
            assert_eq!(
                storage_source(
                    &StorageProvider::Sftp,
                    &credentials,
                    &fields,
                    None,
                    Some("/"),
                    "noland_sftp",
                )
                .unwrap(),
                "noland_sftp:/"
            );
        }
    }
}
