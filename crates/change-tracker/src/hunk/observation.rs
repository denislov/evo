use std::io::Read;
use std::path::{Component, Path, PathBuf};

use sha2::{Digest, Sha256};

use crate::ChangeTrackerError;

pub(super) struct ObservedFile {
    pub(super) revision: String,
    pub(super) content: Option<Vec<u8>>,
}

pub(super) fn normalize_relative(path: &Path) -> Result<PathBuf, ChangeTrackerError> {
    if path.as_os_str().is_empty() || path.is_absolute() {
        return Err(ChangeTrackerError::InvalidFact {
            message: format!(
                "path must be non-empty and workspace-relative: {}",
                path.display()
            ),
        });
    }
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Normal(component) => normalized.push(component),
            Component::CurDir => {}
            _ => {
                return Err(ChangeTrackerError::InvalidFact {
                    message: format!("path escapes the workspace: {}", path.display()),
                });
            }
        }
    }
    if normalized.as_os_str().is_empty() {
        return Err(ChangeTrackerError::InvalidFact {
            message: "path resolves to the workspace root".into(),
        });
    }
    Ok(normalized)
}

pub(super) fn read_observed(
    root: &Path,
    path: &Path,
    max_bytes: usize,
) -> Result<ObservedFile, ChangeTrackerError> {
    let absolute = root.join(path);
    ensure_within_root(root, &absolute)?;
    match std::fs::File::open(&absolute) {
        Ok(mut file) => {
            let mut hasher = Sha256::new();
            let mut retained = Vec::new();
            let mut retain_content = true;
            let mut buffer = [0_u8; 16 * 1024];
            loop {
                let read = file
                    .read(&mut buffer)
                    .map_err(|error| ChangeTrackerError::Io {
                        message: format!("cannot read {}: {error}", absolute.display()),
                    })?;
                if read == 0 {
                    break;
                }
                hasher.update(&buffer[..read]);
                if retain_content {
                    if retained.len().saturating_add(read) <= max_bytes {
                        retained.extend_from_slice(&buffer[..read]);
                    } else {
                        retain_content = false;
                        retained.clear();
                    }
                }
            }
            Ok(ObservedFile {
                revision: format!("{:x}", hasher.finalize()),
                content: retain_content.then_some(retained),
            })
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(ObservedFile {
            revision: revision(&[]),
            content: Some(Vec::new()),
        }),
        Err(error) => Err(ChangeTrackerError::Io {
            message: format!("cannot read {}: {error}", absolute.display()),
        }),
    }
}

fn ensure_within_root(root: &Path, absolute: &Path) -> Result<(), ChangeTrackerError> {
    let mut candidate = absolute;
    let resolved = loop {
        match std::fs::canonicalize(candidate) {
            Ok(resolved) => break resolved,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                candidate = candidate
                    .parent()
                    .ok_or_else(|| ChangeTrackerError::InvalidFact {
                        message: format!("path escapes the workspace: {}", absolute.display()),
                    })?;
            }
            Err(error) => {
                return Err(ChangeTrackerError::Io {
                    message: format!("cannot resolve {}: {error}", absolute.display()),
                });
            }
        }
    };
    if !resolved.starts_with(root) {
        return Err(ChangeTrackerError::InvalidFact {
            message: format!(
                "path resolves outside the workspace: {}",
                absolute.display()
            ),
        });
    }
    Ok(())
}

pub(super) fn revision(content: &[u8]) -> String {
    format!("{:x}", Sha256::digest(content))
}
