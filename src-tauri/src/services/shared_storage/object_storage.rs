use async_trait::async_trait;
use std::path::{Path, PathBuf};

use crate::errors::AppResult;
pub use crate::models::application_bundle::StorageCredential;
use crate::models::application_bundle::{
    ProviderDefinition, ProviderField, ProviderFieldType, ProviderSelectOption,
    SharedStorageTestResult, StorageProvider,
};

// ─── Remote Object ──────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct RemoteObject {
    pub key: String,
    pub size: u64,
    pub content_hash: Option<String>,
}

// ─── Verification Result ───────────────────────────────────

#[derive(Debug, Clone)]
pub struct VerificationResult {
    pub object_key: String,
    pub expected_hash: Option<String>,
    pub actual_hash: Option<String>,
    pub verified: bool,
}

// ─── Object Storage Trait ──────────────────────────────────

#[async_trait]
pub trait ObjectStorage: Send + Sync {
    async fn put_immutable(&self, local_path: &Path, object_key: &str) -> AppResult<RemoteObject>;

    async fn get(&self, object_key: &str, local_path: &Path) -> AppResult<()>;

    async fn exists(&self, object_key: &str) -> AppResult<bool>;

    async fn list(&self, prefix: &str) -> AppResult<Vec<RemoteObject>>;

    async fn verify(&self, local_path: &Path, object_key: &str) -> AppResult<VerificationResult>;

    async fn delete_for_gc(&self, object_key: &str) -> AppResult<()>;

    async fn test_connection(&self) -> AppResult<SharedStorageTestResult>;
}

// ─── Rclone Config ──────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct RcloneRemoteConfig {
    pub name: String,
    pub backend_type: String,
    pub config_entries: Vec<(String, String)>,
    pub config_path: PathBuf,
}

impl RcloneRemoteConfig {
    pub fn to_ini_string(&self) -> String {
        let mut output = format!("[{}]\n", self.name);
        output.push_str(&format!("type = {}\n", self.backend_type));
        for (key, value) in &self.config_entries {
            output.push_str(&format!("{key} = {value}\n"));
        }
        output
    }
}

// ─── Provider Adapter ──────────────────────────────────────

pub trait RcloneProviderAdapter: Send + Sync {
    fn backend_type(&self) -> &'static str;

    fn provider(&self) -> StorageProvider;

    fn create_config(
        &self,
        credentials: &StorageCredential,
        bucket: Option<&str>,
        prefix: Option<&str>,
    ) -> AppResult<RcloneRemoteConfig>;
}

// ─── Provider Definitions ──────────────────────────────────

impl StorageProvider {
    pub fn definition(&self) -> ProviderDefinition {
        match self {
            Self::BackblazeB2 => ProviderDefinition {
                provider: self.clone(),
                label: self.label().to_string(),
                category: self.category(),
                is_oauth: self.is_oauth(),
                description: "Backblaze B2 object storage with S3-compatible API".to_string(),
                fields: vec![
                    ProviderField {
                        key: "key_id".to_string(),
                        label: "Key ID".to_string(),
                        field_type: ProviderFieldType::Text,
                        required: true,
                        placeholder: Some("Your Backblaze key ID".to_string()),
                        help_text: Some("Application Key ID from Backblaze B2".to_string()),
                    },
                    ProviderField {
                        key: "application_key".to_string(),
                        label: "Application Key".to_string(),
                        field_type: ProviderFieldType::Password,
                        required: true,
                        placeholder: Some("Your Backblaze application key".to_string()),
                        help_text: Some(
                            "Use a restricted application key with read/write access".to_string(),
                        ),
                    },
                    ProviderField {
                        key: "bucket".to_string(),
                        label: "Bucket".to_string(),
                        field_type: ProviderFieldType::Text,
                        required: true,
                        placeholder: Some("noland-backups".to_string()),
                        help_text: None,
                    },
                    ProviderField {
                        key: "prefix".to_string(),
                        label: "Prefix".to_string(),
                        field_type: ProviderFieldType::Text,
                        required: false,
                        placeholder: Some("repositories/".to_string()),
                        help_text: Some("Optional path prefix within the bucket".to_string()),
                    },
                ],
            },
            Self::AmazonS3 => ProviderDefinition {
                provider: self.clone(),
                label: self.label().to_string(),
                category: self.category(),
                is_oauth: self.is_oauth(),
                description: "Amazon S3 object storage".to_string(),
                fields: vec![
                    ProviderField {
                        key: "access_key_id".to_string(),
                        label: "Access Key ID".to_string(),
                        field_type: ProviderFieldType::Text,
                        required: true,
                        placeholder: Some("AKIA...".to_string()),
                        help_text: None,
                    },
                    ProviderField {
                        key: "secret_access_key".to_string(),
                        label: "Secret Access Key".to_string(),
                        field_type: ProviderFieldType::Password,
                        required: true,
                        placeholder: None,
                        help_text: None,
                    },
                    ProviderField {
                        key: "region".to_string(),
                        label: "Region".to_string(),
                        field_type: ProviderFieldType::Select {
                            options: vec![
                                ProviderSelectOption {
                                    value: "us-east-1".to_string(),
                                    label: "US East (N. Virginia)".to_string(),
                                },
                                ProviderSelectOption {
                                    value: "us-west-2".to_string(),
                                    label: "US West (Oregon)".to_string(),
                                },
                                ProviderSelectOption {
                                    value: "eu-west-1".to_string(),
                                    label: "EU (Ireland)".to_string(),
                                },
                                ProviderSelectOption {
                                    value: "ap-southeast-1".to_string(),
                                    label: "Asia Pacific (Singapore)".to_string(),
                                },
                            ],
                        },
                        required: true,
                        placeholder: None,
                        help_text: None,
                    },
                    ProviderField {
                        key: "bucket".to_string(),
                        label: "Bucket".to_string(),
                        field_type: ProviderFieldType::Text,
                        required: true,
                        placeholder: Some("noland-backups".to_string()),
                        help_text: None,
                    },
                    ProviderField {
                        key: "prefix".to_string(),
                        label: "Prefix".to_string(),
                        field_type: ProviderFieldType::Text,
                        required: false,
                        placeholder: Some("repositories/".to_string()),
                        help_text: None,
                    },
                ],
            },
            Self::CloudflareR2 => ProviderDefinition {
                provider: self.clone(),
                label: self.label().to_string(),
                category: self.category(),
                is_oauth: self.is_oauth(),
                description: "Cloudflare R2 S3-compatible object storage (no egress fees)"
                    .to_string(),
                fields: vec![
                    ProviderField {
                        key: "account_id".to_string(),
                        label: "Account ID".to_string(),
                        field_type: ProviderFieldType::Text,
                        required: true,
                        placeholder: Some("Your Cloudflare Account ID".to_string()),
                        help_text: None,
                    },
                    ProviderField {
                        key: "access_key_id".to_string(),
                        label: "R2 Access Key ID".to_string(),
                        field_type: ProviderFieldType::Text,
                        required: true,
                        placeholder: None,
                        help_text: Some("Create R2 API tokens in Cloudflare dashboard".to_string()),
                    },
                    ProviderField {
                        key: "secret_access_key".to_string(),
                        label: "R2 Secret Access Key".to_string(),
                        field_type: ProviderFieldType::Password,
                        required: true,
                        placeholder: None,
                        help_text: None,
                    },
                    ProviderField {
                        key: "bucket".to_string(),
                        label: "Bucket".to_string(),
                        field_type: ProviderFieldType::Text,
                        required: true,
                        placeholder: Some("noland-backups".to_string()),
                        help_text: None,
                    },
                    ProviderField {
                        key: "prefix".to_string(),
                        label: "Prefix".to_string(),
                        field_type: ProviderFieldType::Text,
                        required: false,
                        placeholder: Some("repositories/".to_string()),
                        help_text: None,
                    },
                ],
            },
            Self::GoogleDrive => ProviderDefinition {
                provider: self.clone(),
                label: self.label().to_string(),
                category: self.category(),
                is_oauth: self.is_oauth(),
                description: "Google Drive cloud storage for your application backups".to_string(),
                fields: vec![ProviderField {
                    key: "folder".to_string(),
                    label: "Folder Name".to_string(),
                    field_type: ProviderFieldType::Text,
                    required: false,
                    placeholder: Some("Noland Shared Storage".to_string()),
                    help_text: Some(
                        "Folder in your Google Drive (will be created if it doesn't exist)"
                            .to_string(),
                    ),
                }],
            },
            Self::MicrosoftOneDrive => ProviderDefinition {
                provider: self.clone(),
                label: self.label().to_string(),
                category: self.category(),
                is_oauth: self.is_oauth(),
                description: "Microsoft OneDrive for your application backups".to_string(),
                fields: vec![
                    ProviderField {
                        key: "drive_type".to_string(),
                        label: "Drive Type".to_string(),
                        field_type: ProviderFieldType::Select {
                            options: vec![
                                ProviderSelectOption {
                                    value: "personal".to_string(),
                                    label: "Personal OneDrive".to_string(),
                                },
                                ProviderSelectOption {
                                    value: "business".to_string(),
                                    label: "OneDrive for Business".to_string(),
                                },
                                ProviderSelectOption {
                                    value: "sharepoint".to_string(),
                                    label: "SharePoint Document Library".to_string(),
                                },
                            ],
                        },
                        required: true,
                        placeholder: None,
                        help_text: None,
                    },
                    ProviderField {
                        key: "folder".to_string(),
                        label: "Folder Name".to_string(),
                        field_type: ProviderFieldType::Text,
                        required: false,
                        placeholder: Some("Noland Shared Storage".to_string()),
                        help_text: None,
                    },
                ],
            },
            Self::Dropbox => ProviderDefinition {
                provider: self.clone(),
                label: self.label().to_string(),
                category: self.category(),
                is_oauth: self.is_oauth(),
                description: "Dropbox cloud storage with App Folder access".to_string(),
                fields: vec![],
            },
            Self::Box => ProviderDefinition {
                provider: self.clone(),
                label: self.label().to_string(),
                category: self.category(),
                is_oauth: self.is_oauth(),
                description: "Box cloud storage for your application backups".to_string(),
                fields: vec![],
            },
            Self::Wasabi => ProviderDefinition {
                provider: self.clone(),
                label: self.label().to_string(),
                category: self.category(),
                is_oauth: self.is_oauth(),
                description: "Wasabi Hot Cloud Storage (S3-compatible)".to_string(),
                fields: vec![
                    ProviderField {
                        key: "access_key_id".to_string(),
                        label: "Access Key ID".to_string(),
                        field_type: ProviderFieldType::Text,
                        required: true,
                        placeholder: None,
                        help_text: None,
                    },
                    ProviderField {
                        key: "secret_access_key".to_string(),
                        label: "Secret Access Key".to_string(),
                        field_type: ProviderFieldType::Password,
                        required: true,
                        placeholder: None,
                        help_text: None,
                    },
                    ProviderField {
                        key: "region".to_string(),
                        label: "Region".to_string(),
                        field_type: ProviderFieldType::Select {
                            options: vec![
                                ProviderSelectOption {
                                    value: "us-east-1".to_string(),
                                    label: "US East 1".to_string(),
                                },
                                ProviderSelectOption {
                                    value: "us-east-2".to_string(),
                                    label: "US East 2".to_string(),
                                },
                                ProviderSelectOption {
                                    value: "us-west-1".to_string(),
                                    label: "US West".to_string(),
                                },
                                ProviderSelectOption {
                                    value: "eu-central-1".to_string(),
                                    label: "EU Central".to_string(),
                                },
                                ProviderSelectOption {
                                    value: "ap-northeast-1".to_string(),
                                    label: "Asia Pacific (Tokyo)".to_string(),
                                },
                            ],
                        },
                        required: true,
                        placeholder: None,
                        help_text: None,
                    },
                    ProviderField {
                        key: "bucket".to_string(),
                        label: "Bucket".to_string(),
                        field_type: ProviderFieldType::Text,
                        required: true,
                        placeholder: Some("noland-backups".to_string()),
                        help_text: None,
                    },
                    ProviderField {
                        key: "prefix".to_string(),
                        label: "Prefix".to_string(),
                        field_type: ProviderFieldType::Text,
                        required: false,
                        placeholder: Some("repositories/".to_string()),
                        help_text: None,
                    },
                ],
            },
            Self::DigitalOceanSpaces => ProviderDefinition {
                provider: self.clone(),
                label: self.label().to_string(),
                category: self.category(),
                is_oauth: self.is_oauth(),
                description: "DigitalOcean Spaces object storage (S3-compatible)".to_string(),
                fields: vec![
                    ProviderField {
                        key: "access_key_id".to_string(),
                        label: "Access Key ID".to_string(),
                        field_type: ProviderFieldType::Text,
                        required: true,
                        placeholder: None,
                        help_text: None,
                    },
                    ProviderField {
                        key: "secret_access_key".to_string(),
                        label: "Secret Access Key".to_string(),
                        field_type: ProviderFieldType::Password,
                        required: true,
                        placeholder: None,
                        help_text: None,
                    },
                    ProviderField {
                        key: "region".to_string(),
                        label: "Region".to_string(),
                        field_type: ProviderFieldType::Select {
                            options: vec![
                                ProviderSelectOption {
                                    value: "nyc3".to_string(),
                                    label: "New York 3".to_string(),
                                },
                                ProviderSelectOption {
                                    value: "sfo3".to_string(),
                                    label: "San Francisco 3".to_string(),
                                },
                                ProviderSelectOption {
                                    value: "ams3".to_string(),
                                    label: "Amsterdam 3".to_string(),
                                },
                                ProviderSelectOption {
                                    value: "sgp1".to_string(),
                                    label: "Singapore 1".to_string(),
                                },
                            ],
                        },
                        required: true,
                        placeholder: None,
                        help_text: None,
                    },
                    ProviderField {
                        key: "space_name".to_string(),
                        label: "Space Name".to_string(),
                        field_type: ProviderFieldType::Text,
                        required: true,
                        placeholder: Some("noland-backups".to_string()),
                        help_text: None,
                    },
                    ProviderField {
                        key: "prefix".to_string(),
                        label: "Prefix".to_string(),
                        field_type: ProviderFieldType::Text,
                        required: false,
                        placeholder: Some("repositories/".to_string()),
                        help_text: None,
                    },
                ],
            },
            Self::GenericS3 => ProviderDefinition {
                provider: self.clone(),
                label: self.label().to_string(),
                category: self.category(),
                is_oauth: self.is_oauth(),
                description: "Any S3-compatible object storage provider".to_string(),
                fields: vec![
                    ProviderField {
                        key: "endpoint".to_string(),
                        label: "Endpoint URL".to_string(),
                        field_type: ProviderFieldType::Text,
                        required: true,
                        placeholder: Some("https://s3.example.com".to_string()),
                        help_text: None,
                    },
                    ProviderField {
                        key: "access_key_id".to_string(),
                        label: "Access Key ID".to_string(),
                        field_type: ProviderFieldType::Text,
                        required: true,
                        placeholder: None,
                        help_text: None,
                    },
                    ProviderField {
                        key: "secret_access_key".to_string(),
                        label: "Secret Access Key".to_string(),
                        field_type: ProviderFieldType::Password,
                        required: true,
                        placeholder: None,
                        help_text: None,
                    },
                    ProviderField {
                        key: "region".to_string(),
                        label: "Region".to_string(),
                        field_type: ProviderFieldType::Text,
                        required: false,
                        placeholder: Some("auto".to_string()),
                        help_text: None,
                    },
                    ProviderField {
                        key: "bucket".to_string(),
                        label: "Bucket".to_string(),
                        field_type: ProviderFieldType::Text,
                        required: true,
                        placeholder: Some("noland-backups".to_string()),
                        help_text: None,
                    },
                    ProviderField {
                        key: "prefix".to_string(),
                        label: "Prefix".to_string(),
                        field_type: ProviderFieldType::Text,
                        required: false,
                        placeholder: Some("repositories/".to_string()),
                        help_text: None,
                    },
                    ProviderField {
                        key: "force_path_style".to_string(),
                        label: "Force path-style addressing".to_string(),
                        field_type: ProviderFieldType::Toggle,
                        required: false,
                        placeholder: None,
                        help_text: Some("Enable for MinIO or self-hosted S3".to_string()),
                    },
                ],
            },
            Self::GoogleCloudStorage => ProviderDefinition {
                provider: self.clone(),
                label: self.label().to_string(),
                category: self.category(),
                is_oauth: self.is_oauth(),
                description: "Google Cloud Storage for application backups".to_string(),
                fields: vec![
                    ProviderField {
                        key: "service_account_json".to_string(),
                        label: "Service Account JSON".to_string(),
                        field_type: ProviderFieldType::Password,
                        required: true,
                        placeholder: Some("Paste service account key JSON".to_string()),
                        help_text: Some(
                            "Create a service account with Storage Object Admin role".to_string(),
                        ),
                    },
                    ProviderField {
                        key: "bucket".to_string(),
                        label: "Bucket".to_string(),
                        field_type: ProviderFieldType::Text,
                        required: true,
                        placeholder: Some("noland-backups".to_string()),
                        help_text: None,
                    },
                    ProviderField {
                        key: "prefix".to_string(),
                        label: "Prefix".to_string(),
                        field_type: ProviderFieldType::Text,
                        required: false,
                        placeholder: Some("repositories/".to_string()),
                        help_text: None,
                    },
                ],
            },
            Self::AzureBlob => ProviderDefinition {
                provider: self.clone(),
                label: self.label().to_string(),
                category: self.category(),
                is_oauth: self.is_oauth(),
                description: "Azure Blob Storage for application backups".to_string(),
                fields: vec![
                    ProviderField {
                        key: "account_name".to_string(),
                        label: "Storage Account Name".to_string(),
                        field_type: ProviderFieldType::Text,
                        required: true,
                        placeholder: Some("mystorageaccount".to_string()),
                        help_text: None,
                    },
                    ProviderField {
                        key: "account_key".to_string(),
                        label: "Account Key".to_string(),
                        field_type: ProviderFieldType::Password,
                        required: true,
                        placeholder: None,
                        help_text: Some("Storage account access key".to_string()),
                    },
                    ProviderField {
                        key: "container".to_string(),
                        label: "Container".to_string(),
                        field_type: ProviderFieldType::Text,
                        required: true,
                        placeholder: Some("noland-backups".to_string()),
                        help_text: None,
                    },
                    ProviderField {
                        key: "prefix".to_string(),
                        label: "Prefix".to_string(),
                        field_type: ProviderFieldType::Text,
                        required: false,
                        placeholder: Some("repositories/".to_string()),
                        help_text: None,
                    },
                ],
            },
            Self::Sftp => ProviderDefinition {
                provider: self.clone(),
                label: self.label().to_string(),
                category: self.category(),
                is_oauth: self.is_oauth(),
                description: "SFTP server for self-hosted application backups".to_string(),
                fields: vec![
                    ProviderField {
                        key: "host".to_string(),
                        label: "Host".to_string(),
                        field_type: ProviderFieldType::Text,
                        required: true,
                        placeholder: Some("sftp.example.com".to_string()),
                        help_text: None,
                    },
                    ProviderField {
                        key: "port".to_string(),
                        label: "Port".to_string(),
                        field_type: ProviderFieldType::Number,
                        required: false,
                        placeholder: Some("22".to_string()),
                        help_text: None,
                    },
                    ProviderField {
                        key: "username".to_string(),
                        label: "Username".to_string(),
                        field_type: ProviderFieldType::Text,
                        required: true,
                        placeholder: None,
                        help_text: None,
                    },
                    ProviderField {
                        key: "password".to_string(),
                        label: "Password (or leave empty for key auth)".to_string(),
                        field_type: ProviderFieldType::Password,
                        required: false,
                        placeholder: None,
                        help_text: Some("Leave empty to use SSH key authentication".to_string()),
                    },
                    ProviderField {
                        key: "remote_path".to_string(),
                        label: "Remote Path".to_string(),
                        field_type: ProviderFieldType::Text,
                        required: true,
                        placeholder: Some("/home/user/backups".to_string()),
                        help_text: None,
                    },
                    ProviderField {
                        key: "host_key".to_string(),
                        label: "Host Key Fingerprint".to_string(),
                        field_type: ProviderFieldType::Text,
                        required: false,
                        placeholder: Some("SHA256:...".to_string()),
                        help_text: Some("Pin the server host key for security".to_string()),
                    },
                ],
            },
            Self::Webdav => ProviderDefinition {
                provider: self.clone(),
                label: self.label().to_string(),
                category: self.category(),
                is_oauth: self.is_oauth(),
                description: "WebDAV server (Nextcloud, ownCloud, or generic)".to_string(),
                fields: vec![
                    ProviderField {
                        key: "url".to_string(),
                        label: "WebDAV URL".to_string(),
                        field_type: ProviderFieldType::Text,
                        required: true,
                        placeholder: Some(
                            "https://cloud.example.com/remote.php/dav/files/USER/".to_string(),
                        ),
                        help_text: None,
                    },
                    ProviderField {
                        key: "vendor".to_string(),
                        label: "Vendor".to_string(),
                        field_type: ProviderFieldType::Select {
                            options: vec![
                                ProviderSelectOption {
                                    value: "nextcloud".to_string(),
                                    label: "Nextcloud".to_string(),
                                },
                                ProviderSelectOption {
                                    value: "owncloud".to_string(),
                                    label: "ownCloud".to_string(),
                                },
                                ProviderSelectOption {
                                    value: "other".to_string(),
                                    label: "Other WebDAV".to_string(),
                                },
                            ],
                        },
                        required: true,
                        placeholder: None,
                        help_text: None,
                    },
                    ProviderField {
                        key: "username".to_string(),
                        label: "Username".to_string(),
                        field_type: ProviderFieldType::Text,
                        required: true,
                        placeholder: None,
                        help_text: None,
                    },
                    ProviderField {
                        key: "password".to_string(),
                        label: "App Password".to_string(),
                        field_type: ProviderFieldType::Password,
                        required: true,
                        placeholder: None,
                        help_text: Some(
                            "Use an app-specific password, not your main account password"
                                .to_string(),
                        ),
                    },
                ],
            },
        }
    }
}

// ─── List All Providers ────────────────────────────────────

pub fn list_all_providers() -> Vec<ProviderDefinition> {
    vec![
        StorageProvider::BackblazeB2.definition(),
        StorageProvider::AmazonS3.definition(),
        StorageProvider::CloudflareR2.definition(),
        StorageProvider::Wasabi.definition(),
        StorageProvider::DigitalOceanSpaces.definition(),
        StorageProvider::GenericS3.definition(),
        StorageProvider::GoogleDrive.definition(),
        StorageProvider::MicrosoftOneDrive.definition(),
        StorageProvider::Dropbox.definition(),
        StorageProvider::Box.definition(),
        StorageProvider::AzureBlob.definition(),
        StorageProvider::GoogleCloudStorage.definition(),
        StorageProvider::Sftp.definition(),
        StorageProvider::Webdav.definition(),
    ]
}
