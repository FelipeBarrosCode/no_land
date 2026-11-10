//! Write-ahead intents, not O(files²) rewrites of an ever-growing JSON document.
//!
//! The v1 JSON checkpoint remains readable. Each checksummed log record is synced
//! before its filesystem mutations. A short trailing record was never acknowledged
//! and is safe to ignore; corruption of a complete record fails recovery closed.
use std::collections::BTreeSet;
use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use noland_state_core::{Result, StateError};
use serde::{Deserialize, Serialize};

use super::{JournalState, RestoreTransactionJournal, TRANSACTION_JOURNAL_FILE};

const LOG: &str = "restore-intents.v1.log";
const MAX_RECORD: usize = 16 * 1024 * 1024;

#[derive(Serialize, Deserialize)]
pub(super) enum Intent {
    Directory(PathBuf),
    Created(PathBuf),
    Partial(PathBuf),
}

fn validate_log(path: &Path) -> Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(meta) if meta.is_file() && !meta.file_type().is_symlink() => Ok(true),
        Ok(_) => Err(StateError::UnsafePath(format!(
            "invalid restore intent log: {}",
            path.display()
        ))),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error.into()),
    }
}

pub(super) fn append(workspace: &Path, intents: &[Intent]) -> Result<()> {
    if intents.is_empty() {
        return Ok(());
    }
    let bytes = serde_json::to_vec(intents)?;
    if bytes.len() > MAX_RECORD {
        return Err(StateError::Invalid(
            "restore intent batch is too large".into(),
        ));
    }
    let path = workspace.join(LOG);
    let exists = validate_log(&path)?;
    let mut options = OpenOptions::new();
    options.write(true).append(true);
    if !exists {
        options.create_new(true);
    }
    let mut file = options.open(path)?;
    file.write_all(&(bytes.len() as u32).to_le_bytes())?;
    file.write_all(&bytes)?;
    file.write_all(blake3::hash(&bytes).as_bytes())?;
    file.sync_all()?;
    if !exists {
        fs::File::open(workspace)?.sync_all()?;
    }
    Ok(())
}

/// Called only after the replacement checkpoint and its directory are durable.
pub(super) fn checkpoint(workspace: &Path) -> Result<()> {
    let path = workspace.join(LOG);
    if validate_log(&path)? {
        OpenOptions::new()
            .write(true)
            .truncate(true)
            .open(path)?
            .sync_all()?;
    }
    Ok(())
}

pub(super) fn read(workspace: &Path) -> Result<RestoreTransactionJournal> {
    let mut journal: RestoreTransactionJournal =
        serde_json::from_slice(&fs::read(workspace.join(TRANSACTION_JOURNAL_FILE))?)?;
    let path = workspace.join(LOG);
    if !validate_log(&path)? || journal.state == JournalState::Committed {
        return Ok(journal);
    }
    let mut created = journal.created_paths.into_iter().collect::<BTreeSet<_>>();
    let mut directories = journal
        .created_directories
        .into_iter()
        .collect::<BTreeSet<_>>();
    let mut partials = journal.partial_paths.into_iter().collect::<BTreeSet<_>>();
    let mut roots = journal.allowed_roots.into_iter().collect::<BTreeSet<_>>();
    let mut file = fs::File::open(path)?;
    loop {
        let mut length = [0; 4];
        match file.read_exact(&mut length) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Err(error) => return Err(error.into()),
        }
        let length = u32::from_le_bytes(length) as usize;
        if length > MAX_RECORD {
            return Err(StateError::Integrity(
                "invalid restore intent length".into(),
            ));
        }
        let mut bytes = vec![0; length];
        let mut checksum = [0; 32];
        if let Err(error) = file
            .read_exact(&mut bytes)
            .and_then(|()| file.read_exact(&mut checksum))
        {
            if error.kind() == std::io::ErrorKind::UnexpectedEof {
                break;
            }
            return Err(error.into());
        }
        if blake3::hash(&bytes).as_bytes() != &checksum {
            return Err(StateError::Integrity(
                "restore intent checksum mismatch".into(),
            ));
        }
        for intent in serde_json::from_slice::<Vec<Intent>>(&bytes)? {
            match intent {
                Intent::Directory(path) => {
                    roots.insert(path.clone());
                    directories.insert(path);
                }
                Intent::Created(path) => {
                    created.insert(path);
                }
                Intent::Partial(path) => {
                    partials.insert(path);
                }
            }
        }
    }
    journal.created_paths = created.into_iter().collect();
    journal.created_directories = directories.into_iter().collect();
    journal.partial_paths = partials.into_iter().collect();
    journal.allowed_roots = roots.into_iter().collect();
    Ok(journal)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{persist_transaction_journal, TRANSACTION_JOURNAL_VERSION};
    use uuid::Uuid;

    fn workspace() -> PathBuf {
        let id = Uuid::new_v4();
        let workspace = std::env::temp_dir().join(format!("noland-intents-{id}"));
        persist_transaction_journal(
            &workspace,
            &RestoreTransactionJournal {
                version: TRANSACTION_JOURNAL_VERSION,
                restore_id: id,
                state: JournalState::Active,
                allowed_roots: vec![],
                rollback_mappings: vec![],
                prior_symlinks: vec![],
                created_paths: vec![],
                created_directories: vec![],
                partial_paths: vec![],
            },
        )
        .unwrap();
        workspace
    }

    #[test]
    fn journal_growth_is_linear_and_does_not_rewrite_checkpoint_per_file() {
        let workspace = workspace();
        let before = fs::read(workspace.join(TRANSACTION_JOURNAL_FILE)).unwrap();
        for batch in 0..10 {
            let intents = (0..100)
                .map(|index| {
                    Intent::Created(workspace.join(format!("file-{}", batch * 100 + index)))
                })
                .collect::<Vec<_>>();
            append(&workspace, &intents).unwrap();
        }
        assert_eq!(
            fs::read(workspace.join(TRANSACTION_JOURNAL_FILE)).unwrap(),
            before
        );
        assert_eq!(read(&workspace).unwrap().created_paths.len(), 1000);
        assert!(fs::metadata(workspace.join(LOG)).unwrap().len() < 200_000);
        let journal = read(&workspace).unwrap();
        persist_transaction_journal(&workspace, &journal).unwrap();
        checkpoint(&workspace).unwrap();
        assert_eq!(fs::metadata(workspace.join(LOG)).unwrap().len(), 0);
        assert_eq!(read(&workspace).unwrap().created_paths.len(), 1000);
        fs::remove_dir_all(workspace).unwrap();
    }

    #[test]
    fn recovery_ignores_unacknowledged_torn_tail_but_rejects_corrupt_complete_record() {
        let workspace = workspace();
        append(&workspace, &[Intent::Created(workspace.join("safe"))]).unwrap();
        let valid = fs::read(workspace.join(LOG)).unwrap();
        OpenOptions::new()
            .append(true)
            .open(workspace.join(LOG))
            .unwrap()
            .write_all(&[1, 0])
            .unwrap();
        assert_eq!(read(&workspace).unwrap().created_paths.len(), 1);
        let mut corrupt = valid;
        *corrupt.last_mut().unwrap() ^= 1;
        fs::write(workspace.join(LOG), corrupt).unwrap();
        assert!(read(&workspace).is_err());
        assert!(workspace.exists());
        fs::remove_dir_all(workspace).unwrap();
    }

    #[test]
    fn version_one_json_only_journal_remains_readable() {
        let workspace = workspace();
        let mut journal = read(&workspace).unwrap();
        journal.version = 1;
        persist_transaction_journal(&workspace, &journal).unwrap();
        assert_eq!(read(&workspace).unwrap().version, 1);
        fs::remove_dir_all(workspace).unwrap();
    }
}
