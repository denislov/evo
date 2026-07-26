use super::support;

use ai::compatibility::{ThinkingFormat, ThinkingLevelValue};
use ai::model::lookup_model;
use ai::registry::env::env_api_key;
use support::EnvGuard;

#[test]
fn deepseek_model_is_available() {
    let model = lookup_model("deepseek-v4-flash").unwrap();

    assert_eq!(model.provider, "deepseek");
    assert_eq!(model.api, "openai-completions");
    assert_eq!(model.base_url, "https://api.deepseek.com");
}

#[test]
fn deepseek_api_key_uses_deepseek_env_var() {
    let env = EnvGuard::new(&["DEEPSEEK_API_KEY"]);
    env.set("DEEPSEEK_API_KEY", "sk-deepseek-test");

    assert_eq!(
        env_api_key("deepseek"),
        Some("sk-deepseek-test".to_string())
    );
}

#[test]
fn deepseek_models_use_only_openai_compatible_completions() {
    for model_id in ["deepseek-v4-flash", "deepseek-v4-pro"] {
        let model = lookup_model(model_id).unwrap();
        assert_eq!(model.provider, "deepseek");
        assert_eq!(model.api, "openai-completions");
        assert_eq!(model.base_url, "https://api.deepseek.com");
        let map = model
            .thinking_level_map
            .as_ref()
            .expect("DeepSeek thinking levels must be explicit");
        assert_eq!(map.minimal, Some(ThinkingLevelValue::String("high".into())));
        assert_eq!(map.low, Some(ThinkingLevelValue::String("high".into())));
        assert_eq!(map.medium, Some(ThinkingLevelValue::String("high".into())));
        assert_eq!(map.high, Some(ThinkingLevelValue::String("high".into())));
        assert_eq!(map.xhigh, Some(ThinkingLevelValue::String("max".into())));
        assert_eq!(
            ai::compatibility::OpenAICompletionsCompat::from_model(&model).thinking_format,
            Some(ThinkingFormat::DeepSeek)
        );
    }
}
// Internal DeepSeek compatibility tests.
