use ai::model::{Model, ModelCost, ModelInput};
use ai::model::{all_models, calculate_cost, get_model, get_models, get_providers, lookup_model};
use ai::protocol::Usage;

#[test]
fn model_serializes_like_ts_generated_shape() {
    let model = Model {
        id: "gpt-4.1".into(),
        name: "GPT-4.1".into(),
        api: "openai-responses".into(),
        provider: "openai".into(),
        base_url: "https://api.openai.com/v1".into(),
        reasoning: false,
        thinking_level_map: None,
        input: vec![ModelInput::Text, ModelInput::Image],
        cost: ModelCost {
            known: true,
            input: 2.0,
            output: 8.0,
            cache_read: 0.5,
            cache_write: 0.0,
        },
        context_window: 1_047_576,
        max_tokens: 32_768,
        headers: None,
        compat: None,
    };

    let json = serde_json::to_value(&model).unwrap();
    assert_eq!(json["baseUrl"], "https://api.openai.com/v1");
    assert_eq!(json["input"], serde_json::json!(["text", "image"]));
    assert_eq!(json["cost"]["input"], 2.0);
    assert_eq!(json["cost"]["cacheRead"], 0.5);
    assert!(json.get("cacheRead").is_none());
}

#[test]
fn cost_calculation_uses_nested_model_cost() {
    let model = Model {
        id: "unit-test".into(),
        name: "Unit Test".into(),
        api: "test-api".into(),
        provider: "test".into(),
        base_url: "https://example.invalid".into(),
        reasoning: false,
        thinking_level_map: None,
        input: vec![ModelInput::Text],
        cost: ModelCost {
            known: true,
            input: 1.0,
            output: 2.0,
            cache_read: 0.25,
            cache_write: 0.75,
        },
        context_window: 1000,
        max_tokens: 100,
        headers: None,
        compat: None,
    };
    let mut usage = Usage {
        input: 1_000_000,
        output: 500_000,
        cache_read: 2_000_000,
        cache_write: 4_000_000,
        total_tokens: 7_500_000,
        cost: Default::default(),
    };

    calculate_cost(&model, &mut usage);

    assert_eq!(usage.cost.input, 1.0);
    assert_eq!(usage.cost.output, 1.0);
    assert_eq!(usage.cost.cache_read, 0.5);
    assert_eq!(usage.cost.cache_write, 3.0);
}

#[test]
fn lookup_default_anthropic_model_still_works() {
    let model = lookup_model("claude-sonnet-4-5").unwrap();
    assert_eq!(model.provider, "anthropic");
    assert_eq!(model.api, "anthropic-messages");
}

#[test]
fn registry_contains_m2_models_from_ts_reference() {
    let gpt = get_model("openai", "gpt-4.1").unwrap();
    assert_eq!(gpt.api, "openai-responses");
    assert_eq!(gpt.input, vec![ModelInput::Text, ModelInput::Image]);

    let gpt5 = get_model("openai", "gpt-5").unwrap();
    assert_eq!(gpt5.api, "openai-responses");
    assert!(gpt5.reasoning);

    let gemini = get_model("google", "gemini-2.5-flash").unwrap();
    assert_eq!(gemini.api, "google-generative-ai");
    assert!(gemini.input.contains(&ModelInput::Image));

    let deepseek = get_model("deepseek", "deepseek-v4-flash").unwrap();
    assert_eq!(deepseek.api, "openai-completions");
    assert_eq!(deepseek.provider, "deepseek");

    let claude = get_model("anthropic", "claude-sonnet-4-5").unwrap();
    assert_eq!(claude.api, "anthropic-messages");
}

#[test]
fn provider_listing_is_deterministic_and_non_empty() {
    let providers = get_providers();
    assert!(providers.windows(2).all(|w| w[0] <= w[1]));
    assert!(providers.contains(&"anthropic".to_string()));
    assert!(providers.contains(&"openai".to_string()));
    assert!(providers.contains(&"google".to_string()));
}

#[test]
fn provider_model_listing_filters_by_provider() {
    let openai = get_models("openai");
    assert!(openai.iter().any(|m| m.id == "gpt-4.1"));
    assert!(openai.iter().all(|m| m.provider == "openai"));
}

#[test]
fn generated_registry_has_unique_provider_id_pairs() {
    let mut seen = std::collections::BTreeSet::new();
    for model in all_models() {
        assert!(
            seen.insert((model.provider.clone(), model.id.clone())),
            "duplicate model pair: {}/{}",
            model.provider,
            model.id
        );
    }
}

#[test]
fn generated_registry_is_loaded_from_json_asset() {
    let raw = include_str!("../model/generated.json");
    let models: Vec<Model> = serde_json::from_str(raw).unwrap();

    assert_eq!(models.len(), 831);
    assert_eq!(models.len(), all_models().len());
    assert!(
        models
            .iter()
            .any(|model| { model.provider == "anthropic" && model.id == "claude-sonnet-4-5" })
    );
    assert!(
        models
            .iter()
            .all(|model| model.provider != "amazon-bedrock"
                && model.api != "bedrock-converse-stream")
    );
}
