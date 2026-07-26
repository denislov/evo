use super::support;

use ai::registry::env::env_api_key;

use support::EnvGuard;

fn with_env_var(name: &'static str, value: &str, f: impl FnOnce()) {
    let env = EnvGuard::new(&[name]);
    env.set(name, value);
    f();
}

#[test]
fn resolves_openai_google_and_deepseek_keys() {
    with_env_var("OPENAI_API_KEY", "sk-openai", || {
        assert_eq!(env_api_key("openai").as_deref(), Some("sk-openai"));
    });
    with_env_var("GEMINI_API_KEY", "sk-google", || {
        assert_eq!(env_api_key("google").as_deref(), Some("sk-google"));
    });
    with_env_var("DEEPSEEK_API_KEY", "sk-deepseek", || {
        assert_eq!(env_api_key("deepseek").as_deref(), Some("sk-deepseek"));
    });
}

#[test]
fn resolves_openai_compatible_provider_keys() {
    with_env_var("GROQ_API_KEY", "sk-groq", || {
        assert_eq!(env_api_key("groq").as_deref(), Some("sk-groq"));
    });
    with_env_var("XAI_API_KEY", "sk-xai", || {
        assert_eq!(env_api_key("xai").as_deref(), Some("sk-xai"));
    });
    with_env_var("OPENROUTER_API_KEY", "sk-openrouter", || {
        assert_eq!(env_api_key("openrouter").as_deref(), Some("sk-openrouter"));
    });
    with_env_var("AI_GATEWAY_API_KEY", "sk-gateway", || {
        assert_eq!(
            env_api_key("vercel-ai-gateway").as_deref(),
            Some("sk-gateway")
        );
    });
}

#[test]
fn unknown_provider_returns_none() {
    assert_eq!(env_api_key("unknown-provider"), None);
}
// Internal environment-key tests.
