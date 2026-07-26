fn provider_env_vars(provider: &str) -> &'static [&'static str] {
    match provider {
        "anthropic" => &["ANTHROPIC_API_KEY", "CLAUDE_API_KEY", "ANTHROPIC_KEY"],
        "openai" => &["OPENAI_API_KEY"],
        "azure-openai-responses" => &["AZURE_OPENAI_API_KEY"],
        "deepseek" => &["DEEPSEEK_API_KEY", "DEEPSEEK_KEY"],
        "google" => &["GEMINI_API_KEY", "GOOGLE_API_KEY"],
        "groq" => &["GROQ_API_KEY"],
        "cerebras" => &["CEREBRAS_API_KEY"],
        "xai" => &["XAI_API_KEY"],
        "openrouter" => &["OPENROUTER_API_KEY"],
        "vercel-ai-gateway" => &["AI_GATEWAY_API_KEY"],
        "zai" => &["ZAI_API_KEY"],
        "mistral" => &["MISTRAL_API_KEY"],
        "moonshotai" | "moonshotai-cn" => &["MOONSHOT_API_KEY"],
        "huggingface" => &["HF_TOKEN"],
        "fireworks" => &["FIREWORKS_API_KEY"],
        "together" => &["TOGETHER_API_KEY"],
        "opencode" | "opencode-go" => &["OPENCODE_API_KEY"],
        "kimi-coding" => &["KIMI_API_KEY"],
        "cloudflare-workers-ai" | "cloudflare-ai-gateway" => &["CLOUDFLARE_API_KEY"],
        "minimax" => &["MINIMAX_API_KEY"],
        "minimax-cn" => &["MINIMAX_CN_API_KEY"],
        "xiaomi" => &["XIAOMI_API_KEY"],
        "xiaomi-token-plan-cn" => &["XIAOMI_TOKEN_PLAN_CN_API_KEY"],
        "xiaomi-token-plan-ams" => &["XIAOMI_TOKEN_PLAN_AMS_API_KEY"],
        "xiaomi-token-plan-sgp" => &["XIAOMI_TOKEN_PLAN_SGP_API_KEY"],
        "github-copilot" => &["COPILOT_GITHUB_TOKEN"],
        "openai-codex" => &["OPENAI_CODEX_API_KEY", "OPENAI_API_KEY"],
        _ => &[],
    }
}

pub fn env_api_key(provider: &str) -> Option<String> {
    env_api_key_with_source(provider).map(|(value, _source)| value)
}

pub fn env_api_key_with_source(provider: &str) -> Option<(String, String)> {
    for var in provider_env_vars(provider) {
        if let Some(val) = non_empty_env(var) {
            return Some((val, (*var).to_string()));
        }
    }
    None
}

pub(crate) fn non_empty_env(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .filter(|value| !value.trim().is_empty())
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;
    use std::sync::{Mutex, MutexGuard};

    use super::*;

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    struct EnvGuard<'a> {
        _lock: MutexGuard<'a, ()>,
        saved: Vec<(&'static str, Option<OsString>)>,
    }

    impl Drop for EnvGuard<'_> {
        fn drop(&mut self) {
            for (name, value) in self.saved.iter().rev() {
                unsafe {
                    match value {
                        Some(value) => std::env::set_var(name, value),
                        None => std::env::remove_var(name),
                    }
                }
            }
        }
    }

    fn env_guard(names: &[&'static str]) -> EnvGuard<'static> {
        let lock = ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let saved = names
            .iter()
            .map(|name| (*name, std::env::var_os(name)))
            .collect();
        EnvGuard { _lock: lock, saved }
    }

    #[test]
    fn returns_none_when_not_set() {
        let _guard = env_guard(&["ANTHROPIC_API_KEY", "CLAUDE_API_KEY", "ANTHROPIC_KEY"]);
        unsafe {
            std::env::remove_var("ANTHROPIC_API_KEY");
            std::env::remove_var("CLAUDE_API_KEY");
            std::env::remove_var("ANTHROPIC_KEY");
        }
        assert_eq!(env_api_key("anthropic"), None);
    }

    #[test]
    fn returns_anthropic_key() {
        let _guard = env_guard(&["ANTHROPIC_API_KEY"]);
        unsafe {
            std::env::set_var("ANTHROPIC_API_KEY", "sk-ant-test");
        }
        assert_eq!(env_api_key("anthropic"), Some("sk-ant-test".into()));
    }

    #[test]
    fn returns_none_for_unknown_provider() {
        assert_eq!(env_api_key("nonexistent"), None);
    }

    #[test]
    fn returns_deepseek_key() {
        let _guard = env_guard(&["DEEPSEEK_API_KEY"]);
        unsafe {
            std::env::set_var("DEEPSEEK_API_KEY", "sk-deepseek-test");
        }
        assert_eq!(env_api_key("deepseek"), Some("sk-deepseek-test".into()));
    }

    #[test]
    fn returns_minimax_key() {
        let _guard = env_guard(&["MINIMAX_API_KEY"]);
        unsafe {
            std::env::set_var("MINIMAX_API_KEY", "mm-test");
        }
        assert_eq!(env_api_key("minimax"), Some("mm-test".into()));
    }

    #[test]
    fn returns_copilot_token() {
        let _guard = env_guard(&["COPILOT_GITHUB_TOKEN"]);
        unsafe {
            std::env::set_var("COPILOT_GITHUB_TOKEN", "ghp-test");
        }
        assert_eq!(env_api_key("github-copilot"), Some("ghp-test".into()));
    }
}
