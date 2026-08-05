use serde::{Deserialize, Serialize};

/// Durable description of one successful filesystem mutation.
///
/// Revisions are lowercase SHA-256 content hashes. `before_revision` is absent
/// when a vacant target was created. `target_fingerprint` identifies the
/// capability-bound object that was opened, while `unified_diff` is optional
/// and bounded by the producer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChangeReceipt {
    pub path: String,
    pub target_fingerprint: String,
    pub before_revision: Option<String>,
    pub after_revision: String,
    pub after_exists: bool,
    pub byte_delta: i64,
    pub line_delta: i64,
    pub origin: String,
    pub unified_diff: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_with_optional_creation_revision() {
        let receipt = ChangeReceipt {
            path: "notes.txt".into(),
            target_fingerprint: "target".into(),
            before_revision: None,
            after_revision: "after".into(),
            after_exists: true,
            byte_delta: 5,
            line_delta: 1,
            origin: "write".into(),
            unified_diff: None,
        };
        let value = serde_json::to_value(&receipt).expect("receipt serializes");
        assert_eq!(
            serde_json::from_value::<ChangeReceipt>(value).expect("receipt round trips"),
            receipt
        );
    }
}
