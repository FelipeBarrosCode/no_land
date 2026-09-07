use keyring::{Entry, Error as KeyringError};

use crate::errors::{AppError, AppResult};

const SERVICE: &str = "com.noland.connect.vast";
const ACCOUNT: &str = "managed-api-key";

fn entry() -> AppResult<Entry> {
    Entry::new(SERVICE, ACCOUNT).map_err(|error| {
        AppError::State(format!(
            "Unable to access the operating-system credential vault for Vast.ai: {error}"
        ))
    })
}

pub fn store_managed_api_key(api_key: &str) -> AppResult<()> {
    if api_key.trim().is_empty() {
        return Err(AppError::InvalidInput(
            "Cannot store an empty Vast.ai API key".to_string(),
        ));
    }

    entry()?.set_password(api_key.trim()).map_err(|error| {
        AppError::State(format!(
            "Unable to store the managed Vast.ai API key in the operating-system credential vault: {error}"
        ))
    })
}

pub fn load_managed_api_key() -> AppResult<Option<String>> {
    match entry()?.get_password() {
        Ok(api_key) if !api_key.trim().is_empty() => Ok(Some(api_key)),
        Ok(_) | Err(KeyringError::NoEntry) => Ok(None),
        Err(error) => Err(AppError::State(format!(
            "Unable to read the managed Vast.ai API key from the operating-system credential vault: {error}"
        ))),
    }
}

pub fn delete_managed_api_key() -> AppResult<()> {
    match entry()?.delete_credential() {
        Ok(()) | Err(KeyringError::NoEntry) => Ok(()),
        Err(error) => Err(AppError::State(format!(
            "Unable to remove the managed Vast.ai API key from the operating-system credential vault: {error}"
        ))),
    }
}
