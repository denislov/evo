use std::fs;
use std::io;
use std::path::Path;

use tokio_util::sync::CancellationToken;

use super::{RegistryError, WorktreeRecord, WorktreeRegistry};
use crate::worktree::{WorktreeCreationMode, WorktreeError};

/// Remove one recorded worktree's materialization, verifying its identity
/// against the record before touching the directory.
pub(super) fn remove_materialization(
    registry: &WorktreeRegistry,
    record: &WorktreeRecord,
) -> Result<(), RegistryError> {
    super::validate_record(record, registry.root())?;
    if record.creation_mode == WorktreeCreationMode::GitLinked && record.source.is_dir() {
        let registered =
            super::super::git_worktree_registration_exists(&record.source, &record.dest)
                .map_err(RegistryError::Worktree)?;
        if registered {
            super::super::git::run_git(
                &record.source,
                &["worktree", "remove", "--force"],
                Some(&record.dest),
                &CancellationToken::new(),
            )
            .map_err(RegistryError::Worktree)?;
        }
    }
    match fs::symlink_metadata(&record.dest) {
        Ok(metadata) if metadata.is_dir() => {
            fs::remove_dir_all(&record.dest).map_err(|error| RegistryError::Io {
                message: format!(
                    "cannot remove worktree directory {}: {error}",
                    record.dest.display()
                ),
            })?;
        }
        Ok(_) => {
            fs::remove_file(&record.dest).map_err(|error| RegistryError::Io {
                message: format!(
                    "cannot remove worktree path {}: {error}",
                    record.dest.display()
                ),
            })?;
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(RegistryError::Io {
                message: format!(
                    "cannot inspect worktree directory {}: {error}",
                    record.dest.display()
                ),
            });
        }
    }
    if record.creation_mode == WorktreeCreationMode::GitLinked && record.source.is_dir() {
        super::super::git::run_git(
            &record.source,
            &["worktree", "prune", "--expire", "now"],
            None,
            &CancellationToken::new(),
        )
        .map_err(RegistryError::Worktree)?;
        if super::super::git_worktree_registration_exists(&record.source, &record.dest)
            .map_err(RegistryError::Worktree)?
        {
            return Err(RegistryError::Worktree(WorktreeError::GitFailed {
                message: format!(
                    "git worktree registration remains for {}",
                    record.dest.display()
                ),
            }));
        }
    }
    remove_auxiliary_directory(&registry.baseline_dir(&record.id))?;
    remove_auxiliary_directory(&registry.transaction_dir(&record.id))?;
    Ok(())
}

fn remove_auxiliary_directory(path: &Path) -> Result<(), RegistryError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {
            fs::remove_dir_all(path).map_err(|error| RegistryError::Io {
                message: format!(
                    "cannot remove auxiliary directory {}: {error}",
                    path.display()
                ),
            })
        }
        Ok(_) => Err(RegistryError::InvalidRecord {
            message: format!("auxiliary path is not a directory: {}", path.display()),
        }),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(RegistryError::Io {
            message: format!(
                "cannot inspect auxiliary directory {}: {error}",
                path.display()
            ),
        }),
    }
}

/// Recursive byte size of `path`; missing paths count as zero.
pub(super) fn dir_size(path: &Path) -> Result<u64, io::Error> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if !metadata.is_dir() => return Ok(metadata.len()),
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(0),
        Err(error) => return Err(error),
    }
    let mut total = 0u64;
    let mut pending = vec![path.to_path_buf()];
    while let Some(current) = pending.pop() {
        let entries = match fs::read_dir(&current) {
            Ok(entries) => entries,
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error),
        };
        for entry in entries {
            let entry = entry?;
            let metadata = fs::symlink_metadata(entry.path())?;
            if metadata.is_dir() {
                pending.push(entry.path());
            } else {
                total = total.saturating_add(metadata.len());
            }
        }
    }
    Ok(total)
}
