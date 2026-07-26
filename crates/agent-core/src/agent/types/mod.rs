mod config;
mod event;
mod message;
mod thinking;
mod tool;

pub use super::provider::ProviderStreamer;
pub use super::queue::{
    AgentInputQueue, AgentQueueError, MAX_AGENT_QUEUE_BYTES, MAX_AGENT_QUEUE_ITEMS, QueueMode,
};
pub use crate::resources::{
    AgentResources, DiagnosticSeverity, PromptTemplate, ResourceDiagnostic, Skill, SourceTag,
    SourcedPromptTemplate, SourcedResourceDiagnostic, SourcedSkill,
};
pub use config::{
    AgentConfig, AgentConfigError, CompactionConfig, CompactionSettings, MAX_AGENT_TURNS,
    MAX_COMPACTION_INSTRUCTION_BYTES, MAX_COMPACTION_TOKEN_BUDGET,
};
pub use event::{AgentEvent, AgentStream, ProviderRequestSnapshot};
pub use message::AgentMessage;
pub use thinking::ThinkingLevel;
pub use tool::{
    AgentTool, AgentToolArgumentError, AgentToolDefinitionError, AgentToolOutput, AgentToolResult,
    ToolExecutionContext, ToolExecutionMode, ToolFn, ToolUpdateCallback,
    tool_arguments_match_schema,
};

// ── Unit tests ─────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use ai::api::conversation::ContentBlock;
    use std::sync::Arc;

    fn make_text_tool() -> AgentTool {
        AgentTool {
            name: "echo".into(),
            description: "echoes input".into(),
            parameters: serde_json::json!({"type": "object", "properties": {}}),
            execution_mode: None,
            execute: Arc::new(|_context, args, _on_update| {
                let text = args
                    .get("text")
                    .and_then(|v| v.as_str())
                    .unwrap_or("no text");
                let result: Vec<ContentBlock> = vec![ContentBlock::Text {
                    text: text.to_string(),
                    text_signature: None,
                }];
                Box::pin(async move { Ok(AgentToolOutput::new(result)) })
            }),
        }
    }

    #[test]
    fn agent_message_user_text_constructs() {
        let msg = AgentMessage::UserText {
            message_id: "1".into(),
            text: "hello".into(),
        };
        match &msg {
            AgentMessage::UserText { text, .. } => assert_eq!(text, "hello"),
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn agent_tool_has_correct_fields() {
        let tool = make_text_tool();
        assert_eq!(tool.name, "echo");
        assert!(tool.description.contains("echoes"));
    }

    #[tokio::test]
    async fn tool_fn_executes() {
        let tool = make_text_tool();
        let result = (tool.execute)(
            ToolExecutionContext::standalone("echo"),
            serde_json::json!({"text": "hi"}),
            None,
        )
        .await;
        assert!(result.is_ok());
        let output = result.unwrap();
        assert_eq!(output.content.len(), 1);
        assert_eq!(output.details, None);
    }

    #[test]
    fn thinking_level_parses_cli_values() {
        assert_eq!("off".parse::<ThinkingLevel>().unwrap(), ThinkingLevel::Off);
        assert_eq!(
            "minimal".parse::<ThinkingLevel>().unwrap(),
            ThinkingLevel::Minimal
        );
        assert_eq!("low".parse::<ThinkingLevel>().unwrap(), ThinkingLevel::Low);
        assert_eq!(
            "medium".parse::<ThinkingLevel>().unwrap(),
            ThinkingLevel::Medium
        );
        assert_eq!(
            "high".parse::<ThinkingLevel>().unwrap(),
            ThinkingLevel::High
        );
        assert_eq!(
            "xhigh".parse::<ThinkingLevel>().unwrap(),
            ThinkingLevel::XHigh
        );
        assert!("extreme".parse::<ThinkingLevel>().is_err());
    }

    #[test]
    fn tool_execution_mode_parses_cli_values() {
        assert_eq!(
            "parallel".parse::<ToolExecutionMode>().unwrap(),
            ToolExecutionMode::Parallel
        );
        assert_eq!(
            "sequential".parse::<ToolExecutionMode>().unwrap(),
            ToolExecutionMode::Sequential
        );
        assert!("serial".parse::<ToolExecutionMode>().is_err());
    }

    #[test]
    fn queue_mode_parses_cli_values() {
        assert_eq!("all".parse::<QueueMode>().unwrap(), QueueMode::All);
        assert_eq!(
            "one-at-a-time".parse::<QueueMode>().unwrap(),
            QueueMode::OneAtATime
        );
        assert!("one".parse::<QueueMode>().is_err());
    }

    #[test]
    fn agent_config_defaults_match_m4_baseline() {
        let model = ai::api::model::Model {
            id: "test".into(),
            name: "Test".into(),
            api: "test-api".into(),
            provider: "test-provider".into(),
            base_url: "https://example.invalid".into(),
            reasoning: false,
            thinking_level_map: None,
            input: vec![ai::api::model::ModelInput::Text],
            cost: ai::api::model::ModelCost::default(),
            context_window: 8000,
            max_tokens: 1024,
            headers: None,
            compat: None,
        };
        let config = AgentConfig::new(model);
        assert_eq!(config.thinking_level, ThinkingLevel::Off);
        assert_eq!(config.tool_execution, ToolExecutionMode::Parallel);
        assert_eq!(config.steering_mode, QueueMode::OneAtATime);
        assert_eq!(config.follow_up_mode, QueueMode::OneAtATime);
        assert!(config.hooks.is_empty());
        assert!(config.resources.is_empty());
        assert!(config.compaction.is_none());
    }

    #[test]
    fn agent_tool_defaults_to_global_execution_mode() {
        let tool = AgentTool::new_text(
            "echo",
            "echo input",
            serde_json::json!({"type": "object"}),
            |_, _| async { Ok("ok".to_string()) },
        );
        assert_eq!(tool.execution_mode, None);
    }

    #[test]
    fn agent_tool_validation_accepts_object_schema() {
        let tool = make_text_tool();

        assert!(tool.validate().is_ok());
    }

    #[test]
    fn agent_tool_validation_rejects_empty_name() {
        let mut tool = make_text_tool();
        tool.name = "  ".into();

        let error = tool.validate().unwrap_err();

        assert_eq!(error.field(), "name");
        assert!(error.to_string().contains("tool name"));
    }

    #[test]
    fn agent_tool_validation_rejects_non_object_parameters() {
        let mut tool = make_text_tool();
        tool.parameters = serde_json::json!(["not", "a", "schema"]);

        let error = tool.validate().unwrap_err();

        assert_eq!(error.field(), "parameters");
        assert!(error.to_string().contains("schema node must be an object"));
    }

    #[test]
    fn agent_tool_result_ok_constructs() {
        let result = AgentToolResult::ok(vec![ContentBlock::Text {
            text: "hello".into(),
            text_signature: None,
        }]);
        assert!(!result.is_error);
        assert!(!result.terminate);
        assert_eq!(result.details, None);
        assert_eq!(result.content.len(), 1);
    }

    #[test]
    fn agent_tool_result_error_constructs() {
        let result = AgentToolResult::error("something went wrong");
        assert!(result.is_error);
        assert!(!result.terminate);
        assert_eq!(result.details, None);
        assert_eq!(result.content.len(), 1);
    }
}
