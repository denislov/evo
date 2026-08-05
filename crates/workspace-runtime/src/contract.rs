use sha2::{Digest, Sha256};
use std::fmt;
use std::path::{Path, PathBuf};

const MAX_ID_BYTES: usize = 128;

#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct WorkspaceId(String);

impl WorkspaceId {
    /// Parse a persisted workspace id and apply the same bounds as creation.
    pub fn parse(value: impl Into<String>) -> Result<Self, WorkspaceIdentityError> {
        let value = value.into();
        let valid_prefix = [
            WorkspaceKind::Source,
            WorkspaceKind::ManagedChild,
            WorkspaceKind::Projectless,
            WorkspaceKind::Legacy,
        ]
        .into_iter()
        .any(|kind| value.starts_with(&format!("{}-", kind.tag())));
        let has_value = value
            .split_once('-')
            .is_some_and(|(_, suffix)| !suffix.is_empty());
        if !valid_prefix
            || !has_value
            || value.is_empty()
            || value.len() > MAX_ID_BYTES
            || !value
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
        {
            return Err(WorkspaceIdentityError::InvalidId);
        }
        Ok(Self(value))
    }

    pub fn user_supplied(
        kind: WorkspaceKind,
        value: impl Into<String>,
    ) -> Result<Self, WorkspaceIdentityError> {
        let value = value.into();
        let prefix = format!("{}-", kind.tag());
        if value.is_empty()
            || prefix.len().saturating_add(value.len()) > MAX_ID_BYTES
            || !value
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
        {
            return Err(WorkspaceIdentityError::InvalidId);
        }
        Ok(Self(format!("{prefix}{value}")))
    }

    pub fn derived(kind: WorkspaceKind, root: &Path) -> Self {
        let mut digest = Sha256::new();
        digest.update(b"evo-workspace-runtime-v1\0");
        digest.update(kind.tag().as_bytes());
        digest.update([0]);
        update_path_digest(&mut digest, root);
        let encoded = format!("{:x}", digest.finalize());
        Self(format!("{}-{}", kind.tag(), &encoded[..40]))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for WorkspaceId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkspaceKind {
    Source,
    ManagedChild,
    Projectless,
    Legacy,
}

impl WorkspaceKind {
    pub(crate) const fn tag(self) -> &'static str {
        match self {
            Self::Source => "source",
            Self::ManagedChild => "child",
            Self::Projectless => "projectless",
            Self::Legacy => "legacy",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct WorkspaceHandle {
    id: WorkspaceId,
    kind: WorkspaceKind,
    root: PathBuf,
}

impl WorkspaceHandle {
    pub fn new(
        kind: WorkspaceKind,
        root: impl Into<PathBuf>,
    ) -> Result<Self, WorkspaceIdentityError> {
        let root = root.into();
        validate_root(&root)?;
        let id = WorkspaceId::derived(kind, &root);
        Ok(Self { id, kind, root })
    }

    pub fn with_user_id(
        kind: WorkspaceKind,
        value: impl Into<String>,
        root: impl Into<PathBuf>,
    ) -> Result<Self, WorkspaceIdentityError> {
        let root = root.into();
        validate_root(&root)?;
        let id = WorkspaceId::user_supplied(kind, value)?;
        Ok(Self { id, kind, root })
    }

    /// Construct a handle from a fully-formed id, rejecting ids whose kind
    /// prefix does not match `kind`.
    pub fn with_explicit_id(
        id: WorkspaceId,
        kind: WorkspaceKind,
        root: impl Into<PathBuf>,
    ) -> Result<Self, WorkspaceIdentityError> {
        let root = root.into();
        validate_root(&root)?;
        let prefix = format!("{}-", kind.tag());
        if !id.as_str().starts_with(&prefix) {
            return Err(WorkspaceIdentityError::InvalidId);
        }
        Ok(Self { id, kind, root })
    }

    pub fn id(&self) -> &WorkspaceId {
        &self.id
    }

    pub const fn kind(&self) -> WorkspaceKind {
        self.kind
    }

    pub fn root(&self) -> &Path {
        &self.root
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkspaceLifecycle {
    Creating,
    Ready,
    Active,
    MergePending,
    Merged,
    Discarded,
    Cleaning,
    Removed,
}

/// The single legal lifecycle transition table for managed workspaces.
///
/// Shared by the in-memory [`WorkspaceLease`] and the durable registry so a
/// persisted record can never be transitioned into a state the lease would
/// reject.
pub(crate) fn valid_lifecycle_transition(from: WorkspaceLifecycle, to: WorkspaceLifecycle) -> bool {
    matches!(
        (from, to),
        (WorkspaceLifecycle::Creating, WorkspaceLifecycle::Ready)
            | (WorkspaceLifecycle::Ready, WorkspaceLifecycle::Discarded)
            | (WorkspaceLifecycle::Ready, WorkspaceLifecycle::Active)
            | (WorkspaceLifecycle::Active, WorkspaceLifecycle::MergePending)
            | (WorkspaceLifecycle::Active, WorkspaceLifecycle::Discarded)
            | (WorkspaceLifecycle::MergePending, WorkspaceLifecycle::Merged)
            | (
                WorkspaceLifecycle::MergePending,
                WorkspaceLifecycle::Discarded
            )
            | (WorkspaceLifecycle::Merged, WorkspaceLifecycle::Cleaning)
            | (WorkspaceLifecycle::Discarded, WorkspaceLifecycle::Cleaning)
            | (WorkspaceLifecycle::Cleaning, WorkspaceLifecycle::Removed)
    )
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceLease {
    handle: WorkspaceHandle,
    owner_operation: String,
    parent_session: Option<String>,
    base_revision: Option<String>,
    lifecycle: WorkspaceLifecycle,
}

impl WorkspaceLease {
    pub fn new(
        handle: WorkspaceHandle,
        owner_operation: impl Into<String>,
        parent_session: Option<String>,
        base_revision: Option<String>,
    ) -> Result<Self, WorkspaceLeaseError> {
        let owner_operation = owner_operation.into();
        if owner_operation.is_empty() {
            return Err(WorkspaceLeaseError::MissingOwner);
        }
        Ok(Self {
            handle,
            owner_operation,
            parent_session,
            base_revision,
            lifecycle: WorkspaceLifecycle::Creating,
        })
    }

    pub fn handle(&self) -> &WorkspaceHandle {
        &self.handle
    }

    pub fn owner_operation(&self) -> &str {
        &self.owner_operation
    }

    pub fn parent_session(&self) -> Option<&str> {
        self.parent_session.as_deref()
    }

    pub fn base_revision(&self) -> Option<&str> {
        self.base_revision.as_deref()
    }

    pub const fn lifecycle(&self) -> WorkspaceLifecycle {
        self.lifecycle
    }

    pub fn transition(&mut self, next: WorkspaceLifecycle) -> Result<(), WorkspaceLeaseError> {
        if !valid_lifecycle_transition(self.lifecycle, next) {
            return Err(WorkspaceLeaseError::InvalidTransition {
                from: self.lifecycle,
                to: next,
            });
        }
        self.lifecycle = next;
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum WorkspaceIdentityError {
    #[error("workspace id is invalid")]
    InvalidId,
    #[error("workspace root must be an absolute path")]
    RootMustBeAbsolute,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum WorkspaceLeaseError {
    #[error("workspace lease owner operation is missing")]
    MissingOwner,
    #[error("invalid workspace lifecycle transition: {from:?} -> {to:?}")]
    InvalidTransition {
        from: WorkspaceLifecycle,
        to: WorkspaceLifecycle,
    },
}

fn validate_root(root: &Path) -> Result<(), WorkspaceIdentityError> {
    if root.as_os_str().is_empty() || !root.is_absolute() {
        Err(WorkspaceIdentityError::RootMustBeAbsolute)
    } else {
        Ok(())
    }
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
    fn derived_identity_is_stable_and_kind_scoped() {
        let root = PathBuf::from("/tmp/evo-source");
        let source = WorkspaceHandle::new(WorkspaceKind::Source, &root).unwrap();
        let child = WorkspaceHandle::new(WorkspaceKind::ManagedChild, &root).unwrap();
        assert_ne!(source.id(), child.id());
        assert_eq!(
            source,
            WorkspaceHandle::new(WorkspaceKind::Source, root).unwrap()
        );
    }

    #[test]
    fn user_identity_is_always_kind_prefixed() {
        let projectless = WorkspaceHandle::with_user_id(
            WorkspaceKind::Projectless,
            "draft-1",
            "/tmp/evo-projectless",
        )
        .unwrap();
        assert_eq!(projectless.id().as_str(), "projectless-draft-1");
    }

    #[test]
    fn explicit_id_must_match_its_kind_prefix() {
        let root = PathBuf::from("/tmp/evo-child");
        let ok =
            WorkspaceId::user_supplied(WorkspaceKind::ManagedChild, "abc123").expect("valid id");
        assert!(WorkspaceHandle::with_explicit_id(ok, WorkspaceKind::ManagedChild, &root).is_ok());
        let wrong_kind =
            WorkspaceId::user_supplied(WorkspaceKind::Source, "abc123").expect("valid id");
        assert!(
            WorkspaceHandle::with_explicit_id(wrong_kind, WorkspaceKind::ManagedChild, &root)
                .is_err()
        );
    }

    #[test]
    fn lease_lifecycle_is_fail_closed() {
        let handle = WorkspaceHandle::new(WorkspaceKind::ManagedChild, "/tmp/evo-child").unwrap();
        let mut lease =
            WorkspaceLease::new(handle, "op-1", Some("session-1".into()), None).unwrap();
        assert!(lease.transition(WorkspaceLifecycle::Active).is_err());
        lease.transition(WorkspaceLifecycle::Ready).unwrap();
        lease.transition(WorkspaceLifecycle::Active).unwrap();
        lease.transition(WorkspaceLifecycle::Discarded).unwrap();
        lease.transition(WorkspaceLifecycle::Cleaning).unwrap();
        lease.transition(WorkspaceLifecycle::Removed).unwrap();
    }
}
