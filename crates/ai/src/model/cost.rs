use crate::model::Model;
use crate::protocol::Usage;

/// Calculate cost for the given usage against a model's rates.
/// Rates are per million tokens. Updates `usage.cost` in place.
pub fn calculate_cost(model: &Model, usage: &mut Usage) {
    if !model.cost.known {
        usage.cost = crate::protocol::Cost::unknown();
        return;
    }
    usage.cost.known = true;
    usage.cost.input = (usage.input as f64 / 1_000_000.0) * model.cost.input;
    usage.cost.output = (usage.output as f64 / 1_000_000.0) * model.cost.output;
    usage.cost.cache_read = (usage.cache_read as f64 / 1_000_000.0) * model.cost.cache_read;
    usage.cost.cache_write = (usage.cache_write as f64 / 1_000_000.0) * model.cost.cache_write;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::lookup_model;

    #[test]
    fn cost_calculation_basic() {
        let model = lookup_model("claude-haiku-4-5").unwrap();
        let mut usage = Usage {
            input: 1_000_000,
            output: 500_000,
            cache_read: 0,
            cache_write: 0,
            total_tokens: 1_500_000,
            cost: Default::default(),
        };
        calculate_cost(&model, &mut usage);
        assert!((usage.cost.input - 1.0).abs() < 0.001);
        assert!((usage.cost.output - 2.5).abs() < 0.001);
    }

    #[test]
    fn cost_calculation_with_cache() {
        let model = lookup_model("claude-sonnet-4-5").unwrap();
        let mut usage = Usage {
            input: 0,
            output: 0,
            cache_read: 1_000_000,
            cache_write: 2_000_000,
            total_tokens: 3_000_000,
            cost: Default::default(),
        };
        calculate_cost(&model, &mut usage);
        assert!((usage.cost.cache_read - 0.30).abs() < 0.001);
        assert!((usage.cost.cache_write - 7.50).abs() < 0.001);
    }
}
