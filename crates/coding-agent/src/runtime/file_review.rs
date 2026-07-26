use std::io::{Read, Seek, SeekFrom};
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::limits::{
    MAX_FILE_REVIEW_BYTES, MAX_FILE_REVIEW_CONTENT_BYTES, MAX_FILE_REVIEW_DIFF_BYTES,
};
use crate::runtime::capability::{
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
        let snapshot = self.snapshot();
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
            .lock()
            .map_err(|_| review_error(ReviewErrorKind::Unavailable))?;
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::operations::self_healing_edit::runner::SelfHealingEditOutcome;
    use crate::runtime::facade::CodingAgentSessionOptions;

    fn change(path: impl Into<String>) -> CodingAgentFileChangeSnapshot {
        CodingAgentFileChangeSnapshot {
            path: path.into(),
            mutation_kind: "edit".into(),
            operation_id: "op_review".into(),
            tool_call_id: Some("call_review".into()),
            updated_sequence: 7,
            first_changed_line: Some(2),
            added_lines: Some(3),
            removed_lines: Some(1),
            diff: Some("@@ -1 +1 @@\n-old\n+new\n".into()),
        }
    }

    fn request(change: &CodingAgentFileChangeSnapshot) -> CodingAgentFileReviewRequest {
        CodingAgentFileReviewRequest::from(change)
    }

    fn assert_error(error: CodingAgentPublicError, code: &str, retryable: bool) {
        assert_eq!(error.code(), code);
        assert_eq!(error.retryable, retryable);
        let serialized = serde_json::to_string(&error).unwrap();
        assert!(!serialized.contains("outside-secret"));
        assert!(!serialized.contains("review-secret"));
    }

    #[tokio::test]
    async fn session_query_authorizes_current_product_change_and_returns_bounded_dto() {
        let workspace = tempfile::tempdir().unwrap();
        std::fs::create_dir(workspace.path().join("src")).unwrap();
        std::fs::write(workspace.path().join("src/lib.rs"), "one\nnew\nthree\n").unwrap();
        let session = CodingAgentSession::non_persistent(
            CodingAgentSessionOptions::new().with_cwd(workspace.path()),
        )
        .await
        .unwrap();
        assert!(!format!("{session:?}").contains(&workspace.path().display().to_string()));
        let outcome = SelfHealingEditOutcome {
            path: "src/lib.rs".into(),
            message: "edited".into(),
            diff: String::new(),
            patch: String::new(),
            first_changed_line: Some(2),
            attempts: 1,
            diagnostics: Vec::new(),
            check_output: None,
            repair_attempts: Vec::new(),
        };
        session
            .runtime_host
            .event_hub
            .service
            .emit_self_healing_edit_completed("op_review_public", &outcome);
        let snapshot = session.snapshot();
        let current = snapshot.context.changes.first().unwrap();

        let review = session
            .review_changed_file(CodingAgentFileReviewRequest::from(current))
            .await
            .unwrap();

        assert_eq!(review.display_path, "src/lib.rs");
        assert_eq!(review.content, "one\nnew\nthree\n");
        assert_eq!(review.total_bytes, 14);
        assert_eq!(review.line_count, 3);
        assert!(!review.content_truncated);
        assert_eq!(review.first_changed_line, Some(2));
        assert_eq!(
            review.external_editor_target.as_ref().unwrap().path(),
            workspace.path().join("src/lib.rs")
        );
    }

    #[tokio::test]
    async fn identity_and_revision_must_match_the_current_product_change() {
        let workspace = tempfile::tempdir().unwrap();
        std::fs::write(workspace.path().join("review.txt"), "review").unwrap();
        let current = change("review.txt");

        let mut unauthorized = request(&current);
        unauthorized.change.operation_id = "op_untrusted".into();
        assert_error(
            review_changed_file(
                workspace.path(),
                std::slice::from_ref(&current),
                unauthorized,
            )
            .await
            .unwrap_err(),
            "file_review_change_unauthorized",
            false,
        );

        let mut stale = request(&current);
        stale.revision = CodingAgentFileRevision::new(6);
        assert_error(
            review_changed_file(workspace.path(), &[current], stale)
                .await
                .unwrap_err(),
            "file_review_revision_stale",
            true,
        );
    }

    #[tokio::test]
    async fn outside_project_and_missing_or_non_file_targets_fail_closed() {
        let parent = tempfile::tempdir().unwrap();
        let workspace = parent.path().join("workspace");
        std::fs::create_dir(&workspace).unwrap();
        std::fs::write(parent.path().join("outside-secret.txt"), "outside-secret").unwrap();

        let outside = change("../outside-secret.txt");
        assert_error(
            review_changed_file(
                &workspace,
                std::slice::from_ref(&outside),
                request(&outside),
            )
            .await
            .unwrap_err(),
            "file_review_outside_project",
            false,
        );

        let missing = change("missing.txt");
        assert_error(
            review_changed_file(
                &workspace,
                std::slice::from_ref(&missing),
                request(&missing),
            )
            .await
            .unwrap_err(),
            "file_review_not_found",
            true,
        );

        std::fs::create_dir(workspace.join("directory")).unwrap();
        let directory = change("directory");
        assert_error(
            review_changed_file(
                &workspace,
                std::slice::from_ref(&directory),
                request(&directory),
            )
            .await
            .unwrap_err(),
            "file_review_not_file",
            false,
        );
    }

    #[tokio::test]
    async fn deleted_and_renamed_change_facts_have_deterministic_review_outcomes() {
        let workspace = tempfile::tempdir().unwrap();

        let mut deleted = change("deleted.rs");
        deleted.mutation_kind = "delete".into();
        assert_error(
            review_changed_file(
                workspace.path(),
                std::slice::from_ref(&deleted),
                request(&deleted),
            )
            .await
            .unwrap_err(),
            "file_review_not_found",
            true,
        );

        std::fs::write(workspace.path().join("renamed.rs"), "renamed\n").unwrap();
        let mut renamed = change("renamed.rs");
        renamed.mutation_kind = "rename".into();
        let review = review_changed_file(
            workspace.path(),
            std::slice::from_ref(&renamed),
            request(&renamed),
        )
        .await
        .unwrap();
        assert_eq!(review.display_path, "renamed.rs");
        assert_eq!(review.mutation_kind, "rename");
        assert_eq!(review.content, "renamed\n");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn symlink_swap_is_rejected_before_content_or_editor_target_is_returned() {
        use std::os::unix::fs::symlink;

        let workspace = tempfile::tempdir().unwrap();
        std::fs::write(workspace.path().join("review-secret.txt"), "authorized").unwrap();
        std::fs::write(workspace.path().join("replacement.txt"), "replacement").unwrap();
        let current = change("review-secret.txt");
        std::fs::remove_file(workspace.path().join("review-secret.txt")).unwrap();
        symlink(
            workspace.path().join("replacement.txt"),
            workspace.path().join("review-secret.txt"),
        )
        .unwrap();

        assert_error(
            review_changed_file(
                workspace.path(),
                std::slice::from_ref(&current),
                request(&current),
            )
            .await
            .unwrap_err(),
            "file_review_symlink_disallowed",
            false,
        );
    }

    #[tokio::test]
    async fn editor_target_must_still_name_the_reviewed_opened_object() {
        let workspace = tempfile::tempdir().unwrap();
        let path = workspace.path().join("review.txt");
        std::fs::write(&path, "reviewed\n").unwrap();
        let current = change("review.txt");
        let review = review_changed_file(
            workspace.path(),
            std::slice::from_ref(&current),
            request(&current),
        )
        .await
        .unwrap();
        let target = review.external_editor_target.unwrap();

        revalidate_external_editor_target(workspace.path(), &target)
            .await
            .unwrap();

        let replacement = workspace.path().join("replacement.txt");
        std::fs::write(&replacement, "replacement\n").unwrap();
        std::fs::remove_file(&path).unwrap();
        std::fs::rename(replacement, path).unwrap();

        assert_error(
            revalidate_external_editor_target(workspace.path(), &target)
                .await
                .unwrap_err(),
            "file_review_target_changed",
            true,
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn editor_target_revalidation_rejects_a_post_review_symlink_swap() {
        use std::os::unix::fs::symlink;

        let workspace = tempfile::tempdir().unwrap();
        let path = workspace.path().join("review.txt");
        let replacement = workspace.path().join("replacement.txt");
        std::fs::write(&path, "reviewed\n").unwrap();
        std::fs::write(&replacement, "replacement\n").unwrap();
        let current = change("review.txt");
        let review = review_changed_file(
            workspace.path(),
            std::slice::from_ref(&current),
            request(&current),
        )
        .await
        .unwrap();
        let target = review.external_editor_target.unwrap();

        std::fs::remove_file(&path).unwrap();
        symlink(replacement, path).unwrap();

        assert_error(
            revalidate_external_editor_target(workspace.path(), &target)
                .await
                .unwrap_err(),
            "file_review_symlink_disallowed",
            false,
        );
    }

    #[tokio::test]
    async fn oversized_binary_and_malformed_utf8_inputs_are_categorized() {
        let workspace = tempfile::tempdir().unwrap();

        std::fs::write(
            workspace.path().join("oversized.txt"),
            vec![b'x'; MAX_FILE_REVIEW_BYTES + 1],
        )
        .unwrap();
        let oversized = change("oversized.txt");
        assert_error(
            review_changed_file(
                workspace.path(),
                std::slice::from_ref(&oversized),
                request(&oversized),
            )
            .await
            .unwrap_err(),
            "file_review_too_large",
            false,
        );

        std::fs::write(workspace.path().join("binary.dat"), b"text\0binary").unwrap();
        let binary = change("binary.dat");
        assert_error(
            review_changed_file(
                workspace.path(),
                std::slice::from_ref(&binary),
                request(&binary),
            )
            .await
            .unwrap_err(),
            "file_review_binary",
            false,
        );

        std::fs::write(
            workspace.path().join("malformed.txt"),
            [0xf0, 0x28, 0x8c, 0x28],
        )
        .unwrap();
        let malformed = change("malformed.txt");
        assert_error(
            review_changed_file(
                workspace.path(),
                std::slice::from_ref(&malformed),
                request(&malformed),
            )
            .await
            .unwrap_err(),
            "file_review_invalid_utf8",
            false,
        );
    }

    #[tokio::test]
    async fn safe_file_and_diff_payloads_are_truncated_on_utf8_boundaries() {
        let workspace = tempfile::tempdir().unwrap();
        let content = "界".repeat(MAX_FILE_REVIEW_CONTENT_BYTES / 3 + 32);
        std::fs::write(workspace.path().join("large.txt"), &content).unwrap();
        let mut current = change("large.txt");
        current.diff = Some("差".repeat(MAX_FILE_REVIEW_DIFF_BYTES / 3 + 32));

        let review = review_changed_file(
            workspace.path(),
            std::slice::from_ref(&current),
            request(&current),
        )
        .await
        .unwrap();

        assert!(review.content_truncated);
        assert!(review.content.len() <= MAX_FILE_REVIEW_CONTENT_BYTES);
        assert!(review.content.is_char_boundary(review.content.len()));
        assert!(review.diff_truncated);
        assert!(review.diff.as_ref().unwrap().len() <= MAX_FILE_REVIEW_DIFF_BYTES);
    }
}
