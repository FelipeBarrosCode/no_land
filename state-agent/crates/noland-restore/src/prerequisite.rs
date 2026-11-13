//! Small local launcher files share the restore's durable rollback decision.
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::PathBuf;

use noland_state_core::{disk, join_validated, Result, StateError};

use crate::{
    canonicalize_root_if_present, destination_partial_path, sync_parent_directory,
    validate_parent_chain, JournalState, RestoreTransaction,
};

#[derive(Debug, Clone)]
pub struct RestorePrerequisite {
    pub root: PathBuf,
    pub relative_path: String,
    pub contents: Vec<u8>,
}

impl RestoreTransaction<'_> {
    pub(super) fn publish_prerequisites(
        &mut self,
        prerequisites: &[RestorePrerequisite],
    ) -> Result<()> {
        for prerequisite in prerequisites {
            let root = canonicalize_root_if_present(prerequisite.root.clone())?;
            let destination = join_validated(&root, &prerequisite.relative_path)?;
            validate_parent_chain(&root, &destination)?;
            if let Some(metadata) = crate::symlink_metadata_if_present(&destination)? {
                if !metadata.is_file() {
                    return Err(StateError::UnsafePath(
                        "launcher prerequisite is not a regular file".into(),
                    ));
                }
                // Preserve existing bytes, including a real appmanifest restored
                // from the bundle. Only a missing prerequisite needs a fallback.
                continue;
            }
            self.disk_reservations
                .push(disk::DiskReservation::reserve(&[(
                    destination.clone(),
                    prerequisite.contents.len() as u64 + 4096,
                )])?);
            self.allowed_roots.insert(root.clone());
            self.persist_journal(JournalState::Active)?;
            self.create_directory_chain_journaled(
                &root,
                destination.parent().expect("validated destination"),
            )?;
            let partial = destination_partial_path(&destination, self.plan.restore_id)?;
            self.track_partial(&partial)?;
            let mut output = OpenOptions::new()
                .write(true)
                .create_new(true)
                .custom_flags(libc::O_NOFOLLOW)
                .mode(0o600)
                .open(&partial)?;
            output.write_all(&prerequisite.contents)?;
            output.sync_all()?;
            drop(output);
            validate_parent_chain(&root, &destination)?;
            if crate::symlink_metadata_if_present(&destination)?.is_some() {
                return Err(StateError::Integrity(
                    "launcher prerequisite appeared during restore".into(),
                ));
            }
            self.track_created_path(&destination)?;
            fs::rename(&partial, &destination)?;
            sync_parent_directory(&destination)?;
            self.clear_partial(&partial)?;
        }
        Ok(())
    }
}
