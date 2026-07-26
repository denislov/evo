//! Stable facade import and behavior coverage.

use agent_core::api::agent::{
    AfterToolCallContext, AfterToolCallHook, AfterToolCallResult, Agent, AgentConfig, AgentEvent,
    AgentHooks, AgentLoopTurnUpdate, AgentMessage, AgentResources, AgentStream,
    BeforeProviderRequestContext, BeforeProviderRequestHook, BeforeProviderRequestResult,
    BeforeToolCallContext, BeforeToolCallHook, BeforeToolCallResult, CompactionConfig,
    CompactionSettings, ConvertToLlmHook, HookFuture, PrepareNextTurnContext, PrepareNextTurnHook,
    ProviderRequestSnapshot, QueueMode, ShouldStopAfterTurnContext, ShouldStopAfterTurnHook,
    ThinkingLevel, TransformContextHook,
};
use agent_core::api::execution::{
    ExecOptions, ExecutionEnv, ExecutionError, ExecutionErrorCode, ExecutionOutput, FileError,
    FileErrorCode, FileInfo, FileKind, FileSystem, Shell, ShellCaptureOptions, ShellCaptureResult,
    TruncationLimit, TruncationResult, bash_execution_to_text, format_size, sanitize_binary_output,
    truncate_head, truncate_line, truncate_tail,
};
use agent_core::api::resources::{
    DiagnosticSeverity, MAX_RESOURCE_DEPTH, MAX_RESOURCE_ENTRIES, MAX_RESOURCE_FILE_BYTES,
    MAX_RESOURCE_FILES, MAX_RESOURCE_ROOTS, MAX_RESOURCE_TOTAL_BYTES, PromptTemplate,
    ResourceDiagnostic, ResourceLoadError, ResourceLoadLimit, ResourceLoadPolicy, Skill, SourceTag,
    SourcedPromptTemplate, SourcedResourceDiagnostic, SourcedSkill,
    format_prompt_template_invocation, format_skill_invocation, format_skills_for_system_prompt,
    load_prompt_templates, load_prompt_templates_async, load_prompt_templates_with_policy,
    load_skills, load_skills_async, load_skills_with_policy, load_sourced_prompt_templates,
    load_sourced_prompt_templates_async, load_sourced_prompt_templates_with_policy,
    load_sourced_skills, load_sourced_skills_async, load_sourced_skills_with_policy,
    parse_command_args, parse_frontmatter, substitute_args,
};
use agent_core::api::testing::{
    InMemoryExecutionEnv, assemble_context, convert_to_context, default_convert_to_llm,
};
use agent_core::api::tool::{
    AgentTool, AgentToolDefinitionError, AgentToolOutput, AgentToolResult, ToolExecutionMode,
    ToolFn, ToolUpdateCallback,
};

#[test]
fn categorized_facade_exposes_only_intentional_contract_groups() {
    use agent_core::api::agent::{
        Agent, AgentConfig, AgentEvent, AgentMessage, AgentResources, AgentStream,
        ProviderStreamer, QueueMode, ThinkingLevel,
    };
    use agent_core::api::compaction::{
        ContextUsageEstimate, estimate_tokens, summarize_with_provider_streamer,
    };
    use agent_core::api::execution::{ExecOptions, ExecutionEnv, FileError, FileSystem};
    use agent_core::api::resources::{PromptTemplate, ResourceDiagnostic, Skill};
    use agent_core::api::testing::InMemoryExecutionEnv;
    use agent_core::api::tool::{
        AgentTool, AgentToolOutput, AgentToolResult, ToolExecutionContext, ToolExecutionMode,
        ToolFn, ToolUpdateCallback,
    };
    use agent_core::api::transcript::{SessionEntry, SessionHeader, StoredAgentMessage};

    let categories = [
        std::any::type_name::<Agent>(),
        std::any::type_name::<AgentConfig>(),
        std::any::type_name::<AgentEvent>(),
        std::any::type_name::<AgentMessage>(),
        std::any::type_name::<AgentResources>(),
        std::any::type_name::<AgentStream>(),
        std::any::type_name::<ProviderStreamer>(),
        std::any::type_name::<QueueMode>(),
        std::any::type_name::<ThinkingLevel>(),
        std::any::type_name::<AgentTool>(),
        std::any::type_name::<AgentToolOutput>(),
        std::any::type_name::<AgentToolResult>(),
        std::any::type_name::<ToolExecutionMode>(),
        std::any::type_name::<ToolExecutionContext>(),
        std::any::type_name::<ToolFn>(),
        std::any::type_name::<ToolUpdateCallback>(),
        std::any::type_name::<ExecOptions>(),
        std::any::type_name::<FileError>(),
        std::any::type_name::<PromptTemplate>(),
        std::any::type_name::<ResourceDiagnostic>(),
        std::any::type_name::<Skill>(),
        std::any::type_name::<ContextUsageEstimate>(),
        std::any::type_name::<SessionEntry>(),
        std::any::type_name::<SessionHeader>(),
        std::any::type_name::<StoredAgentMessage>(),
        std::any::type_name::<InMemoryExecutionEnv>(),
    ];
    assert!(categories.iter().all(|name| !name.is_empty()));

    fn accepts_execution<T: ExecutionEnv + FileSystem>() {}
    let _ = accepts_execution::<InMemoryExecutionEnv>;
    let _ = estimate_tokens;
    let _ = summarize_with_provider_streamer;
}

#[test]
fn low_level_runtime_symbols_are_importable_from_api_facade() {
    #[allow(clippy::too_many_arguments)]
    fn accepts_types(
        _agent: Option<Agent>,
        _config: Option<AgentConfig>,
        _event: Option<AgentEvent>,
        _hooks: Option<AgentHooks>,
        _message: Option<AgentMessage>,
        _resources: Option<AgentResources>,
        _stream: Option<AgentStream>,
        _tool: Option<AgentTool>,
        _tool_definition_error: Option<AgentToolDefinitionError>,
        _tool_output: Option<AgentToolOutput>,
        _tool_result: Option<AgentToolResult>,
        _compaction_config: Option<CompactionConfig>,
        _compaction_settings: Option<CompactionSettings>,
        _exec_options: Option<ExecOptions>,
        _execution_output: Option<ExecutionOutput>,
        _file_info: Option<FileInfo>,
        _file_kind: Option<FileKind>,
        _prompt_template: Option<PromptTemplate>,
        _provider_snapshot: Option<ProviderRequestSnapshot>,
        _queue_mode: Option<QueueMode>,
        _diagnostic: Option<ResourceDiagnostic>,
        _skill: Option<Skill>,
        _source_tag: Option<SourceTag>,
        _sourced_template: Option<SourcedPromptTemplate>,
        _sourced_diagnostic: Option<SourcedResourceDiagnostic>,
        _sourced_skill: Option<SourcedSkill>,
        _thinking: Option<ThinkingLevel>,
        _tool_mode: Option<ToolExecutionMode>,
        _tool_fn: Option<ToolFn>,
        _tool_update: Option<ToolUpdateCallback>,
        _execution_error: Option<ExecutionError>,
        _file_error: Option<FileError>,
    ) {
    }

    accepts_types(
        None, None, None, None, None, None, None, None, None, None, None, None, None, None, None,
        None, None, None, None, None, None, None, None, None, None, None, None, None, None, None,
        None, None,
    );

    fn accepts_traits<T: ExecutionEnv + FileSystem + Shell>(_env: &T) {}
    let env = InMemoryExecutionEnv::new(".");
    accepts_traits(&env);
}

#[test]
fn message_conversion_helpers_are_importable_from_api_facade() {
    let messages = vec![AgentMessage::UserText {
        message_id: "user_1".into(),
        text: "hello".into(),
    }];
    let resources = AgentResources::default();
    let llm_messages = default_convert_to_llm(&messages, &resources);
    let assembled = assemble_context(&None, &messages, llm_messages, &[], &resources);
    let converted = convert_to_context(&None, &messages, &[], &resources);

    assert_eq!(assembled.messages, converted.messages);
    assert!(bash_execution_to_text("pwd", "", Some(0), false, false, None).contains("`pwd`"));
}

#[test]
fn low_level_error_codes_are_importable_from_api_facade() {
    use agent_core::api::agent::{
        AgentConfigError, MAX_AGENT_TURNS, MAX_COMPACTION_INSTRUCTION_BYTES,
        MAX_COMPACTION_TOKEN_BUDGET,
    };
    use agent_core::api::compaction::{
        CompactionError, MAX_SUMMARY_INPUT_BYTES, MAX_SUMMARY_RECORDS,
    };

    let _: Option<AgentConfigError> = None;
    let _: Option<CompactionError> = None;
    let _limits = (
        MAX_AGENT_TURNS,
        MAX_COMPACTION_TOKEN_BUDGET,
        MAX_COMPACTION_INSTRUCTION_BYTES,
        MAX_SUMMARY_INPUT_BYTES,
        MAX_SUMMARY_RECORDS,
    );

    let execution_error = ExecutionError::Timeout {
        message: "command timed out".into(),
    };
    assert_eq!(execution_error.code(), ExecutionErrorCode::Timeout);
    assert_eq!(
        ExecutionErrorCode::ShellUnavailable.as_str(),
        "shell_unavailable"
    );

    let file_error = FileError::NotFound {
        message: "missing".into(),
        path: Some(std::path::PathBuf::from("missing.txt")),
    };
    assert_eq!(file_error.code(), FileErrorCode::NotFound);
    assert_eq!(FileErrorCode::NotADirectory.as_str(), "not_a_directory");
}

#[test]
fn hook_contract_symbols_are_importable_from_api_facade() {
    let _: Option<HookFuture<()>> = None;
    let _: Option<BeforeProviderRequestHook> = None;
    let _: Option<BeforeProviderRequestContext> = None;
    let _: Option<BeforeProviderRequestResult> = None;
    let _: Option<BeforeToolCallHook> = None;
    let _: Option<BeforeToolCallContext> = None;
    let _: Option<BeforeToolCallResult> = None;
    let _: Option<AfterToolCallHook> = None;
    let _: Option<AfterToolCallContext> = None;
    let _: Option<AfterToolCallResult> = None;
    let _: Option<ShouldStopAfterTurnHook> = None;
    let _: Option<ShouldStopAfterTurnContext> = None;
    let _: Option<PrepareNextTurnHook> = None;
    let _: Option<PrepareNextTurnContext> = None;
    let _: Option<AgentLoopTurnUpdate> = None;
    let _: Option<TransformContextHook> = None;
    let _: Option<ConvertToLlmHook> = None;

    assert!(AgentHooks::default().is_empty());
}

#[test]
fn resource_argument_helpers_are_importable_from_api_facade() {
    let args = parse_command_args(r#"alpha "two words" gamma"#);

    assert_eq!(args, vec!["alpha", "two words", "gamma"]);
    assert_eq!(
        substitute_args("run $1 with ${2:-fallback}", &args),
        "run alpha with two words"
    );

    let diagnostic = ResourceDiagnostic {
        severity: DiagnosticSeverity::Warning,
        code: "resource.missing".into(),
        message: "missing resource".into(),
        path: std::path::PathBuf::from("AGENTS.md"),
    };
    assert_eq!(diagnostic.severity, DiagnosticSeverity::Warning);
}

#[test]
fn resource_loading_helpers_are_importable_from_api_facade() {
    let _: Option<ResourceLoadError> = None;
    let _: Option<ResourceLoadLimit> = None;
    let _ = ResourceLoadPolicy::default();
    let _limits = (
        MAX_RESOURCE_ROOTS,
        MAX_RESOURCE_ENTRIES,
        MAX_RESOURCE_FILES,
        MAX_RESOURCE_FILE_BYTES,
        MAX_RESOURCE_TOTAL_BYTES,
        MAX_RESOURCE_DEPTH,
    );
    let _ = load_skills_with_policy;
    let _ = load_prompt_templates_with_policy;
    let _ = load_sourced_skills_with_policy;
    let _ = load_sourced_prompt_templates_with_policy;
    let _ = load_skills_async;
    let _ = load_prompt_templates_async;
    let _ = load_sourced_skills_async;
    let _ = load_sourced_prompt_templates_async;

    let (metadata, body, diagnostics) =
        parse_frontmatter("---\nname: review\ndescription: Review changes\n---\n\nReview $1");
    assert!(diagnostics.is_empty());
    assert_eq!(metadata["name"].as_str(), Some("review"));
    assert_eq!(body, "Review $1");

    let skill = Skill {
        name: "review".into(),
        description: "Review code".into(),
        location: "/skills/review/SKILL.md".into(),
        content: "Review instructions".into(),
        disable_model_invocation: false,
    };
    let skills_xml = format_skills_for_system_prompt(std::slice::from_ref(&skill));
    assert!(skills_xml.contains("<name>review</name>"));
    assert!(skills_xml.contains("Review code"));

    let invocation = format_skill_invocation(
        "review",
        "/skills/review/SKILL.md",
        "Review instructions",
        Some("Use concise findings."),
    );
    assert!(invocation.contains("<skill name=\"review\""));
    assert!(invocation.contains("Use concise findings."));
    assert_eq!(
        format_prompt_template_invocation("review", "Review $1", &["diff".to_string()]),
        "Review diff"
    );

    let temp_dir = tempfile::tempdir().expect("temp dir");
    let skill_dir = temp_dir.path().join("review");
    std::fs::create_dir(&skill_dir).expect("skill dir");
    std::fs::write(
        skill_dir.join("SKILL.md"),
        "---\nname: review\ndescription: Review code\n---\n\nReview instructions",
    )
    .expect("skill file");
    let template_file = temp_dir.path().join("review-template.md");
    std::fs::write(
        &template_file,
        "---\nname: review-template\ndescription: Review template\n---\n\nReview $1",
    )
    .expect("template file");

    let (skills, skill_diagnostics) = load_skills(std::slice::from_ref(&skill_dir));
    assert!(skill_diagnostics.is_empty());
    assert_eq!(skills[0].name, "review");

    let (templates, template_diagnostics) =
        load_prompt_templates(std::slice::from_ref(&template_file));
    assert!(template_diagnostics.is_empty());
    assert_eq!(templates[0].name, "review-template");

    let source = SourceTag {
        source_path: temp_dir.path().to_path_buf(),
        source_type: "project".into(),
    };
    let sourced_skill_inputs = vec![(skill_dir, source.clone())];
    let (sourced_skills, sourced_skill_diagnostics) = load_sourced_skills(&sourced_skill_inputs);
    assert!(sourced_skill_diagnostics.is_empty());
    assert_eq!(sourced_skills[0].source.source_type, "project");

    let sourced_template_inputs = vec![(template_file, source)];
    let (sourced_templates, sourced_template_diagnostics) =
        load_sourced_prompt_templates(&sourced_template_inputs);
    assert!(sourced_template_diagnostics.is_empty());
    assert_eq!(sourced_templates[0].template.name, "review-template");
}

#[test]
fn output_boundary_helpers_are_importable_from_api_facade() {
    let cleaned = sanitize_binary_output("ok\u{0} still\n");
    assert_eq!(cleaned, "ok still\n");
    assert_eq!(format_size(2048), "2.0KB");

    let limit = TruncationLimit {
        max_lines: 2,
        max_bytes: 8,
    };
    let head = truncate_head("one\ntwo\nthree", limit);
    let tail = truncate_tail("one\ntwo\nthree", limit);
    let _result_type_name = std::any::type_name::<TruncationResult>();

    assert!(head.truncated);
    assert_eq!(head.content, "one\ntwo");
    assert!(tail.truncated);
    assert_eq!(tail.content, "three");

    let (line, truncated) = truncate_line("abcdef", 3);
    assert_eq!(line, "abc... [truncated]");
    assert!(truncated);

    let capture_options = ShellCaptureOptions::default();
    let capture_result = ShellCaptureResult {
        output: String::new(),
        exit_code: Some(0),
        cancelled: false,
        truncated: false,
        full_output_path: None,
    };
    assert!(capture_options.max_bytes > 0);
    assert_eq!(capture_result.exit_code, Some(0));
}

#[test]
fn add_tool_rejects_invalid_tool_before_provider_context() {
    let agent = Agent::new(AgentConfig::new(test_model()));
    let invalid_tool = AgentTool::new_text(
        " ",
        "empty names are invalid",
        serde_json::json!({"type": "object"}),
        |_, _| async { Ok("ok".to_string()) },
    );

    let error = agent.add_tool(invalid_tool).unwrap_err();

    assert_eq!(error.field(), "name");
    let (context, _) = agent.provider_request_snapshot();
    assert!(context.tools.unwrap_or_default().is_empty());
}

#[test]
fn add_tool_rejects_unsupported_schema_and_duplicate_names_atomically() {
    let agent = Agent::new(AgentConfig::new(test_model()));
    let invalid_schema = AgentTool::new_text(
        "invalid",
        "invalid schema",
        serde_json::json!({
            "type": "object",
            "oneOf": [{"type": "object"}]
        }),
        |_, _| async { Ok("ok".to_string()) },
    );
    let error = agent.add_tool(invalid_schema).unwrap_err();
    assert_eq!(error.field(), "parameters");

    let first = AgentTool::new_text(
        "echo",
        "first",
        serde_json::json!({"type": "object"}),
        |_, _| async { Ok("first".to_string()) },
    );
    let duplicate = AgentTool::new_text(
        "echo",
        "duplicate",
        serde_json::json!({"type": "object"}),
        |_, _| async { Ok("duplicate".to_string()) },
    );
    agent.add_tool(first).unwrap();
    let error = agent.add_tool(duplicate).unwrap_err();
    assert_eq!(error.field(), "name");

    let (context, _) = agent.provider_request_snapshot();
    let tools = context.tools.unwrap();
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0].name, "echo");
    assert_eq!(tools[0].description.as_deref(), Some("first"));
}

fn test_model() -> ai::api::model::Model {
    ai::api::model::Model {
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
    }
}

#[test]
fn transcript_types_are_importable_from_neutral_module() {
    use agent_core::api::transcript::{
        SessionEntry, SessionHeader, SessionTreeNode, StoredAgentMessage, StoredUsage,
        StoredUsageCost, create_session_id, create_timestamp, generate_entry_id,
    };

    let _ = std::any::type_name::<SessionHeader>();
    let _ = std::any::type_name::<SessionEntry>();
    let _ = std::any::type_name::<SessionTreeNode>();
    let _ = std::any::type_name::<StoredAgentMessage>();
    let _ = std::any::type_name::<StoredUsage>();
    let _ = std::any::type_name::<StoredUsageCost>();
    assert!(!create_session_id().is_empty());
    assert!(create_timestamp().ends_with('Z'));
    let existing = std::collections::HashSet::new();
    assert!(!generate_entry_id(&existing).is_empty());
}
