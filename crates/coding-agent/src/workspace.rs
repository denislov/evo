use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

const MANAGED_SCRATCH_DIRECTORY: &str = "scratch";
const MAX_WORKSPACE_ID_BYTES: usize = 64;
const MAX_WORKSPACE_DISPLAY_NAME_CHARS: usize = 128;

/// Product identity for the filesystem scope owned by one session.
///
/// `Projectless` deliberately carries no execution path. Its managed scratch
/// directory is resolved from `workspace_id` only when a runtime context is
/// built. `Legacy` is reserved for old durable sessions at the migration
/// boundary and cannot be produced by [`CodingAgentWorkspaceSelection`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CodingAgentWorkspaceScope {
    Project { cwd: PathBuf },
    Projectless { workspace_id: String },
    Legacy { cwd: Option<PathBuf> },
}

/// User-selectable workspace target for a new session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CodingAgentWorkspaceSelection {
    Project { cwd: PathBuf },
    Projectless { workspace_id: String },
}

impl CodingAgentWorkspaceSelection {
    pub fn project(cwd: impl Into<PathBuf>) -> Self {
        Self::Project { cwd: cwd.into() }
    }

    pub fn projectless(workspace_id: impl Into<String>) -> Self {
        Self::Projectless {
            workspace_id: workspace_id.into(),
        }
    }

    /// Resolve UI selection into immutable product identity and execution cwd.
    ///
    /// Project paths are made absolute and canonicalized. Projectless paths
    /// are created below the product-managed `<global-config>/scratch` root;
    /// the resulting scratch path is never copied into the public workspace
    /// overview.
    pub fn resolve(
        self,
        global_config_dir: impl AsRef<Path>,
    ) -> Result<CodingAgentResolvedWorkspace, CodingAgentWorkspaceResolutionError> {
        match self {
            Self::Project { cwd } => {
                let cwd = normalize_project_directory(&cwd)?;
                let scope = CodingAgentWorkspaceScope::Project { cwd: cwd.clone() };
                Ok(CodingAgentResolvedWorkspace::new(scope, cwd))
            }
            Self::Projectless { workspace_id } => {
                validate_workspace_id(&workspace_id)?;
                let execution_cwd =
                    resolve_managed_scratch(global_config_dir.as_ref(), &workspace_id)?;
                let scope = CodingAgentWorkspaceScope::Projectless { workspace_id };
                Ok(CodingAgentResolvedWorkspace::new(scope, execution_cwd))
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CodingAgentWorkspaceKind {
    Project,
    Projectless,
    Legacy,
}

/// Bounded, authority-free workspace facts safe for list and navigation UIs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodingAgentWorkspaceOverview {
    pub group_id: String,
    pub kind: CodingAgentWorkspaceKind,
    pub display_name: String,
    /// User-selected project identity when one is known.
    ///
    /// Managed scratch paths are intentionally never exposed here.
    pub display_path: Option<PathBuf>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CodingAgentWorkspaceMigrationOutcome {
    NotRequired,
    Pending,
    Migrated,
    Unavailable,
}

/// Safe migration and availability evidence for a durable workspace.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodingAgentWorkspaceMigration {
    pub outcome: CodingAgentWorkspaceMigrationOutcome,
    /// Bounded product diagnostic. Managed scratch paths are never included.
    pub diagnostic: Option<String>,
}

/// Fully resolved workspace facts used to build one runtime context.
///
/// `execution_cwd` can be a managed scratch directory while `overview`
/// remains Projectless, preventing adapters from presenting scratch as a
/// regular project.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodingAgentResolvedWorkspace {
    pub scope: CodingAgentWorkspaceScope,
    pub execution_cwd: PathBuf,
    pub overview: CodingAgentWorkspaceOverview,
}

impl CodingAgentResolvedWorkspace {
    fn new(scope: CodingAgentWorkspaceScope, execution_cwd: PathBuf) -> Self {
        let overview = scope.overview();
        Self {
            scope,
            execution_cwd,
            overview,
        }
    }
}

impl CodingAgentWorkspaceScope {
    pub const fn kind(&self) -> CodingAgentWorkspaceKind {
        match self {
            Self::Project { .. } => CodingAgentWorkspaceKind::Project,
            Self::Projectless { .. } => CodingAgentWorkspaceKind::Projectless,
            Self::Legacy { .. } => CodingAgentWorkspaceKind::Legacy,
        }
    }

    pub fn workspace_group_id(&self) -> String {
        let mut digest = Sha256::new();
        match self {
            Self::Project { cwd } => {
                digest.update(b"evo-workspace-project-v1\0");
                update_path_digest(&mut digest, cwd);
                format!("project:{:x}", digest.finalize())
            }
            Self::Projectless { workspace_id } => {
                digest.update(b"evo-workspace-projectless-v1\0");
                digest.update(workspace_id.as_bytes());
                format!("projectless:{:x}", digest.finalize())
            }
            Self::Legacy { cwd: Some(cwd) } => {
                digest.update(b"evo-workspace-legacy-v1\0");
                update_path_digest(&mut digest, cwd);
                format!("legacy:{:x}", digest.finalize())
            }
            Self::Legacy { cwd: None } => "legacy:unscoped".into(),
        }
    }

    pub fn overview(&self) -> CodingAgentWorkspaceOverview {
        match self {
            Self::Project { cwd } => CodingAgentWorkspaceOverview {
                group_id: self.workspace_group_id(),
                kind: self.kind(),
                display_name: project_display_name(cwd),
                display_path: Some(cwd.clone()),
            },
            Self::Projectless { .. } => CodingAgentWorkspaceOverview {
                group_id: self.workspace_group_id(),
                kind: self.kind(),
                display_name: "Projectless".into(),
                display_path: None,
            },
            Self::Legacy { cwd } => CodingAgentWorkspaceOverview {
                group_id: self.workspace_group_id(),
                kind: self.kind(),
                display_name: cwd
                    .as_deref()
                    .map(project_display_name)
                    .unwrap_or_else(|| "Legacy session".into()),
                display_path: cwd.clone(),
            },
        }
    }

    /// Resolve the immutable scope to its runtime cwd.
    ///
    /// This is also the authoritative validation point for scopes decoded
    /// from durable state. A deleted project remains representable in an
    /// overview but returns a typed unavailable-path error when opened.
    pub fn resolve_execution_cwd(
        &self,
        global_config_dir: impl AsRef<Path>,
    ) -> Result<PathBuf, CodingAgentWorkspaceResolutionError> {
        match self {
            Self::Project { cwd } => normalize_project_directory(cwd),
            Self::Projectless { workspace_id } => {
                validate_workspace_id(workspace_id)?;
                resolve_managed_scratch(global_config_dir.as_ref(), workspace_id)
            }
            Self::Legacy { cwd: Some(cwd) } => normalize_project_directory(cwd),
            Self::Legacy { cwd: None } => {
                Err(CodingAgentWorkspaceResolutionError::LegacyCwdMissing)
            }
        }
    }
}

impl fmt::Display for CodingAgentWorkspaceScope {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.overview().display_name)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CodingAgentWorkspaceResolutionError {
    #[error("project path must not be empty")]
    ProjectPathEmpty,
    #[error("project path contains a NUL byte")]
    ProjectPathContainsNul,
    #[error("persisted project path must be absolute")]
    PersistedProjectPathNotAbsolute,
    #[error("persisted project path must be valid Unicode")]
    PersistedProjectPathNotUnicode,
    #[error("project directory does not exist: {path}")]
    ProjectNotFound { path: PathBuf },
    #[error("project path is not a directory: {path}")]
    ProjectNotDirectory { path: PathBuf },
    #[error("project directory is unavailable: {path}")]
    ProjectUnavailable { path: PathBuf },
    #[error("workspace id is invalid")]
    InvalidWorkspaceId,
    #[error("managed scratch path is a symbolic link: {path}")]
    ManagedScratchSymbolicLink { path: PathBuf },
    #[error("managed scratch path is not a directory: {path}")]
    ManagedScratchNotDirectory { path: PathBuf },
    #[error("managed scratch path is unavailable: {path}")]
    ManagedScratchUnavailable { path: PathBuf },
    #[error("legacy workspace has no recoverable cwd")]
    LegacyCwdMissing,
}

fn normalize_project_directory(
    path: &Path,
) -> Result<PathBuf, CodingAgentWorkspaceResolutionError> {
    if path.as_os_str().is_empty() {
        return Err(CodingAgentWorkspaceResolutionError::ProjectPathEmpty);
    }
    if path.to_string_lossy().contains('\0') {
        return Err(CodingAgentWorkspaceResolutionError::ProjectPathContainsNul);
    }
    let absolute = std::path::absolute(path).map_err(|_| {
        CodingAgentWorkspaceResolutionError::ProjectUnavailable {
            path: path.to_path_buf(),
        }
    })?;
    let metadata = fs::metadata(&absolute).map_err(|error| match error.kind() {
        io::ErrorKind::NotFound => CodingAgentWorkspaceResolutionError::ProjectNotFound {
            path: absolute.clone(),
        },
        _ => CodingAgentWorkspaceResolutionError::ProjectUnavailable {
            path: absolute.clone(),
        },
    })?;
    if !metadata.is_dir() {
        return Err(CodingAgentWorkspaceResolutionError::ProjectNotDirectory { path: absolute });
    }
    absolute
        .canonicalize()
        .map_err(|_| CodingAgentWorkspaceResolutionError::ProjectUnavailable { path: absolute })
}

pub(crate) fn validate_workspace_id(id: &str) -> Result<(), CodingAgentWorkspaceResolutionError> {
    let valid = !id.is_empty()
        && id.len() <= MAX_WORKSPACE_ID_BYTES
        && id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'));
    if valid {
        Ok(())
    } else {
        Err(CodingAgentWorkspaceResolutionError::InvalidWorkspaceId)
    }
}

pub(crate) fn validate_persisted_project_path(
    path: &Path,
) -> Result<(), CodingAgentWorkspaceResolutionError> {
    if path.as_os_str().is_empty() {
        return Err(CodingAgentWorkspaceResolutionError::ProjectPathEmpty);
    }
    if path.to_string_lossy().contains('\0') {
        return Err(CodingAgentWorkspaceResolutionError::ProjectPathContainsNul);
    }
    if !path.is_absolute() {
        return Err(CodingAgentWorkspaceResolutionError::PersistedProjectPathNotAbsolute);
    }
    Ok(())
}

pub(crate) fn projectless_workspace_id_for_session(session_id: &str) -> String {
    let digest = format!("{:x}", Sha256::digest(session_id.as_bytes()));
    format!("session-{}", &digest[..40])
}

pub(crate) struct LegacyWorkspaceInference {
    pub(crate) scope: CodingAgentWorkspaceScope,
    pub(crate) migration: CodingAgentWorkspaceMigration,
}

pub(crate) fn infer_legacy_workspace(
    cwd: Option<&str>,
    global_config_dir: &Path,
) -> LegacyWorkspaceInference {
    let Some(cwd) = cwd.filter(|cwd| !cwd.is_empty() && !cwd.contains('\0')) else {
        return unavailable_legacy("Legacy session has no valid workspace path.");
    };
    let cwd = PathBuf::from(cwd);
    if validate_persisted_project_path(&cwd).is_err() {
        return unavailable_legacy("Legacy session workspace path is not absolute.");
    }

    let scratch_root = absolute_without_io(global_config_dir).join(MANAGED_SCRATCH_DIRECTORY);
    let scratch_root = scratch_root.canonicalize().unwrap_or(scratch_root);
    if cwd.parent() == Some(scratch_root.as_path()) {
        let Some(workspace_id) = cwd.file_name().and_then(|value| value.to_str()) else {
            return unavailable_legacy("Legacy scratch workspace identity is invalid.");
        };
        if validate_workspace_id(workspace_id).is_err() {
            return unavailable_legacy("Legacy scratch workspace identity is invalid.");
        }
        if fs::symlink_metadata(&cwd).is_ok_and(|metadata| metadata.file_type().is_symlink()) {
            return unavailable_legacy("Legacy scratch workspace is a symbolic link.");
        }
        let scope = CodingAgentWorkspaceScope::Projectless {
            workspace_id: workspace_id.to_owned(),
        };
        return LegacyWorkspaceInference {
            migration: workspace_migration_status(
                &scope,
                CodingAgentWorkspaceMigrationOutcome::Pending,
                global_config_dir,
            ),
            scope,
        };
    }

    let scope = CodingAgentWorkspaceScope::Project { cwd };
    LegacyWorkspaceInference {
        migration: workspace_migration_status(
            &scope,
            CodingAgentWorkspaceMigrationOutcome::Pending,
            global_config_dir,
        ),
        scope,
    }
}

pub(crate) fn workspace_migration_status(
    scope: &CodingAgentWorkspaceScope,
    outcome: CodingAgentWorkspaceMigrationOutcome,
    global_config_dir: &Path,
) -> CodingAgentWorkspaceMigration {
    let diagnostic = match scope {
        CodingAgentWorkspaceScope::Project { cwd } => match fs::metadata(cwd) {
            Ok(metadata) if metadata.is_dir() => None,
            Ok(_) => Some("Project workspace path is not a directory.".into()),
            Err(_) => Some("Project workspace directory is unavailable.".into()),
        },
        CodingAgentWorkspaceScope::Projectless { workspace_id } => {
            let root = absolute_without_io(global_config_dir).join(MANAGED_SCRATCH_DIRECTORY);
            let workspace = root.join(workspace_id);
            match (
                fs::symlink_metadata(&root),
                fs::symlink_metadata(&workspace),
            ) {
                (Ok(root), Ok(workspace))
                    if root.is_dir()
                        && !root.file_type().is_symlink()
                        && workspace.is_dir()
                        && !workspace.file_type().is_symlink() =>
                {
                    None
                }
                _ => Some("Managed projectless workspace is unavailable.".into()),
            }
        }
        CodingAgentWorkspaceScope::Legacy { .. } => {
            Some("Legacy session workspace is unavailable.".into())
        }
    };
    CodingAgentWorkspaceMigration {
        outcome,
        diagnostic,
    }
}

fn unavailable_legacy(diagnostic: &str) -> LegacyWorkspaceInference {
    LegacyWorkspaceInference {
        scope: CodingAgentWorkspaceScope::Legacy { cwd: None },
        migration: CodingAgentWorkspaceMigration {
            outcome: CodingAgentWorkspaceMigrationOutcome::Unavailable,
            diagnostic: Some(diagnostic.into()),
        },
    }
}

fn absolute_without_io(path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf())
    }
}

fn resolve_managed_scratch(
    global_config_dir: &Path,
    workspace_id: &str,
) -> Result<PathBuf, CodingAgentWorkspaceResolutionError> {
    let root = global_config_dir.join(MANAGED_SCRATCH_DIRECTORY);
    ensure_managed_directory(&root)?;
    let workspace = root.join(workspace_id);
    ensure_managed_directory(&workspace)?;
    workspace.canonicalize().map_err(|_| {
        CodingAgentWorkspaceResolutionError::ManagedScratchUnavailable { path: workspace }
    })
}

fn ensure_managed_directory(path: &Path) -> Result<(), CodingAgentWorkspaceResolutionError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => validate_managed_directory_metadata(path, &metadata),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            fs::create_dir_all(path).map_err(|_| {
                CodingAgentWorkspaceResolutionError::ManagedScratchUnavailable {
                    path: path.to_path_buf(),
                }
            })?;
            let metadata = fs::symlink_metadata(path).map_err(|_| {
                CodingAgentWorkspaceResolutionError::ManagedScratchUnavailable {
                    path: path.to_path_buf(),
                }
            })?;
            validate_managed_directory_metadata(path, &metadata)
        }
        Err(_) => Err(
            CodingAgentWorkspaceResolutionError::ManagedScratchUnavailable {
                path: path.to_path_buf(),
            },
        ),
    }
}

fn validate_managed_directory_metadata(
    path: &Path,
    metadata: &fs::Metadata,
) -> Result<(), CodingAgentWorkspaceResolutionError> {
    if metadata.file_type().is_symlink() {
        return Err(
            CodingAgentWorkspaceResolutionError::ManagedScratchSymbolicLink {
                path: path.to_path_buf(),
            },
        );
    }
    if !metadata.is_dir() {
        return Err(
            CodingAgentWorkspaceResolutionError::ManagedScratchNotDirectory {
                path: path.to_path_buf(),
            },
        );
    }
    Ok(())
}

fn project_display_name(path: &Path) -> String {
    let candidate = path
        .file_name()
        .filter(|name| !name.is_empty())
        .unwrap_or(path.as_os_str());
    candidate
        .to_string_lossy()
        .chars()
        .take(MAX_WORKSPACE_DISPLAY_NAME_CHARS)
        .collect()
}

fn update_path_digest(digest: &mut Sha256, path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt as _;
        digest.update(path.as_os_str().as_bytes());
    }
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt as _;
        for unit in path.as_os_str().encode_wide() {
            digest.update(unit.to_le_bytes());
        }
    }
    #[cfg(not(any(unix, windows)))]
    digest.update(path.to_string_lossy().as_bytes());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn project_selection_normalizes_identity_and_keeps_equal_scopes_equal() {
        let temp = tempfile::tempdir().unwrap();
        let project = temp.path().join("project");
        fs::create_dir(&project).unwrap();

        let first = CodingAgentWorkspaceSelection::project(&project)
            .resolve(temp.path())
            .unwrap();
        let second = CodingAgentWorkspaceSelection::project(project.join("..").join("project"))
            .resolve(temp.path())
            .unwrap();

        assert_eq!(first.scope, second.scope);
        assert_eq!(first.execution_cwd, project.canonicalize().unwrap());
        assert_eq!(first.overview, second.overview);
        assert_eq!(first.overview.kind, CodingAgentWorkspaceKind::Project);
        assert_eq!(first.overview.display_name, "project");
        assert_eq!(first.overview.display_path, Some(first.execution_cwd));
    }

    #[test]
    fn stable_group_ids_distinguish_same_named_projects_and_scope_kinds() {
        let first = CodingAgentWorkspaceScope::Project {
            cwd: PathBuf::from("/one/evo"),
        };
        let first_again = first.clone();
        let second = CodingAgentWorkspaceScope::Project {
            cwd: PathBuf::from("/two/evo"),
        };
        let projectless = CodingAgentWorkspaceScope::Projectless {
            workspace_id: "workspace-stable".into(),
        };

        assert_eq!(first.workspace_group_id(), first_again.workspace_group_id());
        assert_ne!(first.workspace_group_id(), second.workspace_group_id());
        assert_ne!(first.workspace_group_id(), projectless.workspace_group_id());
        assert_eq!(first.to_string(), "evo");
        assert_eq!(projectless.to_string(), "Projectless");
    }

    #[test]
    fn project_path_errors_are_typed_before_context_loading() {
        let temp = tempfile::tempdir().unwrap();
        let file = temp.path().join("file.txt");
        fs::write(&file, "not a directory").unwrap();

        assert_eq!(
            CodingAgentWorkspaceSelection::project(PathBuf::new())
                .resolve(temp.path())
                .unwrap_err(),
            CodingAgentWorkspaceResolutionError::ProjectPathEmpty
        );
        assert!(matches!(
            CodingAgentWorkspaceSelection::project(temp.path().join("missing"))
                .resolve(temp.path()),
            Err(CodingAgentWorkspaceResolutionError::ProjectNotFound { .. })
        ));
        assert!(matches!(
            CodingAgentWorkspaceSelection::project(file).resolve(temp.path()),
            Err(CodingAgentWorkspaceResolutionError::ProjectNotDirectory { .. })
        ));
        assert_eq!(
            CodingAgentWorkspaceSelection::project(PathBuf::from("bad\0path"))
                .resolve(temp.path())
                .unwrap_err(),
            CodingAgentWorkspaceResolutionError::ProjectPathContainsNul
        );
    }

    #[test]
    fn projectless_scope_resolves_managed_scratch_without_exposing_it_as_project() {
        let global = tempfile::tempdir().unwrap();
        let resolved = CodingAgentWorkspaceSelection::projectless("workspace-stable")
            .resolve(global.path())
            .unwrap();
        let expected = global
            .path()
            .join("scratch/workspace-stable")
            .canonicalize()
            .unwrap();

        assert_eq!(resolved.execution_cwd, expected);
        assert_eq!(
            resolved.scope,
            CodingAgentWorkspaceScope::Projectless {
                workspace_id: "workspace-stable".into()
            }
        );
        assert_eq!(
            resolved.overview.kind,
            CodingAgentWorkspaceKind::Projectless
        );
        assert_eq!(resolved.overview.display_name, "Projectless");
        assert_eq!(resolved.overview.display_path, None);
        assert!(!resolved.overview.group_id.contains("workspace-stable"));
    }

    #[test]
    fn projectless_ids_cannot_escape_the_managed_root() {
        let global = tempfile::tempdir().unwrap();
        for invalid in ["", "../escape", "nested/path", "x\\y", &"x".repeat(65)] {
            assert_eq!(
                CodingAgentWorkspaceSelection::projectless(invalid)
                    .resolve(global.path())
                    .unwrap_err(),
                CodingAgentWorkspaceResolutionError::InvalidWorkspaceId
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn projectless_scope_rejects_symlinked_managed_workspace() {
        use std::os::unix::fs::symlink;

        let global = tempfile::tempdir().unwrap();
        let external = tempfile::tempdir().unwrap();
        let scratch = global.path().join(MANAGED_SCRATCH_DIRECTORY);
        fs::create_dir(&scratch).unwrap();
        let workspace = scratch.join("workspace-link");
        symlink(external.path(), &workspace).unwrap();

        assert_eq!(
            CodingAgentWorkspaceSelection::projectless("workspace-link")
                .resolve(global.path())
                .unwrap_err(),
            CodingAgentWorkspaceResolutionError::ManagedScratchSymbolicLink { path: workspace }
        );
    }

    #[test]
    fn legacy_scope_preserves_identity_but_missing_cwd_cannot_execute() {
        let legacy = CodingAgentWorkspaceScope::Legacy { cwd: None };
        assert_eq!(legacy.overview().kind, CodingAgentWorkspaceKind::Legacy);
        assert_eq!(legacy.overview().group_id, "legacy:unscoped");
        assert_eq!(legacy.overview().display_path, None);
        assert_eq!(
            legacy.resolve_execution_cwd("/unused").unwrap_err(),
            CodingAgentWorkspaceResolutionError::LegacyCwdMissing
        );
    }
}
