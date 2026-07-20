use std::{
    fs,
    path::{Path, PathBuf},
};

use async_trait::async_trait;

use super::secret_store::{SecretBytes, SecretStore};
use crate::moonlight::domain::{MoonlightError, SecretReference};

#[derive(Debug, Clone)]
pub struct FileSecretStore {
    root_dir: PathBuf,
}

impl FileSecretStore {
    pub fn new(root_dir: PathBuf) -> Self {
        Self { root_dir }
    }

    fn file_path_for_reference(&self, reference: &SecretReference) -> PathBuf {
        if let Some(relative) = reference.0.strip_prefix("moonlight-file://") {
            let candidate = relative.trim().trim_matches('/');
            if !candidate.is_empty()
                && !candidate.contains("..")
                && !candidate.contains('\\')
                && !Path::new(candidate).is_absolute()
            {
                return self.root_dir.join(candidate);
            }
        }

        let digest = sha256_hex(reference.0.as_bytes());
        self.root_dir.join(format!("{digest}.pem"))
    }

    fn ensure_root_dir(&self) -> Result<(), MoonlightError> {
        fs::create_dir_all(&self.root_dir).map_err(MoonlightError::from)?;
        set_private_dir_permissions(&self.root_dir)?;
        Ok(())
    }
}

#[async_trait]
impl SecretStore for FileSecretStore {
    async fn get(
        &self,
        reference: &SecretReference,
    ) -> Result<Option<SecretBytes>, MoonlightError> {
        let path = self.file_path_for_reference(reference);
        match fs::read(path) {
            Ok(bytes) => Ok(Some(SecretBytes(bytes))),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(MoonlightError::Persistence(error.to_string())),
        }
    }

    async fn put(
        &self,
        reference: &SecretReference,
        value: SecretBytes,
    ) -> Result<(), MoonlightError> {
        self.ensure_root_dir()?;
        let path = self.file_path_for_reference(reference);
        let temp_path = path.with_extension("tmp");
        fs::write(&temp_path, &value.0).map_err(MoonlightError::from)?;
        set_private_file_permissions(&temp_path)?;
        fs::rename(&temp_path, &path).map_err(MoonlightError::from)?;
        set_private_file_permissions(&path)?;
        Ok(())
    }

    async fn remove(&self, reference: &SecretReference) -> Result<(), MoonlightError> {
        let path = self.file_path_for_reference(reference);
        match fs::remove_file(path) {
            Ok(_) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(MoonlightError::Persistence(error.to_string())),
        }
    }
}

#[cfg(unix)]
fn set_private_dir_permissions(path: &Path) -> Result<(), MoonlightError> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).map_err(MoonlightError::from)
}

#[cfg(not(unix))]
fn set_private_dir_permissions(_path: &Path) -> Result<(), MoonlightError> {
    Ok(())
}

#[cfg(unix)]
fn set_private_file_permissions(path: &Path) -> Result<(), MoonlightError> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600)).map_err(MoonlightError::from)
}

#[cfg(not(unix))]
fn set_private_file_permissions(_path: &Path) -> Result<(), MoonlightError> {
    Ok(())
}

fn sha256_hex(input: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(input);
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::FileSecretStore;
    use crate::moonlight::domain::SecretReference;

    #[test]
    fn uses_stable_file_name_for_moonlight_file_scheme() {
        let store =
            FileSecretStore::new(std::env::temp_dir().join("noland-file-secret-store-test"));
        let path =
            store.file_path_for_reference(&SecretReference::new("moonlight-file://client.key"));
        assert!(path.ends_with("client.key"));
    }

    #[test]
    fn falls_back_to_hashed_name_for_other_references() {
        let store =
            FileSecretStore::new(std::env::temp_dir().join("noland-file-secret-store-test"));
        let path = store.file_path_for_reference(&SecretReference::new("os-keychain://legacy/key"));
        assert_eq!(
            path.extension().and_then(|value| value.to_str()),
            Some("pem")
        );
    }
}
