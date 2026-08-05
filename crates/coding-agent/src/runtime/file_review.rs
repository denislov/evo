use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Component, Path, PathBuf};

use change_tracker::{ChangeSource, HunkId, RejectPlan, RejectReplacement, TrackingContext};
use serde::{Deserialize, Serialize};

use crate::limits::{
    MAX_FILE_REVIEW_BYTES, MAX_FILE_REVIEW_CONTENT_BYTES, MAX_FILE_REVIEW_DIFF_BYTES,
};
use crate::mutex::MutexExt;
use crate::runtime::facade::{
    CodingAgentErrorCategory, CodingAgentErrorContext, CodingAgentFileChangeSnapshot,
    CodingAgentPublicError, CodingAgentSession,
};
use crate::runtime::intent::{IntentRouter, QueryIntent};
use crate::tools::filesystem::mutation_receipt::content_revision;
use crate::tools::filesystem::mutation_receipt::{bounded_diff, receipt};
use workspace_runtime::api::{
    FileMutation, FilesystemReviewTargetError, FilesystemTarget, WorkspaceAccessHandle,
};

#[derive(Debug, Clone, PartialEq, Eq, Hash, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CodingAgentFileChangeIdentity {
    pub operation_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    pub path: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Deserialize, Serialize)]
#[serde(transparent)]
pub struct CodingAgentFileRevision(u64);

impl CodingAgentFileRevision {
    pub fn new(value: u64) -> Self {
        Self(value)
    }

    pub fn value(self) -> u64 {
        self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CodingAgentFileReviewRequest {
    pub change: CodingAgentFileChangeIdentity,
    pub revision: CodingAgentFileRevision,
}

impl CodingAgentFileReviewRequest {
    pub fn new(change: CodingAgentFileChangeIdentity, revision: CodingAgentFileRevision) -> Self {
        Self { change, revision }
    }
}

impl From<&CodingAgentFileChangeSnapshot> for CodingAgentFileReviewRequest {
    fn from(change: &CodingAgentFileChangeSnapshot) -> Self {
        Self {
            change: CodingAgentFileChangeIdentity {
                operation_id: change.operation_id.clone(),
                tool_call_id: change.tool_call_id.clone(),
                path: change.path.clone(),
            },
            revision: CodingAgentFileRevision::new(change.updated_sequence),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CodingAgentFileReviewActionRequest {
    pub change: CodingAgentFileChangeIdentity,
    pub revision: CodingAgentFileRevision,
    pub after_revision: String,
}

impl From<&CodingAgentFileChangeSnapshot> for CodingAgentFileReviewActionRequest {
    fn from(change: &CodingAgentFileChangeSnapshot) -> Self {
        Self {
            change: CodingAgentFileChangeIdentity {
                operation_id: change.operation_id.clone(),
                tool_call_id: change.tool_call_id.clone(),
                path: change.path.clone(),
            },
            revision: CodingAgentFileRevision::new(change.updated_sequence),
            after_revision: change.after_revision.clone(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CodingAgentHunkReviewActionRequest {
    #[serde(flatten)]
    pub file: CodingAgentFileReviewActionRequest,
    pub hunk_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CodingAgentExternalEditorTarget {
    path: PathBuf,
    project_relative_path: String,
    identity_fingerprint: String,
}

impl CodingAgentExternalEditorTarget {
    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn project_relative_path(&self) -> &str {
        &self.project_relative_path
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CodingAgentFileReview {
    pub change: CodingAgentFileChangeIdentity,
    pub revision: CodingAgentFileRevision,
    pub display_path: String,
    pub mutation_kind: String,
    pub content: String,
    pub total_bytes: usize,
    pub line_count: usize,
    pub content_truncated: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diff: Option<String>,
    pub diff_truncated: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub first_changed_line: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub added_lines: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub removed_lines: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub external_editor_target: Option<CodingAgentExternalEditorTarget>,
}

impl CodingAgentSession {
    pub async fn open_change(
        &self,
        request: CodingAgentFileReviewRequest,
    ) -> Result<CodingAgentFileReview, CodingAgentPublicError> {
        IntentRouter::admit_query(
            &self.runtime_host.operation_supervisor.control,
            QueryIntent::ChangedFileReview,
        );
        let changes = self.runtime_host.review_service.product_changes()?;
        open_change(self.runtime_host.project_root.as_path(), &changes, request).await
    }

    pub fn list_changes(
        &self,
    ) -> Result<Vec<CodingAgentFileChangeSnapshot>, CodingAgentPublicError> {
        IntentRouter::admit_query(
            &self.runtime_host.operation_supervisor.control,
            QueryIntent::ChangedFileReview,
        );
        self.runtime_host
            .review_service
            .product_changes()
            .map_err(CodingAgentPublicError::from)
    }

    pub async fn accept_hunk(
        &self,
        request: CodingAgentHunkReviewActionRequest,
    ) -> Result<(), CodingAgentPublicError> {
        let change = self.authorize_review_action(&request.file)?;
        let target =
            prepare_action_target(self.runtime_host.project_root.as_path(), &change).await?;
        verify_action_target(&target, &change).await?;
        let hunk_id = HunkId::parse(request.hunk_id).map_err(map_tracker_error)?;
        let handle = self.runtime_host.review_service.tracker_handle()?;
        handle
            .accept_hunk(
                &change.path,
                change.updated_sequence,
                hunk_id,
                &change.after_revision,
                target.target_fingerprint(),
            )
            .await
            .map_err(map_tracker_error)?;
        self.runtime_host.review_service.refresh_latest(&handle)?;
        Ok(())
    }

    pub async fn accept_file(
        &self,
        request: CodingAgentFileReviewActionRequest,
    ) -> Result<(), CodingAgentPublicError> {
        let change = self.authorize_review_action(&request)?;
        let target =
            prepare_action_target(self.runtime_host.project_root.as_path(), &change).await?;
        verify_action_target(&target, &change).await?;
        let handle = self.runtime_host.review_service.tracker_handle()?;
        handle
            .accept_file(
                &change.path,
                change.updated_sequence,
                &change.after_revision,
                target.target_fingerprint(),
            )
            .await
            .map_err(map_tracker_error)?;
        self.runtime_host.review_service.refresh_latest(&handle)?;
        Ok(())
    }

    pub async fn reject_hunk(
        &self,
        request: CodingAgentHunkReviewActionRequest,
    ) -> Result<(), CodingAgentPublicError> {
        let change = self.authorize_review_action(&request.file)?;
        let hunk_id = HunkId::parse(request.hunk_id).map_err(map_tracker_error)?;
        reject_change(self, &change, Some(hunk_id)).await
    }

    pub async fn reject_file(
        &self,
        request: CodingAgentFileReviewActionRequest,
    ) -> Result<(), CodingAgentPublicError> {
        let change = self.authorize_review_action(&request)?;
        reject_change(self, &change, None).await
    }

    pub async fn discard_child_proposal(
        &mut self,
        worktree_id: String,
    ) -> Result<String, CodingAgentPublicError> {
        let outcome = self
            .run(crate::runtime::facade::CodingAgentOperation::DiscardChildWorktree { worktree_id })
            .await?;
        let crate::runtime::facade::CodingAgentOperationOutcome::WorktreeDiscarded {
            worktree_id,
            ..
        } = outcome
        else {
            return Err(review_error(ReviewErrorKind::Unavailable));
        };
        Ok(worktree_id)
    }

    fn authorize_review_action(
        &self,
        request: &CodingAgentFileReviewActionRequest,
    ) -> Result<CodingAgentFileChangeSnapshot, CodingAgentPublicError> {
        IntentRouter::admit_query(
            &self.runtime_host.operation_supervisor.control,
            QueryIntent::ChangedFileReview,
        );
        let changes = self.runtime_host.review_service.product_changes()?;
        let change = authorize_change_identity(&changes, &request.change, request.revision)?;
        if change.after_revision != request.after_revision {
            return Err(review_error(ReviewErrorKind::StaleRevision));
        }
        Ok(change.clone())
    }

    /// Revalidate a product-issued editor target immediately before an
    /// adapter launches an external editor.
    ///
    /// The target is intentionally opaque to adapters apart from its display
    /// path. Reopening it through the session filesystem authority and
    /// comparing the opened-object fingerprint prevents a stale review DTO
    /// from authorizing a path that has since been replaced.
    pub async fn revalidate_external_editor_target(
        &self,
        target: &CodingAgentExternalEditorTarget,
    ) -> Result<(), CodingAgentPublicError> {
        IntentRouter::admit_query(
            &self.runtime_host.operation_supervisor.control,
            QueryIntent::ChangedFileReview,
        );
        revalidate_external_editor_target(self.runtime_host.project_root.as_path(), target).await
    }
}

async fn revalidate_external_editor_target(
    project_root: &Path,
    target: &CodingAgentExternalEditorTarget,
) -> Result<(), CodingAgentPublicError> {
    let filesystem = WorkspaceAccessHandle::open_source(project_root.to_path_buf())
        .map_err(|_| review_error(ReviewErrorKind::Unavailable))?;
    let current = filesystem
        .prepare_workspace_review_target(&target.project_relative_path)
        .await
        .map_err(map_target_error)?;
    if current.display_path() != target.path
        || current.target_fingerprint() != target.identity_fingerprint
    {
        return Err(review_error(ReviewErrorKind::TargetChanged));
    }
    Ok(())
}

async fn prepare_action_target(
    project_root: &Path,
    change: &CodingAgentFileChangeSnapshot,
) -> Result<FilesystemTarget, CodingAgentPublicError> {
    let filesystem = WorkspaceAccessHandle::open_source(project_root.to_path_buf())
        .map_err(|_| review_error(ReviewErrorKind::Unavailable))?;
    if change.after_exists {
        filesystem
            .prepare_workspace_review_target(&change.path)
            .await
            .map_err(map_target_error)
    } else {
        filesystem
            .prepare_target_for_tool("write", &change.path)
            .await
            .map_err(|_| review_error(ReviewErrorKind::Unavailable))
    }
}

async fn prepare_reject_target(
    project_root: &Path,
    path: &str,
) -> Result<FilesystemTarget, CodingAgentPublicError> {
    WorkspaceAccessHandle::open_source(project_root.to_path_buf())
        .map_err(|_| review_error(ReviewErrorKind::Unavailable))?
        .prepare_target_for_tool("write", path)
        .await
        .map_err(|_| review_error(ReviewErrorKind::Unavailable))
}

async fn verify_action_target(
    target: &FilesystemTarget,
    change: &CodingAgentFileChangeSnapshot,
) -> Result<Option<Vec<u8>>, CodingAgentPublicError> {
    if !change.after_exists {
        if !target.is_vacant() {
            return Err(review_error(ReviewErrorKind::TargetChanged));
        }
        target
            .revalidate_identity()
            .map_err(|_| review_error(ReviewErrorKind::TargetChanged))?;
        return Ok(None);
    }
    if target.is_vacant() {
        return Err(review_error(ReviewErrorKind::TargetChanged));
    }
    let bytes = read_bounded_review_file(target).await?;
    if content_revision(&bytes) != change.after_revision {
        return Err(review_error(ReviewErrorKind::StaleRevision));
    }
    target
        .revalidate_identity()
        .map_err(|_| review_error(ReviewErrorKind::TargetChanged))?;
    Ok(Some(bytes))
}

async fn reject_change(
    session: &CodingAgentSession,
    change: &CodingAgentFileChangeSnapshot,
    hunk_id: Option<HunkId>,
) -> Result<(), CodingAgentPublicError> {
    let target =
        prepare_reject_target(session.runtime_host.project_root.as_path(), &change.path).await?;
    let mutation = FileMutation::begin(&target)
        .await
        .map_err(|_| review_error(ReviewErrorKind::Unavailable))?;
    let _ = verify_action_target(&target, change).await?;
    let handle = session.runtime_host.review_service.tracker_handle()?;
    let plan = match hunk_id {
        Some(hunk_id) => {
            handle
                .prepare_reject_hunk(
                    &change.path,
                    change.updated_sequence,
                    hunk_id,
                    &change.after_revision,
                    target.target_fingerprint(),
                )
                .await
        }
        None => {
            handle
                .prepare_reject_file(
                    &change.path,
                    change.updated_sequence,
                    &change.after_revision,
                    target.target_fingerprint(),
                )
                .await
        }
    }
    .map_err(map_tracker_error)?;
    let before = verify_action_target(&target, change).await?;
    let after = commit_reject_plan(target, mutation, &plan).await?;
    let before_text = before
        .as_deref()
        .map_or(Some(""), |bytes| std::str::from_utf8(bytes).ok());
    let after_text = after
        .as_deref()
        .map_or(Some(""), |bytes| std::str::from_utf8(bytes).ok());
    let diff = before_text
        .zip(after_text)
        .map(|(before, after)| {
            crate::tools::filesystem::diff::generate_unified_patch(&change.path, before, after)
        })
        .and_then(bounded_diff);
    let receipt = receipt(
        change.path.clone(),
        plan.target_fingerprint,
        before.as_deref(),
        after.as_deref(),
        "review_reject",
        diff,
    );
    handle
        .record_receipt(
            receipt,
            ChangeSource::HookEdit,
            TrackingContext {
                session_id: change.session_id.clone().unwrap_or_else(|| "review".into()),
                turn_id: change.turn_id.clone().unwrap_or_else(|| "review".into()),
                operation_id: change.operation_id.clone(),
                tool_call_id: Some("review.reject".into()),
            },
        )
        .await
        .map_err(map_tracker_error)?;
    session
        .runtime_host
        .review_service
        .refresh_latest(&handle)?;
    Ok(())
}

async fn commit_reject_plan(
    target: FilesystemTarget,
    mutation: workspace_runtime::api::MutationGuard,
    plan: &RejectPlan,
) -> Result<Option<Vec<u8>>, CodingAgentPublicError> {
    if plan.target_fingerprint != target.target_fingerprint()
        || plan.expected_exists == target.is_vacant()
    {
        return Err(review_error(ReviewErrorKind::TargetChanged));
    }
    let replacement = plan.replacement.clone();
    let after = match &replacement {
        RejectReplacement::Write(bytes) => Some(bytes.clone()),
        RejectReplacement::Delete => None,
    };
    tokio::task::spawn_blocking(move || {
        let _mutation = mutation;
        target.revalidate_identity()?;
        match replacement {
            RejectReplacement::Delete => target.remove_file(),
            RejectReplacement::Write(bytes) if target.is_vacant() => {
                let mut file = target.create_vacant_file()?;
                file.write_all(&bytes).map_err(|error| error.to_string())?;
                file.sync_all().map_err(|error| error.to_string())
            }
            RejectReplacement::Write(bytes) => {
                let file = target.opened_file()?;
                let mut file = file
                    .lock_resource("review reject opened file")
                    .map_err(|error| error.to_string())?;
                file.seek(SeekFrom::Start(0))
                    .map_err(|error| error.to_string())?;
                file.set_len(0).map_err(|error| error.to_string())?;
                file.write_all(&bytes).map_err(|error| error.to_string())?;
                file.sync_all().map_err(|error| error.to_string())
            }
        }
    })
    .await
    .map_err(|_| review_error(ReviewErrorKind::Unavailable))?
    .map_err(|_| review_error(ReviewErrorKind::TargetChanged))?;
    Ok(after)
}

async fn open_change(
    project_root: &Path,
    changes: &[CodingAgentFileChangeSnapshot],
    request: CodingAgentFileReviewRequest,
) -> Result<CodingAgentFileReview, CodingAgentPublicError> {
    let change = authorize_change(changes, &request)?;
    let filesystem = WorkspaceAccessHandle::open_source(project_root.to_path_buf())
        .map_err(|_| review_error(ReviewErrorKind::Unavailable))?;
    let target = if change.after_exists {
        filesystem
            .prepare_workspace_review_target(&change.path)
            .await
            .map_err(map_target_error)?
    } else {
        filesystem
            .prepare_target_for_tool("write", &change.path)
            .await
            .map_err(|_| review_error(ReviewErrorKind::Unavailable))?
    };
    let bytes = if change.after_exists {
        read_bounded_review_file(&target).await?
    } else if target.is_vacant() {
        Vec::new()
    } else {
        return Err(review_error(ReviewErrorKind::TargetChanged));
    };
    if content_revision(&bytes) != change.after_revision {
        return Err(review_error(ReviewErrorKind::StaleRevision));
    }
    if looks_binary(&bytes) {
        return Err(review_error(ReviewErrorKind::Binary));
    }
    let text =
        std::str::from_utf8(&bytes).map_err(|_| review_error(ReviewErrorKind::MalformedUtf8))?;
    let line_count = text.lines().count();
    let (content, content_truncated) = bounded_text(text, MAX_FILE_REVIEW_CONTENT_BYTES);
    let (diff, diff_truncated) = match change.diff.as_deref() {
        Some(diff) => {
            let (diff, truncated) = bounded_text(diff, MAX_FILE_REVIEW_DIFF_BYTES);
            (Some(diff), truncated)
        }
        None => (None, false),
    };
    let display_path = project_relative_display(target.relative_path());
    let external_editor_target = change
        .after_exists
        .then(|| CodingAgentExternalEditorTarget {
            path: target.display_path().to_path_buf(),
            project_relative_path: display_path.clone(),
            identity_fingerprint: target.target_fingerprint().to_owned(),
        });

    Ok(CodingAgentFileReview {
        change: request.change,
        revision: request.revision,
        display_path,
        mutation_kind: change.mutation_kind.clone(),
        content,
        total_bytes: bytes.len(),
        line_count,
        content_truncated,
        diff,
        diff_truncated,
        first_changed_line: change.first_changed_line,
        added_lines: change.added_lines,
        removed_lines: change.removed_lines,
        external_editor_target,
    })
}

fn authorize_change<'a>(
    changes: &'a [CodingAgentFileChangeSnapshot],
    request: &CodingAgentFileReviewRequest,
) -> Result<&'a CodingAgentFileChangeSnapshot, CodingAgentPublicError> {
    authorize_change_identity(changes, &request.change, request.revision)
}

fn authorize_change_identity<'a>(
    changes: &'a [CodingAgentFileChangeSnapshot],
    identity: &CodingAgentFileChangeIdentity,
    revision: CodingAgentFileRevision,
) -> Result<&'a CodingAgentFileChangeSnapshot, CodingAgentPublicError> {
    let Some(change) = changes.iter().find(|change| {
        change.operation_id == identity.operation_id
            && change.tool_call_id == identity.tool_call_id
            && change.path == identity.path
    }) else {
        return Err(review_error(ReviewErrorKind::Unauthorized));
    };
    if change.updated_sequence != revision.value() {
        return Err(review_error(ReviewErrorKind::StaleRevision));
    }
    Ok(change)
}

async fn read_bounded_review_file(
    target: &FilesystemTarget,
) -> Result<Vec<u8>, CodingAgentPublicError> {
    let target = target.clone();
    tokio::task::spawn_blocking(move || {
        let file = target
            .opened_file()
            .map_err(|_| review_error(ReviewErrorKind::Unavailable))?;
        let mut file = file
            .lock_resource("file review opened file")
            .map_err(CodingAgentPublicError::from)?;
        file.seek(SeekFrom::Start(0))
            .map_err(|_| review_error(ReviewErrorKind::Unavailable))?;
        let metadata = file
            .metadata()
            .map_err(|_| review_error(ReviewErrorKind::Unavailable))?;
        if metadata.len() > MAX_FILE_REVIEW_BYTES as u64 {
            return Err(review_error(ReviewErrorKind::Oversized));
        }
        let mut bytes = Vec::with_capacity(metadata.len() as usize);
        std::io::Read::by_ref(&mut *file)
            .take(MAX_FILE_REVIEW_BYTES as u64 + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| review_error(ReviewErrorKind::Unavailable))?;
        if bytes.len() > MAX_FILE_REVIEW_BYTES {
            return Err(review_error(ReviewErrorKind::Oversized));
        }
        Ok(bytes)
    })
    .await
    .map_err(|_| review_error(ReviewErrorKind::Unavailable))?
}

fn looks_binary(bytes: &[u8]) -> bool {
    const SAMPLE_BYTES: usize = 8 * 1024;
    let sample = &bytes[..bytes.len().min(SAMPLE_BYTES)];
    if sample.contains(&0) {
        return true;
    }
    let controls = sample
        .iter()
        .filter(|byte| matches!(byte, 0x01..=0x08 | 0x0b | 0x0c | 0x0e..=0x1f | 0x7f))
        .count();
    !sample.is_empty() && controls.saturating_mul(10) > sample.len() * 3
}

fn bounded_text(value: &str, max_bytes: usize) -> (String, bool) {
    if value.len() <= max_bytes {
        return (value.to_owned(), false);
    }
    let mut end = max_bytes;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    (value[..end].to_owned(), true)
}

fn project_relative_display(path: &Path) -> String {
    path.components()
        .filter_map(|component| match component {
            Component::Normal(part) => Some(part.to_string_lossy().into_owned()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("/")
}

fn map_target_error(error: FilesystemReviewTargetError) -> CodingAgentPublicError {
    let kind = match error {
        FilesystemReviewTargetError::OutsideProject => ReviewErrorKind::OutsideProject,
        FilesystemReviewTargetError::SymlinkDisallowed => ReviewErrorKind::SymlinkDisallowed,
        FilesystemReviewTargetError::NotFound => ReviewErrorKind::NotFound,
        FilesystemReviewTargetError::NotFile => ReviewErrorKind::NotFile,
        FilesystemReviewTargetError::TargetChanged => ReviewErrorKind::TargetChanged,
        FilesystemReviewTargetError::Inaccessible => ReviewErrorKind::Unavailable,
    };
    review_error(kind)
}

fn map_tracker_error(error: change_tracker::ChangeTrackerError) -> CodingAgentPublicError {
    match &error {
        change_tracker::ChangeTrackerError::InvalidFact { message }
            if message.contains("fingerprint") =>
        {
            review_error(ReviewErrorKind::TargetChanged)
        }
        change_tracker::ChangeTrackerError::InvalidFact { message }
            if message.contains("stale")
                || message.contains("no longer active")
                || message.contains("workspace changed") =>
        {
            review_error(ReviewErrorKind::StaleRevision)
        }
        change_tracker::ChangeTrackerError::InvalidFact { .. } => {
            review_error(ReviewErrorKind::ActionUnavailable)
        }
        _ => review_error(ReviewErrorKind::Unavailable),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ReviewErrorKind {
    Unauthorized,
    StaleRevision,
    OutsideProject,
    SymlinkDisallowed,
    NotFound,
    NotFile,
    TargetChanged,
    Oversized,
    Binary,
    MalformedUtf8,
    Unavailable,
    ActionUnavailable,
}

fn review_error(kind: ReviewErrorKind) -> CodingAgentPublicError {
    let (category, code, retryable, summary, context) = match kind {
        ReviewErrorKind::Unauthorized => (
            CodingAgentErrorCategory::Input,
            "file_review_change_unauthorized",
            false,
            "The changed-file identity is not authorized by the current product snapshot.",
            CodingAgentErrorContext::None,
        ),
        ReviewErrorKind::StaleRevision => (
            CodingAgentErrorCategory::Concurrency,
            "file_review_revision_stale",
            true,
            "The changed-file revision is stale; refresh the product snapshot and retry.",
            CodingAgentErrorContext::None,
        ),
        ReviewErrorKind::OutsideProject => (
            CodingAgentErrorCategory::Capability,
            "file_review_outside_project",
            false,
            "The changed file is outside the product project boundary.",
            CodingAgentErrorContext::None,
        ),
        ReviewErrorKind::SymlinkDisallowed => (
            CodingAgentErrorCategory::Capability,
            "file_review_symlink_disallowed",
            false,
            "The changed-file review path contains a symbolic link or junction.",
            CodingAgentErrorContext::None,
        ),
        ReviewErrorKind::NotFound => (
            CodingAgentErrorCategory::Resource,
            "file_review_not_found",
            true,
            "The changed file is no longer available.",
            CodingAgentErrorContext::None,
        ),
        ReviewErrorKind::NotFile => (
            CodingAgentErrorCategory::Input,
            "file_review_not_file",
            false,
            "The changed-file review target is not a regular file.",
            CodingAgentErrorContext::None,
        ),
        ReviewErrorKind::TargetChanged => (
            CodingAgentErrorCategory::Concurrency,
            "file_review_target_changed",
            true,
            "The changed-file target changed while it was being validated.",
            CodingAgentErrorContext::None,
        ),
        ReviewErrorKind::Oversized => (
            CodingAgentErrorCategory::Capacity,
            "file_review_too_large",
            false,
            "The changed file exceeds the product review safety limit.",
            CodingAgentErrorContext::Capacity {
                limit: MAX_FILE_REVIEW_BYTES,
            },
        ),
        ReviewErrorKind::Binary => (
            CodingAgentErrorCategory::Input,
            "file_review_binary",
            false,
            "The changed file is binary and cannot be rendered as text.",
            CodingAgentErrorContext::None,
        ),
        ReviewErrorKind::MalformedUtf8 => (
            CodingAgentErrorCategory::Input,
            "file_review_invalid_utf8",
            false,
            "The changed file is not valid UTF-8 text.",
            CodingAgentErrorContext::None,
        ),
        ReviewErrorKind::Unavailable => (
            CodingAgentErrorCategory::Resource,
            "file_review_unavailable",
            true,
            "The changed file could not be read through the product filesystem authority.",
            CodingAgentErrorContext::None,
        ),
        ReviewErrorKind::ActionUnavailable => (
            CodingAgentErrorCategory::Capability,
            "file_review_action_unavailable",
            false,
            "The changed file does not retain patchable baseline content for this action.",
            CodingAgentErrorContext::None,
        ),
    };
    CodingAgentPublicError {
        category,
        code: code.into(),
        retryable,
        summary: summary.into(),
        context,
    }
}

#[cfg(test)]
#[path = "file_review_tests.rs"]
mod tests;
