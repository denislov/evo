use std::collections::{BTreeSet, HashSet};
use std::sync::LazyLock;

use crate::model::Model;

/// Static model lookup by id. Searches deterministic priority then lexical.
pub fn lookup_model(id: &str) -> Option<Model> {
    const PRIORITY: &[&str] = &["anthropic", "openai", "google", "deepseek"];
    for provider in PRIORITY {
        if let Some(model) = get_model(provider, id) {
            return Some(model);
        }
    }
    all_models().iter().find(|model| model.id == id).cloned()
}

pub fn get_model(provider: &str, id: &str) -> Option<Model> {
    all_models()
        .iter()
        .find(|model| model.provider == provider && model.id == id)
        .cloned()
}

pub fn get_models(provider: &str) -> Vec<Model> {
    all_models()
        .iter()
        .filter(|model| model.provider == provider)
        .cloned()
        .collect()
}

pub fn get_providers() -> Vec<String> {
    let mut providers = BTreeSet::new();
    for model in all_models() {
        providers.insert(model.provider.clone());
    }
    providers.into_iter().collect()
}

pub fn all_models() -> &'static [Model] {
    static MODELS: LazyLock<Vec<Model>> = LazyLock::new(|| {
        let models: Vec<Model> = serde_json::from_str(include_str!("generated.json"))
            .expect("generated model registry JSON should be valid");
        validate_models(&models)
            .expect("generated model registry should satisfy runtime invariants");
        models
    });
    &MODELS
}

pub(crate) fn validate_models(models: &[Model]) -> Result<(), String> {
    let supported_apis: HashSet<&str> = crate::providers::builtin_provider_apis()
        .iter()
        .copied()
        .collect();
    let mut identities = HashSet::new();
    let mut previous_identity: Option<(&str, &str)> = None;

    for model in models {
        let identity = format!("{}/{}", model.provider, model.id);
        if model.id.trim().is_empty()
            || model.name.trim().is_empty()
            || model.api.trim().is_empty()
            || model.provider.trim().is_empty()
        {
            return Err(format!(
                "{identity}: identity, name, API, and provider must be non-empty"
            ));
        }
        if !identities.insert((model.provider.as_str(), model.id.as_str())) {
            return Err(format!("{identity}: duplicate provider/model identity"));
        }
        if let Some(previous) = previous_identity
            && previous > (model.provider.as_str(), model.id.as_str())
        {
            return Err(format!(
                "{identity}: generated catalog is not sorted by provider then model ID"
            ));
        }
        previous_identity = Some((model.provider.as_str(), model.id.as_str()));

        if !supported_apis.contains(model.api.as_str()) {
            return Err(format!("{identity}: unsupported API `{}`", model.api));
        }
        if model.base_url.trim().is_empty() && model.api != "azure-openai-responses" {
            return Err(format!("{identity}: base URL must be non-empty"));
        }
        if model.input.is_empty() {
            return Err(format!(
                "{identity}: at least one input capability is required"
            ));
        }
        if model.context_window == 0 || model.max_tokens == 0 {
            return Err(format!("{identity}: token limits must be positive"));
        }
        if model.max_tokens > model.context_window {
            return Err(format!(
                "{identity}: maxTokens {} exceeds contextWindow {}",
                model.max_tokens, model.context_window
            ));
        }

        let rates = [
            model.cost.input,
            model.cost.output,
            model.cost.cache_read,
            model.cost.cache_write,
        ];
        if model.cost.known {
            if rates.iter().any(|rate| !rate.is_finite() || *rate < 0.0) {
                return Err(format!(
                    "{identity}: known prices must be finite and non-negative"
                ));
            }
        } else if rates.iter().any(|rate| *rate != 0.0) {
            return Err(format!(
                "{identity}: unknown prices must use zero numeric fields"
            ));
        }

        let compat_matches_api = matches!(
            (&model.compat, model.api.as_str()),
            (None, _)
                | (
                    Some(crate::compatibility::ModelCompat::AnthropicMessages(_)),
                    "anthropic-messages",
                )
                | (
                    Some(crate::compatibility::ModelCompat::OpenAICompletions(_)),
                    "openai-completions",
                )
                | (
                    Some(crate::compatibility::ModelCompat::OpenAIResponses(_)),
                    "openai-responses" | "azure-openai-responses" | "openai-codex-responses",
                )
        );
        if !compat_matches_api {
            return Err(format!(
                "{identity}: compatibility metadata does not match API `{}`",
                model.api
            ));
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lookup_known_model() {
        let model = lookup_model("claude-sonnet-4-5").unwrap();
        assert_eq!(model.id, "claude-sonnet-4-5");
        assert!((model.cost.input - 3.0).abs() < 0.001);
    }

    #[test]
    fn lookup_unknown_model() {
        assert!(lookup_model("nonexistent").is_none());
    }

    #[test]
    fn lookup_deepseek_model() {
        let model = lookup_model("deepseek-v4-flash").unwrap();
        assert_eq!(model.provider, "deepseek");
    }

    #[test]
    fn generated_catalog_satisfies_runtime_invariants() {
        validate_models(all_models()).unwrap();
    }

    #[test]
    fn invalid_catalog_records_have_model_specific_diagnostics() {
        let mut invalid = lookup_model("claude-sonnet-4-5").unwrap();
        invalid.max_tokens = invalid.context_window + 1;
        let error = validate_models(&[invalid]).unwrap_err();
        assert!(error.contains("anthropic/claude-sonnet-4-5"));
        assert!(error.contains("maxTokens"));
    }

    #[test]
    fn unknown_price_is_explicit_and_never_calculates_negative_cost() {
        let model = lookup_model("openrouter/auto").unwrap();
        assert!(!model.cost.known);
        let mut usage = crate::protocol::Usage {
            input: 1_000_000,
            output: 1_000_000,
            ..Default::default()
        };
        crate::model::calculate_cost(&model, &mut usage);
        assert!(!usage.cost.known);
        assert_eq!(usage.cost.input, 0.0);
        assert_eq!(usage.cost.output, 0.0);
    }

    #[test]
    fn every_generated_compatibility_field_has_a_registered_disposition() {
        let raw: serde_json::Value = serde_json::from_str(include_str!("generated.json")).unwrap();
        for model in raw.as_array().unwrap() {
            let Some(compat) = model.get("compat").and_then(serde_json::Value::as_object) else {
                continue;
            };
            for (field, value) in compat {
                if value.is_null() {
                    continue;
                }
                assert!(
                    crate::compatibility::compatibility_field_disposition(field).is_some(),
                    "{}/{} has compatibility field `{field}` without a disposition",
                    model["provider"].as_str().unwrap_or("unknown"),
                    model["id"].as_str().unwrap_or("unknown")
                );
            }
        }
    }

    #[test]
    fn retained_compatibility_struct_fields_are_registered() {
        const FIELDS: &[&str] = &[
            "supportsEagerToolInputStreaming",
            "sendSessionAffinityHeaders",
            "supportsLongCacheRetention",
            "supportsCacheControlOnTools",
            "supportsTemperature",
            "forceAdaptiveThinking",
            "allowEmptySignature",
            "supportsStore",
            "supportsUsageInStreaming",
            "supportsDeveloperRole",
            "supportsReasoningEffort",
            "maxTokensField",
            "requiresToolResultName",
            "requiresAssistantAfterToolResult",
            "requiresThinkingAsText",
            "requiresReasoningContentOnAssistantMessages",
            "thinkingFormat",
            "openRouterRouting",
            "vercelGatewayRouting",
            "zaiToolStream",
            "supportsStrictMode",
            "cacheControlFormat",
            "sendSessionIdHeader",
        ];
        for field in FIELDS {
            assert!(
                crate::compatibility::compatibility_field_disposition(field).is_some(),
                "retained compatibility field `{field}` needs a disposition"
            );
        }
    }
}
