use ai_protocol::api::model::Model;
use ai_protocol::api::stream::StreamOptions;
use tool_contract::api::definition::ToolExecutionMode;

use crate::hooks::AgentHooks;

use super::{AgentResources, ProviderStreamer, QueueMode, ThinkingLevel};

// ── Compaction types ───────────────────────────────

pub const MAX_AGENT_TURNS: u32 = 10_000;
pub const MAX_COMPACTION_TOKEN_BUDGET: u32 = 16 * 1024 * 1024;
pub const MAX_COMPACTION_INSTRUCTION_BYTES: usize = 64 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AgentConfigError {
    #[error("max_turns must be between 1 and {MAX_AGENT_TURNS}, got {value}")]
    MaxTurns { value: u32 },
    #[error(
        "compaction reserve_tokens + keep_recent_tokens must be at most \
         {MAX_COMPACTION_TOKEN_BUDGET}, got {total}"
    )]
    CompactionTokenBudget { total: u64 },
    #[error(
        "compaction custom instructions must be at most \
         {MAX_COMPACTION_INSTRUCTION_BYTES} bytes, got {bytes}"
    )]
    CompactionInstructions { bytes: usize },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CompactionSettings {
    pub enabled: bool,
    pub reserve_tokens: u32,
    pub keep_recent_tokens: u32,
}

impl Default for CompactionSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            reserve_tokens: 16_384,
            keep_recent_tokens: 20_000,
        }
    }
}

impl CompactionSettings {
    pub fn validate(self) -> Result<(), AgentConfigError> {
        let total = u64::from(self.reserve_tokens) + u64::from(self.keep_recent_tokens);
        if total > u64::from(MAX_COMPACTION_TOKEN_BUDGET) {
            return Err(AgentConfigError::CompactionTokenBudget { total });
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Default)]
pub struct CompactionConfig {
    pub settings: CompactionSettings,
    pub custom_instructions: Option<String>,
}

// ── AgentConfig ────────────────────────────────────

#[derive(Clone)]
pub struct AgentConfig {
    pub model: Model,
    pub system_prompt: Option<String>,
    /// Optional turn ceiling. `None` means no hard cap (the loop only stops
    /// when the model finishes or an explicit hook requests it). Provided to
    /// match the TS `pi/packages/agent` `while (true)` semantics.
    pub max_turns: Option<u32>,
    pub stream_options: Option<StreamOptions>,
    pub thinking_level: ThinkingLevel,
    pub tool_execution: ToolExecutionMode,
    /// Generic caller-owned identity attached to every tool invocation.
    pub tool_execution_scope: Option<String>,
    pub steering_mode: QueueMode,
    pub follow_up_mode: QueueMode,
    pub hooks: AgentHooks,
    pub resources: AgentResources,
    pub compaction: Option<CompactionConfig>,
    pub provider_streamer: Option<ProviderStreamer>,
}

impl std::fmt::Debug for AgentConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AgentConfig")
            .field("model", &self.model)
            .field("system_prompt", &self.system_prompt)
            .field("max_turns", &self.max_turns)
            .field("stream_options", &self.stream_options)
            .field("thinking_level", &self.thinking_level)
            .field("tool_execution", &self.tool_execution)
            .field("tool_execution_scope", &self.tool_execution_scope)
            .field("steering_mode", &self.steering_mode)
            .field("follow_up_mode", &self.follow_up_mode)
            .field("hooks", &self.hooks)
            .field("resources", &self.resources)
            .field("compaction", &self.compaction)
            .field("provider_streamer", &self.provider_streamer.is_some())
            .finish()
    }
}

impl AgentConfig {
    pub fn new(model: Model) -> Self {
        Self {
            model,
            system_prompt: None,
            max_turns: None,
            stream_options: None,
            thinking_level: ThinkingLevel::Off,
            tool_execution: ToolExecutionMode::Parallel,
            tool_execution_scope: None,
            steering_mode: QueueMode::OneAtATime,
            follow_up_mode: QueueMode::OneAtATime,
            hooks: AgentHooks::default(),
            resources: AgentResources::default(),
            compaction: None,
            provider_streamer: None,
        }
    }

    pub fn validate(&self) -> Result<(), AgentConfigError> {
        if let Some(max_turns) = self.max_turns
            && !(1..=MAX_AGENT_TURNS).contains(&max_turns)
        {
            return Err(AgentConfigError::MaxTurns { value: max_turns });
        }
        if let Some(compaction) = &self.compaction {
            compaction.settings.validate()?;
            if let Some(instructions) = &compaction.custom_instructions
                && instructions.len() > MAX_COMPACTION_INSTRUCTION_BYTES
            {
                return Err(AgentConfigError::CompactionInstructions {
                    bytes: instructions.len(),
                });
            }
        }
        Ok(())
    }
}
