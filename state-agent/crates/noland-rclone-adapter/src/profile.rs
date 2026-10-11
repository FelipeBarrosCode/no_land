use serde::{Deserialize, Serialize};

use crate::{AdapterError, ProviderKind, Result};

const MIB: u64 = 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GoogleDriveTransferOptions {
    pub chunk_size: u64,
    pub upload_cutoff: u64,
}

impl Default for GoogleDriveTransferOptions {
    fn default() -> Self {
        Self {
            chunk_size: 64 * MIB,
            upload_cutoff: 8 * MIB,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ProviderOptions {
    GoogleDrive(GoogleDriveTransferOptions),
    S3,
    BackblazeB2,
    AzureBlob,
    OneDrive,
    Dropbox,
    Box,
    Sftp,
    #[default]
    Generic,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderTransferConfig {
    pub upload_concurrency: usize,
    pub download_concurrency: usize,
    pub options: ProviderOptions,
}

impl ProviderTransferConfig {
    pub fn for_provider(provider: ProviderKind) -> Self {
        let capabilities = ProviderCapabilities::for_provider(provider);
        let options = match provider {
            ProviderKind::AmazonS3
            | ProviderKind::CloudflareR2
            | ProviderKind::Wasabi
            | ProviderKind::DigitalOceanSpaces
            | ProviderKind::GenericS3 => ProviderOptions::S3,
            ProviderKind::BackblazeB2 => ProviderOptions::BackblazeB2,
            ProviderKind::GoogleDrive => {
                ProviderOptions::GoogleDrive(GoogleDriveTransferOptions::default())
            }
            ProviderKind::MicrosoftOneDrive => ProviderOptions::OneDrive,
            ProviderKind::Dropbox => ProviderOptions::Dropbox,
            ProviderKind::Box => ProviderOptions::Box,
            ProviderKind::AzureBlob => ProviderOptions::AzureBlob,
            ProviderKind::Sftp => ProviderOptions::Sftp,
            ProviderKind::GoogleCloudStorage | ProviderKind::Webdav | ProviderKind::Local => {
                ProviderOptions::Generic
            }
        };
        Self {
            upload_concurrency: capabilities.preferred_upload_concurrency,
            download_concurrency: capabilities.preferred_download_concurrency,
            options,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderCapabilities {
    pub supports_bulk_listing: bool,
    pub supports_conditional_create: bool,
    pub supports_atomic_move: bool,
    pub supports_server_side_copy: bool,
    pub supports_native_multipart: bool,
    pub preferred_upload_concurrency: usize,
    pub max_upload_concurrency: usize,
    pub preferred_download_concurrency: usize,
    pub max_download_concurrency: usize,
    pub estimated_memory_per_transfer: u64,
    pub preferred_remote_object_size: Option<u64>,
}

impl ProviderCapabilities {
    pub fn for_provider(provider: ProviderKind) -> Self {
        let supports_native_multipart = matches!(
            provider,
            ProviderKind::AmazonS3
                | ProviderKind::BackblazeB2
                | ProviderKind::CloudflareR2
                | ProviderKind::Wasabi
                | ProviderKind::DigitalOceanSpaces
                | ProviderKind::GenericS3
                | ProviderKind::GoogleDrive
                | ProviderKind::GoogleCloudStorage
                | ProviderKind::MicrosoftOneDrive
                | ProviderKind::Dropbox
                | ProviderKind::Box
                | ProviderKind::AzureBlob
        );
        let (
            preferred_upload_concurrency,
            max_upload_concurrency,
            preferred_download_concurrency,
            max_download_concurrency,
            estimated_memory_per_transfer,
        ) = match provider {
            ProviderKind::BackblazeB2 => (16, 32, 16, 32, 128 * MIB),
            ProviderKind::AmazonS3
            | ProviderKind::CloudflareR2
            | ProviderKind::Wasabi
            | ProviderKind::DigitalOceanSpaces
            | ProviderKind::GenericS3
            | ProviderKind::GoogleCloudStorage => (8, 32, 8, 32, 128 * MIB),
            ProviderKind::AzureBlob => (8, 32, 8, 32, 128 * MIB),
            ProviderKind::Dropbox => (8, 32, 8, 32, 128 * MIB),
            ProviderKind::GoogleDrive | ProviderKind::MicrosoftOneDrive => {
                (4, 16, 4, 16, 128 * MIB)
            }
            ProviderKind::Box => (4, 8, 4, 8, 64 * MIB),
            ProviderKind::Sftp => (4, 8, 4, 8, 32 * MIB),
            ProviderKind::Webdav => (4, 8, 4, 8, 64 * MIB),
            ProviderKind::Local => (8, 32, 8, 32, 32 * MIB),
        };

        Self {
            supports_bulk_listing: true,
            // Noland currently guarantees immutability through rclone's
            // `--immutable`, not a provider-native conditional create API.
            supports_conditional_create: false,
            supports_atomic_move: provider == ProviderKind::Local,
            // Server-side copy is not exposed through SharedStorageProvider yet.
            supports_server_side_copy: false,
            supports_native_multipart,
            preferred_upload_concurrency,
            max_upload_concurrency,
            preferred_download_concurrency,
            max_download_concurrency,
            estimated_memory_per_transfer,
            preferred_remote_object_size: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransferProfile {
    pub upload_concurrency: usize,
    pub download_concurrency: usize,
    pub rclone_backend_args: Vec<String>,
    pub rclone_global_args: Vec<String>,
}

impl TransferProfile {
    pub fn for_provider(provider: ProviderKind, backend_type: &str) -> Result<Self> {
        validate_provider_backend(provider, backend_type)?;
        let config = ProviderTransferConfig::for_provider(provider);
        let rclone_backend_args = match config.options {
            ProviderOptions::GoogleDrive(options) => vec![
                "--drive-chunk-size".into(),
                format_size_mib(options.chunk_size)?,
                "--drive-upload-cutoff".into(),
                format_size_mib(options.upload_cutoff)?,
            ],
            ProviderOptions::S3 => vec![
                "--s3-upload-cutoff".into(),
                "64M".into(),
                "--s3-chunk-size".into(),
                "16M".into(),
                "--s3-upload-concurrency".into(),
                "4".into(),
            ],
            ProviderOptions::BackblazeB2 => vec![
                "--b2-upload-cutoff".into(),
                "64M".into(),
                "--b2-chunk-size".into(),
                "16M".into(),
                "--b2-upload-concurrency".into(),
                "4".into(),
            ],
            ProviderOptions::AzureBlob => vec![
                "--azureblob-chunk-size".into(),
                "8M".into(),
                "--azureblob-upload-concurrency".into(),
                "8".into(),
            ],
            ProviderOptions::OneDrive => {
                vec!["--onedrive-chunk-size".into(), "20M".into()]
            }
            ProviderOptions::Dropbox => vec![
                "--dropbox-chunk-size".into(),
                "64M".into(),
                "--dropbox-batch-mode".into(),
                "sync".into(),
            ],
            ProviderOptions::Box => vec!["--box-upload-cutoff".into(), "50M".into()],
            ProviderOptions::Sftp => vec!["--sftp-concurrency".into(), "128".into()],
            ProviderOptions::Generic => Vec::new(),
        };
        Ok(Self {
            upload_concurrency: config.upload_concurrency,
            download_concurrency: config.download_concurrency,
            rclone_backend_args,
            rclone_global_args: Vec::new(),
        })
    }
}

pub fn provider_arg_is_allowed(provider: ProviderKind, arg: &str) -> bool {
    let required_provider = if arg.starts_with("--s3-") {
        matches!(
            provider,
            ProviderKind::AmazonS3
                | ProviderKind::CloudflareR2
                | ProviderKind::Wasabi
                | ProviderKind::DigitalOceanSpaces
                | ProviderKind::GenericS3
        )
    } else if arg.starts_with("--b2-") {
        provider == ProviderKind::BackblazeB2
    } else if arg.starts_with("--drive-") {
        provider == ProviderKind::GoogleDrive
    } else if arg.starts_with("--gcs-") {
        provider == ProviderKind::GoogleCloudStorage
    } else if arg.starts_with("--onedrive-") {
        provider == ProviderKind::MicrosoftOneDrive
    } else if arg.starts_with("--dropbox-") {
        provider == ProviderKind::Dropbox
    } else if arg.starts_with("--box-") {
        provider == ProviderKind::Box
    } else if arg.starts_with("--azureblob-") {
        provider == ProviderKind::AzureBlob
    } else if arg.starts_with("--sftp-") {
        provider == ProviderKind::Sftp
    } else if arg.starts_with("--webdav-") {
        provider == ProviderKind::Webdav
    } else {
        true
    };
    required_provider
}

pub fn configured_backend_type<'a>(config_ini: &'a str, remote_name: &str) -> Option<&'a str> {
    let wanted_section = format!("[{remote_name}]");
    let mut in_section = false;
    for raw_line in config_ini.lines() {
        let line = raw_line.trim();
        if line.starts_with('[') && line.ends_with(']') {
            in_section = line == wanted_section;
            continue;
        }
        if in_section {
            if let Some((key, value)) = line.split_once('=') {
                if key.trim() == "type" {
                    return Some(value.trim());
                }
            }
        }
    }
    None
}

pub fn validate_provider_backend(provider: ProviderKind, backend_type: &str) -> Result<()> {
    let expected = provider.backend_type();
    if expected != backend_type {
        return Err(AdapterError::Invalid(format!(
            "provider/backend mismatch: {} requires rclone backend `{expected}`, got `{backend_type}`",
            provider.as_str()
        )));
    }
    Ok(())
}

fn format_size_mib(bytes: u64) -> Result<String> {
    if bytes == 0 || bytes % MIB != 0 {
        return Err(AdapterError::Invalid(format!(
            "rclone transfer size {bytes} is not a positive whole MiB"
        )));
    }
    Ok(format!("{}M", bytes / MIB))
}

#[cfg(test)]
mod tests {
    use super::*;

    const NON_DRIVE_PROVIDERS: &[ProviderKind] = &[
        ProviderKind::AmazonS3,
        ProviderKind::BackblazeB2,
        ProviderKind::CloudflareR2,
        ProviderKind::Wasabi,
        ProviderKind::DigitalOceanSpaces,
        ProviderKind::GenericS3,
        ProviderKind::GoogleCloudStorage,
        ProviderKind::MicrosoftOneDrive,
        ProviderKind::Dropbox,
        ProviderKind::Box,
        ProviderKind::AzureBlob,
        ProviderKind::Sftp,
        ProviderKind::Webdav,
        ProviderKind::Local,
    ];

    #[test]
    fn drive_profile_defaults_to_four_uploads_and_has_automatic_ceiling() {
        let profile = TransferProfile::for_provider(ProviderKind::GoogleDrive, "drive").unwrap();
        assert_eq!(profile.upload_concurrency, 4);
        assert_eq!(
            profile.rclone_backend_args,
            ["--drive-chunk-size", "64M", "--drive-upload-cutoff", "8M"]
        );

        let capabilities = ProviderCapabilities::for_provider(ProviderKind::GoogleDrive);
        assert_eq!(capabilities.max_upload_concurrency, 16);
        assert_eq!(capabilities.max_download_concurrency, 16);
    }

    #[test]
    fn non_drive_profiles_never_receive_drive_flags() {
        for provider in NON_DRIVE_PROVIDERS {
            let profile =
                TransferProfile::for_provider(*provider, provider.backend_type()).unwrap();
            assert!(profile
                .rclone_backend_args
                .iter()
                .all(|arg| !arg.starts_with("--drive-")));
        }
    }

    #[test]
    fn every_tunable_backend_receives_only_its_own_optimization_flags() {
        let expected = [
            (ProviderKind::AmazonS3, "--s3-chunk-size"),
            (ProviderKind::CloudflareR2, "--s3-chunk-size"),
            (ProviderKind::Wasabi, "--s3-chunk-size"),
            (ProviderKind::DigitalOceanSpaces, "--s3-chunk-size"),
            (ProviderKind::GenericS3, "--s3-chunk-size"),
            (ProviderKind::BackblazeB2, "--b2-chunk-size"),
            (ProviderKind::GoogleDrive, "--drive-chunk-size"),
            (ProviderKind::MicrosoftOneDrive, "--onedrive-chunk-size"),
            (ProviderKind::Dropbox, "--dropbox-chunk-size"),
            (ProviderKind::Box, "--box-upload-cutoff"),
            (ProviderKind::AzureBlob, "--azureblob-chunk-size"),
            (ProviderKind::Sftp, "--sftp-concurrency"),
        ];
        for (provider, flag) in expected {
            let profile = TransferProfile::for_provider(provider, provider.backend_type()).unwrap();
            assert!(profile.rclone_backend_args.iter().any(|arg| arg == flag));
            assert!(profile
                .rclone_backend_args
                .iter()
                .all(|arg| provider_arg_is_allowed(provider, arg)));
        }
        for provider in [
            ProviderKind::GoogleCloudStorage,
            ProviderKind::Webdav,
            ProviderKind::Local,
        ] {
            assert!(
                TransferProfile::for_provider(provider, provider.backend_type())
                    .unwrap()
                    .rclone_backend_args
                    .is_empty()
            );
        }
    }

    #[test]
    fn provider_backend_mismatch_is_rejected() {
        assert!(TransferProfile::for_provider(ProviderKind::GoogleDrive, "s3").is_err());
        assert!(TransferProfile::for_provider(ProviderKind::AmazonS3, "drive").is_err());
    }

    #[test]
    fn reads_backend_type_from_the_named_rclone_section() {
        let config = "[other]\ntype = local\n[noland_drive]\ntype = drive\ntoken = secret\n";
        assert_eq!(
            configured_backend_type(config, "noland_drive"),
            Some("drive")
        );
        assert_eq!(configured_backend_type(config, "missing"), None);
    }
}
