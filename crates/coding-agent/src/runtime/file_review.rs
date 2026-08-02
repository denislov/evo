use std::io::{Read, Seek, SeekFrom};
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::limits::{
    MAX_FILE_REVIEW_BYTES, MAX_FILE_REVIEW_CONTENT_BYTES, MAX_FILE_REVIEW_DIFF_BYTES,
};
use crate::mutex::MutexExt;
use crate::platform::fs::capability::{
    FilesystemCapability, FilesystemReviewTargetError, FilesystemTarget,
};
use crate::runtime::facade::{
    CodingAgentErrorCategory, CodingAgentErrorContext, CodingAgentFileChangeSnapshot,
    CodingAgentPublicError, CodingAgentSession,
};
use crate::runtime::intent::{IntentRouter, QueryIntent};

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
    pub async fn review_changed_file(
        &self,
        request: CodingAgentFileReviewRequest,
    ) -> Result<CodingAgentFileReview, CodingAgentPublicError> {
        IntentRouter::admit_query(
            &self.runtime_host.operation_supervisor.control,
            QueryIntent::ChangedFileReview,
        );
        let snapshot = self.snapshot()?;
        review_changed_file(
            self.runtime_host.project_root.as_path(),
            &snapshot.context.changes,
            request,
        )
        .await
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
    let filesystem = FilesystemCapability::new(project_root.to_path_buf())
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

async fn review_changed_file(
    project_root: &Path,
    changes: &[CodingAgentFileChangeSnapshot],
    request: CodingAgentFileReviewRequest,
) -> Result<CodingAgentFileReview, CodingAgentPublicError> {
    let change = authorize_change(changes, &request)?;
    let filesystem = FilesystemCapability::new(project_root.to_path_buf())
        .map_err(|_| review_error(ReviewErrorKind::Unavailable))?;
    let target = filesystem
        .prepare_workspace_review_target(&change.path)
        .await
        .map_err(map_target_error)?;
    let bytes = read_bounded_review_file(&target).await?;
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
    let external_editor_target = CodingAgentExternalEditorTarget {
        path: target.display_path().to_path_buf(),
        project_relative_path: display_path.clone(),
        identity_fingerprint: target.target_fingerprint().to_owned(),
    };

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
        external_editor_target: Some(external_editor_target),
    })
}

fn authorize_change<'a>(
    changes: &'a [CodingAgentFileChangeSnapshot],
    request: &CodingAgentFileReviewRequest,
) -> Result<&'a CodingAgentFileChangeSnapshot, CodingAgentPublicError> {
    let Some(change) = changes.iter().find(|change| {
        change.operation_id == request.change.operation_id
            && change.tool_call_id == request.change.tool_call_id
    }) else {
        return Err(review_error(ReviewErrorKind::Unauthorized));
    };
    if change.path != request.change.path {
        return Err(review_error(ReviewErrorKind::Unauthorized));
    }
    if change.updated_sequence != request.revision.value() {
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
        file.by_ref()
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
    };
    CodingAgentPublicError {
        category,
        code: code.into(),
        retryable,
        summary: summary.into(),
        context,
    }
}
