use sha2::{Digest, Sha256};

use super::state::FileVersion;
use crate::{ChangeReceipt, ChangeTrackerError};

pub(super) fn baseline_from_receipt(
    receipt: &ChangeReceipt,
    current: &FileVersion,
) -> Result<FileVersion, ChangeTrackerError> {
    let Some(before_revision) = receipt.before_revision.as_ref() else {
        return Ok(FileVersion::missing(empty_revision()));
    };
    if before_revision == &empty_revision() {
        return Ok(FileVersion::existing(
            before_revision.clone(),
            Some(Vec::new()),
        ));
    }
    let Some(diff) = receipt.unified_diff.as_deref() else {
        return Ok(FileVersion::existing(before_revision.clone(), None));
    };
    let Some((old, new)) = parse_full_file_diff(diff) else {
        return Ok(FileVersion::existing(before_revision.clone(), None));
    };
    let old =
        select_revision(old, before_revision).ok_or_else(|| ChangeTrackerError::InvalidFact {
            message: format!(
                "receipt diff old side does not match before_revision for {}",
                receipt.path
            ),
        })?;
    if current.exists {
        let reconstructed_after = select_revision(new, &current.revision).ok_or_else(|| {
            ChangeTrackerError::InvalidFact {
                message: format!(
                    "receipt diff new side does not match after_revision for {}",
                    receipt.path
                ),
            }
        })?;
        if current
            .content
            .as_deref()
            .is_some_and(|bytes| bytes != reconstructed_after)
        {
            return Err(ChangeTrackerError::InvalidFact {
                message: format!(
                    "receipt diff new side does not match workspace bytes for {}",
                    receipt.path
                ),
            });
        }
    } else if !new.is_empty() || current.revision != empty_revision() {
        return Err(ChangeTrackerError::InvalidFact {
            message: format!(
                "deleted receipt has a non-empty new side for {}",
                receipt.path
            ),
        });
    }
    Ok(FileVersion::existing(before_revision.clone(), Some(old)))
}

fn parse_full_file_diff(diff: &str) -> Option<(String, String)> {
    let mut headers = 0_usize;
    let mut old = Vec::new();
    let mut new = Vec::new();
    for line in diff.lines() {
        if line.starts_with("@@ -") {
            headers = headers.saturating_add(1);
            continue;
        }
        if headers == 0 || line.starts_with("--- ") || line.starts_with("+++ ") {
            continue;
        }
        let (prefix, body) = line.split_at_checked(1)?;
        match prefix {
            " " => {
                old.push(body);
                new.push(body);
            }
            "-" => old.push(body),
            "+" => new.push(body),
            _ => return None,
        }
    }
    (headers == 1).then(|| (old.join("\n"), new.join("\n")))
}

fn select_revision(candidate: String, revision: &str) -> Option<Vec<u8>> {
    let bytes = candidate.into_bytes();
    if hash(&bytes) == revision {
        return Some(bytes);
    }
    let mut terminated = bytes;
    terminated.push(b'\n');
    (hash(&terminated) == revision).then_some(terminated)
}

fn hash(content: &[u8]) -> String {
    format!("{:x}", Sha256::digest(content))
}

fn empty_revision() -> String {
    hash(&[])
}
