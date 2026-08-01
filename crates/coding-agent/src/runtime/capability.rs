use cap_std::ambient_authority;
use cap_std::fs::{Dir, File, OpenOptions};
use sha2::{Digest, Sha256};
use std::collections::{BTreeSet, HashMap};
use std::fmt;
use std::io::ErrorKind;
use std::path::{Component, Path, PathBuf};

use super::operation::control::OperationKind;
use super::snapshot::SnapshotCoordinator;
use crate::profiles::ProfileId;
use crate::runtime::facade::CodingSessionError;
use crate::session::event::PersistedRuntimeGenerationRef;
use std::sync::{Arc, Mutex};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct CapabilityGeneration(u64);

impl CapabilityGeneration {
    pub(crate) fn new(value: u64) -> Self {
        Self(value.max(1))
    }

    pub(crate) fn get(self) -> u64 {
        self.0
    }

    pub(crate) fn next(self) -> Result<Self, CodingSessionError> {
        self.0
            .checked_add(1)
            .map(Self)
            .ok_or_else(|| CodingSessionError::UnsupportedCapability {
                capability: "capability generation is exhausted".into(),
            })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ActorId {
    Client,
    ChildOperation(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ModelCapability {
    pub(crate) profile_id: Option<ProfileId>,
}

impl ModelCapability {
    pub(crate) fn require<'a>(
        value: Option<&'a ModelCapability>,
        runtime_profile_id: Option<&ProfileId>,
    ) -> Result<&'a ModelCapability, CodingSessionError> {
        let capability = value.ok_or_else(|| CodingSessionError::UnsupportedCapability {
            capability: "model capability is not granted".into(),
        })?;
        if capability.profile_id.as_ref() != runtime_profile_id {
            return Err(CodingSessionError::UnsupportedCapability {
                capability: format!(
                    "model capability profile mismatch: granted={}, runtime={}",
                    capability
                        .profile_id
                        .as_ref()
                        .map(ProfileId::as_str)
                        .unwrap_or("<none>"),
                    runtime_profile_id
                        .map(ProfileId::as_str)
                        .unwrap_or("<none>")
                ),
            });
        }
        Ok(capability)
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct ToolCapabilitySet {
    allow_all: bool,
    allowed: BTreeSet<String>,
}

impl ToolCapabilitySet {
    pub(crate) fn from_names(names: impl IntoIterator<Item = String>) -> Self {
        Self {
            allow_all: false,
            allowed: names.into_iter().collect(),
        }
    }

    pub(crate) fn allows(&self, name: &str) -> bool {
        self.allow_all || self.allowed.contains(name)
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct CommandCapabilitySet {
    allowed: BTreeSet<String>,
}

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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShellCapability {
    pub(crate) cwd: PathBuf,
    pub(crate) shell_path: Option<String>,
    pub(crate) command_prefix: Option<String>,
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
                    .lock()
                    .map(|bindings| bindings.len())
                    .unwrap_or_default(),
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

pub(crate) fn tool_uses_filesystem(name: &str) -> bool {
    matches!(name, "read" | "write" | "edit" | "grep" | "find" | "ls")
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
        let key = FilesystemInvocationKey {
            operation_id: operation_id.to_owned(),
            tool_call_id: tool_call_id.to_owned(),
        };
        let mut bindings = self
            .bindings
            .lock()
            .map_err(|_| CodingSessionError::Resource {
                message: "filesystem target binding lock is poisoned".into(),
            })?;
        if bindings.contains_key(&key) {
            return Err(CodingSessionError::Resource {
                message: format!("filesystem target is already bound for tool call {tool_call_id}"),
            });
        }
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
            },
        );
        Ok(descriptor)
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
            .lock()
            .map_err(|_| CodingSessionError::Resource {
                message: "filesystem target binding lock is poisoned".into(),
            })?
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
        if let Ok(mut bindings) = self.bindings.lock() {
            bindings.remove(&key);
        }
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

fn ambient_parent_and_leaf(path: &Path) -> Result<(Dir, PathBuf), CodingSessionError> {
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

fn ambient_write_parent_and_leaf(
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

fn prepare_write_leaf(
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

fn target_object_fingerprint(
    object: &FilesystemTargetObject,
) -> Result<String, CodingSessionError> {
    match object {
        FilesystemTargetObject::File(file) => {
            let file = file.lock().map_err(|_| CodingSessionError::Resource {
                message: "filesystem target file lock is poisoned".into(),
            })?;
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

fn audit_identity_fingerprint(identity: &str) -> String {
    format!("{:x}", Sha256::digest(identity.as_bytes()))
}

fn review_metadata_error(error: std::io::Error) -> FilesystemReviewTargetError {
    match error.kind() {
        ErrorKind::NotFound => FilesystemReviewTargetError::NotFound,
        _ => FilesystemReviewTargetError::Inaccessible,
    }
}

#[cfg(unix)]
fn metadata_identity(metadata: &cap_std::fs::Metadata) -> String {
    use cap_std::fs::MetadataExt;
    format!("unix:{}:{}", metadata.dev(), metadata.ino())
}

#[cfg(windows)]
fn metadata_identity(metadata: &cap_std::fs::Metadata) -> String {
    use cap_std::fs::MetadataExt;
    format!(
        "windows:{:?}:{:?}",
        metadata.volume_serial_number(),
        metadata.file_index()
    )
}

#[cfg(not(any(unix, windows)))]
fn metadata_identity(metadata: &cap_std::fs::Metadata) -> String {
    format!("portable:{}:{:?}", metadata.len(), metadata.modified().ok())
}

#[cfg(unix)]
fn ambient_metadata_identity(metadata: &std::fs::Metadata) -> String {
    use std::os::unix::fs::MetadataExt;
    format!("unix:{}:{}", metadata.dev(), metadata.ino())
}

#[cfg(windows)]
fn ambient_metadata_identity(metadata: &std::fs::Metadata) -> String {
    use std::os::windows::fs::MetadataExt;
    format!(
        "windows:{:?}:{:?}",
        metadata.volume_serial_number(),
        metadata.file_index()
    )
}

#[cfg(not(any(unix, windows)))]
fn ambient_metadata_identity(metadata: &std::fs::Metadata) -> String {
    format!("portable:{}:{:?}", metadata.len(), metadata.modified().ok())
}

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

impl ShellCapability {
    pub fn new(cwd: PathBuf) -> Self {
        Self {
            cwd,
            shell_path: None,
            command_prefix: None,
        }
    }

    pub(crate) fn with_configuration(
        cwd: PathBuf,
        shell_path: Option<String>,
        command_prefix: Option<String>,
    ) -> Self {
        Self {
            cwd,
            shell_path,
            command_prefix,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SessionReadCapability {
    pub(crate) persistent: bool,
}

impl SessionReadCapability {
    pub(crate) fn require(
        value: Option<&SessionReadCapability>,
    ) -> Result<&SessionReadCapability, CodingSessionError> {
        value.ok_or_else(|| CodingSessionError::UnsupportedCapability {
            capability: "session read capability is not granted".into(),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SessionWriteCapability {
    pub(crate) persistent: bool,
}

impl SessionWriteCapability {
    pub(crate) fn require(
        value: Option<&SessionWriteCapability>,
    ) -> Result<&SessionWriteCapability, CodingSessionError> {
        value.ok_or_else(|| CodingSessionError::UnsupportedCapability {
            capability: "session write capability is not granted".into(),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct UiCapability;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct OperationCapabilitySnapshot {
    pub(crate) generation: CapabilityGeneration,
    pub(crate) operation_id: String,
    pub(crate) actor: ActorId,
    pub(crate) model: Option<ModelCapability>,
    pub(crate) tools: ToolCapabilitySet,
    pub(crate) commands: CommandCapabilitySet,
    pub(crate) filesystem: Option<FilesystemCapability>,
    pub(crate) shell: Option<ShellCapability>,
    pub(crate) session_read: Option<SessionReadCapability>,
    pub(crate) session_write: Option<SessionWriteCapability>,
    pub(crate) ui: Option<UiCapability>,
}

impl OperationCapabilitySnapshot {
    pub(crate) fn persisted_runtime_generation_ref(&self) -> PersistedRuntimeGenerationRef {
        PersistedRuntimeGenerationRef {
            profile_id: self
                .model
                .as_ref()
                .and_then(|model| model.profile_id.clone()),
            capability_generation: Some(self.generation.get()),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CapabilityRevocationPolicy {
    RequestCancelOlderOperations,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct InstalledCapabilityGeneration {
    pub(crate) generation: CapabilityGeneration,
    pub(crate) revocation: CapabilityRevocationPolicy,
    pub(crate) cancellation_requested_operation_ids: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CapabilitySnapshotInput {
    pub(crate) operation_id: String,
    pub(crate) operation_kind: OperationKind,
    pub(crate) session_access: SessionCapabilityAccess,
    pub(crate) actor: ActorId,
    pub(crate) uses_model: bool,
    pub(crate) model_profile_id: Option<ProfileId>,
    pub(crate) persistent_session: bool,
    pub(crate) cwd: Option<PathBuf>,
    pub(crate) shell_path: Option<String>,
    pub(crate) shell_command_prefix: Option<String>,
    pub(crate) runtime_tools: Vec<String>,
    pub(crate) profile_tools: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SessionCapabilityAccess {
    None,
    Read,
    Write,
}

#[derive(Debug, Clone)]
pub(crate) struct CapabilitySnapshotService {
    snapshot_coordinator: Arc<SnapshotCoordinator>,
}

impl CapabilitySnapshotService {
    pub(crate) fn new() -> Self {
        Self::with_snapshot_coordinator(SnapshotCoordinator::new())
    }

    pub(crate) fn with_snapshot_coordinator(
        snapshot_coordinator: Arc<SnapshotCoordinator>,
    ) -> Self {
        Self {
            snapshot_coordinator,
        }
    }

    pub(crate) fn current_generation(&self) -> CapabilityGeneration {
        self.snapshot_coordinator.current_capability_generation()
    }

    pub(crate) fn snapshot(
        &self,
        input: CapabilitySnapshotInput,
    ) -> Result<OperationCapabilitySnapshot, CodingSessionError> {
        let writes_session = matches!(input.session_access, SessionCapabilityAccess::Write);
        let reads_session = !matches!(input.session_access, SessionCapabilityAccess::None);
        let model = input.uses_model.then_some(ModelCapability {
            profile_id: input.model_profile_id,
        });
        let allowed_tools = if input.profile_tools.is_empty() {
            Vec::new()
        } else {
            input
                .runtime_tools
                .into_iter()
                .filter(|name| input.profile_tools.iter().any(|allowed| allowed == name))
                .collect::<Vec<_>>()
        };
        let cwd = input.cwd;
        let filesystem = cwd
            .as_ref()
            .filter(|_| allowed_tools.iter().any(|name| tool_uses_filesystem(name)))
            .map(|cwd| FilesystemCapability::new(cwd.clone()))
            .transpose()?;
        let shell = cwd
            .as_ref()
            .filter(|_| allowed_tools.iter().any(|name| name == "bash"))
            .map(|cwd| {
                ShellCapability::with_configuration(
                    cwd.clone(),
                    input.shell_path,
                    input.shell_command_prefix,
                )
            });
        Ok(OperationCapabilitySnapshot {
            generation: self.current_generation(),
            operation_id: input.operation_id,
            actor: input.actor,
            model,
            tools: ToolCapabilitySet::from_names(allowed_tools),
            commands: CommandCapabilitySet::default(),
            filesystem,
            shell,
            session_read: reads_session.then_some(SessionReadCapability {
                persistent: input.persistent_session,
            }),
            session_write: writes_session.then_some(SessionWriteCapability {
                persistent: input.persistent_session,
            }),
            ui: None,
        })
    }
}

impl Default for CapabilitySnapshotService {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod symlink_escape_tests {
    use super::*;

    fn capability(root: &std::path::Path) -> FilesystemCapability {
        FilesystemCapability::new(root.to_path_buf()).expect("capability opens")
    }

    #[test]
    fn read_through_a_workspace_symlink_is_rejected() {
        let temp = tempfile::tempdir().expect("tempdir");
        let outside = temp.path().join("outside-secret");
        std::fs::write(&outside, "secret").expect("write outside file");
        let workspace = temp.path().join("workspace");
        std::fs::create_dir(&workspace).expect("create workspace");
        std::fs::create_dir(workspace.join("sub")).expect("create subdir");
        // A symlink inside the workspace pointing at the outside directory.
        std::os::unix::fs::symlink(&outside, workspace.join("sub").join("link")).expect("symlink");

        let capability = capability(&workspace);
        let error = capability
            .prepare_target_blocking("read", "sub/link")
            .expect_err("a workspace symlink must be rejected");
        let CodingSessionError::UnsupportedCapability { capability: message } = &error else {
            panic!("expected UnsupportedCapability, got {error:?}");
        };
        assert!(
            message.contains("symbolic link"),
            "rejection must mention the symlink, got: {message}"
        );
    }

    #[test]
    fn write_through_a_workspace_symlink_parent_is_rejected() {
        let temp = tempfile::tempdir().expect("tempdir");
        let outside = temp.path().join("outside-dir");
        std::fs::create_dir(&outside).expect("create outside dir");
        let workspace = temp.path().join("workspace");
        std::fs::create_dir(&workspace).expect("create workspace");
        std::os::unix::fs::symlink(&outside, workspace.join("linked")).expect("symlink");

        let capability = capability(&workspace);
        let error = capability
            .prepare_target_blocking("write", "linked/new-file.txt")
            .expect_err("writing through a workspace symlink parent must be rejected");
        assert!(
            error.to_string().contains("symbolic link"),
            "rejection must mention the symlink, got: {error}"
        );
    }

    #[test]
    fn plain_workspace_paths_still_open() {
        let temp = tempfile::tempdir().expect("tempdir");
        let workspace = temp.path().join("workspace");
        std::fs::create_dir(&workspace).expect("create workspace");
        std::fs::create_dir(workspace.join("sub")).expect("create subdir");
        std::fs::write(workspace.join("sub").join("file.txt"), "hello").expect("write file");

        let capability = capability(&workspace);
        let target = capability
            .prepare_target_blocking("read", "sub/file.txt")
            .expect("a plain workspace path opens");
        assert!(target.object.is_some());
    }
}
