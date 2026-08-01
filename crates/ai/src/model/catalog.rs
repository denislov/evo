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
        if model.base_url.trim().is_empty() {
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
                    "openai-responses" | "openai-codex-responses",
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
