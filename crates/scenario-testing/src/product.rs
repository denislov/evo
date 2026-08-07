use coding_agent::api::client::{
    CodingAgentClientMessageStatus, CodingAgentClientProjection, CodingAgentClientProjectionApply,
    CodingAgentClientProjectionLifecycle, CodingAgentClientToolStatus, CodingAgentContextSnapshot,
    CodingAgentSnapshot, CodingAgentSnapshotCursor, UI_SNAPSHOT_PROTOCOL_VERSION,
};
use coding_agent::api::view::{CodingAgentCapabilities, CodingAgentSessionView, ProfileId};
use serde::{Deserialize, Serialize};

use crate::LoadedScenario;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SemanticTerminalState {
    pub cursor: u64,
    pub messages: Vec<SemanticMessage>,
    pub tools: Vec<SemanticTool>,
    pub context: serde_json::Value,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SemanticMessage {
    pub operation_id: String,
    pub turn_id: String,
    pub message_id: Option<String>,
    pub text: String,
    pub thinking: String,
    pub reasoning_duration_millis: Option<u64>,
    pub status: String,
    pub started_sequence: u64,
    pub updated_sequence: u64,
    pub truncated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SemanticTool {
    pub operation_id: String,
    pub turn_id: String,
    pub tool_call_id: String,
    pub name: String,
    pub arguments: String,
    pub detail: String,
    pub status: String,
    pub started_sequence: u64,
    pub updated_sequence: u64,
    pub truncated: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum ScenarioRunError {
    #[error("scenario {scenario} rejected event {sequence}: {outcome}")]
    Event {
        scenario: String,
        sequence: u64,
        outcome: String,
    },
    #[error("scenario {0} requested reconnect replay without events")]
    EmptyReconnect(String),
    #[error("scenario {0} changed semantic state after duplicate reconnect delivery")]
    ReconnectChangedState(String),
}

pub fn initial_snapshot() -> CodingAgentSnapshot {
    CodingAgentSnapshot {
        cursor: CodingAgentSnapshotCursor {
            stream_id: "test-stream".into(),
            snapshot_protocol_major: UI_SNAPSHOT_PROTOCOL_VERSION.major,
            last_event_sequence: 0,
            last_session_sequence: 0,
            capability_generation: 0,
        },
        version: UI_SNAPSHOT_PROTOCOL_VERSION,
        session: CodingAgentSessionView::new("session-1", None, ProfileId::from("default")),
        capabilities: CodingAgentCapabilities::idle(true),
        active_operation: None,
        drafts: Vec::new(),
        submitted_operation: None,
        pending_authorizations: Vec::new(),
        context: CodingAgentContextSnapshot::default(),
    }
}

pub fn apply_scenario(
    projection: &mut CodingAgentClientProjection,
    scenario: &LoadedScenario,
) -> Result<SemanticTerminalState, ScenarioRunError> {
    for event in &scenario.events {
        let outcome = projection.apply(event);
        if !matches!(outcome, CodingAgentClientProjectionApply::Applied(_)) {
            return Err(ScenarioRunError::Event {
                scenario: scenario.scenario.name.clone(),
                sequence: event.sequence(),
                outcome: format!("{outcome:?}"),
            });
        }
    }
    let terminal = semantic_state(projection);
    if scenario.scenario.reconnect.replay_last_event {
        let event = scenario
            .events
            .last()
            .ok_or_else(|| ScenarioRunError::EmptyReconnect(scenario.scenario.name.clone()))?;
        if !matches!(
            projection.apply(event),
            CodingAgentClientProjectionApply::IgnoredDuplicate
        ) || semantic_state(projection) != terminal
        {
            return Err(ScenarioRunError::ReconnectChangedState(
                scenario.scenario.name.clone(),
            ));
        }
    }
    Ok(terminal)
}

pub fn semantic_state(projection: &CodingAgentClientProjection) -> SemanticTerminalState {
    debug_assert_eq!(
        projection.lifecycle(),
        CodingAgentClientProjectionLifecycle::Running
    );
    SemanticTerminalState {
        cursor: projection.snapshot().cursor.last_event_sequence,
        messages: projection
            .messages()
            .iter()
            .map(|message| SemanticMessage {
                operation_id: message.operation_id.clone(),
                turn_id: message.turn_id.clone(),
                message_id: message.message_id.clone(),
                text: message.text.clone(),
                thinking: message.thinking.clone(),
                reasoning_duration_millis: message.reasoning_duration_millis,
                status: match message.status {
                    CodingAgentClientMessageStatus::Streaming => "streaming",
                    CodingAgentClientMessageStatus::Completed => "completed",
                }
                .into(),
                started_sequence: message.started_sequence,
                updated_sequence: message.updated_sequence,
                truncated: message.truncated,
            })
            .collect(),
        tools: projection
            .tools()
            .iter()
            .map(|tool| SemanticTool {
                operation_id: tool.operation_id.clone(),
                turn_id: tool.turn_id.clone(),
                tool_call_id: tool.tool_call_id.clone(),
                name: tool.name.clone(),
                arguments: tool.arguments.clone(),
                detail: tool.detail.clone(),
                status: match tool.status {
                    CodingAgentClientToolStatus::Running => "running",
                    CodingAgentClientToolStatus::Completed => "completed",
                    CodingAgentClientToolStatus::Failed => "failed",
                }
                .into(),
                started_sequence: tool.started_sequence,
                updated_sequence: tool.updated_sequence,
                truncated: tool.truncated,
            })
            .collect(),
        context: serde_json::to_value(&projection.snapshot().context)
            .expect("client context is serializable"),
    }
}
