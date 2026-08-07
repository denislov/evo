use crate::protocol::types::{
    CompactionProtocolResult, CompactionReason, ProtocolDelegationFoldedBlock, ProtocolEvent,
    ProtocolEventPayload, ProtocolSelfHealingEditCheckOutput, ProtocolSelfHealingEditReplacement,
    ToolExecutionResult,
};
use coding_agent::api::event::{
    CodingAgentAgentProductEvent, CodingAgentCapabilityProductEvent,
    CodingAgentDelegationEventContext, CodingAgentDelegationProductEvent,
    CodingAgentDiagnosticProductEvent, CodingAgentMessageProductEvent, CodingAgentProductEvent,
    CodingAgentProductEventCapabilityRevocation, CodingAgentProductEventCheckOutput,
    CodingAgentProductEventKind, CodingAgentProductEventProfileKind,
    CodingAgentProductEventReplacement, CodingAgentRecoveryResolution,
    CodingAgentRuntimeProductEvent, CodingAgentSessionProductEvent,
    CodingAgentSessionWriteFailureStatus, CodingAgentTeamProductEvent, CodingAgentToolProductEvent,
    CodingAgentWorkflowProductEvent,
};
use serde::Serialize;

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(tag = "type")]
enum WireContentBlock {
    #[serde(rename = "text")]
    Text { text: String },
    #[serde(rename = "thinking")]
    Thinking { thinking: String },
    #[serde(rename = "image")]
    Image {
        data: String,
        #[serde(rename = "mimeType")]
        mime_type: String,
    },
}
#[derive(Debug, Clone, Serialize, PartialEq)]
struct WireUsageCost {
    #[serde(skip_serializing_if = "wire_is_true")]
    known: bool,
    input: f64,
    output: f64,
    #[serde(rename = "cacheRead")]
    cache_read: f64,
    #[serde(rename = "cacheWrite")]
    cache_write: f64,
}

impl Default for WireUsageCost {
    fn default() -> Self {
        Self {
            known: true,
            input: 0.0,
            output: 0.0,
            cache_read: 0.0,
            cache_write: 0.0,
        }
    }
}

#[derive(Debug, Clone, Serialize, PartialEq, Default)]
struct WireProviderUsage {
    input: u32,
    output: u32,
    #[serde(rename = "cacheRead")]
    cache_read: u32,
    #[serde(rename = "cacheWrite")]
    cache_write: u32,
    #[serde(rename = "totalTokens")]
    total_tokens: u32,
    cost: WireUsageCost,
}

#[derive(Debug, Clone, Serialize, PartialEq, Default)]
struct WireStoredUsage {
    input: u32,
    output: u32,
    #[serde(rename = "cacheRead")]
    cache_read: u32,
    #[serde(rename = "cacheWrite")]
    cache_write: u32,
    total: u32,
    cost: WireUsageCost,
}

const fn wire_is_true(value: &bool) -> bool {
    *value
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
enum WireStopReason {
    Stop,
    Error,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
struct WireAssistantMessage {
    content: Vec<WireContentBlock>,
    api: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    provider: Option<String>,
    model: String,
    #[serde(rename = "responseModel", skip_serializing_if = "Option::is_none")]
    response_model: Option<String>,
    #[serde(rename = "responseId", skip_serializing_if = "Option::is_none")]
    response_id: Option<String>,
    usage: WireProviderUsage,
    #[serde(rename = "stopReason")]
    stop_reason: WireStopReason,
    #[serde(rename = "errorMessage", skip_serializing_if = "Option::is_none")]
    error_message: Option<String>,
    timestamp: u64,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(tag = "role")]
enum WireStoredAgentMessage {
    #[serde(rename = "assistant")]
    Assistant {
        content: Vec<WireContentBlock>,
        api: String,
        provider: String,
        model: String,
        #[serde(rename = "responseModel", skip_serializing_if = "Option::is_none")]
        response_model: Option<String>,
        #[serde(rename = "responseId", skip_serializing_if = "Option::is_none")]
        response_id: Option<String>,
        usage: WireStoredUsage,
        #[serde(rename = "stopReason")]
        stop_reason: WireStopReason,
        #[serde(rename = "errorMessage", skip_serializing_if = "Option::is_none")]
        error_message: Option<String>,
        timestamp: u64,
    },
    #[serde(rename = "toolResult")]
    ToolResult {
        #[serde(rename = "toolCallId")]
        tool_call_id: String,
        #[serde(rename = "toolName")]
        tool_name: String,
        content: Vec<WireContentBlock>,
        #[serde(rename = "isError")]
        is_error: bool,
        timestamp: u64,
    },
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(tag = "type")]
enum WireAssistantMessageEvent {
    #[serde(rename = "text_delta")]
    TextDelta {
        #[serde(rename = "contentIndex")]
        content_index: u32,
        delta: String,
        partial: WireAssistantMessage,
    },
    #[serde(rename = "thinking_delta")]
    ThinkingDelta {
        #[serde(rename = "contentIndex")]
        content_index: u32,
        delta: String,
        partial: WireAssistantMessage,
    },
}
mod adapter;

pub struct CodingProtocolEventAdapter {
    api: String,
    provider: String,
    model: String,
    messages: Vec<WireStoredAgentMessage>,
    current_assistant: Option<WireAssistantMessage>,
    current_tool_results: Vec<WireStoredAgentMessage>,
    assistant_open: bool,
}

impl CodingProtocolEventAdapter {
    fn ensure_assistant(&mut self) -> WireAssistantMessage {
        if self.current_assistant.is_none() {
            self.current_assistant = Some(self.assistant_message(""));
        }
        self.current_assistant
            .clone()
            .expect("assistant was inserted when missing")
    }

    fn append_assistant_text(&mut self, text: &str) -> (u32, WireAssistantMessage) {
        let mut message = self.ensure_assistant();
        let content_index = append_text_content(&mut message, text);
        self.current_assistant = Some(message.clone());
        (content_index, message)
    }

    fn append_assistant_thinking(&mut self, text: &str) -> (u32, WireAssistantMessage) {
        let mut message = self.ensure_assistant();
        let content_index = append_thinking_content(&mut message, text);
        self.current_assistant = Some(message.clone());
        (content_index, message)
    }

    fn assistant_message(&self, text: &str) -> WireAssistantMessage {
        WireAssistantMessage {
            content: text_content(text),
            api: self.api.clone(),
            provider: (!self.provider.is_empty()).then(|| self.provider.clone()),
            model: self.model.clone(),
            response_model: None,
            response_id: None,
            usage: WireProviderUsage::default(),
            stop_reason: WireStopReason::Stop,
            error_message: None,
            timestamp: 0,
        }
    }

    fn compaction_events(
        reason: CompactionReason,
        summary: &str,
        first_kept_message_id: &str,
        tokens_before: u32,
    ) -> Vec<ProtocolEventPayload> {
        vec![
            ProtocolEventPayload::CompactionStart { reason },
            ProtocolEventPayload::CompactionEnd {
                reason,
                result: Some(CompactionProtocolResult {
                    summary: summary.to_owned(),
                    first_kept_message_id: first_kept_message_id.to_owned(),
                    tokens_before,
                    details: None,
                }),
                aborted: false,
                will_retry: false,
                error_message: None,
            },
        ]
    }

    fn push_tool_result(
        &mut self,
        tool_call_id: &str,
        tool_name: &str,
        text: &str,
        is_error: bool,
    ) -> Vec<ProtocolEventPayload> {
        let content = text_content(text);
        let tool_result = WireStoredAgentMessage::ToolResult {
            tool_call_id: tool_call_id.to_string(),
            tool_name: tool_name.to_string(),
            content: content.clone(),
            is_error,
            timestamp: 0,
        };
        self.current_tool_results.push(tool_result.clone());

        vec![
            ProtocolEventPayload::ToolExecutionEnd {
                tool_call_id: tool_call_id.to_string(),
                tool_name: tool_name.to_string(),
                result: ToolExecutionResult::new(wire_values(content), false, None),
                is_error,
            },
            ProtocolEventPayload::MessageStart {
                message: wire_value(tool_result.clone()),
            },
            ProtocolEventPayload::MessageEnd {
                message: wire_value(tool_result),
            },
        ]
    }

    fn push_prompt_failed_message(&mut self, error: &str) -> Vec<ProtocolEventPayload> {
        let message = stored_error_assistant(&self.api, &self.provider, &self.model, error);
        self.messages.push(message.clone());
        vec![
            ProtocolEventPayload::MessageStart {
                message: wire_value(message.clone()),
            },
            ProtocolEventPayload::MessageEnd {
                message: wire_value(message.clone()),
            },
            ProtocolEventPayload::TurnEnd {
                message: wire_value(message),
                tool_results: Vec::new(),
            },
            ProtocolEventPayload::AgentEnd {
                messages: wire_values(self.messages.clone()),
            },
        ]
    }

    fn finish_current_turn(&mut self) -> Vec<ProtocolEventPayload> {
        let Some(message) = self.current_assistant.take() else {
            return Vec::new();
        };

        let stored = stored_assistant(&message);
        if !self.messages.contains(&stored) {
            self.messages.push(stored.clone());
        }
        for tool_result in &self.current_tool_results {
            if !self.messages.contains(tool_result) {
                self.messages.push(tool_result.clone());
            }
        }

        let events = vec![
            ProtocolEventPayload::MessageEnd {
                message: wire_value(stored.clone()),
            },
            ProtocolEventPayload::TurnEnd {
                message: wire_value(stored),
                tool_results: wire_values(self.current_tool_results.clone()),
            },
        ];
        self.current_tool_results.clear();
        self.assistant_open = false;
        events
    }
}

fn wire_value<T: serde::Serialize>(value: T) -> serde_json::Value {
    serde_json::to_value(value).expect("protocol projection value should serialize")
}

fn wire_values<T: serde::Serialize>(values: impl IntoIterator<Item = T>) -> Vec<serde_json::Value> {
    values.into_iter().map(wire_value).collect()
}

fn protocol_self_healing_replacements(
    replacements: &[CodingAgentProductEventReplacement],
) -> Vec<ProtocolSelfHealingEditReplacement> {
    replacements
        .iter()
        .map(|replacement| ProtocolSelfHealingEditReplacement {
            old_text: replacement.old_text.clone(),
            new_text: replacement.new_text.clone(),
        })
        .collect()
}

fn protocol_self_healing_check_output(
    output: &CodingAgentProductEventCheckOutput,
) -> ProtocolSelfHealingEditCheckOutput {
    ProtocolSelfHealingEditCheckOutput {
        command: output.command.clone(),
        stdout: output.stdout.clone(),
        stderr: output.stderr.clone(),
        exit_code: output.exit_code,
    }
}

#[allow(
    clippy::too_many_arguments,
    reason = "protocol projection keeps the complete typed delegation record explicit"
)]
fn delegation_folded_block(
    tool_call_id: &str,
    target_kind: CodingAgentProductEventProfileKind,
    target_id: &str,
    task: &str,
    status: &str,
    child_operation_id: Option<String>,
    summary: Option<String>,
    is_error: bool,
) -> ProtocolDelegationFoldedBlock {
    ProtocolDelegationFoldedBlock {
        tool_call_id: tool_call_id.to_string(),
        target_kind: profile_kind_to_protocol(target_kind).to_string(),
        target_id: target_id.to_string(),
        task: task.to_string(),
        status: status.to_string(),
        child_operation_id,
        summary,
        is_error,
    }
}

fn profile_kind_to_protocol(kind: CodingAgentProductEventProfileKind) -> &'static str {
    match kind {
        CodingAgentProductEventProfileKind::Agent => "agent",
        CodingAgentProductEventProfileKind::Team => "team",
    }
}

fn capability_revocation_to_protocol(
    revocation: CodingAgentProductEventCapabilityRevocation,
) -> &'static str {
    match revocation {
        CodingAgentProductEventCapabilityRevocation::RequestCancelOlderOperations => {
            "request_cancel_older_operations"
        }
    }
}

fn append_text_content(message: &mut WireAssistantMessage, text: &str) -> u32 {
    let last_index = message.content.len().saturating_sub(1) as u32;
    match message.content.last_mut() {
        Some(WireContentBlock::Text { text: existing }) => {
            existing.push_str(text);
            last_index
        }
        _ => {
            let index = message.content.len() as u32;
            message.content.push(WireContentBlock::Text {
                text: text.to_string(),
            });
            index
        }
    }
}

fn append_thinking_content(message: &mut WireAssistantMessage, text: &str) -> u32 {
    let last_index = message.content.len().saturating_sub(1) as u32;
    match message.content.last_mut() {
        Some(WireContentBlock::Thinking { thinking }) => {
            thinking.push_str(text);
            last_index
        }
        _ => {
            let index = message.content.len() as u32;
            message.content.push(WireContentBlock::Thinking {
                thinking: text.to_string(),
            });
            index
        }
    }
}

fn text_content(text: &str) -> Vec<WireContentBlock> {
    if text.is_empty() {
        Vec::new()
    } else {
        vec![WireContentBlock::Text {
            text: text.to_string(),
        }]
    }
}

fn stored_assistant(message: &WireAssistantMessage) -> WireStoredAgentMessage {
    WireStoredAgentMessage::Assistant {
        content: message.content.clone(),
        api: message.api.clone(),
        provider: message.provider.clone().unwrap_or_default(),
        model: message.model.clone(),
        response_model: message.response_model.clone(),
        response_id: message.response_id.clone(),
        usage: WireStoredUsage {
            input: message.usage.input,
            output: message.usage.output,
            cache_read: message.usage.cache_read,
            cache_write: message.usage.cache_write,
            total: message.usage.total_tokens,
            cost: WireUsageCost {
                known: message.usage.cost.known,
                input: message.usage.cost.input,
                output: message.usage.cost.output,
                cache_read: message.usage.cost.cache_read,
                cache_write: message.usage.cost.cache_write,
            },
        },
        stop_reason: message.stop_reason.clone(),
        error_message: message.error_message.clone(),
        timestamp: message.timestamp,
    }
}

fn stored_error_assistant(
    api: &str,
    provider: &str,
    model: &str,
    error: &str,
) -> WireStoredAgentMessage {
    WireStoredAgentMessage::Assistant {
        content: Vec::new(),
        api: api.to_string(),
        provider: provider.to_string(),
        model: model.to_string(),
        response_model: None,
        response_id: None,
        usage: WireStoredUsage::default(),
        stop_reason: WireStopReason::Error,
        error_message: Some(error.to_string()),
        timestamp: 0,
    }
}
