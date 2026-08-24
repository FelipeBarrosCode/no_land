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
    session_from_input(&input, operation_id, TokenMode::Ephemeral)
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
        StorageCredential::SshKey { username, .. } => {
            return Err(AppError::InvalidInput(format!(
                "SSH key credentials for user {username} are not mapped to rclone yet."
            )));
        }
    })
}
