//! Provider-agnostic rclone adapter shared by the desktop client and the
//! remote state agent.
//!
//! Desktop code turns a connected Shared Storage profile into an
//! [`EphemeralRcloneSession`]. The agent only runs generic rclone operations
//! (`copyto` / `lsf` / `mkdir`) against that session.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

mod config;
mod providers;
mod session;
mod tuning;

pub use config::{RcloneRemoteConfig, RcloneRoot};
pub use providers::{adapter_for, Dispatcher};
pub use session::{session_from_input, TokenMode};
pub use tuning::{classify_remote_error, ProviderRootIdentity, RemoteErrorClass, TransferTuning};

#[derive(Debug, thiserror::Error)]
pub enum AdapterError {
    #[error("{0}")]
    Invalid(String),
}

pub type Result<T> = std::result::Result<T, AdapterError>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderKind {
    AmazonS3,
    BackblazeB2,
    CloudflareR2,
    Wasabi,
    DigitalOceanSpaces,
    GenericS3,
    GoogleDrive,
    GoogleCloudStorage,
    MicrosoftOneDrive,
    Dropbox,
    Box,
    AzureBlob,
    Sftp,
    Webdav,
    /// Local filesystem remote. Used in tests and never by production profiles.
    Local,
}

impl ProviderKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::AmazonS3 => "amazon_s3",
            Self::BackblazeB2 => "backblaze_b2",
            Self::CloudflareR2 => "cloudflare_r2",
            Self::Wasabi => "wasabi",
            Self::DigitalOceanSpaces => "digitalocean_spaces",
            Self::GenericS3 => "generic_s3",
            Self::GoogleDrive => "google_drive",
            Self::GoogleCloudStorage => "google_cloud_storage",
            Self::MicrosoftOneDrive => "microsoft_onedrive",
            Self::Dropbox => "dropbox",
            Self::Box => "box",
            Self::AzureBlob => "azure_blob",
            Self::Sftp => "sftp",
            Self::Webdav => "webdav",
            Self::Local => "local",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::AmazonS3 => "Amazon S3",
            Self::BackblazeB2 => "Backblaze B2",
            Self::CloudflareR2 => "Cloudflare R2",
            Self::Wasabi => "Wasabi",
            Self::DigitalOceanSpaces => "DigitalOcean Spaces",
            Self::GenericS3 => "Generic S3",
            Self::GoogleDrive => "Google Drive",
            Self::GoogleCloudStorage => "Google Cloud Storage",
            Self::MicrosoftOneDrive => "Microsoft OneDrive",
            Self::Dropbox => "Dropbox",
            Self::Box => "Box",
            Self::AzureBlob => "Azure Blob Storage",
            Self::Sftp => "SFTP",
            Self::Webdav => "WebDAV",
            Self::Local => "Local",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        Some(match raw {
            "amazon_s3" | "AmazonS3" => Self::AmazonS3,
            "backblaze_b2" | "BackblazeB2" => Self::BackblazeB2,
            "cloudflare_r2" | "CloudflareR2" => Self::CloudflareR2,
            "wasabi" | "Wasabi" => Self::Wasabi,
            "digitalocean_spaces" | "DigitalOceanSpaces" => Self::DigitalOceanSpaces,
            "generic_s3" | "GenericS3" => Self::GenericS3,
            "google_drive" | "GoogleDrive" => Self::GoogleDrive,
            "google_cloud_storage" | "GoogleCloudStorage" => Self::GoogleCloudStorage,
            "microsoft_onedrive" | "MicrosoftOneDrive" => Self::MicrosoftOneDrive,
            "dropbox" | "Dropbox" => Self::Dropbox,
            "box" | "Box" => Self::Box,
            "azure_blob" | "AzureBlob" => Self::AzureBlob,
            "sftp" | "Sftp" => Self::Sftp,
            "webdav" | "Webdav" => Self::Webdav,
            "local" | "Local" => Self::Local,
            _ => return None,
        })
    }

    pub fn is_oauth(self) -> bool {
        matches!(
            self,
            Self::GoogleDrive | Self::MicrosoftOneDrive | Self::Dropbox | Self::Box
        )
    }

    pub fn uses_bucket_root(self) -> bool {
        matches!(
            self,
            Self::BackblazeB2
                | Self::AmazonS3
                | Self::CloudflareR2
                | Self::Wasabi
                | Self::DigitalOceanSpaces
                | Self::GenericS3
                | Self::GoogleCloudStorage
                | Self::AzureBlob
        )
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", content = "value", rename_all = "snake_case")]
pub enum AdapterCredential {
    S3 {
        access_key_id: String,
        secret_access_key: String,
        session_token: Option<String>,
    },
    BackblazeB2 {
        key_id: String,
        application_key: String,
    },
    OAuth2 {
        access_token: String,
        refresh_token: Option<String>,
        expires_at: i64,
    },
    UsernamePassword {
        username: String,
        password: String,
    },
    ServiceAccount {
        json: String,
    },
    /// Path for `type = local` remotes.
    LocalPath {
        path: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AdapterInput {
    pub provider: ProviderKind,
    pub remote_name: String,
    pub credentials: AdapterCredential,
    pub fields: BTreeMap<String, String>,
    pub bucket: Option<String>,
    pub prefix: Option<String>,
}

pub trait RcloneProviderAdapter: Send + Sync {
    fn backend_type(&self) -> &'static str;
    fn create_config(&self, input: &AdapterInput) -> Result<RcloneRemoteConfig>;
    fn storage_root(&self, input: &AdapterInput) -> Result<RcloneRoot> {
        default_storage_root(input)
    }
}

/// Ready-to-hand-off session. Contains a complete rclone.conf and no
/// long-lived refresh token when built with [`TokenMode::Ephemeral`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EphemeralRcloneSession {
    pub operation_id: String,
    pub provider: String,
    pub backend_type: String,
    pub remote_name: String,
    pub root: String,
    pub config_ini: String,
    pub expires_at_unix: i64,
}

impl EphemeralRcloneSession {
    pub fn storage_identity(&self) -> ProviderRootIdentity {
        ProviderRootIdentity::new(
            &self.provider,
            &self.backend_type,
            &self.remote_name,
            &self.root,
        )
    }

    pub fn root_cache_key(&self) -> String {
        self.storage_identity().cache_key()
    }
}

impl Drop for EphemeralRcloneSession {
    fn drop(&mut self) {
        self.config_ini.clear();
    }
}

pub fn default_storage_root(input: &AdapterInput) -> Result<RcloneRoot> {
    if input.provider.uses_bucket_root() {
        let bucket = input
            .bucket
            .clone()
            .or_else(|| input.fields.get("bucket").cloned())
            .or_else(|| input.fields.get("space_name").cloned())
            .or_else(|| input.fields.get("container").cloned())
            .unwrap_or_else(|| "noland".into());
        let prefix = input.prefix.clone().unwrap_or_default();
        let root = if prefix.trim().is_empty() {
            bucket
        } else {
            format!("{}/{}", bucket, prefix.trim_matches('/'))
        };
        return Ok(RcloneRoot {
            remote_name: input.remote_name.clone(),
            root,
        });
    }
    let folder = input
        .prefix
        .clone()
        .or_else(|| input.fields.get("folder").cloned())
        .or_else(|| input.fields.get("remote_path").cloned())
        .unwrap_or_else(|| "Noland Shared Storage".into());
    Ok(RcloneRoot {
        remote_name: input.remote_name.clone(),
        root: folder.trim_matches('/').to_string(),
    })
}
