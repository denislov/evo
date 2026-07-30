use crate::agent::types::ThinkingLevel;
use ai::api::model::{Model, ThinkingConfig};
use ai::api::stream::StreamOptions;

pub(crate) fn stream_options_for_turn(
    model: &Model,
    mut options: StreamOptions,
    thinking_level: ThinkingLevel,
) -> StreamOptions {
    if !model.reasoning {
        options.thinking = None;
        return options;
    }

    match thinking_level {
        ThinkingLevel::Off => {
            options.thinking = Some(ThinkingConfig {
                enabled: false,
                budget_tokens: None,
                effort: None,
            });
        }
        _ => {
            let budget_tokens = match thinking_level {
                ThinkingLevel::Minimal => Some(1024u32),
                ThinkingLevel::Low => Some(2048u32),
                ThinkingLevel::Medium => Some(4096u32),
                ThinkingLevel::High => Some(8192u32),
                ThinkingLevel::XHigh => Some(16384u32),
                ThinkingLevel::Off => None,
            };
            options.thinking = Some(ThinkingConfig {
                enabled: true,
                budget_tokens,
                effort: Some(thinking_level.to_string()),
            });
        }
    }

    options
}
