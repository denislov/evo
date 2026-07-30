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
