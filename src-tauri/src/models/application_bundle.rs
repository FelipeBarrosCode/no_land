use serde::{Deserialize, Serialize};
use std::path::PathBuf;

// ─── Bundle Identity ────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct ApplicationBundleId(pub String);

impl ApplicationBundleId {
    pub fn new_steam(app_id: &str) -> Self {
        Self(format!("steam:{app_id}"))
    }

    pub fn new_epic(catalog_id: &str) -> Self {
        Self(format!("epic:{catalog_id}"))
    }

    pub fn new_gog(product_id: &str) -> Self {
        Self(format!("gog:{product_id}"))
    }

    pub fn new_battlenet(product_code: &str) -> Self {
        Self(format!("battlenet:{product_code}"))
    }

    pub fn new_bottles(bottle_id: &str, app_id: &str) -> Self {
        Self(format!("bottles:{bottle_id}:{app_id}"))
    }

    pub fn new_native(desktop_entry_id: &str) -> Self {
        Self(format!("native:{desktop_entry_id}"))
    }

    pub fn new_custom(uuid: &str) -> Self {
        Self(format!("custom:{uuid}"))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for ApplicationBundleId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

// ─── Application Kinds ──────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum ApplicationKind {
    SteamProton,
    SteamLinux,
    SteamWindows,
    EpicProton,
    GogProton,
    HeroicProton,
    Bottles,
    WinePrefix,
    NativeLinux,
    NativeWindows,
    AppImage,
    Flatpak,
    Custom,
}

// ─── Store Metadata ─────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum ApplicationStore {
    Steam,
    Epic,
    Gog,
    BattleNet,
    Itch,
    Ubisoft,
    Ea,
    Amazon,
    Heroic,
    Lutris,
    Bottles,
    Manual,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StoreMetadata {
    pub store: ApplicationStore,
    pub application_id: String,
    pub build_id: Option<String>,
    pub branch: Option<String>,
    pub installed_version: Option<String>,
}

// ─── Bundle Component Kinds ─────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum BundleComponentKind {
    ApplicationContent,
    SaveFiles,
    UserConfiguration,
    UserProfile,
    Modifications,
    Mods,
    Plugins,
    RuntimePrefix,
    WinePrefix,
    LauncherMetadata,
    InstallationRecipe,
    UserDocuments,
    OptionalCache,
    SystemOverlay,
    WorkshopContent,
    Screenshots,
    CrashLogs,
    ShaderCache,
    Custom(String),
}

// ─── Logical Paths ──────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum LogicalRoot {
    UserHome,
    Documents,
    Downloads,
    Pictures,
    Videos,
    Music,
    Desktop,
    ApplicationData,
    LocalApplicationData,
    GameLibrary,
    SteamRoot,
    SteamLibrary,
    #[serde(rename = "EpicLibrary")]
    EpicLibrary,
    GogLibrary,
    BottlesRoot,
    WinePrefixRoot,
    ApplicationInstallRoot,
    SavedGames,
    Custom(String),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LogicalPath {
    pub root: LogicalRoot,
    pub relative_path: PathBuf,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub actual_path: Option<PathBuf>,
}

// ─── Bundle Components ──────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct BundleComponentId(pub String);

impl std::fmt::Display for BundleComponentId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BundleComponent {
    pub id: BundleComponentId,
    pub bundle_id: ApplicationBundleId,
    pub kind: BundleComponentKind,
    pub display_name: String,
    pub required: bool,
    pub selected_by_default: bool,
    pub restore_order: u32,
    pub logical_size: u64,
    pub file_count: u64,
    pub paths: Vec<ComponentPath>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ComponentPath {
    pub component_id: BundleComponentId,
    pub logical_root: LogicalRoot,
    pub relative_path: PathBuf,
    pub actual_path: Option<PathBuf>,
    pub discovery_source: String,
    pub confidence: f32,
    pub include_policy: String,
    pub first_seen_at: i64,
    pub last_seen_at: i64,
}

// ─── Bundle Relationships ───────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum BundleRelationshipKind {
    RequiredDependency,
    OptionalDependency,
    PluginHost,
    ModManager,
    CompanionApplication,
    SharedData,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BundleRelationship {
    pub bundle_id: ApplicationBundleId,
    pub relationship_kind: BundleRelationshipKind,
    pub display_name: String,
}

// ─── Bundle Availability ────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum BundleAvailability {
    Installed,
    PartiallyInstalled,
    Restoring,
    CloudOnly,
    BackupPending,
    BackupFailed,
    CorruptedLocalState,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum BackupStatus {
    NoBackup,
    Uploading,
    Committed,
    VerificationFailed,
}

// ─── Main Application Bundle ────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApplicationBundle {
    pub id: ApplicationBundleId,
    pub display_name: String,
    pub kind: ApplicationKind,
    pub store: Option<ApplicationStore>,
    pub store_application_id: Option<String>,
    pub store_metadata: Option<StoreMetadata>,
    pub icon_object_key: Option<String>,
    pub components: Vec<BundleComponent>,
    pub relationships: Vec<BundleRelationship>,
    pub availability: BundleAvailability,
    pub backup_status: BackupStatus,
    pub latest_snapshot_id: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
}

// ─── Bundle Manifest ────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApplicationBundleManifest {
    pub format_version: u32,
    pub bundle_id: ApplicationBundleId,
    pub display_name: String,
    pub application_kind: ApplicationKind,
    pub store_metadata: Option<StoreMetadata>,
    pub icon_object_key: Option<String>,
    pub components: Vec<BundleComponentEntry>,
    pub relationships: Vec<BundleRelationship>,
    pub latest_snapshot_id: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BundleComponentEntry {
    pub id: String,
    pub kind: BundleComponentKind,
    pub display_name: String,
    pub required: bool,
    pub selected_by_default: bool,
    pub restore_order: u32,
}

// ─── Restore Plan ──────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RestorePlan {
    pub complete_components: Vec<String>,
    pub personal_state_components: Vec<String>,
    pub optional_components: Vec<String>,
}

// ─── Snapshots ──────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct SnapshotId(pub String);

impl std::fmt::Display for SnapshotId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BundleSnapshot {
    pub id: SnapshotId,
    pub bundle_id: ApplicationBundleId,
    pub parent_snapshot_id: Option<String>,
    pub state: String,
    pub reason: String,
    pub total_logical_size: u64,
    pub new_physical_size: u64,
    pub created_at: i64,
    pub committed_at: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SnapshotComponent {
    pub snapshot_id: String,
    pub component_id: String,
    pub manifest_object_key: String,
    pub logical_size: u64,
    pub file_count: u64,
}

// ─── Application Catalog ────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApplicationCatalog {
    pub repository_id: String,
    pub generation: u64,
    pub generated_at: i64,
    pub bundles: Vec<ApplicationCatalogEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApplicationCatalogEntry {
    pub bundle_id: ApplicationBundleId,
    pub display_name: String,
    pub kind: ApplicationKind,
    pub icon_object_key: Option<String>,
    pub latest_snapshot_id: Option<String>,
    pub total_logical_size: u64,
    pub required_restore_size: u64,
    pub last_backed_up_at: i64,
    pub component_summary: ComponentSummary,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ComponentSummary {
    pub has_application_content: bool,
    pub has_saves: bool,
    pub has_mods: bool,
    pub has_runtime: bool,
    pub has_configuration: bool,
    pub has_plugins: bool,
    pub component_count: u32,
}

// ─── Restore Models ─────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum RestoreMode {
    Complete,
    PersonalState,
    Custom,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RestorePlanRequest {
    pub bundle_id: ApplicationBundleId,
    pub snapshot_id: Option<String>,
    pub restore_mode: RestoreMode,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub selected_components: Option<Vec<String>>,
}

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

// ─── Ownership ──────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum OwnershipSource {
    DeclaredProfile,
    StoreMetadata,
    RuntimePrefix,
    ProcessWrite,
    InstallerTransaction,
    DirectoryHeuristic,
    UserApproved,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OwnershipClaim {
    pub bundle_id: ApplicationBundleId,
    pub path: PathBuf,
    pub component_kind: BundleComponentKind,
    pub source: OwnershipSource,
    pub confidence: f32,
    pub first_seen_at: i64,
    pub last_seen_at: i64,
}

// ─── Installation Recipe ───────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", content = "spec", rename_all = "snake_case")]
pub enum InstallationRecipe {
    Steam {
        application_id: String,
        preferred_runner: Option<String>,
        launch_options: Option<String>,
    },
    Epic {
        catalog_item_id: String,
    },
    Gog {
        product_id: String,
    },
    Bottles {
        bottle_name: String,
        application_id: String,
        runner: Option<String>,
    },
    NativePackage {
        package_manager: String,
        packages: Vec<PackageSpec>,
    },
    AppImage {
        download_url: Option<String>,
        local_cache_path: Option<PathBuf>,
    },
    Custom {
        executable: PathBuf,
        installation_root: PathBuf,
        environment: Option<std::collections::HashMap<String, String>>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PackageSpec {
    pub name: String,
    pub version: Option<String>,
}

// ─── Restore Result ─────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BundleRestoreResult {
    pub job_id: String,
    pub bundle_id: ApplicationBundleId,
    pub snapshot_id: String,
    pub status: String,
    pub total_components: u32,
    pub completed_components: u32,
    pub total_files: u64,
    pub restored_files: u64,
    pub total_bytes: u64,
    pub restored_bytes: u64,
    pub started_at: String,
    pub finished_at: Option<String>,
    pub error: Option<String>,
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
