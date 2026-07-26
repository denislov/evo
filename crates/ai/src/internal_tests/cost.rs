use ai::model::{calculate_cost, lookup_model};
use ai::protocol::*;

#[test]
fn haiku_cost() {
    let model = lookup_model("claude-haiku-4-5").unwrap();
    let mut usage = Usage {
        input: 1_000_000,
        output: 1_000_000,
        cache_read: 0,
        cache_write: 0,
        total_tokens: 2_000_000,
        cost: Cost::default(),
    };
    calculate_cost(&model, &mut usage);
    assert!((usage.cost.input - 1.0).abs() < 0.01);
    assert!((usage.cost.output - 5.0).abs() < 0.01);
}

#[test]
fn opus_cost_with_cache() {
    let model = lookup_model("claude-opus-4-5").unwrap();
    let mut usage = Usage {
        input: 0,
        output: 0,
        cache_read: 1_000_000,
        cache_write: 1_000_000,
        total_tokens: 2_000_000,
        cost: Cost::default(),
    };
    calculate_cost(&model, &mut usage);
    assert!((usage.cost.cache_read - 0.5).abs() < 0.01);
    assert!((usage.cost.cache_write - 6.25).abs() < 0.01);
}

#[test]
fn zero_usage_zero_cost() {
    let model = lookup_model("claude-sonnet-4-5").unwrap();
    let mut usage = Usage::default();
    calculate_cost(&model, &mut usage);
    assert_eq!(usage.cost.input, 0.0);
    assert_eq!(usage.cost.output, 0.0);
}
// Internal model cost tests.
