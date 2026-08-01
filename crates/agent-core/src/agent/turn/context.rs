use std::collections::VecDeque;
use std::sync::{Arc, RwLock};

use ai::api::conversation::{AssistantMessage, Context};
use ai::api::stream::StreamOptions;
use futures::channel::mpsc;
use tokio_util::sync::CancellationToken;

use crate::agent::AgentState;
use crate::agent::types::{
    AgentConfig, AgentEvent, AgentMessage, AgentTool, AgentToolResult, ProviderRequestSnapshot,
};

#[derive(Debug, Clone, PartialEq)]
pub struct PendingToolCall {
    pub index: usize,
    pub id: String,
    pub name: String,
    pub arguments: serde_json::Value,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RuntimeCompactionState {
    pub summary: Option<String>,
    pub first_kept_message_id: Option<String>,
    pub tokens_before: Option<u32>,
}

#[derive(Debug, Clone)]
pub struct AgentTurnProviderRequestOverride {
    pub context: Context,
    pub stream_options: Option<StreamOptions>,
}

#[derive(Clone)]
pub struct AgentTurnContext {
    pub config: AgentConfig,
    pub messages: Vec<AgentMessage>,
    pub tools: Vec<AgentTool>,
    pub steering_queue: VecDeque<AgentMessage>,
    pub follow_up_queue: VecDeque<AgentMessage>,
    pub cancel_token: CancellationToken,
    pub turn: u32,
    pub provider_request: Option<ProviderRequestSnapshot>,
    pub provider_request_override: Option<AgentTurnProviderRequestOverride>,
    pub(crate) provider_request_override_consumed: bool,
    pub assistant_message: Option<AssistantMessage>,
    pub pending_tool_calls: Vec<PendingToolCall>,
    pub tool_results: Vec<AgentToolResult>,
    pub tool_results_all_terminate: bool,
    pub runtime_compaction: RuntimeCompactionState,
    pub(crate) live_state: Option<Arc<RwLock<AgentState>>>,
    event_sender: Option<mpsc::UnboundedSender<AgentEvent>>,
}

impl AgentTurnContext {
    pub(crate) fn from_state(state: &AgentState) -> Self {
        Self {
            config: state.config.clone(),
            messages: state.messages.clone(),
            tools: state.tools.clone(),
            steering_queue: state.steering_queue.clone(),
            follow_up_queue: state.follow_up_queue.clone(),
            cancel_token: state.cancel_token.clone(),
            turn: 0,
            provider_request: None,
            provider_request_override: state.provider_request_override.as_ref().map(|request| {
                AgentTurnProviderRequestOverride {
                    context: request.context.clone(),
                    stream_options: request.stream_options.clone(),
                }
            }),
            provider_request_override_consumed: false,
            assistant_message: None,
            pending_tool_calls: Vec::new(),
            tool_results: Vec::new(),
            tool_results_all_terminate: false,
            runtime_compaction: RuntimeCompactionState::default(),
            live_state: None,
            event_sender: None,
        }
    }

    pub(crate) fn attach_runtime(
        &mut self,
        live_state: Arc<RwLock<AgentState>>,
        event_sender: mpsc::UnboundedSender<AgentEvent>,
    ) {
        self.live_state = Some(live_state);
        self.event_sender = Some(event_sender);
    }

    pub(crate) fn emit(&mut self, event: AgentEvent) {
        if let Some(sender) = &self.event_sender {
            let _ = sender.unbounded_send(event);
        }
    }

    pub(crate) fn sync_live_queues(&mut self) {
        let Some(live_state) = &self.live_state else {
            return;
        };
        let mut state = live_state.write().unwrap();
        self.steering_queue.extend(state.steering_queue.drain(..));
        self.follow_up_queue.extend(state.follow_up_queue.drain(..));
    }

    pub(crate) fn take_provider_request_override(
        &mut self,
    ) -> Option<AgentTurnProviderRequestOverride> {
        if let Some(live_state) = &self.live_state {
            let mut state = live_state.write().unwrap();
            return state.provider_request_override.take().map(|request| {
                AgentTurnProviderRequestOverride {
                    context: request.context,
                    stream_options: request.stream_options,
                }
            });
        }

        let request = self.provider_request_override.take();
        if request.is_some() {
            self.provider_request_override_consumed = true;
        }
        request
    }

    pub(crate) fn apply_to_state(&mut self, state: &mut AgentState) {
        state.messages = std::mem::take(&mut self.messages);
        state.config.model = self.config.model.clone();
        state.config.stream_options = self.config.stream_options.clone();
        state.config.thinking_level = self.config.thinking_level;
        state.cancel_token = self.cancel_token.clone();

        let mut steering_queue = std::mem::take(&mut self.steering_queue);
        steering_queue.extend(state.steering_queue.drain(..));
        state.steering_queue = steering_queue;

        let mut follow_up_queue = std::mem::take(&mut self.follow_up_queue);
        follow_up_queue.extend(state.follow_up_queue.drain(..));
        state.follow_up_queue = follow_up_queue;

        if self.live_state.is_none() && self.provider_request_override_consumed {
            state.provider_request_override = None;
        }
    }
}
