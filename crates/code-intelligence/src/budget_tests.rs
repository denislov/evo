//! `IndexBudget` / `IndexBudgetTracker` 的边界测试。

use crate::{BudgetKind, IndexBudget, IndexBudgetTracker};

fn small_budget() -> IndexBudget {
    IndexBudget {
        max_files: 3,
        max_total_bytes: 100,
        max_parse_secs_per_file: 5,
        max_concurrent_parses: 2,
    }
}

#[test]
fn default_budget_is_sane_and_serializable() {
    let budget = IndexBudget::default();
    assert!(budget.max_files > 0);
    assert!(budget.max_total_bytes > 0);
    assert!(budget.max_parse_secs_per_file > 0);
    assert!(budget.max_concurrent_parses > 0);
    let json = serde_json::to_value(budget).unwrap();
    assert_eq!(json["maxFiles"], 200_000);
    assert_eq!(json["maxParseSecsPerFile"], 30);
    let back: IndexBudget = serde_json::from_value(json).unwrap();
    assert_eq!(back, budget);
}

#[test]
fn partial_json_uses_defaults() {
    let budget: IndexBudget = serde_json::from_value(serde_json::json!({})).unwrap();
    assert_eq!(budget, IndexBudget::default());
    let budget: IndexBudget = serde_json::from_value(serde_json::json!({"maxFiles": 10})).unwrap();
    assert_eq!(budget.max_files, 10);
    assert_eq!(
        budget.max_total_bytes,
        IndexBudget::default().max_total_bytes
    );
}

#[test]
fn reserve_file_within_limit() {
    let mut tracker = IndexBudgetTracker::new(small_budget());
    tracker.reserve_file(30).unwrap();
    tracker.reserve_file(30).unwrap();
    tracker.reserve_file(30).unwrap();
    let snapshot = tracker.snapshot();
    assert_eq!(snapshot.files, 3);
    assert_eq!(snapshot.total_bytes, 90);
}

#[test]
fn file_count_exceeded_is_reported() {
    let mut tracker = IndexBudgetTracker::new(small_budget());
    for _ in 0..3 {
        tracker.reserve_file(1).unwrap();
    }
    let err = tracker.reserve_file(1).unwrap_err();
    assert!(matches!(
        err,
        crate::CodeIntelligenceError::BudgetExceeded {
            kind: BudgetKind::Files,
            limit: 3,
            observed: 4
        }
    ));
}

#[test]
fn total_bytes_exceeded_is_reported() {
    let mut tracker = IndexBudgetTracker::new(small_budget());
    tracker.reserve_file(60).unwrap();
    let err = tracker.reserve_file(50).unwrap_err();
    assert!(matches!(
        err,
        crate::CodeIntelligenceError::BudgetExceeded {
            kind: BudgetKind::TotalBytes,
            limit: 100,
            observed: 110
        }
    ));
}

#[test]
fn failed_reserve_leaves_no_half_state() {
    let mut tracker = IndexBudgetTracker::new(small_budget());
    tracker.reserve_file(60).unwrap();
    assert!(tracker.reserve_file(50).is_err());
    let snapshot = tracker.snapshot();
    assert_eq!(snapshot.files, 1);
    assert_eq!(snapshot.total_bytes, 60);
}

#[test]
fn concurrent_parses_are_bounded_and_paired() {
    let mut tracker = IndexBudgetTracker::new(small_budget());
    tracker.parse_start().unwrap();
    tracker.parse_start().unwrap();
    assert!(matches!(
        tracker.parse_start().unwrap_err(),
        crate::CodeIntelligenceError::BudgetExceeded {
            kind: BudgetKind::ConcurrentParses,
            ..
        }
    ));
    tracker.parse_end();
    tracker.parse_start().unwrap();
    assert_eq!(tracker.snapshot().active_parses, 2);
    tracker.parse_end();
    tracker.parse_end();
    assert_eq!(tracker.snapshot().active_parses, 0);
}

#[test]
fn parse_time_limit_reflects_config() {
    let mut tracker = IndexBudgetTracker::new(small_budget());
    assert_eq!(
        tracker.parse_time_limit(),
        Some(std::time::Duration::from_secs(5))
    );
    tracker = IndexBudgetTracker::new(IndexBudget {
        max_parse_secs_per_file: 0,
        ..small_budget()
    });
    assert_eq!(tracker.parse_time_limit(), None);
}

#[test]
fn zero_limit_means_unlimited() {
    let budget = IndexBudget {
        max_files: 0,
        max_total_bytes: 0,
        max_parse_secs_per_file: 0,
        max_concurrent_parses: 0,
    };
    let mut tracker = IndexBudgetTracker::new(budget);
    for _ in 0..10_000 {
        tracker.reserve_file(1).unwrap();
        tracker.parse_start().unwrap();
    }
    let snapshot = tracker.snapshot();
    assert_eq!(snapshot.files, 10_000);
    assert_eq!(snapshot.active_parses, 10_000);
    assert_eq!(tracker.parse_time_limit(), None);
}

#[test]
fn budget_snapshot_round_trip_json() {
    let mut tracker = IndexBudgetTracker::new(small_budget());
    tracker.reserve_file(30).unwrap();
    tracker.parse_start().unwrap();
    let snapshot = tracker.snapshot();
    let json = serde_json::to_value(snapshot).unwrap();
    let back: crate::BudgetSnapshot = serde_json::from_value(json).unwrap();
    assert_eq!(back, snapshot);
}

#[test]
fn budget_kind_display_and_serde_are_consistent() {
    for kind in [
        BudgetKind::Files,
        BudgetKind::TotalBytes,
        BudgetKind::ParseDurationSecs,
        BudgetKind::ConcurrentParses,
    ] {
        let json = serde_json::to_value(kind).unwrap();
        let back: BudgetKind = serde_json::from_value(json.clone()).unwrap();
        assert_eq!(back, kind);
        assert_eq!(json, serde_json::json!(kind.as_str()));
    }
}
