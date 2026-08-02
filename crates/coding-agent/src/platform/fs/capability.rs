use cap_std::ambient_authority;
use cap_std::fs::{Dir, File, OpenOptions};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::fmt;
use std::io::ErrorKind;
use std::path::{Component, Path, PathBuf};
use std::time::Instant;

use crate::kernel::error::CodingSessionError;
use crate::mutex::MutexExt;
use std::sync::{Arc, Mutex};

const MAX_FILESYSTEM_BINDINGS: usize = 64;

#[derive(Clone)]
pub struct FilesystemCapability {
    pub(crate) cwd: PathBuf,
    root: Arc<Dir>,
    bindings: Arc<Mutex<HashMap<FilesystemInvocationKey, BoundFilesystemInvocation>>>,
}

#[derive(Clone)]
pub struct FilesystemTarget {
    relative: PathBuf,
    display: PathBuf,
    target_fingerprint: String,
    object: Option<FilesystemTargetObject>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct FilesystemInvocationKey {
    operation_id: String,
    tool_call_id: String,
}

struct BoundFilesystemInvocation {
    tool_name: String,
    request_path: PathBuf,
    target: FilesystemTarget,
    created_at: Instant,
}

#[derive(Clone)]
enum FilesystemTargetObject {
    File(Arc<Mutex<File>>),
    Directory(Arc<Dir>),
    Unavailable(String),
    Vacant {
        parent: Arc<Dir>,
        missing_parents: Vec<PathBuf>,
        leaf: PathBuf,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FilesystemPathPreview {
    pub(crate) display: PathBuf,
    pub(crate) workspace_local: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FilesystemReviewTargetError {
    OutsideProject,
    SymlinkDisallowed,
    NotFound,
    NotFile,
    TargetChanged,
    Inaccessible,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FilesystemBindingDescriptor {
    pub(crate) display: PathBuf,
    pub(crate) target_fingerprint: String,
}

fn lexically_normalize(path: &Path) -> PathBuf {
    let mut stack: Vec<Component<'_>> = Vec::new();
    for component in path.components() {
        match component {
            Component::ParentDir => match stack.last() {
                Some(Component::Normal(_)) => {
                    stack.pop();
                }
                _ => stack.push(component),
            },
            Component::CurDir => {}
            other => stack.push(other),
        }
    }
    let mut result = PathBuf::new();
    for component in stack {
        result.push(component.as_os_str());
    }
    result
}

fn filesystem_home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .filter(|path| !path.as_os_str().is_empty())
}

fn capability_error_detail(error: &CodingSessionError) -> String {
    let rendered = error.to_string();
    rendered
        .strip_prefix("unsupported capability: ")
        .unwrap_or(&rendered)
        .to_owned()
}

impl fmt::Debug for FilesystemCapability {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("FilesystemCapability")
            .field("cwd", &self.cwd)
            .field("root", &"<directory-handle>")
            .field(
                "bound_invocations",
                &self
                    .bindings
                    .lock_or_recover("filesystem target bindings")
                    .len(),
            )
            .finish()
    }
}

impl PartialEq for FilesystemCapability {
    fn eq(&self, other: &Self) -> bool {
        self.cwd == other.cwd
    }
}

impl Eq for FilesystemCapability {}

impl fmt::Debug for FilesystemTarget {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("FilesystemTarget")
            .field("relative", &self.relative)
            .field("display", &self.display)
            .field("target_fingerprint", &self.target_fingerprint)
            .field(
                "object",
                &match self.object {
                    Some(FilesystemTargetObject::File(_)) => "<file-handle>",
                    Some(FilesystemTargetObject::Directory(_)) => "<directory-handle>",
                    Some(FilesystemTargetObject::Unavailable(_)) => "<unavailable-target>",
                    Some(FilesystemTargetObject::Vacant { .. }) => {
                        "<parent-directory-handle-plus-vacant-leaf>"
                    }
                    None => "<unbound-preview>",
                },
            )
            .finish()
    }
}

impl FilesystemCapability {
    pub fn new(cwd: PathBuf) -> Result<Self, CodingSessionError> {
        let cwd = std::path::absolute(&cwd).map_err(|error| CodingSessionError::Resource {
            message: format!(
                "cannot make filesystem capability root absolute ({}): {error}",
                cwd.display()
            ),
        })?;
        let cwd = lexically_normalize(&cwd);
        let root = Dir::open_ambient_dir(&cwd, ambient_authority()).map_err(|error| {
            CodingSessionError::Resource {
                message: format!(
                    "cannot open filesystem capability root ({}): {error}",
                    cwd.display()
                ),
            }
        })?;
        Ok(Self {
            cwd,
            root: Arc::new(root),
            bindings: Arc::new(Mutex::new(HashMap::new())),
        })
    }

    pub(crate) fn target(
        &self,
        path: impl AsRef<Path>,
    ) -> Result<FilesystemTarget, CodingSessionError> {
        let requested = path.as_ref().to_string_lossy();
        let preview = self.preview_path(&requested)?;
        if !preview.workspace_local {
            return Err(CodingSessionError::UnsupportedCapability {
                capability: format!(
                    "filesystem path is outside the granted workspace root: {}",
                    preview.display.display()
                ),
            });
        }
        let relative = preview
            .display
            .strip_prefix(&self.cwd)
            .map(Path::to_path_buf)
            .map_err(|_| CodingSessionError::UnsupportedCapability {
                capability: format!(
                    "filesystem path is outside the granted workspace root: {}",
                    preview.display.display()
                ),
            })?;
        let relative = if relative.as_os_str().is_empty() {
            PathBuf::from(".")
        } else {
            relative
        };
        Ok(FilesystemTarget {
            display: preview.display,
            relative,
            target_fingerprint: "unbound-preview".into(),
            object: None,
        })
    }

    pub(crate) fn preview_path(
        &self,
        path: &str,
    ) -> Result<FilesystemPathPreview, CodingSessionError> {
        let requested = if path == "~" {
            filesystem_home_dir().ok_or_else(|| CodingSessionError::UnsupportedCapability {
                capability: "filesystem home path cannot be expanded".into(),
            })?
        } else if let Some(rest) = path.strip_prefix("~/") {
            filesystem_home_dir()
                .ok_or_else(|| CodingSessionError::UnsupportedCapability {
                    capability: "filesystem home path cannot be expanded".into(),
                })?
                .join(rest)
        } else if path.starts_with('~') {
            return Err(CodingSessionError::UnsupportedCapability {
                capability: format!("unsupported filesystem home expression: {path}"),
            });
        } else {
            let requested = Path::new(path);
            if requested.is_absolute() {
                requested.to_path_buf()
            } else {
                self.cwd.join(requested)
            }
        };
        let display = lexically_normalize(&requested);
        let workspace_local = display
            .strip_prefix(&self.cwd)
            .map(|relative| {
                !relative.components().any(|component| {
                    matches!(
                        component,
                        Component::ParentDir | Component::RootDir | Component::Prefix(_)
                    )
                })
            })
            .unwrap_or(false);
        Ok(FilesystemPathPreview {
            display,
            workspace_local,
        })
    }

    pub(crate) async fn bind_tool_target(
        &self,
        operation_id: &str,
        tool_call_id: &str,
        tool_name: &str,
        path: &str,
    ) -> Result<FilesystemBindingDescriptor, CodingSessionError> {
        let key = FilesystemInvocationKey {
            operation_id: operation_id.to_owned(),
            tool_call_id: tool_call_id.to_owned(),
        };
        {
            let bindings = self.bindings.lock_resource("filesystem target bindings")?;
            Self::ensure_binding_slot(&bindings, &key, tool_call_id)?;
        }
        let capability = self.clone();
        let tool_name_owned = tool_name.to_owned();
        let path_owned = path.to_owned();
        let target = tokio::task::spawn_blocking(move || {
            capability.prepare_target_blocking(&tool_name_owned, &path_owned)
        })
        .await
        .map_err(|error| CodingSessionError::Resource {
            message: format!("filesystem target binding task failed: {error}"),
        })??;
        let mut bindings = self.bindings.lock_resource("filesystem target bindings")?;
        Self::ensure_binding_slot(&bindings, &key, tool_call_id)?;
        let descriptor = FilesystemBindingDescriptor {
            display: target.display.clone(),
            target_fingerprint: target.target_fingerprint.clone(),
        };
        bindings.insert(
            key,
            BoundFilesystemInvocation {
                tool_name: tool_name.to_owned(),
                request_path: target.display.clone(),
                target,
                created_at: Instant::now(),
            },
        );
        Ok(descriptor)
    }

    fn ensure_binding_slot(
        bindings: &HashMap<FilesystemInvocationKey, BoundFilesystemInvocation>,
        key: &FilesystemInvocationKey,
        tool_call_id: &str,
    ) -> Result<(), CodingSessionError> {
        if bindings.contains_key(key) {
            return Err(CodingSessionError::Resource {
                message: format!("filesystem target is already bound for tool call {tool_call_id}"),
            });
        }
        if bindings.len() >= MAX_FILESYSTEM_BINDINGS {
            let oldest_age_ms = bindings
                .values()
                .map(|binding| binding.created_at.elapsed().as_millis())
                .max()
                .unwrap_or_default();
            return Err(CodingSessionError::Resource {
                message: format!(
                    "filesystem binding table capacity exceeded ({MAX_FILESYSTEM_BINDINGS} entries; oldest binding age {oldest_age_ms} ms); cancel or finish pending tool authorizations before retrying"
                ),
            });
        }
        Ok(())
    }

    pub(crate) fn take_bound_tool_target(
        &self,
        operation_id: &str,
        tool_call_id: &str,
        tool_name: &str,
        path: &str,
    ) -> Result<FilesystemTarget, CodingSessionError> {
        let expected = self.preview_path(path)?;
        let key = FilesystemInvocationKey {
            operation_id: operation_id.to_owned(),
            tool_call_id: tool_call_id.to_owned(),
        };
        let binding = self
            .bindings
            .lock_resource("filesystem target bindings")?
            .remove(&key)
            .ok_or_else(|| CodingSessionError::UnsupportedCapability {
                capability: format!(
                    "filesystem execution has no authorization-bound target for tool call {tool_call_id}"
                ),
            })?;
        if binding.tool_name != tool_name || binding.request_path != expected.display {
            return Err(CodingSessionError::UnsupportedCapability {
                capability: format!(
                    "filesystem execution target does not match the authorization-bound target for tool call {tool_call_id}"
                ),
            });
        }
        Ok(binding.target)
    }

    pub(crate) fn discard_bound_tool_target(&self, operation_id: &str, tool_call_id: &str) {
        let key = FilesystemInvocationKey {
            operation_id: operation_id.to_owned(),
            tool_call_id: tool_call_id.to_owned(),
        };
        self.bindings
            .lock_or_recover("filesystem target bindings")
            .remove(&key);
    }

    pub(crate) fn discard_operation_bindings(&self, operation_id: &str) {
        let mut bindings = self.bindings.lock_or_recover("filesystem target bindings");
        bindings.retain(|key, _| key.operation_id != operation_id);
    }

    #[cfg(test)]
    pub(crate) fn bound_len(&self) -> usize {
        self.bindings
            .lock_or_recover("test filesystem target bindings")
            .len()
    }

    pub(crate) async fn prepare_target_for_tool(
        &self,
        tool_name: &str,
        path: &str,
    ) -> Result<FilesystemTarget, CodingSessionError> {
        let capability = self.clone();
        let tool_name = tool_name.to_owned();
        let path = path.to_owned();
        tokio::task::spawn_blocking(move || capability.prepare_target_blocking(&tool_name, &path))
            .await
            .map_err(|error| CodingSessionError::Resource {
                message: format!("filesystem target preparation task failed: {error}"),
            })?
    }

    pub(crate) async fn prepare_workspace_review_target(
        &self,
        path: &str,
    ) -> Result<FilesystemTarget, FilesystemReviewTargetError> {
        let capability = self.clone();
        let path = path.to_owned();
        tokio::task::spawn_blocking(move || {
            capability.prepare_workspace_review_target_blocking(&path)
        })
        .await
        .map_err(|_| FilesystemReviewTargetError::Inaccessible)?
    }

    /// Reject any workspace-local target whose path resolves through a
    /// symbolic link. Missing components are allowed (write tools create
    /// leaves and parent directories on demand); anything that exists must
    /// not be a symlink, mirroring [`Self::prepare_workspace_review_target_blocking`].
    fn reject_workspace_symlink_components(
        &self,
        relative: &Path,
    ) -> Result<(), CodingSessionError> {
        let mut parent = self
            .root
            .try_clone()
            .map_err(|error| CodingSessionError::Resource {
                message: format!("cannot clone workspace root handle: {error}"),
            })?;
        let components = relative
            .components()
            .filter_map(|component| match component {
                Component::Normal(name) => Some(PathBuf::from(name)),
                _ => None,
            })
            .collect::<Vec<_>>();
        for (index, component) in components.iter().enumerate() {
            let is_leaf = index + 1 == components.len();
            let metadata = match parent.symlink_metadata(component) {
                Ok(metadata) => metadata,
                Err(error) if error.kind() == ErrorKind::NotFound => continue,
                Err(error) => {
                    return Err(CodingSessionError::UnsupportedCapability {
                        capability: format!(
                            "cannot inspect filesystem target {}: {error}",
                            component.display()
                        ),
                    });
                }
            };
            if metadata.file_type().is_symlink() {
                return Err(CodingSessionError::UnsupportedCapability {
                    capability: format!(
                        "filesystem target resolves through a symbolic link: {}",
                        component.display()
                    ),
                });
            }
            if !is_leaf {
                if !metadata.is_dir() {
                    return Err(CodingSessionError::UnsupportedCapability {
                        capability: format!(
                            "filesystem target parent is not a directory: {}",
                            component.display()
                        ),
                    });
                }
                parent = parent.open_dir(component).map_err(|error| {
                    CodingSessionError::UnsupportedCapability {
                        capability: format!(
                            "cannot open filesystem target parent {}: {error}",
                            component.display()
                        ),
                    }
                })?;
            }
        }
        Ok(())
    }

    fn prepare_workspace_review_target_blocking(
        &self,
        path: &str,
    ) -> Result<FilesystemTarget, FilesystemReviewTargetError> {
        let mut target = self
            .target(path)
            .map_err(|_| FilesystemReviewTargetError::OutsideProject)?;
        let ambient_root = std::fs::symlink_metadata(&self.cwd).map_err(review_metadata_error)?;
        if ambient_root.file_type().is_symlink() {
            return Err(FilesystemReviewTargetError::SymlinkDisallowed);
        }
        let opened_root = self.root.dir_metadata().map_err(review_metadata_error)?;
        if ambient_metadata_identity(&ambient_root) != metadata_identity(&opened_root) {
            return Err(FilesystemReviewTargetError::TargetChanged);
        }

        let components = target
            .relative
            .components()
            .filter_map(|component| match component {
                Component::Normal(name) => Some(PathBuf::from(name)),
                _ => None,
            })
            .collect::<Vec<_>>();
        if components.is_empty() {
            return Err(FilesystemReviewTargetError::NotFile);
        }

        let mut parent = self
            .root
            .try_clone()
            .map_err(|_| FilesystemReviewTargetError::Inaccessible)?;
        for component in &components[..components.len() - 1] {
            let metadata = parent
                .symlink_metadata(component)
                .map_err(review_metadata_error)?;
            if metadata.file_type().is_symlink() {
                return Err(FilesystemReviewTargetError::SymlinkDisallowed);
            }
            if !metadata.is_dir() {
                return Err(FilesystemReviewTargetError::NotFile);
            }
            let directory = parent.open_dir(component).map_err(review_metadata_error)?;
            let opened = directory.dir_metadata().map_err(review_metadata_error)?;
            if metadata_identity(&metadata) != metadata_identity(&opened) {
                return Err(FilesystemReviewTargetError::TargetChanged);
            }
            parent = directory;
        }

        let leaf = components
            .last()
            .expect("non-empty review path components were checked");
        let metadata = parent
            .symlink_metadata(leaf)
            .map_err(review_metadata_error)?;
        if metadata.file_type().is_symlink() {
            return Err(FilesystemReviewTargetError::SymlinkDisallowed);
        }
        if !metadata.is_file() {
            return Err(FilesystemReviewTargetError::NotFile);
        }
        let mut options = OpenOptions::new();
        options.read(true);
        let file = parent
            .open_with(leaf, &options)
            .map_err(review_metadata_error)?;
        let opened = file.metadata().map_err(review_metadata_error)?;
        if metadata_identity(&metadata) != metadata_identity(&opened) {
            return Err(FilesystemReviewTargetError::TargetChanged);
        }

        let object = FilesystemTargetObject::File(Arc::new(Mutex::new(file)));
        target.target_fingerprint = target_object_fingerprint(&object)
            .map_err(|_| FilesystemReviewTargetError::Inaccessible)?;
        target.object = Some(object);
        Ok(target)
    }

    fn prepare_target_blocking(
        &self,
        tool_name: &str,
        path: &str,
    ) -> Result<FilesystemTarget, CodingSessionError> {
        let preview = self.preview_path(path)?;
        let mut target = if preview.workspace_local {
            let target = self.target(path)?;
            // cap-std opens follow symbolic links by default, so a symlink
            // inside the workspace (e.g. node_modules pointing at ~/.aws or a
            // checked-in link to /etc) would let read/grep/write escape the
            // granted root. The review path already rejects symlinks
            // component-by-component; apply the same check before the main
            // tool path opens anything.
            self.reject_workspace_symlink_components(target.relative_path())?;
            target
        } else {
            FilesystemTarget {
                relative: preview
                    .display
                    .file_name()
                    .map(PathBuf::from)
                    .unwrap_or_else(|| PathBuf::from(".")),
                display: preview.display,
                target_fingerprint: "unbound-preview".into(),
                object: None,
            }
        };
        let workspace_local = preview.workspace_local;
        let object = match tool_name {
            "read" => self
                .open_target_file(&target, workspace_local, true, false)
                .map(|file| FilesystemTargetObject::File(Arc::new(Mutex::new(file))))
                .unwrap_or_else(|error| {
                    FilesystemTargetObject::Unavailable(format!(
                        "read: cannot open {}: {}",
                        target.display.display(),
                        capability_error_detail(&error)
                    ))
                }),
            "edit" => self
                .open_target_file(&target, workspace_local, true, true)
                .map(|file| FilesystemTargetObject::File(Arc::new(Mutex::new(file))))
                .unwrap_or_else(|error| {
                    FilesystemTargetObject::Unavailable(format!(
                        "edit: cannot open {}: {}",
                        target.display.display(),
                        capability_error_detail(&error)
                    ))
                }),
            "grep" => match self.open_target_directory(&target, workspace_local) {
                Ok(directory) => FilesystemTargetObject::Directory(Arc::new(directory)),
                Err(_) => self
                    .open_target_file(&target, workspace_local, true, false)
                    .map(|file| FilesystemTargetObject::File(Arc::new(Mutex::new(file))))
                    .unwrap_or_else(|error| {
                        FilesystemTargetObject::Unavailable(format!(
                            "grep: cannot open {}: {}",
                            target.display.display(),
                            capability_error_detail(&error)
                        ))
                    }),
            },
            "find" | "ls" => self
                .open_target_directory(&target, workspace_local)
                .map(|directory| FilesystemTargetObject::Directory(Arc::new(directory)))
                .unwrap_or_else(|error| {
                    let message = if workspace_local {
                        match self.root.metadata(&target.relative) {
                            Ok(_) => format!(
                                "{tool_name}: not a directory: {}",
                                target.display.display()
                            ),
                            Err(metadata_error) if metadata_error.kind() == ErrorKind::NotFound => {
                                format!("{tool_name}: path not found: {}", target.display.display())
                            }
                            Err(_) => format!(
                                "{tool_name}: cannot open directory {}: {error}",
                                target.display.display()
                            ),
                        }
                    } else {
                        format!(
                            "{tool_name}: cannot open directory {}: {error}",
                            target.display.display()
                        )
                    };
                    FilesystemTargetObject::Unavailable(message)
                }),
            "write" => {
                if workspace_local {
                    self.prepare_write_target(&target)?
                } else {
                    self.prepare_external_write_target(&target)?
                }
            }
            _ => {
                return Err(CodingSessionError::UnsupportedCapability {
                    capability: format!(
                        "tool `{tool_name}` has no filesystem target binding contract"
                    ),
                });
            }
        };
        target.target_fingerprint = target_object_fingerprint(&object)?;
        target.object = Some(object);
        Ok(target)
    }

    fn open_target_file(
        &self,
        target: &FilesystemTarget,
        workspace_local: bool,
        read: bool,
        write: bool,
    ) -> Result<File, CodingSessionError> {
        let mut options = OpenOptions::new();
        options.read(read).write(write);
        let result = if workspace_local {
            self.root.open_with(&target.relative, &options)
        } else {
            let (parent, leaf) = ambient_parent_and_leaf(&target.display)?;
            parent.open_with(&leaf, &options)
        };
        result.map_err(|error| CodingSessionError::UnsupportedCapability {
            capability: format!(
                "cannot open file through the granted filesystem authority ({}): {error}",
                target.display.display()
            ),
        })
    }

    fn open_target_directory(
        &self,
        target: &FilesystemTarget,
        workspace_local: bool,
    ) -> Result<Dir, CodingSessionError> {
        let result = if workspace_local {
            self.root.open_dir(&target.relative)
        } else {
            Dir::open_ambient_dir(&target.display, ambient_authority())
        };
        result.map_err(|error| CodingSessionError::UnsupportedCapability {
            capability: format!(
                "cannot open directory through the granted filesystem authority ({}): {error}",
                target.display.display()
            ),
        })
    }

    fn prepare_external_write_target(
        &self,
        target: &FilesystemTarget,
    ) -> Result<FilesystemTargetObject, CodingSessionError> {
        let (parent, missing_parents, leaf) = ambient_write_parent_and_leaf(&target.display)?;
        prepare_write_leaf(Arc::new(parent), missing_parents, leaf, target)
    }

    fn prepare_write_target(
        &self,
        target: &FilesystemTarget,
    ) -> Result<FilesystemTargetObject, CodingSessionError> {
        let leaf = target
            .relative
            .file_name()
            .map(PathBuf::from)
            .ok_or_else(|| CodingSessionError::UnsupportedCapability {
                capability: format!(
                    "write target must name a file beneath the granted root: {}",
                    target.display.display()
                ),
            })?;
        if leaf == Path::new(".") {
            return Err(CodingSessionError::UnsupportedCapability {
                capability: "write target cannot be the granted workspace directory".into(),
            });
        }
        let parent_relative = target
            .relative
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        let mut parent =
            Arc::new(
                self.root
                    .try_clone()
                    .map_err(|error| CodingSessionError::Resource {
                        message: format!("write: cannot clone workspace root handle: {error}"),
                    })?,
            );
        let mut missing_parents = Vec::new();
        for component in parent_relative.components() {
            let Component::Normal(name) = component else {
                continue;
            };
            let name = PathBuf::from(name);
            if !missing_parents.is_empty() {
                missing_parents.push(name);
                continue;
            }
            match parent.open_dir(&name) {
                Ok(directory) => parent = Arc::new(directory),
                Err(error) if error.kind() == ErrorKind::NotFound => {
                    missing_parents.push(name);
                }
                Err(error) => {
                    return Err(CodingSessionError::UnsupportedCapability {
                        capability: format!(
                            "write: cannot freeze parent directory for {}: {error}",
                            target.display.display()
                        ),
                    });
                }
            }
        }
        prepare_write_leaf(parent, missing_parents, leaf, target)
    }
}

mod path;

use path::*;

impl FilesystemTarget {
    pub(crate) fn relative_path(&self) -> &Path {
        &self.relative
    }

    pub(crate) fn display_path(&self) -> &Path {
        &self.display
    }

    pub(crate) fn target_fingerprint(&self) -> &str {
        &self.target_fingerprint
    }

    pub(crate) fn opened_file(&self) -> Result<Arc<Mutex<File>>, String> {
        match self.object.as_ref() {
            Some(FilesystemTargetObject::File(file)) => Ok(file.clone()),
            Some(FilesystemTargetObject::Unavailable(error)) => Err(error.clone()),
            _ => Err(format!(
                "filesystem target is not bound to an opened file: {}",
                self.display.display()
            )),
        }
    }

    pub(crate) fn opened_directory(&self) -> Result<Arc<Dir>, String> {
        match self.object.as_ref() {
            Some(FilesystemTargetObject::Directory(directory)) => Ok(directory.clone()),
            Some(FilesystemTargetObject::Unavailable(error)) => Err(error.clone()),
            _ => Err(format!(
                "filesystem target is not bound to an opened directory: {}",
                self.display.display()
            )),
        }
    }

    pub(crate) fn create_vacant_file(&self) -> Result<File, String> {
        match self.object.as_ref() {
            Some(FilesystemTargetObject::Vacant {
                parent,
                missing_parents,
                leaf,
            }) => {
                let mut current = parent.clone();
                for component in missing_parents {
                    current.create_dir(component).map_err(|error| {
                        format!(
                            "write: failed to create authorization-bound parent {} for {}: {error}",
                            component.display(),
                            self.display.display()
                        )
                    })?;
                    let created = current.symlink_metadata(component).map_err(|error| {
                        format!(
                            "write: cannot fingerprint created parent {} for {}: {error}",
                            component.display(),
                            self.display.display()
                        )
                    })?;
                    let child = current.open_dir(component).map_err(|error| {
                        format!(
                            "write: cannot open created parent {} for {}: {error}",
                            component.display(),
                            self.display.display()
                        )
                    })?;
                    let opened = child.dir_metadata().map_err(|error| {
                        format!(
                            "write: cannot fingerprint opened parent {} for {}: {error}",
                            component.display(),
                            self.display.display()
                        )
                    })?;
                    if metadata_identity(&created) != metadata_identity(&opened) {
                        return Err(format!(
                            "write: created parent identity changed before it was opened: {}",
                            self.display.display()
                        ));
                    }
                    current = Arc::new(child);
                }
                let mut options = OpenOptions::new();
                options.write(true).create_new(true);
                current.open_with(leaf, &options).map_err(|error| {
                    format!(
                        "write: failed to create authorization-bound target {}: {error}",
                        self.display.display()
                    )
                })
            }
            _ => Err(format!(
                "filesystem target is not bound to a vacant leaf: {}",
                self.display.display()
            )),
        }
    }

    pub(crate) fn is_vacant(&self) -> bool {
        matches!(self.object, Some(FilesystemTargetObject::Vacant { .. }))
    }
}

#[cfg(test)]
mod tests_file;
