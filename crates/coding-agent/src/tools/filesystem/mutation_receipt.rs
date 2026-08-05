use change_tracker::ChangeReceipt;
use sha2::{Digest, Sha256};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ContentRevision {
    pub(crate) hash: String,
    pub(crate) bytes: u64,
    pub(crate) lines: i64,
}

pub(crate) fn content_revision(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

pub(crate) fn bounded_diff(diff: String) -> Option<String> {
    (diff.len() <= crate::limits::MAX_CHANGE_RECEIPT_DIFF_BYTES).then_some(diff)
}

pub(crate) fn line_count(bytes: &[u8]) -> i64 {
    if bytes.is_empty() {
        return 0;
    }
    let newline_count = bytes.iter().filter(|byte| **byte == b'\n').count();
    let has_trailing_newline = bytes.last() == Some(&b'\n');
    i64::try_from(newline_count + usize::from(!has_trailing_newline)).unwrap_or(i64::MAX)
}

pub(crate) fn validate_fence(
    expected_revision: Option<&str>,
    expected_target_fingerprint: Option<&str>,
    actual_revision: Option<&str>,
    actual_target_fingerprint: &str,
    path: &str,
) -> Result<(), String> {
    if let Some(expected) = expected_target_fingerprint
        && expected != actual_target_fingerprint
    {
        return Err(format!(
            "mutation fence rejected {path}: target fingerprint changed (expected {expected}, observed {actual_target_fingerprint})"
        ));
    }
    if let Some(expected) = expected_revision
        && actual_revision != Some(expected)
    {
        return Err(format!(
            "mutation fence rejected {path}: content revision changed (expected {expected}, observed {})",
            actual_revision.unwrap_or("<missing>")
        ));
    }
    Ok(())
}

pub(crate) fn revision(bytes: &[u8]) -> ContentRevision {
    ContentRevision {
        hash: content_revision(bytes),
        bytes: u64::try_from(bytes.len()).unwrap_or(u64::MAX),
        lines: line_count(bytes),
    }
}

pub(crate) fn receipt(
    path: String,
    target_fingerprint: String,
    before: Option<&[u8]>,
    after: &[u8],
    origin: &str,
    unified_diff: Option<String>,
) -> ChangeReceipt {
    let before = before.map(revision);
    let after = revision(after);
    receipt_from_revisions(
        path,
        target_fingerprint,
        before.as_ref(),
        &after,
        origin,
        unified_diff,
    )
}

pub(crate) fn receipt_from_revisions(
    path: String,
    target_fingerprint: String,
    before: Option<&ContentRevision>,
    after: &ContentRevision,
    origin: &str,
    unified_diff: Option<String>,
) -> ChangeReceipt {
    let before_len = before.map_or(0, |revision| revision.bytes);
    let byte_delta = i64::try_from(after.bytes)
        .unwrap_or(i64::MAX)
        .saturating_sub(i64::try_from(before_len).unwrap_or(i64::MAX));
    let line_delta = after
        .lines
        .saturating_sub(before.map_or(0, |revision| revision.lines));
    ChangeReceipt {
        path,
        target_fingerprint,
        before_revision: before.map(|revision| revision.hash.clone()),
        after_revision: after.hash.clone(),
        byte_delta,
        line_delta,
        origin: origin.into(),
        unified_diff,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn revision_and_line_delta_are_deterministic() {
        let before = b"one\ntwo\n";
        let after = b"one\ntwo\nthree\n";
        let receipt = receipt(
            "notes.txt".into(),
            "target".into(),
            Some(before),
            after,
            "edit",
            Some("@@".into()),
        );
        assert_eq!(receipt.byte_delta, 6);
        assert_eq!(receipt.line_delta, 1);
        assert_eq!(receipt.before_revision, Some(content_revision(before)));
        assert_eq!(receipt.after_revision, content_revision(after));
    }

    #[test]
    fn stale_revision_and_identity_fail_closed() {
        assert!(
            validate_fence(
                Some("old"),
                Some("target"),
                Some("new"),
                "target",
                "notes.txt"
            )
            .is_err()
        );
        assert!(validate_fence(None, Some("old-target"), None, "target", "notes.txt").is_err());
    }
}
