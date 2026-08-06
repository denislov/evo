use std::collections::HashSet;
use std::io::Write;
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::access::WorkspaceAccessHandle;
use crate::contract::WorkspaceKind;
use crate::fs::{
    CapWalkEntryKind, CapWalkRoot, FileMutation, FilesystemTarget, OpenedEditFile,
    walk_target_unfiltered,
};

const MAX_RESTORE_ENTRIES: usize = 4096;
const MAX_RESTORE_BYTES: usize = 32 * 1024 * 1024;
const MAX_SNAPSHOT_FILE_BYTES: u64 = MAX_RESTORE_BYTES as u64;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceSnapshot {
    pub files: Vec<WorkspaceFileSnapshot>,
}

impl WorkspaceSnapshot {
    pub fn file(&self, path: &Path) -> Option<&WorkspaceFileSnapshot> {
        self.files.iter().find(|file| file.path == path)
    }

    pub fn validate(&self) -> Result<(), WorkspaceSnapshotError> {
        if self.files.len() > MAX_RESTORE_ENTRIES {
            return Err(WorkspaceSnapshotError::BudgetExceeded {
                message: format!("file count exceeds {MAX_RESTORE_ENTRIES}"),
            });
        }
        let mut previous: Option<&Path> = None;
        let mut retained_bytes = 0_usize;
        for file in &self.files {
            if previous.is_some_and(|previous| previous >= file.path.as_path()) {
                return Err(WorkspaceSnapshotError::Invalid {
                    message: format!(
                        "workspace snapshot paths are duplicate or unsorted: {}",
                        file.path.display()
                    ),
                });
            }
            if path_text(&file.path).is_err()
                || !file.exists
                || file.content.is_none()
                || file.revision.len() != 64
                || file
                    .content
                    .as_deref()
                    .is_some_and(|content| revision(content) != file.revision)
            {
                return Err(WorkspaceSnapshotError::Invalid {
                    message: format!("invalid workspace snapshot file: {}", file.path.display()),
                });
            }
            retained_bytes =
                retained_bytes.saturating_add(file.content.as_ref().map_or(0, Vec::len));
            previous = Some(file.path.as_path());
        }
        if retained_bytes > MAX_RESTORE_BYTES {
            return Err(WorkspaceSnapshotError::BudgetExceeded {
                message: format!("content exceeds {MAX_RESTORE_BYTES} bytes"),
            });
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum WorkspaceSnapshotError {
    #[error("workspace kind {kind:?} does not support rewind snapshots")]
    UnsupportedWorkspace { kind: WorkspaceKind },
    #[error("workspace snapshot failed: {message}")]
    Capture { message: String },
    #[error("workspace snapshot is invalid: {message}")]
    Invalid { message: String },
    #[error("workspace snapshot exceeds its bounds: {message}")]
    BudgetExceeded { message: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceFileSnapshot {
    pub path: PathBuf,
    pub exists: bool,
    pub revision: String,
    pub content: Option<Vec<u8>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceRestoreEntry {
    pub expected: WorkspaceFileSnapshot,
    pub replacement: WorkspaceFileSnapshot,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceRestorePlan {
    pub entries: Vec<WorkspaceRestoreEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceRestoreReport {
    pub restored: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum WorkspaceRestoreError {
    #[error("workspace kind {kind:?} does not support rewind restore")]
    UnsupportedWorkspace { kind: WorkspaceKind },
    #[error("invalid workspace restore plan: {message}")]
    Invalid { message: String },
    #[error("workspace restore target changed: {path}")]
    TargetChanged { path: PathBuf },
    #[error("workspace restore failed for {path}: {message}")]
    Apply { path: PathBuf, message: String },
    #[error("workspace restore failed and rollback was incomplete: {message}")]
    Rollback { message: String },
}

pub async fn capture_workspace_snapshot(
    workspace: &WorkspaceAccessHandle,
) -> Result<WorkspaceSnapshot, WorkspaceSnapshotError> {
    match workspace.identity().kind() {
        WorkspaceKind::ManagedChild | WorkspaceKind::Projectless => {}
        kind => return Err(WorkspaceSnapshotError::UnsupportedWorkspace { kind }),
    }
    let target = workspace
        .prepare_target_for_tool("find", ".")
        .await
        .map_err(|error| WorkspaceSnapshotError::Capture {
            message: format!("cannot open workspace root: {error}"),
        })?;
    tokio::task::spawn_blocking(move || capture_workspace_snapshot_blocking(&target))
        .await
        .map_err(|error| WorkspaceSnapshotError::Capture {
            message: format!("snapshot worker failed: {error}"),
        })?
}

fn capture_workspace_snapshot_blocking(
    target: &FilesystemTarget,
) -> Result<WorkspaceSnapshot, WorkspaceSnapshotError> {
    let CapWalkRoot::Directory(entries) = walk_target_unfiltered(target)
        .map_err(|message| WorkspaceSnapshotError::Capture { message })?
    else {
        return Err(WorkspaceSnapshotError::Capture {
            message: "workspace root is not a directory".into(),
        });
    };
    let mut files = Vec::new();
    let mut retained_bytes = 0_usize;
    for entry in entries {
        match entry.kind {
            CapWalkEntryKind::Directory => continue,
            CapWalkEntryKind::Other => {
                return Err(WorkspaceSnapshotError::Capture {
                    message: format!(
                        "unsupported non-file workspace entry: {}",
                        entry.relative.display()
                    ),
                });
            }
            CapWalkEntryKind::File => {}
        }
        if files.len() >= MAX_RESTORE_ENTRIES {
            return Err(WorkspaceSnapshotError::BudgetExceeded {
                message: format!("file count exceeds {MAX_RESTORE_ENTRIES}"),
            });
        }
        let content = entry
            .read_bounded(MAX_SNAPSHOT_FILE_BYTES)
            .map_err(|error| WorkspaceSnapshotError::Capture {
                message: format!("cannot read {}: {error}", entry.relative.display()),
            })?
            .ok_or_else(|| WorkspaceSnapshotError::BudgetExceeded {
                message: format!(
                    "{} exceeds the per-file limit of {MAX_SNAPSHOT_FILE_BYTES} bytes",
                    entry.relative.display()
                ),
            })?;
        retained_bytes = retained_bytes.saturating_add(content.len());
        if retained_bytes > MAX_RESTORE_BYTES {
            return Err(WorkspaceSnapshotError::BudgetExceeded {
                message: format!("content exceeds {MAX_RESTORE_BYTES} bytes"),
            });
        }
        files.push(WorkspaceFileSnapshot {
            path: entry.relative,
            exists: true,
            revision: revision(&content),
            content: Some(content),
        });
    }
    files.sort_by(|left, right| left.path.cmp(&right.path));
    let snapshot = WorkspaceSnapshot { files };
    snapshot.validate()?;
    Ok(snapshot)
}

struct PreparedEntry {
    target: FilesystemTarget,
    expected: WorkspaceFileSnapshot,
    replacement: WorkspaceFileSnapshot,
    rollback: WorkspaceFileSnapshot,
}

pub async fn restore_workspace_snapshot(
    workspace: &WorkspaceAccessHandle,
    plan: WorkspaceRestorePlan,
) -> Result<WorkspaceRestoreReport, WorkspaceRestoreError> {
    restore_workspace_snapshot_inner(workspace, plan, None).await
}

async fn restore_workspace_snapshot_inner(
    workspace: &WorkspaceAccessHandle,
    plan: WorkspaceRestorePlan,
    fault: Option<&ApplyFault>,
) -> Result<WorkspaceRestoreReport, WorkspaceRestoreError> {
    match workspace.identity().kind() {
        WorkspaceKind::ManagedChild | WorkspaceKind::Projectless => {}
        kind => return Err(WorkspaceRestoreError::UnsupportedWorkspace { kind }),
    }
    validate_plan(&plan)?;
    let mut prepared = Vec::with_capacity(plan.entries.len());
    for entry in plan.entries {
        let target = prepare_target(workspace, &entry.expected).await?;
        let current = read_target(&target, &entry.expected.path).await?;
        if !same_revision(&current, &entry.expected) {
            return Err(WorkspaceRestoreError::TargetChanged {
                path: entry.expected.path,
            });
        }
        prepared.push(PreparedEntry {
            target,
            rollback: current,
            expected: entry.expected,
            replacement: entry.replacement,
        });
    }

    let mut applied = Vec::new();
    for (index, entry) in prepared.iter().enumerate() {
        if let Err(error) = apply_target(&entry.target, &entry.replacement, fault).await {
            applied.push(index);
            let rollback = rollback_entries(workspace, &prepared, &applied).await;
            return match rollback {
                Ok(()) => Err(WorkspaceRestoreError::Apply {
                    path: entry.expected.path.clone(),
                    message: error,
                }),
                Err(rollback_error) => Err(WorkspaceRestoreError::Rollback {
                    message: format!(
                        "apply failed for {}: {error}; {rollback_error}",
                        entry.expected.path.display()
                    ),
                }),
            };
        }
        applied.push(index);
    }
    Ok(WorkspaceRestoreReport {
        restored: applied.len(),
    })
}

async fn rollback_entries(
    workspace: &WorkspaceAccessHandle,
    prepared: &[PreparedEntry],
    applied: &[usize],
) -> Result<(), String> {
    let mut failures = Vec::new();
    for index in applied.iter().rev().copied() {
        let entry = &prepared[index];
        let expected = entry.replacement.clone();
        let target = match prepare_target(workspace, &expected).await {
            Ok(target) => target,
            Err(error) => {
                failures.push(format!("{}: {error}", expected.path.display()));
                continue;
            }
        };
        match read_target(&target, &expected.path).await {
            Ok(current) if same_revision(&current, &expected) => {}
            Ok(_) => {
                failures.push(format!(
                    "{} changed before rollback",
                    expected.path.display()
                ));
                continue;
            }
            Err(error) => {
                failures.push(format!("{}: {error}", expected.path.display()));
                continue;
            }
        }
        if let Err(error) = apply_target(&target, &entry.rollback, None).await {
            failures.push(format!("{}: {error}", expected.path.display()));
        } else if !entry.rollback.exists
            && let Err(error) = entry.target.remove_vacant_created_parents()
        {
            failures.push(format!("{}: {error}", expected.path.display()));
        }
    }
    if failures.is_empty() {
        Ok(())
    } else {
        Err(failures.join("; "))
    }
}

async fn prepare_target(
    workspace: &WorkspaceAccessHandle,
    expected: &WorkspaceFileSnapshot,
) -> Result<FilesystemTarget, WorkspaceRestoreError> {
    let path = path_text(&expected.path)?;
    workspace
        .prepare_target_for_tool("write", path)
        .await
        .map_err(|error| WorkspaceRestoreError::Apply {
            path: expected.path.clone(),
            message: error.to_string(),
        })
}

async fn read_target(
    target: &FilesystemTarget,
    path: &Path,
) -> Result<WorkspaceFileSnapshot, WorkspaceRestoreError> {
    if target.is_vacant() {
        return Ok(WorkspaceFileSnapshot {
            path: path.to_path_buf(),
            exists: false,
            revision: revision(&[]),
            content: Some(Vec::new()),
        });
    }
    let file = target
        .opened_file()
        .map_err(|message| WorkspaceRestoreError::Apply {
            path: path.to_path_buf(),
            message,
        })?;
    let content = OpenedEditFile::new(file, target.display_path().to_path_buf())
        .read_file()
        .await
        .map_err(|message| WorkspaceRestoreError::Apply {
            path: path.to_path_buf(),
            message,
        })?;
    Ok(WorkspaceFileSnapshot {
        path: path.to_path_buf(),
        exists: true,
        revision: revision(&content),
        content: Some(content),
    })
}

async fn apply_target(
    target: &FilesystemTarget,
    replacement: &WorkspaceFileSnapshot,
    fault: Option<&ApplyFault>,
) -> Result<(), String> {
    target.revalidate_identity()?;
    let mutation = FileMutation::begin(target).await?;
    if !replacement.exists {
        if target.is_vacant() {
            return Ok(());
        }
        let target_for_task = target.clone();
        let result = tokio::task::spawn_blocking(move || {
            let _mutation = mutation;
            target_for_task.revalidate_identity()?;
            target_for_task.remove_file()
        })
        .await
        .map_err(|error| format!("workspace restore delete task failed: {error}"))?;
        result?;
        return fail_after_apply(target, replacement, fault);
    }
    let content = replacement
        .content
        .as_deref()
        .ok_or_else(|| "workspace restore replacement content is unavailable".to_owned())?;
    if target.is_vacant() {
        let target_for_task = target.clone();
        let content = content.to_vec();
        let result = tokio::task::spawn_blocking(move || {
            let _mutation = mutation;
            target_for_task.revalidate_identity()?;
            let mut file = target_for_task.create_vacant_file()?;
            file.write_all(&content).map_err(|error| {
                format!(
                    "workspace restore cannot write {}: {error}",
                    target_for_task.display_path().display()
                )
            })?;
            file.sync_all().map_err(|error| {
                format!(
                    "workspace restore cannot sync {}: {error}",
                    target_for_task.display_path().display()
                )
            })
        })
        .await
        .map_err(|error| format!("workspace restore create task failed: {error}"))?;
        result?;
        return fail_after_apply(target, replacement, fault);
    }
    if let Some(partial) = partial_failure_content(target, fault) {
        OpenedEditFile::new(target.opened_file()?, target.display_path().to_path_buf())
            .write_file(partial, mutation)
            .await?;
        return Err("injected failure after partial workspace restore write".into());
    }
    OpenedEditFile::new(target.opened_file()?, target.display_path().to_path_buf())
        .write_file(content, mutation)
        .await?;
    fail_after_apply(target, replacement, fault)
}

#[derive(Debug)]
#[allow(
    dead_code,
    reason = "constructed only by deterministic restore fault tests"
)]
struct ApplyFault {
    path: PathBuf,
    mode: ApplyFaultMode,
}

#[derive(Debug)]
#[allow(
    dead_code,
    reason = "constructed only by deterministic restore fault tests"
)]
enum ApplyFaultMode {
    AfterApply,
    PartialWrite(Vec<u8>),
}

fn partial_failure_content<'a>(
    target: &FilesystemTarget,
    fault: Option<&'a ApplyFault>,
) -> Option<&'a [u8]> {
    fault.and_then(|fault| {
        (fault.path == target.relative_path())
            .then_some(&fault.mode)
            .and_then(|mode| match mode {
                ApplyFaultMode::PartialWrite(content) => Some(content.as_slice()),
                ApplyFaultMode::AfterApply => None,
            })
    })
}

fn fail_after_apply(
    target: &FilesystemTarget,
    _replacement: &WorkspaceFileSnapshot,
    fault: Option<&ApplyFault>,
) -> Result<(), String> {
    if fault.is_some_and(|fault| {
        fault.path == target.relative_path() && matches!(fault.mode, ApplyFaultMode::AfterApply)
    }) {
        Err("injected failure after workspace restore apply".into())
    } else {
        Ok(())
    }
}

fn validate_plan(plan: &WorkspaceRestorePlan) -> Result<(), WorkspaceRestoreError> {
    if plan.entries.len() > MAX_RESTORE_ENTRIES {
        return Err(invalid(format!(
            "{} entries exceed the limit of {MAX_RESTORE_ENTRIES}",
            plan.entries.len()
        )));
    }
    let mut paths = HashSet::new();
    let mut retained = 0_usize;
    for entry in &plan.entries {
        validate_snapshot(&entry.expected)?;
        validate_snapshot(&entry.replacement)?;
        if entry.expected.path != entry.replacement.path
            || !paths.insert(entry.expected.path.clone())
        {
            return Err(invalid(format!(
                "duplicate or mismatched restore path: {}",
                entry.expected.path.display()
            )));
        }
        retained = retained
            .saturating_add(entry.expected.content.as_ref().map_or(0, Vec::len))
            .saturating_add(entry.replacement.content.as_ref().map_or(0, Vec::len));
    }
    if retained > MAX_RESTORE_BYTES {
        return Err(invalid(format!(
            "restore content exceeds the {MAX_RESTORE_BYTES} byte limit"
        )));
    }
    Ok(())
}

fn validate_snapshot(snapshot: &WorkspaceFileSnapshot) -> Result<(), WorkspaceRestoreError> {
    path_text(&snapshot.path)?;
    if snapshot.revision.len() != 64
        || (!snapshot.exists && snapshot.content.as_deref() != Some(&[]))
        || snapshot
            .content
            .as_deref()
            .is_some_and(|content| revision(content) != snapshot.revision)
    {
        return Err(invalid(format!(
            "invalid file snapshot for {}",
            snapshot.path.display()
        )));
    }
    if snapshot.exists && snapshot.content.is_none() {
        return Err(invalid(format!(
            "replacement content is unavailable for {}",
            snapshot.path.display()
        )));
    }
    Ok(())
}

fn path_text(path: &Path) -> Result<&str, WorkspaceRestoreError> {
    if path.as_os_str().is_empty()
        || path.is_absolute()
        || path
            .components()
            .any(|component| !matches!(component, Component::Normal(_) | Component::CurDir))
    {
        return Err(invalid(format!(
            "restore path is not workspace-relative: {}",
            path.display()
        )));
    }
    path.to_str()
        .ok_or_else(|| invalid("restore path must be valid Unicode"))
}

fn same_revision(left: &WorkspaceFileSnapshot, right: &WorkspaceFileSnapshot) -> bool {
    left.exists == right.exists && left.revision == right.revision
}

fn revision(content: &[u8]) -> String {
    format!("{:x}", Sha256::digest(content))
}

fn invalid(message: impl Into<String>) -> WorkspaceRestoreError {
    WorkspaceRestoreError::Invalid {
        message: message.into(),
    }
}

#[cfg(test)]
mod tests {
    use tempfile::TempDir;

    use super::*;
    use crate::access::WorkspaceAccessHandle;
    use crate::contract::{WorkspaceHandle, WorkspaceKind};

    fn workspace(root: &Path) -> WorkspaceAccessHandle {
        WorkspaceAccessHandle::open(
            WorkspaceHandle::new(WorkspaceKind::Projectless, root).unwrap(),
            None,
            None,
        )
        .unwrap()
    }

    fn file(path: &str, content: &[u8]) -> WorkspaceFileSnapshot {
        WorkspaceFileSnapshot {
            path: path.into(),
            exists: true,
            revision: revision(content),
            content: Some(content.to_vec()),
        }
    }

    fn missing(path: &str) -> WorkspaceFileSnapshot {
        WorkspaceFileSnapshot {
            path: path.into(),
            exists: false,
            revision: revision(&[]),
            content: Some(Vec::new()),
        }
    }

    #[tokio::test]
    async fn snapshot_captures_ignored_and_dependency_files_but_not_git_metadata() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join(".gitignore"), "ignored.txt\n").unwrap();
        std::fs::write(dir.path().join("ignored.txt"), "ignored\n").unwrap();
        std::fs::create_dir_all(dir.path().join("node_modules/pkg")).unwrap();
        std::fs::write(
            dir.path().join("node_modules/pkg/index.js"),
            "module.exports = 1;\n",
        )
        .unwrap();
        std::fs::create_dir_all(dir.path().join(".git/refs")).unwrap();
        std::fs::write(dir.path().join(".git/HEAD"), "ref: refs/heads/main\n").unwrap();

        let snapshot = capture_workspace_snapshot(&workspace(dir.path()))
            .await
            .unwrap();
        assert_eq!(
            snapshot
                .files
                .iter()
                .map(|file| file.path.as_path())
                .collect::<Vec<_>>(),
            vec![
                Path::new(".gitignore"),
                Path::new("ignored.txt"),
                Path::new("node_modules/pkg/index.js"),
            ]
        );
        assert_eq!(
            snapshot
                .file(Path::new("ignored.txt"))
                .and_then(|file| file.content.as_deref()),
            Some(b"ignored\n".as_slice())
        );
    }

    #[tokio::test]
    async fn source_workspace_snapshot_is_rejected() {
        let dir = TempDir::new().unwrap();
        let workspace = WorkspaceAccessHandle::open_source(dir.path().to_path_buf()).unwrap();
        assert_eq!(
            capture_workspace_snapshot(&workspace).await.unwrap_err(),
            WorkspaceSnapshotError::UnsupportedWorkspace {
                kind: WorkspaceKind::Source
            }
        );
    }

    #[tokio::test]
    async fn restores_modified_created_and_deleted_files() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("modified.txt"), "current\n").unwrap();
        std::fs::write(dir.path().join("created.txt"), "created\n").unwrap();
        let workspace = workspace(dir.path());
        let report = restore_workspace_snapshot(
            &workspace,
            WorkspaceRestorePlan {
                entries: vec![
                    WorkspaceRestoreEntry {
                        expected: file("modified.txt", b"current\n"),
                        replacement: file("modified.txt", b"before\n"),
                    },
                    WorkspaceRestoreEntry {
                        expected: file("created.txt", b"created\n"),
                        replacement: missing("created.txt"),
                    },
                    WorkspaceRestoreEntry {
                        expected: missing("deleted.txt"),
                        replacement: file("deleted.txt", b"restored\n"),
                    },
                ],
            },
        )
        .await
        .unwrap();
        assert_eq!(report.restored, 3);
        assert_eq!(
            std::fs::read(dir.path().join("modified.txt")).unwrap(),
            b"before\n"
        );
        assert!(!dir.path().join("created.txt").exists());
        assert_eq!(
            std::fs::read(dir.path().join("deleted.txt")).unwrap(),
            b"restored\n"
        );
    }

    #[tokio::test]
    async fn stale_target_fails_before_any_file_changes() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("first.txt"), "one\n").unwrap();
        std::fs::write(dir.path().join("second.txt"), "changed\n").unwrap();
        let workspace = workspace(dir.path());
        let error = restore_workspace_snapshot(
            &workspace,
            WorkspaceRestorePlan {
                entries: vec![
                    WorkspaceRestoreEntry {
                        expected: file("first.txt", b"one\n"),
                        replacement: file("first.txt", b"restored\n"),
                    },
                    WorkspaceRestoreEntry {
                        expected: file("second.txt", b"two\n"),
                        replacement: file("second.txt", b"restored\n"),
                    },
                ],
            },
        )
        .await
        .unwrap_err();
        assert!(matches!(error, WorkspaceRestoreError::TargetChanged { .. }));
        assert_eq!(
            std::fs::read(dir.path().join("first.txt")).unwrap(),
            b"one\n"
        );
    }

    #[tokio::test]
    async fn source_workspace_is_rejected_before_preflight() {
        let dir = TempDir::new().unwrap();
        let workspace = WorkspaceAccessHandle::open_source(dir.path().to_path_buf()).unwrap();
        let error = restore_workspace_snapshot(
            &workspace,
            WorkspaceRestorePlan {
                entries: Vec::new(),
            },
        )
        .await
        .unwrap_err();
        assert_eq!(
            error,
            WorkspaceRestoreError::UnsupportedWorkspace {
                kind: WorkspaceKind::Source
            }
        );
    }

    #[tokio::test]
    async fn failure_after_apply_rolls_back_current_and_prior_entries() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("first.txt"), "one\n").unwrap();
        std::fs::write(dir.path().join("second.txt"), "two\n").unwrap();
        let workspace = workspace(dir.path());
        let error = restore_workspace_snapshot_inner(
            &workspace,
            WorkspaceRestorePlan {
                entries: vec![
                    WorkspaceRestoreEntry {
                        expected: file("first.txt", b"one\n"),
                        replacement: file("first.txt", b"restored-one\n"),
                    },
                    WorkspaceRestoreEntry {
                        expected: file("second.txt", b"two\n"),
                        replacement: file("second.txt", b"restored-two\n"),
                    },
                ],
            },
            Some(&ApplyFault {
                path: "second.txt".into(),
                mode: ApplyFaultMode::AfterApply,
            }),
        )
        .await
        .unwrap_err();
        assert!(matches!(error, WorkspaceRestoreError::Apply { .. }));
        assert_eq!(
            std::fs::read(dir.path().join("first.txt")).unwrap(),
            b"one\n"
        );
        assert_eq!(
            std::fs::read(dir.path().join("second.txt")).unwrap(),
            b"two\n"
        );
    }

    #[tokio::test]
    async fn unprovable_partial_write_reports_rollback_uncertainty() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("notes.txt"), "before\n").unwrap();
        let workspace = workspace(dir.path());
        let error = restore_workspace_snapshot_inner(
            &workspace,
            WorkspaceRestorePlan {
                entries: vec![WorkspaceRestoreEntry {
                    expected: file("notes.txt", b"before\n"),
                    replacement: file("notes.txt", b"replacement\n"),
                }],
            },
            Some(&ApplyFault {
                path: "notes.txt".into(),
                mode: ApplyFaultMode::PartialWrite(b"partial\n".to_vec()),
            }),
        )
        .await
        .unwrap_err();
        assert!(matches!(error, WorkspaceRestoreError::Rollback { .. }));
        assert_eq!(
            std::fs::read(dir.path().join("notes.txt")).unwrap(),
            b"partial\n"
        );
    }

    #[tokio::test]
    async fn rollback_removes_parents_created_for_a_vacant_target() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("fail.txt"), "before\n").unwrap();
        let workspace = workspace(dir.path());
        let error = restore_workspace_snapshot_inner(
            &workspace,
            WorkspaceRestorePlan {
                entries: vec![
                    WorkspaceRestoreEntry {
                        expected: missing("nested/deep/new.txt"),
                        replacement: file("nested/deep/new.txt", b"created\n"),
                    },
                    WorkspaceRestoreEntry {
                        expected: file("fail.txt", b"before\n"),
                        replacement: file("fail.txt", b"after\n"),
                    },
                ],
            },
            Some(&ApplyFault {
                path: "fail.txt".into(),
                mode: ApplyFaultMode::AfterApply,
            }),
        )
        .await
        .unwrap_err();
        assert!(matches!(error, WorkspaceRestoreError::Apply { .. }));
        assert!(!dir.path().join("nested").exists());
        assert_eq!(
            std::fs::read(dir.path().join("fail.txt")).unwrap(),
            b"before\n"
        );
    }
}
