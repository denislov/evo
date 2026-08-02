use super::*;

pub(super) fn ambient_parent_and_leaf(path: &Path) -> Result<(Dir, PathBuf), CodingSessionError> {
    let leaf = path.file_name().map(PathBuf::from).ok_or_else(|| {
        CodingSessionError::UnsupportedCapability {
            capability: format!("filesystem target must name an entry: {}", path.display()),
        }
    })?;
    let parent_path = path
        .parent()
        .ok_or_else(|| CodingSessionError::UnsupportedCapability {
            capability: format!("filesystem target has no parent: {}", path.display()),
        })?;
    let parent = Dir::open_ambient_dir(parent_path, ambient_authority()).map_err(|error| {
        CodingSessionError::UnsupportedCapability {
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
) -> Result<(Dir, Vec<PathBuf>, PathBuf), CodingSessionError> {
    let leaf = path.file_name().map(PathBuf::from).ok_or_else(|| {
        CodingSessionError::UnsupportedCapability {
            capability: format!("filesystem target must name an entry: {}", path.display()),
        }
    })?;
    let mut cursor = path
        .parent()
        .ok_or_else(|| CodingSessionError::UnsupportedCapability {
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
                    CodingSessionError::UnsupportedCapability {
                        capability: format!(
                            "cannot find an existing external ancestor for {}",
                            path.display()
                        ),
                    }
                })?;
                missing_reversed.push(name);
                cursor = cursor
                    .parent()
                    .ok_or_else(|| CodingSessionError::UnsupportedCapability {
                        capability: format!(
                            "cannot find an existing external ancestor for {}",
                            path.display()
                        ),
                    })?
                    .to_path_buf();
            }
            Err(error) => {
                return Err(CodingSessionError::UnsupportedCapability {
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
) -> Result<FilesystemTargetObject, CodingSessionError> {
    if !missing_parents.is_empty() {
        return Ok(FilesystemTargetObject::Vacant {
            parent,
            missing_parents,
            leaf,
        });
    }
    let mut options = OpenOptions::new();
    options.write(true);
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
            Ok(_) => Err(CodingSessionError::UnsupportedCapability {
                capability: format!(
                    "write target exists but cannot be opened safely: {}",
                    target.display.display()
                ),
            }),
            Err(metadata_error) => Err(CodingSessionError::UnsupportedCapability {
                capability: format!(
                    "write target cannot be inspected safely ({}): {metadata_error}",
                    target.display.display()
                ),
            }),
        },
        Err(error) => Err(CodingSessionError::UnsupportedCapability {
            capability: format!(
                "write target cannot be opened safely ({}): {error}",
                target.display.display()
            ),
        }),
    }
}

pub(super) fn target_object_fingerprint(
    object: &FilesystemTargetObject,
) -> Result<String, CodingSessionError> {
    match object {
        FilesystemTargetObject::File(file) => {
            let file = file.lock_resource("filesystem target file")?;
            file.metadata()
                .map(|metadata| audit_identity_fingerprint(&metadata_identity(&metadata)))
                .map_err(|error| CodingSessionError::Resource {
                    message: format!("cannot fingerprint opened filesystem file: {error}"),
                })
        }
        FilesystemTargetObject::Directory(directory) => directory
            .dir_metadata()
            .map(|metadata| audit_identity_fingerprint(&metadata_identity(&metadata)))
            .map_err(|error| CodingSessionError::Resource {
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
            .map_err(|error| CodingSessionError::Resource {
                message: format!("cannot fingerprint filesystem creation parent: {error}"),
            }),
        FilesystemTargetObject::Unavailable(_) => Ok("unavailable".into()),
    }
}

pub(super) fn audit_identity_fingerprint(identity: &str) -> String {
    format!("{:x}", Sha256::digest(identity.as_bytes()))
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
