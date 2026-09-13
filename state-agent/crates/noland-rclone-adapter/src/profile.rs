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
        if provider == ProviderKind::GoogleDrive {
            Self {
                upload_concurrency: 4,
                download_concurrency: 4,
                options: ProviderOptions::GoogleDrive(GoogleDriveTransferOptions::default()),
            }
        } else {
            Self {
                upload_concurrency: 2,
                download_concurrency: 4,
                options: ProviderOptions::Generic,
            }
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
        let preferred_upload_concurrency = if provider == ProviderKind::GoogleDrive {
            4
        } else {
            2
        };
        let max_upload_concurrency = if provider == ProviderKind::GoogleDrive {
            4
        } else {
            usize::MAX
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
            preferred_download_concurrency: 4,
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
            ProviderOptions::Generic => Vec::new(),
        };
        Ok(Self {
            upload_concurrency: config.upload_concurrency,
            download_concurrency: config.download_concurrency,
            rclone_backend_args,
            rclone_global_args: Vec::new(),
        })
    }

    pub fn with_upload_concurrency(
        mut self,
        provider: ProviderKind,
        concurrency: usize,
    ) -> Result<Self> {
        let capabilities = ProviderCapabilities::for_provider(provider);
        if concurrency == 0 || concurrency > capabilities.max_upload_concurrency {
            return Err(AdapterError::Invalid(format!(
                "{} upload concurrency must be between 1 and {}",
                provider.label(),
                capabilities.max_upload_concurrency
            )));
        }
        self.upload_concurrency = concurrency;
        Ok(self)
    }
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
    fn drive_profile_uses_four_uploads_and_larger_chunks() {
        let profile = TransferProfile::for_provider(ProviderKind::GoogleDrive, "drive").unwrap();
        assert_eq!(profile.upload_concurrency, 4);
        assert_eq!(
            profile.rclone_backend_args,
            ["--drive-chunk-size", "64M", "--drive-upload-cutoff", "8M"]
        );

        let profile = profile
            .with_upload_concurrency(ProviderKind::GoogleDrive, 2)
            .unwrap();
        assert_eq!(profile.upload_concurrency, 2);
        assert!(
            TransferProfile::for_provider(ProviderKind::GoogleDrive, "drive")
                .unwrap()
                .with_upload_concurrency(ProviderKind::GoogleDrive, 5)
                .is_err()
        );
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
