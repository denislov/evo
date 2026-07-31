//! Safe managed workspace resolution for projectless desktop sessions.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::preferences::{DesktopPreferences, valid_scratch_workspace_id};

const SCRATCH_DIRECTORY: &str = "scratch";
static WORKSPACE_ID_SEQUENCE: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, thiserror::Error)]
pub enum ScratchWorkspaceError {
    #[error("scratch workspace path is a symbolic link: {path}")]
    SymbolicLink { path: PathBuf },
    #[error("scratch workspace path is not a directory: {path}")]
    NotDirectory { path: PathBuf },
    #[error("scratch workspace I/O failed at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
}

/// Resolve the stable projectless workspace below the product-owned global root.
///
/// Empty workspaces are deliberately retained: the id is persistent adapter
/// state and the directory may later receive agent-created files. Reclamation
/// is therefore tied to an explicit preference reset, never to window close.
pub fn resolve_scratch_workspace(
    global_config_dir: &Path,
    preferences: &mut DesktopPreferences,
) -> Result<PathBuf, ScratchWorkspaceError> {
    let root = global_config_dir.join(SCRATCH_DIRECTORY);
    ensure_scratch_directory(&root)?;

    if preferences
        .scratch_workspace_id
        .as_deref()
        .is_some_and(|id| !valid_scratch_workspace_id(id))
    {
        preferences.scratch_workspace_id = None;
    }

    if let Some(id) = preferences.scratch_workspace_id.as_deref() {
        let workspace = root.join(id);
        ensure_scratch_directory(&workspace)?;
        return Ok(workspace);
    }

    loop {
        let id = generate_workspace_id();
        let workspace = root.join(&id);
        match fs::create_dir(&workspace) {
            Ok(()) => {
                preferences.scratch_workspace_id = Some(id);
                return Ok(workspace);
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(source) => {
                return Err(ScratchWorkspaceError::Io {
                    path: workspace,
                    source,
                });
            }
        }
    }
}

fn ensure_scratch_directory(path: &Path) -> Result<(), ScratchWorkspaceError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => {
            if metadata.file_type().is_symlink() {
                return Err(ScratchWorkspaceError::SymbolicLink {
                    path: path.to_path_buf(),
                });
            }
            if !metadata.is_dir() {
                return Err(ScratchWorkspaceError::NotDirectory {
                    path: path.to_path_buf(),
                });
            }
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            fs::create_dir_all(path).map_err(|source| ScratchWorkspaceError::Io {
                path: path.to_path_buf(),
                source,
            })?;
            let metadata =
                fs::symlink_metadata(path).map_err(|source| ScratchWorkspaceError::Io {
                    path: path.to_path_buf(),
                    source,
                })?;
            if metadata.file_type().is_symlink() {
                return Err(ScratchWorkspaceError::SymbolicLink {
                    path: path.to_path_buf(),
                });
            }
            if !metadata.is_dir() {
                return Err(ScratchWorkspaceError::NotDirectory {
                    path: path.to_path_buf(),
                });
            }
        }
        Err(source) => {
            return Err(ScratchWorkspaceError::Io {
                path: path.to_path_buf(),
                source,
            });
        }
    }
    Ok(())
}

fn generate_workspace_id() -> String {
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    let sequence = WORKSPACE_ID_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    format!(
        "workspace-{timestamp:x}-{:x}-{sequence:x}",
        std::process::id()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scratch_workspace_id_is_created_once_and_reused_from_preferences() {
        let temp = tempfile::tempdir().unwrap();
        let mut preferences = DesktopPreferences::default();

        let first = resolve_scratch_workspace(temp.path(), &mut preferences).unwrap();
        let id = preferences
            .scratch_workspace_id
            .clone()
            .expect("workspace resolution persists its id");
        let second = resolve_scratch_workspace(temp.path(), &mut preferences).unwrap();

        assert_eq!(first, temp.path().join("scratch").join(&id));
        assert_eq!(second, first);
        assert!(first.is_dir());
        assert!(valid_scratch_workspace_id(&id));
    }

    #[test]
    fn scratch_workspace_revalidates_untrusted_ids_at_the_path_boundary() {
        let temp = tempfile::tempdir().unwrap();
        let mut preferences = DesktopPreferences {
            scratch_workspace_id: Some("../escape".into()),
            ..DesktopPreferences::default()
        };

        let workspace = resolve_scratch_workspace(temp.path(), &mut preferences).unwrap();
        let id = preferences
            .scratch_workspace_id
            .as_deref()
            .expect("workspace resolver replaces an invalid id");
        assert!(valid_scratch_workspace_id(id));
        assert_eq!(workspace, temp.path().join(SCRATCH_DIRECTORY).join(id));
    }

    #[cfg(unix)]
    #[test]
    fn scratch_workspace_rejects_a_symbolic_link_root() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().unwrap();
        let external = tempfile::tempdir().unwrap();
        symlink(external.path(), temp.path().join(SCRATCH_DIRECTORY)).unwrap();
        let mut preferences = DesktopPreferences::default();

        assert!(matches!(
            resolve_scratch_workspace(temp.path(), &mut preferences),
            Err(ScratchWorkspaceError::SymbolicLink { .. })
        ));
    }
}
