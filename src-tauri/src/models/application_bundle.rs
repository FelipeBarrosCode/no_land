use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SharedStorageProfile {
    pub id: String,
    pub display_name: String,
    pub provider: StorageProvider,
    pub provider_label: String,
    pub bucket: Option<String>,
    pub prefix: Option<String>,
    pub credential_vault_reference: String,
    pub repository_id: String,
    pub status: SharedStorageStatus,
    pub last_verified_at: Option<i64>,
    pub protected_bundles_count: u32,
    pub total_stored_bytes: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum SharedStorageStatus {
    NotConfigured,
    AuthenticationRequired,
    Testing,
    Connected,
    Expired,
    Invalid,
    Unreachable,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", content = "value", rename_all = "snake_case")]
pub enum StorageCredential {
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
    SshKey {
        username: String,
        private_key: String,
        passphrase: Option<String>,
    },
    ServiceAccount {
        json: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum StorageProvider {
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
}

impl StorageProvider {
    pub fn label(&self) -> &'static str {
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
        }
    }

    pub fn category(&self) -> StorageProviderCategory {
        match self {
            Self::AmazonS3
            | Self::BackblazeB2
            | Self::CloudflareR2
            | Self::Wasabi
            | Self::DigitalOceanSpaces
            | Self::GenericS3 => StorageProviderCategory::ObjectStorage,
            Self::GoogleDrive | Self::MicrosoftOneDrive | Self::Dropbox | Self::Box => {
                StorageProviderCategory::CloudDrives
            }
            Self::AzureBlob | Self::GoogleCloudStorage | Self::Sftp | Self::Webdav => {
                StorageProviderCategory::EnterpriseAndSelfHosted
            }
        }
    }

    pub fn is_oauth(&self) -> bool {
        matches!(
            self,
            Self::GoogleDrive | Self::MicrosoftOneDrive | Self::Dropbox | Self::Box
        )
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum StorageProviderCategory {
    ObjectStorage,
    CloudDrives,
    EnterpriseAndSelfHosted,
}

// ─── Shared Storage Test ───────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SharedStorageTestResult {
    pub authenticated: bool,
    pub can_list: bool,
    pub can_write: bool,
    pub can_read: bool,
    pub can_delete_test_object: bool,
    pub repository_accessible: bool,
    pub latency_ms: Option<u64>,
    pub error: Option<String>,
}

// ─── Provider Definition (for UI) ──────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderDefinition {
    pub provider: StorageProvider,
    pub label: String,
    pub category: StorageProviderCategory,
    pub is_oauth: bool,
    pub description: String,
    pub fields: Vec<ProviderField>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderField {
    pub key: String,
    pub label: String,
    pub field_type: ProviderFieldType,
    pub required: bool,
    pub placeholder: Option<String>,
    pub help_text: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ProviderFieldType {
    Text,
    Password,
    Number,
    Select { options: Vec<ProviderSelectOption> },
    Toggle,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ProviderSelectOption {
    pub value: String,
    pub label: String,
}


// ─── Profile Reference (persisted in app state) ────────────

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProfileReference {
    pub id: String,
    pub display_name: String,
    pub provider_label: String,
    #[serde(default)]
    pub provider: Option<StorageProvider>,
    #[serde(default)]
    pub bucket: Option<String>,
    #[serde(default)]
    pub prefix: Option<String>,
    #[serde(default)]
    pub active: bool,
}
