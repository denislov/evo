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

#[cfg(test)]
mod tests {
    use crate::agent::types::ThinkingLevel;
    use ai::api::model::{Model, ModelCost, ModelInput};
    use ai::api::stream::StreamOptions;

    fn model(reasoning: bool) -> Model {
        Model {
            id: "faux-model".into(),
            name: "Faux Model".into(),
            api: "faux".into(),
            provider: "faux".into(),
            base_url: String::new(),
            reasoning,
            thinking_level_map: None,
            input: vec![ModelInput::Text],
            cost: ModelCost {
                known: true,
                input: 0.0,
                output: 0.0,
                cache_read: 0.0,
                cache_write: 0.0,
            },
            context_window: 0,
            max_tokens: 0,
            headers: None,
            compat: None,
        }
    }

    #[test]
    fn thinking_options_are_applied_for_reasoning_models() {
        let options = super::stream_options_for_turn(
            &model(true),
            StreamOptions::default(),
            ThinkingLevel::High,
        );

        let thinking = options.thinking.expect("thinking should be enabled");
        assert!(thinking.enabled);
        assert_eq!(thinking.budget_tokens, Some(8192));
        assert_eq!(thinking.effort.as_deref(), Some("high"));
    }

    #[test]
    fn thinking_options_are_omitted_for_non_reasoning_models() {
        let options = super::stream_options_for_turn(
            &model(false),
            StreamOptions::default(),
            ThinkingLevel::High,
        );

        assert!(options.thinking.is_none());
    }

    #[test]
    fn thinking_off_is_explicit_for_reasoning_models() {
        let options = super::stream_options_for_turn(
            &model(true),
            StreamOptions::default(),
            ThinkingLevel::Off,
        );

        assert_eq!(
            options.thinking,
            Some(ai::api::model::ThinkingConfig {
                enabled: false,
                budget_tokens: None,
                effort: None,
            })
        );
    }
}
