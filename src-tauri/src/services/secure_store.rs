//! One OS-protected item for desktop secrets, shared by all credential consumers.
//! The mutex covers reads and writes so concurrent callers cannot prompt together
//! or overwrite another caller's credentials. Never persist this cache to disk.
use std::{
    collections::BTreeMap,
    sync::{Mutex, OnceLock},
};

use keyring::Entry;
use serde::{Deserialize, Serialize};

use crate::errors::{AppError, AppResult};

const SERVICE: &str = "noland-connect.desktop-secrets.v2";
const ACCOUNT: &str = "vault";

#[derive(Clone, Default, PartialEq, Serialize, Deserialize)]
struct Document {
    // Explicit nulls are tombstones: deleted credentials must not be reimported.
    entries: BTreeMap<String, Option<String>>,
}

trait Backend: Send + Sync {
    fn read(&self, service: &str, account: &str) -> AppResult<Option<String>>;
    fn write(&self, value: &str) -> AppResult<()>;
}

struct OsBackend;

impl Backend for OsBackend {
    fn read(&self, service: &str, account: &str) -> AppResult<Option<String>> {
        let entry = Entry::new(service, account).map_err(storage_error)?;
        match entry.get_password() {
            Ok(value) => Ok(Some(value)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(error) => Err(storage_error(error)),
        }
    }

    fn write(&self, value: &str) -> AppResult<()> {
        Entry::new(SERVICE, ACCOUNT)
            .map_err(storage_error)?
            .set_password(value)
            .map_err(storage_error)
    }
}

fn storage_error(error: keyring::Error) -> AppError {
    AppError::SecureStorage(format!(
        "Could not access the protected credential vault: {error}"
    ))
}

struct Vault<B> {
    backend: B,
    cache: Mutex<Option<Document>>,
}

fn key(service: &str, account: &str) -> String {
    // Encoding a tuple avoids collisions between arbitrary service/account names.
    serde_json::to_string(&(service, account)).expect("string tuple serialization")
}

impl<B: Backend> Vault<B> {
    fn new(backend: B) -> Self {
        Self {
            backend,
            cache: Mutex::new(None),
        }
    }

    fn locked(&self) -> AppResult<std::sync::MutexGuard<'_, Option<Document>>> {
        let mut cache = self
            .cache
            .lock()
            .map_err(|_| AppError::SecureStorage("Credential vault cache is poisoned".into()))?;
        if cache.is_none() {
            let document = match self.backend.read(SERVICE, ACCOUNT)? {
                Some(value) => serde_json::from_str(&value).map_err(|_| {
                    AppError::SecureStorage("Protected credential vault is invalid".into())
                })?,
                None => Document::default(),
            };
            *cache = Some(document);
        }
        Ok(cache)
    }

    fn commit(&self, cache: &mut Option<Document>, next: Document) -> AppResult<()> {
        if cache.as_ref() == Some(&next) {
            return Ok(());
        }
        let serialized = serde_json::to_string(&next)?;
        self.backend.write(&serialized)?;
        // Verify persistence before publishing new credentials to other callers.
        let verified = self.backend.read(SERVICE, ACCOUNT)?;
        if verified.as_deref() != Some(serialized.as_str()) {
            return Err(AppError::SecureStorage(
                "Credential vault failed read-back verification".into(),
            ));
        }
        *cache = Some(next);
        Ok(())
    }

    fn get(&self, service: &str, account: &str) -> AppResult<Option<String>> {
        let mut cache = self.locked()?;
        let id = key(service, account);
        if let Some(value) = cache.as_ref().unwrap().entries.get(&id) {
            return Ok(value.clone());
        }
        // One-time lazy import. Leave old OS-protected entries intact until the
        // new value has been verified; never fall back after a denial or failure.
        let value = self.backend.read(service, account)?;
        let mut next = cache.as_ref().unwrap().clone();
        next.entries.insert(id, value.clone());
        if value.is_some() {
            self.commit(&mut cache, next)?;
        } else {
            // Missing legacy entries need no write or authorization prompt.
            *cache = Some(next);
        }
        Ok(value)
    }

    fn update(&self, service: &str, values: &BTreeMap<String, Option<String>>) -> AppResult<()> {
        let mut cache = self.locked()?;
        let mut next = cache.as_ref().unwrap().clone();
        for (account, value) in values {
            next.entries.insert(key(service, account), value.clone());
        }
        self.commit(&mut cache, next)
    }
}

#[cfg(not(target_os = "windows"))]
fn vault() -> &'static Vault<OsBackend> {
    static VAULT: OnceLock<Vault<OsBackend>> = OnceLock::new();
    VAULT.get_or_init(|| Vault::new(OsBackend))
}

#[cfg(not(target_os = "windows"))]
pub fn get(service: &str, account: &str) -> AppResult<Option<String>> {
    vault().get(service, account)
}

#[cfg(not(target_os = "windows"))]
pub fn update(service: &str, values: &BTreeMap<String, Option<String>>) -> AppResult<()> {
    vault().update(service, values)
}

pub fn put(service: &str, account: &str, value: &str) -> AppResult<()> {
    update(
        service,
        &BTreeMap::from([(account.to_owned(), Some(value.to_owned()))]),
    )
}

pub fn delete(service: &str, account: &str) -> AppResult<()> {
    update(service, &BTreeMap::from([(account.to_owned(), None)]))
}

// Windows Credential Manager limits each credential blob's size. Keep existing
// per-entry storage there rather than making large storage profiles un-saveable.
#[cfg(target_os = "windows")]
fn windows_cache() -> &'static Mutex<BTreeMap<String, Option<String>>> {
    static CACHE: OnceLock<Mutex<BTreeMap<String, Option<String>>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(BTreeMap::new()))
}

#[cfg(target_os = "windows")]
pub fn get(service: &str, account: &str) -> AppResult<Option<String>> {
    let mut cache = windows_cache()
        .lock()
        .map_err(|_| AppError::SecureStorage("Credential cache is poisoned".into()))?;
    let id = key(service, account);
    if let Some(value) = cache.get(&id) {
        return Ok(value.clone());
    }
    let value = OsBackend.read(service, account)?;
    cache.insert(id, value.clone());
    Ok(value)
}

#[cfg(target_os = "windows")]
pub fn update(service: &str, values: &BTreeMap<String, Option<String>>) -> AppResult<()> {
    let mut cache = windows_cache()
        .lock()
        .map_err(|_| AppError::SecureStorage("Credential cache is poisoned".into()))?;
    for (account, value) in values {
        let id = key(service, account);
        if cache.get(&id) == Some(value) {
            continue;
        }
        let entry = Entry::new(service, account).map_err(storage_error)?;
        match value {
            Some(value) => {
                entry.set_password(value).map_err(storage_error)?;
                if entry.get_password().map_err(storage_error)? != *value {
                    return Err(AppError::SecureStorage(
                        "Credential failed read-back verification".into(),
                    ));
                }
            }
            None => match entry.delete_credential() {
                Ok(()) | Err(keyring::Error::NoEntry) => {}
                Err(error) => return Err(storage_error(error)),
            },
        }
        cache.insert(id, value.clone());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc,
    };

    #[derive(Default)]
    struct MemoryBackend {
        values: Mutex<BTreeMap<String, String>>,
        vault_reads: AtomicUsize,
        legacy_reads: AtomicUsize,
        writes: AtomicUsize,
        denied: AtomicBool,
        reject_write: AtomicBool,
        corrupt_readback: AtomicBool,
    }

    impl Backend for Arc<MemoryBackend> {
        fn read(&self, service: &str, account: &str) -> AppResult<Option<String>> {
            if self.denied.load(Ordering::SeqCst) {
                return Err(AppError::SecureStorage("Access denied".into()));
            }
            if service == SERVICE && account == ACCOUNT {
                self.vault_reads.fetch_add(1, Ordering::SeqCst);
                if self.corrupt_readback.load(Ordering::SeqCst)
                    && self.writes.load(Ordering::SeqCst) > 0
                {
                    return Ok(Some("invalid".into()));
                }
            } else {
                self.legacy_reads.fetch_add(1, Ordering::SeqCst);
            }
            Ok(self
                .values
                .lock()
                .unwrap()
                .get(&key(service, account))
                .cloned())
        }

        fn write(&self, value: &str) -> AppResult<()> {
            if self.reject_write.load(Ordering::SeqCst) {
                return Err(AppError::SecureStorage("Write denied".into()));
            }
            self.writes.fetch_add(1, Ordering::SeqCst);
            self.values
                .lock()
                .unwrap()
                .insert(key(SERVICE, ACCOUNT), value.into());
            Ok(())
        }
    }

    #[test]
    fn all_consumers_share_one_cached_read() {
        let backend = Arc::new(MemoryBackend::default());
        let document = Document {
            entries: BTreeMap::from([
                (key("state", "vast"), Some("api-key".into())),
                (key("turn", "default"), Some("turn-token".into())),
                (key("network", "instance-1"), Some("control-key".into())),
            ]),
        };
        backend.values.lock().unwrap().insert(
            key(SERVICE, ACCOUNT),
            serde_json::to_string(&document).unwrap(),
        );
        let vault = Arc::new(Vault::new(backend.clone()));
        std::thread::scope(|scope| {
            for _ in 0..8 {
                let vault = vault.clone();
                scope.spawn(move || {
                    for (service, account) in [
                        ("state", "vast"),
                        ("turn", "default"),
                        ("network", "instance-1"),
                    ] {
                        assert!(vault.get(service, account).unwrap().is_some());
                    }
                });
            }
        });
        assert_eq!(backend.vault_reads.load(Ordering::SeqCst), 1);
        assert_eq!(backend.legacy_reads.load(Ordering::SeqCst), 0);
        assert_eq!(backend.writes.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn batch_saves_once_and_unchanged_saves_do_not_access_keychain() {
        let backend = Arc::new(MemoryBackend::default());
        let vault = Vault::new(backend.clone());
        let values = BTreeMap::from([
            ("vast".into(), Some("key".into())),
            ("storage".into(), Some("token".into())),
        ]);
        vault.update("state", &values).unwrap();
        vault.update("state", &values).unwrap();
        assert_eq!(backend.writes.load(Ordering::SeqCst), 1);
        assert_eq!(backend.vault_reads.load(Ordering::SeqCst), 2); // initial read + verification
        let restarted = Vault::new(backend.clone());
        assert_eq!(
            restarted.get("state", "storage").unwrap().as_deref(),
            Some("token")
        );
    }

    #[test]
    fn legacy_import_and_deletion_survive_restart_without_resurrection() {
        let backend = Arc::new(MemoryBackend::default());
        backend
            .values
            .lock()
            .unwrap()
            .insert(key("old", "credential"), "legacy-secret".into());
        let vault = Vault::new(backend.clone());
        assert_eq!(
            vault.get("old", "credential").unwrap().as_deref(),
            Some("legacy-secret")
        );
        let restarted = Vault::new(backend.clone());
        assert!(restarted.get("old", "credential").unwrap().is_some());
        restarted
            .update("old", &BTreeMap::from([("credential".into(), None)]))
            .unwrap();
        let restarted = Vault::new(backend.clone());
        assert!(restarted.get("old", "credential").unwrap().is_none());
        assert_eq!(backend.legacy_reads.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn concurrent_updates_preserve_all_consumers() {
        let backend = Arc::new(MemoryBackend::default());
        let vault = Arc::new(Vault::new(backend.clone()));
        std::thread::scope(|scope| {
            for index in 0..8 {
                let vault = vault.clone();
                scope.spawn(move || {
                    vault
                        .update(
                            "network",
                            &BTreeMap::from([(index.to_string(), Some(index.to_string()))]),
                        )
                        .unwrap()
                });
            }
        });
        let restarted = Vault::new(backend);
        for index in 0..8 {
            assert_eq!(
                restarted.get("network", &index.to_string()).unwrap(),
                Some(index.to_string())
            );
        }
    }

    #[test]
    fn denied_or_corrupt_vault_never_falls_back_to_legacy() {
        let backend = Arc::new(MemoryBackend::default());
        backend.denied.store(true, Ordering::SeqCst);
        assert!(Vault::new(backend.clone())
            .get("old", "credential")
            .is_err());
        backend.denied.store(false, Ordering::SeqCst);
        backend
            .values
            .lock()
            .unwrap()
            .insert(key(SERVICE, ACCOUNT), "invalid".into());
        assert!(Vault::new(backend.clone())
            .get("old", "credential")
            .is_err());
        assert_eq!(backend.legacy_reads.load(Ordering::SeqCst), 0);
        assert_eq!(backend.writes.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn failed_writes_and_readback_do_not_publish_new_secrets() {
        for readback in [false, true] {
            let backend = Arc::new(MemoryBackend::default());
            let vault = Vault::new(backend.clone());
            assert!(vault.get("state", "key").unwrap().is_none());
            backend.reject_write.store(!readback, Ordering::SeqCst);
            backend.corrupt_readback.store(readback, Ordering::SeqCst);
            assert!(vault
                .update(
                    "state",
                    &BTreeMap::from([("key".into(), Some("new-secret".into()))])
                )
                .is_err());
            assert!(vault.get("state", "key").unwrap().is_none());
        }
    }
}
