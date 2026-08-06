/// Provider-neutral agent runtime configuration, messages, events, and
/// lifecycle. Product policy and provider construction do not belong here.
pub mod agent {
    pub use crate::agent::types::{
        AgentConfig, AgentConfigError, AgentEvent, AgentInputQueue, AgentMessage, AgentQueueError,
        AgentResources, AgentStream, CompactionConfig, CompactionSampler, CompactionSettings,
        MAX_AGENT_QUEUE_BYTES, MAX_AGENT_QUEUE_ITEMS, MAX_AGENT_TURNS,
        MAX_COMPACTION_INSTRUCTION_BYTES, MAX_COMPACTION_TOKEN_BUDGET, ProviderRequestSnapshot,
        ProviderStreamer, QueueMode, ThinkingLevel,
    };
    pub use crate::agent::{Agent, AgentAdmissionError};
    pub use crate::hooks::{
        AfterToolCallContext, AfterToolCallHook, AfterToolCallResult, AgentHooks,
        AgentLoopTurnUpdate, BeforeProviderRequestContext, BeforeProviderRequestHook,
        BeforeProviderRequestResult, BeforeToolCallContext, BeforeToolCallHook,
        BeforeToolCallResult, ConvertToLlmHook, HookFuture, PrepareNextTurnContext,
        PrepareNextTurnHook, ShouldStopAfterTurnContext, ShouldStopAfterTurnHook,
        ShouldStopAfterTurnResult, TransformContextHook,
    };
}

/// Tool definitions and provider-neutral tool execution results.
pub mod tool {
    pub use crate::agent::types::{
        AgentToolOutput, AgentToolResult, ToolExecutionContext, ToolUpdateCallback,
    };
}

/// Capability-neutral filesystem and shell execution contracts plus output
/// shaping helpers used by coding tools.
pub mod execution {
    pub use crate::execution::capture::{
        MAX_SHELL_OUTPUT_EVENTS, MAX_SHELL_RETAINED_BYTES, MAX_SHELL_RETAINED_LINES,
        MAX_SHELL_SPOOL_BYTES, ShellCaptureOptions, ShellCaptureResult, bash_execution_to_text,
        execute_shell_with_capture, sanitize_binary_output,
    };
    pub use crate::execution::truncate::{
        TruncationLimit, TruncationResult, format_size, truncate_head, truncate_line, truncate_tail,
    };
    pub use crate::execution::{
        ExecOptions, ExecutionEnv, ExecutionEvent, ExecutionOutput, ExecutionStream, FileInfo,
        FileKind, FileSystem, MAX_SHELL_OUTPUT_CHUNK_BYTES, Shell,
    };
    pub use crate::execution::{ExecutionError, ExecutionErrorCode, FileError, FileErrorCode};
}

/// Provider-neutral skills, prompt templates, diagnostics, and parsing.
pub mod resources {
    pub use crate::resources::{
        AgentResources, DiagnosticSeverity, MAX_RESOURCE_DEPTH, MAX_RESOURCE_ENTRIES,
        MAX_RESOURCE_FILE_BYTES, MAX_RESOURCE_FILES, MAX_RESOURCE_ROOTS, MAX_RESOURCE_TOTAL_BYTES,
        PromptTemplate, ResourceDiagnostic, ResourceLoadError, ResourceLoadLimit,
        ResourceLoadPolicy, Skill, SourceTag, SourcedPromptTemplate, SourcedResourceDiagnostic,
        SourcedSkill,
    };
    pub use crate::resources::{
        format_prompt_template_invocation, format_skill_invocation,
        format_skills_for_system_prompt, load_prompt_templates, load_prompt_templates_async,
        load_prompt_templates_with_policy, load_skills, load_skills_async, load_skills_with_policy,
        load_sourced_prompt_templates, load_sourced_prompt_templates_async,
        load_sourced_prompt_templates_with_policy, load_sourced_skills, load_sourced_skills_async,
        load_sourced_skills_with_policy, parse_command_args, parse_frontmatter, substitute_args,
    };
}

/// Token estimation and summarization primitives. Durable compaction policy
/// remains owned by the product session layer.
pub mod compaction {
    pub use crate::compaction::error::CompactionError;
    pub use crate::compaction::estimate::{
        ContextUsageEstimate, TokenEstimationConfig, calculate_context_tokens,
        estimate_context_tokens, estimate_tokens,
    };
    pub use crate::compaction::prepare::{prepare_compaction, should_compact};
    pub use crate::compaction::summarize::{
        MAX_SUMMARY_INPUT_BYTES, MAX_SUMMARY_RECORDS, build_summarization_context,
        serialize_conversation, summarize, summarize_with_provider_streamer,
    };
}

/// Provider-neutral transcript records, tree projection, and identifiers.
pub mod transcript {
    pub use crate::transcript::{
        SessionEntry, SessionHeader, SessionIdGenerator, SessionMetadata, SessionTreeNode,
        StoredAgentMessage, StoredUsage, StoredUsageCost, TranscriptIdError,
        agent_message_to_stored, create_session_id, create_timestamp, generate_entry_id,
    };
}
