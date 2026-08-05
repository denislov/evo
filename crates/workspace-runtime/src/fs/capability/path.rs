use super::*;

pub(super) fn ambient_parent_and_leaf(path: &Path) -> Result<(Dir, PathBuf), WorkspaceError> {
    let leaf = path.file_name().map(PathBuf::from).ok_or_else(|| {
        WorkspaceError::UnsupportedCapability {
            capability: format!("filesystem target must name an entry: {}", path.display()),
        }
    })?;
    let parent_path = path
        .parent()
        .ok_or_else(|| WorkspaceError::UnsupportedCapability {
            capability: format!("filesystem target has no parent: {}", path.display()),
        })?;
    let parent = Dir::open_ambient_dir(parent_path, ambient_authority()).map_err(|error| {
        WorkspaceError::UnsupportedCapability {
            capability: format!(
                "cannot open explicitly authorized external parent ({}): {error}",
                parent_path.display()
            ),
        }
    })?;
    Ok((parent, leaf))
}

pub(super) fn ambient_write_parent_and_leaf(
    path: &Path,
) -> Result<(Dir, Vec<PathBuf>, PathBuf), WorkspaceError> {
    let leaf = path.file_name().map(PathBuf::from).ok_or_else(|| {
        WorkspaceError::UnsupportedCapability {
            capability: format!("filesystem target must name an entry: {}", path.display()),
        }
    })?;
    let mut cursor = path
        .parent()
        .ok_or_else(|| WorkspaceError::UnsupportedCapability {
            capability: format!("filesystem target has no parent: {}", path.display()),
        })?
        .to_path_buf();
    let mut missing_reversed = Vec::new();
    loop {
        match Dir::open_ambient_dir(&cursor, ambient_authority()) {
            Ok(parent) => {
                missing_reversed.reverse();
                return Ok((parent, missing_reversed, leaf));
            }
            Err(error) if error.kind() == ErrorKind::NotFound => {
                let name = cursor.file_name().map(PathBuf::from).ok_or_else(|| {
                    WorkspaceError::UnsupportedCapability {
                        capability: format!(
                            "cannot find an existing external ancestor for {}",
                            path.display()
                        ),
                    }
                })?;
                missing_reversed.push(name);
                cursor = cursor
                    .parent()
                    .ok_or_else(|| WorkspaceError::UnsupportedCapability {
                        capability: format!(
                            "cannot find an existing external ancestor for {}",
                            path.display()
                        ),
                    })?
                    .to_path_buf();
            }
            Err(error) => {
                return Err(WorkspaceError::UnsupportedCapability {
                    capability: format!(
                        "cannot open explicitly authorized external parent ({}): {error}",
                        cursor.display()
                    ),
                });
            }
        }
    }
}

pub(super) fn prepare_write_leaf(
    parent: Arc<Dir>,
    missing_parents: Vec<PathBuf>,
    leaf: PathBuf,
    target: &FilesystemTarget,
) -> Result<FilesystemTargetObject, WorkspaceError> {
    if !missing_parents.is_empty() {
        return Ok(FilesystemTargetObject::Vacant {
            parent,
            missing_parents,
            leaf,
        });
    }
    let mut options = OpenOptions::new();
    // Mutation fences need to inspect the pre-write revision through the same
    // capability-bound handle before truncating the file.
    options.read(true).write(true);
    match parent.open_with(&leaf, &options) {
        Ok(file) => Ok(FilesystemTargetObject::File(Arc::new(Mutex::new(file)))),
        Err(error) if error.kind() == ErrorKind::NotFound => match parent.symlink_metadata(&leaf) {
            Err(metadata_error) if metadata_error.kind() == ErrorKind::NotFound => {
                Ok(FilesystemTargetObject::Vacant {
                    parent,
                    missing_parents: Vec::new(),
                    leaf,
                })
            }
            Ok(_) => Err(WorkspaceError::UnsupportedCapability {
                capability: format!(
                    "write target exists but cannot be opened safely: {}",
                    target.display.display()
                ),
            }),
            Err(metadata_error) => Err(WorkspaceError::UnsupportedCapability {
                capability: format!(
                    "write target cannot be inspected safely ({}): {metadata_error}",
                    target.display.display()
                ),
            }),
        },
        Err(error) => Err(WorkspaceError::UnsupportedCapability {
            capability: format!(
                "write target cannot be opened safely ({}): {error}",
                target.display.display()
            ),
        }),
    }
}

pub(super) fn target_object_fingerprint(
    object: &FilesystemTargetObject,
) -> Result<String, WorkspaceError> {
    match object {
        FilesystemTargetObject::File(file) => {
            let file = lock_resource(file, "filesystem target file")?;
            file.metadata()
                .map(|metadata| audit_identity_fingerprint(&metadata_identity(&metadata)))
                .map_err(|error| WorkspaceError::Resource {
                    message: format!("cannot fingerprint opened filesystem file: {error}"),
                })
        }
        FilesystemTargetObject::Directory(directory) => directory
            .dir_metadata()
            .map(|metadata| audit_identity_fingerprint(&metadata_identity(&metadata)))
            .map_err(|error| WorkspaceError::Resource {
                message: format!("cannot fingerprint opened filesystem directory: {error}"),
            }),
        FilesystemTargetObject::Vacant {
            parent,
            missing_parents,
            leaf,
        } => parent
            .dir_metadata()
            .map(|metadata| {
                audit_identity_fingerprint(&format!(
                    "{}:vacant:{}:{}",
                    metadata_identity(&metadata),
                    missing_parents
                        .iter()
                        .map(|part| part.to_string_lossy())
                        .collect::<Vec<_>>()
                        .join("/"),
                    leaf.to_string_lossy()
                ))
            })
            .map_err(|error| WorkspaceError::Resource {
                message: format!("cannot fingerprint filesystem creation parent: {error}"),
            }),
        FilesystemTargetObject::Unavailable(_) => Ok("unavailable".into()),
    }
}

pub(super) fn audit_identity_fingerprint(identity: &str) -> String {
    format!("{:x}", Sha256::digest(identity.as_bytes()))
}

pub(super) fn remove_bound_file(target: &FilesystemTarget) -> Result<(), String> {
    if !matches!(target.object, Some(FilesystemTargetObject::File(_))) {
        return Err(format!(
            "filesystem target is not an opened file: {}",
            target.display.display()
        ));
    }
    if let Some(root) = &target.root {
        let metadata = root
            .symlink_metadata(&target.relative)
            .map_err(|error| error.to_string())?;
        let fingerprint = audit_identity_fingerprint(&metadata_identity(&metadata));
        if fingerprint != target.target_fingerprint {
            return Err(format!(
                "filesystem target identity changed before deletion: {}",
                target.display.display()
            ));
        }
        root.remove_file(&target.relative)
            .map_err(|error| error.to_string())
    } else {
        let (parent, leaf) =
            ambient_parent_and_leaf(&target.display).map_err(|error| error.to_string())?;
        let metadata = parent
            .symlink_metadata(&leaf)
            .map_err(|error| error.to_string())?;
        let fingerprint = audit_identity_fingerprint(&metadata_identity(&metadata));
        if fingerprint != target.target_fingerprint {
            return Err(format!(
                "filesystem target identity changed before deletion: {}",
                target.display.display()
            ));
        }
        parent.remove_file(&leaf).map_err(|error| error.to_string())
    }
}

pub(super) fn review_metadata_error(error: std::io::Error) -> FilesystemReviewTargetError {
    match error.kind() {
        ErrorKind::NotFound => FilesystemReviewTargetError::NotFound,
        _ => FilesystemReviewTargetError::Inaccessible,
    }
}

#[cfg(unix)]
pub(super) fn metadata_identity(metadata: &cap_std::fs::Metadata) -> String {
    use cap_std::fs::MetadataExt;
    format!("unix:{}:{}", metadata.dev(), metadata.ino())
}

#[cfg(windows)]
pub(super) fn metadata_identity(metadata: &cap_std::fs::Metadata) -> String {
    use cap_std::fs::MetadataExt;
    format!(
        "windows:{:?}:{:?}",
        metadata.volume_serial_number(),
        metadata.file_index()
    )
}

#[cfg(not(any(unix, windows)))]
pub(super) fn metadata_identity(metadata: &cap_std::fs::Metadata) -> String {
    format!("portable:{}:{:?}", metadata.len(), metadata.modified().ok())
}

#[cfg(unix)]
pub(super) fn ambient_metadata_identity(metadata: &std::fs::Metadata) -> String {
    use std::os::unix::fs::MetadataExt;
    format!("unix:{}:{}", metadata.dev(), metadata.ino())
}

#[cfg(windows)]
pub(super) fn ambient_metadata_identity(metadata: &std::fs::Metadata) -> String {
    use std::os::windows::fs::MetadataExt;
    format!(
        "windows:{:?}:{:?}",
        metadata.volume_serial_number(),
        metadata.file_index()
    )
}

#[cfg(not(any(unix, windows)))]
pub(super) fn ambient_metadata_identity(metadata: &std::fs::Metadata) -> String {
    format!("portable:{}:{:?}", metadata.len(), metadata.modified().ok())
}
