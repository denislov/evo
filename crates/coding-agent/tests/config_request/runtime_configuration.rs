//! Product runtime construction behavior.

use crate::internal_tests::product_fixture::command::ApplicationError;
use crate::internal_tests::product_fixture::configuration::{
    CompactionSettings, DEFAULT_MODEL_ID, DEFAULT_SYSTEM_PROMPT, RetrySettings, Settings,
    TerminalSettings, TuiMode, build_agent_config, select_model,
};
use agent_core::api::resources::AgentResources;
use coding_agent::api::embedding::CodingAgentInvocationOptions;

fn invocation() -> CodingAgentInvocationOptions {
    CodingAgentInvocationOptions {
        prompt: Some("hello".into()),
        ..Default::default()
    }
}

fn runtime_settings() -> Settings {
    Settings {
        default_provider: None,
        default_model: None,
        default_thinking_level: None,
        session_naming_model: None,
        steering_mode: "one-at-a-time".into(),
        follow_up_mode: "one-at-a-time".into(),
        session_dir: None,
        skills: Vec::new(),
        prompts: Vec::new(),
        themes: Vec::new(),
        theme: None,
        no_context_files: false,
        hide_thinking_block: false,
        quiet_startup: false,
        enable_skill_commands: true,
        double_escape_action: "tree".into(),
        tree_filter_mode: "default".into(),
        shell_path: None,
        shell_command_prefix: None,
        http_idle_timeout_ms: 300000,
        enabled_models: Vec::new(),
        terminal: TerminalSettings {
            mode: TuiMode::Inline,
            show_images: true,
            show_progress: true,
            clear_on_shrink: false,
            auto_resize_images: true,
            block_images: false,
            image_width_cells: 60,
        },
        compaction: CompactionSettings {
            enabled: true,
            reserve_tokens: 1234,
            keep_recent_tokens: 5678,
        },
        retry: RetrySettings {
            enabled: true,
            max_retries: 7,
            base_delay_ms: 4444,
        },
    }
}

#[test]
fn selects_default_model_when_no_override_is_provided() {
    let invocation = invocation();
    let model = select_model(&invocation, None, None, None).unwrap();
    assert_eq!(model.id, DEFAULT_MODEL_ID);
}

#[test]
fn selects_explicit_model_from_static_table() {
    let invocation = CodingAgentInvocationOptions {
        model: Some("claude-haiku-4-5".into()),
        ..invocation()
    };
    let model = select_model(&invocation, None, None, None).unwrap();
    assert_eq!(model.id, "claude-haiku-4-5");
}

#[test]
fn unknown_model_returns_typed_error() {
    let invocation = CodingAgentInvocationOptions {
        model: Some("missing-model".into()),
        ..invocation()
    };
    assert_eq!(
        select_model(&invocation, None, None, None).unwrap_err(),
        ApplicationError::UnknownModel("missing-model".into())
    );
}

#[test]
fn model_override_is_used_when_cli_model_is_absent() {
    let invocation = invocation();
    let mut override_model = select_model(&invocation, None, None, None).unwrap();
    override_model.id = "override-model".into();
    let model = select_model(&invocation, None, None, Some(override_model)).unwrap();
    assert_eq!(model.id, "override-model");
}

#[test]
fn model_override_wins_over_settings_default_model() {
    let invocation = invocation();
    let mut override_model = select_model(&invocation, None, None, None).unwrap();
    override_model.id = "override-model".into();
    override_model.provider = "override-provider".into();
    let model = select_model(
        &invocation,
        Some("deepseek"),
        Some("claude-haiku-4-5"),
        Some(override_model),
    )
    .unwrap();
    assert_eq!(model.id, "override-model");
}

#[test]
fn selects_default_provider_when_cli_provider_is_absent() {
    let invocation = invocation();

    let model = select_model(&invocation, Some("deepseek"), None, None).unwrap();

    assert_eq!(model.provider, "deepseek");
}

#[test]
fn cli_provider_overrides_settings_default_provider() {
    let invocation = CodingAgentInvocationOptions {
        provider: Some("google".into()),
        ..invocation()
    };

    let model = select_model(&invocation, Some("deepseek"), None, None).unwrap();

    assert_eq!(model.provider, "google");
}

#[test]
fn builds_agent_config_with_defaults() {
    let invocation = invocation();
    let model = select_model(&invocation, None, None, None).unwrap();
    let config = build_agent_config(
        model,
        invocation.system_prompt.clone(),
        invocation.max_turns,
        invocation.api_key.clone(),
        None,
        None,
        AgentResources::default(),
        None,
    );
    assert_eq!(config.system_prompt.as_deref(), Some(DEFAULT_SYSTEM_PROMPT));
    assert_eq!(config.max_turns, None);
    assert!(config.stream_options.is_none());
}

#[test]
fn builds_agent_config_with_cli_overrides() {
    let invocation = CodingAgentInvocationOptions {
        api_key: Some("sk-test".into()),
        system_prompt: Some("Be brief.".into()),
        max_turns: Some(9),
        ..invocation()
    };
    let model = select_model(&invocation, None, None, None).unwrap();
    let config = build_agent_config(
        model,
        invocation.system_prompt.clone(),
        invocation.max_turns,
        invocation.api_key.clone(),
        None,
        None,
        AgentResources::default(),
        None,
    );
    assert_eq!(config.system_prompt.as_deref(), Some("Be brief."));
    assert_eq!(config.max_turns, Some(9));
    assert_eq!(
        config.stream_options.unwrap().api_key.as_deref(),
        Some("sk-test")
    );
}

#[test]
fn build_agent_config_applies_settings_retry_and_compaction() {
    let invocation = invocation();
    let model = select_model(&invocation, None, None, None).unwrap();
    let settings = runtime_settings();

    let config = build_agent_config(
        model,
        invocation.system_prompt.clone(),
        invocation.max_turns,
        None,
        None,
        None,
        AgentResources::default(),
        Some(&settings),
    );

    let stream = config.stream_options.unwrap();
    assert!(stream.transport.is_none());
    assert_eq!(stream.timeout_ms, Some(300000));
    assert_eq!(stream.max_retries, Some(7));
    assert_eq!(stream.max_retry_delay_ms, Some(4444));

    let compaction = config.compaction.unwrap();
    assert!(compaction.settings.enabled);
    assert_eq!(compaction.settings.reserve_tokens, 1234);
    assert_eq!(compaction.settings.keep_recent_tokens, 5678);
}

#[test]
fn build_agent_config_honors_disabled_settings_retry_and_compaction() {
    let invocation = invocation();
    let model = select_model(&invocation, None, None, None).unwrap();
    let mut settings = runtime_settings();
    settings.retry.enabled = false;
    settings.compaction.enabled = false;
    settings.http_idle_timeout_ms = 0;

    let config = build_agent_config(
        model,
        invocation.system_prompt.clone(),
        invocation.max_turns,
        None,
        None,
        None,
        AgentResources::default(),
        Some(&settings),
    );

    assert!(config.stream_options.is_none());
    assert!(config.compaction.is_none());
}

#[test]
fn build_agent_config_applies_http_idle_timeout_when_retry_is_disabled() {
    let invocation = invocation();
    let model = select_model(&invocation, None, None, None).unwrap();
    let mut settings = runtime_settings();
    settings.retry.enabled = false;
    settings.http_idle_timeout_ms = 12_345;

    let config = build_agent_config(
        model,
        invocation.system_prompt.clone(),
        invocation.max_turns,
        None,
        None,
        None,
        AgentResources::default(),
        Some(&settings),
    );

    let stream = config.stream_options.unwrap();
    assert!(stream.transport.is_none());
    assert_eq!(stream.timeout_ms, Some(12_345));
    assert!(stream.max_retries.is_none());
    assert!(stream.max_retry_delay_ms.is_none());
}

#[test]
fn build_agent_config_applies_default_thinking_level_from_settings() {
    let invocation = invocation();
    let model = select_model(&invocation, None, None, None).unwrap();
    let mut settings = runtime_settings();
    settings.default_thinking_level = Some("high".into());

    let config = build_agent_config(
        model,
        invocation.system_prompt.clone(),
        invocation.max_turns,
        None,
        None,
        None,
        AgentResources::default(),
        Some(&settings),
    );

    assert_eq!(
        config.thinking_level,
        agent_core::api::agent::ThinkingLevel::High
    );
}

#[test]
fn direct_deepseek_default_and_explicit_off_remain_product_visible_off() {
    let model = ai::api::model::lookup_model("deepseek-v4-flash").unwrap();
    for requested in [None, Some(agent_core::api::agent::ThinkingLevel::Off)] {
        let config = build_agent_config(
            model.clone(),
            None,
            None,
            None,
            requested,
            None,
            AgentResources::default(),
            None,
        );

        assert_eq!(
            config.thinking_level,
            agent_core::api::agent::ThinkingLevel::Off
        );
        assert_eq!(config.model.api, "openai-completions");
    }
}
