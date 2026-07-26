//! Fuzzy matching and ranking behavior.

use tui::api::input::{fuzzy_filter_indices, fuzzy_match};

#[test]
fn fuzzy_match_allows_ordered_non_contiguous_characters() {
    let matched = fuzzy_match("mdl", "model-selector");
    assert!(matched.matches);
    assert!(!fuzzy_match("mld", "model-selector").matches);
}

#[test]
fn fuzzy_match_prefers_exact_and_consecutive_matches() {
    let exact = fuzzy_match("model", "model");
    let spaced = fuzzy_match("model", "m-o-d-e-l");
    assert!(exact.matches);
    assert!(spaced.matches);
    assert!(exact.score < spaced.score);
}

#[test]
fn fuzzy_filter_indices_requires_all_tokens_and_sorts_by_score() {
    let items = vec!["model selector", "session model", "settings"];
    let indices = fuzzy_filter_indices(&items, "mod ctor", |item| *item);
    assert_eq!(indices, vec![0]);
}

#[test]
fn fuzzy_match_supports_swapped_letter_digit_queries() {
    assert!(fuzzy_match("gpt5", "gpt-5").matches);
    assert!(fuzzy_match("5gpt", "gpt-5").matches);
}
