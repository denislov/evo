use crate::compatibility::ThinkingFormat;

#[derive(Debug, Clone, Default)]
pub struct CompatFlags {
    pub supports_developer_role: bool,
    pub supports_reasoning_effort: bool,
    pub supports_usage_in_streaming: bool,
    pub supports_strict_mode: bool,
    pub max_tokens_field: String,
    pub thinking_format: Option<ThinkingFormat>,
    pub requires_reasoning_content_on_assistant_messages: bool,
}

pub fn resolve_completions_compat(model: &crate::model::Model) -> CompatFlags {
    let compat = crate::compatibility::OpenAICompletionsCompat::from_model(model);
    let mut flags = CompatFlags {
        supports_developer_role: false,
        supports_reasoning_effort: false,
        supports_usage_in_streaming: true,
        supports_strict_mode: false,
        max_tokens_field: "max_completion_tokens".to_string(),
        thinking_format: compat.thinking_format.clone(),
        requires_reasoning_content_on_assistant_messages: false,
    };

    if let Some(value) = compat.supports_developer_role {
        flags.supports_developer_role = value;
    }
    if let Some(value) = compat.supports_reasoning_effort {
        flags.supports_reasoning_effort = value;
    }
    if let Some(value) = compat.supports_usage_in_streaming {
        flags.supports_usage_in_streaming = value;
    }
    if let Some(value) = compat.supports_strict_mode {
        flags.supports_strict_mode = value;
    }
    if let Some(value) = compat.max_tokens_field {
        flags.max_tokens_field = value;
    }
    if let Some(value) = compat.requires_reasoning_content_on_assistant_messages {
        flags.requires_reasoning_content_on_assistant_messages = value;
    }

    flags
}
