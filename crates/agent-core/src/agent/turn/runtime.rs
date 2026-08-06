use std::pin::Pin;

use ai_protocol::api::conversation::ContentBlock;
use futures::channel::mpsc;
use futures::{FutureExt, StreamExt};

use super::context::{AgentTurnContext, AgentTurnProviderRequestOverride};
use super::nodes;
use super::nodes::{AgentTurnDecision, AgentTurnError};
use crate::agent::queue::{AgentInputQueue, enqueue_message};
use crate::agent::runtime::next_message_id;
use crate::agent::types::{AgentEvent, AgentMessage, AgentQueueError};

/// Defense-in-depth fuse for one typed turn, not a user-visible turn budget.
///
/// The legal graph is acyclic and currently visits at most nine states from
/// `Start` to a terminal state. Keep this ceiling independent and above that
/// proven maximum so an accidental future back-edge fails closed instead of
/// spinning inside one turn.
const TURN_STATE_VISIT_FUSE: usize = 16;
const MAX_LEGAL_TURN_STATE_VISITS: usize = 9;
const _: () = assert!(TURN_STATE_VISIT_FUSE > MAX_LEGAL_TURN_STATE_VISITS);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AgentTurnState {
    Finish,
    Start,
    DrainQueuedInput,
    CompactRuntimeContext,
    PrepareProviderRequest,
    ApplyProviderHook,
    ProviderStream,
    DecideAfterAssistant,
    ExecuteTools,
    PrepareNextTurn,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AgentTurnResult {
    Continue,
    Finish,
}

type TurnRunOutcome = (Result<AgentTurnResult, AgentTurnError>, AgentTurnContext);

/// One multi-turn agent loop, driven from inside the agent actor.
///
/// The actor owns `AgentState` exclusively, so `TurnRunner` holds a turn-local
/// working copy (`AgentTurnContext`) and never touches a lock. Queued input
/// arriving mid-turn is appended to the context's queues by the actor via
/// [`TurnRunner::steer`] / [`TurnRunner::follow_up`], and the loop consumes it
/// at the same nodes that previously synced live queues
/// (`drain_queued_input` and `maybe_prepare_next_turn`).
///
/// Events are buffered on an internal unbounded channel and surfaced one at a
/// time through [`TurnRunner::next_event`]. When the actor observes the
/// consumer's event stream has been dropped, it commits the context back to
/// the state and drops the runner; no drop guard is needed because the actor
/// is the only owner and always commits at a turn boundary.
pub(crate) struct TurnRunner {
    context: Option<AgentTurnContext>,
    turn: u32,
    event_rx: Option<mpsc::UnboundedReceiver<AgentEvent>>,
    run_fut: Option<Pin<Box<dyn Future<Output = TurnRunOutcome> + Send>>>,
    /// Error event produced by the completed turn, yielded after any events
    /// still buffered in `event_rx`.
    pending_error: Option<AgentEvent>,
    done: bool,
    /// Set when a single turn finishes with `Continue`. The actor loop clears
    /// this to start the next turn. Between setting and clearing, `next_event`
    /// returns `None`, giving the actor a chance to process pending commands
    /// or detect a dropped consumer.
    turn_continues: bool,
}

impl TurnRunner {
    pub(crate) fn new(context: AgentTurnContext) -> Self {
        Self {
            context: Some(context),
            turn: 0,
            event_rx: None,
            run_fut: None,
            pending_error: None,
            done: false,
            turn_continues: false,
        }
    }

    /// Returns the next buffered turn event, or `None` once the whole loop
    /// has finished (all buffered events already drained).
    pub(crate) async fn next_event(&mut self) -> Option<AgentEvent> {
        loop {
            // Drain whatever the running turn future already buffered.
            if let Some(rx) = &mut self.event_rx {
                match rx.next().now_or_never() {
                    Some(Some(event)) => return Some(event),
                    Some(None) => self.event_rx = None,
                    None => {}
                }
            }
            if let Some(event) = self.pending_error.take() {
                return Some(event);
            }
            if self.run_fut.is_none() {
                if self.done || self.turn_continues {
                    return None;
                }
                self.start_turn();
                continue;
            }
            if self.event_rx.is_none() {
                let outcome = self.run_fut.as_mut().expect("run future is set").await;
                self.run_fut = None;
                self.context = Some(outcome.1);
                self.finish_outcome(outcome.0);
                continue;
            }
            tokio::select! {
                event = async {
                    self.event_rx.as_mut().expect("event receiver is set").next().await
                } => {
                    match event {
                        Some(event) => return Some(event),
                        None => self.event_rx = None,
                    }
                }
                outcome = async {
                    self.run_fut.as_mut().expect("run future is set").await
                } => {
                    self.run_fut = None;
                    self.context = Some(outcome.1);
                    self.finish_outcome(outcome.0);
                }
            }
        }
    }

    fn start_turn(&mut self) {
        let mut context = self.context.take().expect("turn context is held");
        context.turn = self.turn;
        let cancel = context.cancel_token.clone();
        let (event_sender, event_rx) = mpsc::unbounded();
        context.attach_event_sender(event_sender);
        // The turn future owns the context and returns it on completion; the
        // actor commits it to the shared state at the turn boundary.
        let run = async move {
            let outcome = run_typed_turn(&mut context, cancel).await;
            (outcome, context)
        };
        self.event_rx = Some(event_rx);
        self.run_fut = Some(Box::pin(run));
    }

    fn finish_outcome(&mut self, outcome: Result<AgentTurnResult, AgentTurnError>) {
        let cancelled = self
            .context
            .as_ref()
            .is_some_and(|ctx| ctx.cancel_token.is_cancelled());
        self.turn = self.context.as_ref().map_or(self.turn, |ctx| ctx.turn);
        match outcome {
            Ok(AgentTurnResult::Continue) => self.turn_continues = true,
            Ok(AgentTurnResult::Finish) => self.done = true,
            Err(error) => {
                self.done = true;
                self.pending_error = Some(AgentEvent::AgentError {
                    error: if cancelled {
                        "aborted".into()
                    } else {
                        error.to_string()
                    },
                });
            }
        }
    }

    /// Enqueues steering input into the turn's working copy. Called by the
    /// actor when a `Steer` command arrives while this runner owns the turn.
    pub(crate) fn steer(&mut self, text: String) -> Result<(), AgentQueueError> {
        let context = self.context.as_mut().expect("turn context is held");
        let message_id = next_message_id(
            &context.messages,
            &context.steering_queue,
            &context.follow_up_queue,
            "steer",
        );
        enqueue_message(
            &mut context.steering_queue,
            AgentInputQueue::Steering,
            AgentMessage::UserText { message_id, text },
        )
    }

    pub(crate) fn steer_content(
        &mut self,
        content: Vec<ContentBlock>,
    ) -> Result<(), AgentQueueError> {
        let context = self.context.as_mut().expect("turn context is held");
        let message_id = next_message_id(
            &context.messages,
            &context.steering_queue,
            &context.follow_up_queue,
            "steer",
        );
        enqueue_message(
            &mut context.steering_queue,
            AgentInputQueue::Steering,
            AgentMessage::Custom {
                message_id,
                custom_type: "input".into(),
                content,
                display: true,
                details: None,
                timestamp: 0,
            },
        )
    }

    pub(crate) fn follow_up(&mut self, text: String) -> Result<(), AgentQueueError> {
        let context = self.context.as_mut().expect("turn context is held");
        let message_id = next_message_id(
            &context.messages,
            &context.steering_queue,
            &context.follow_up_queue,
            "followup",
        );
        enqueue_message(
            &mut context.follow_up_queue,
            AgentInputQueue::FollowUp,
            AgentMessage::UserText { message_id, text },
        )
    }

    pub(crate) fn follow_up_content(
        &mut self,
        content: Vec<ContentBlock>,
    ) -> Result<(), AgentQueueError> {
        let context = self.context.as_mut().expect("turn context is held");
        let message_id = next_message_id(
            &context.messages,
            &context.steering_queue,
            &context.follow_up_queue,
            "followup",
        );
        enqueue_message(
            &mut context.follow_up_queue,
            AgentInputQueue::FollowUp,
            AgentMessage::Custom {
                message_id,
                custom_type: "input".into(),
                content,
                display: true,
                details: None,
                timestamp: 0,
            },
        )
    }

    pub(crate) fn clear_queues(&mut self) {
        let context = self.context.as_mut().expect("turn context is held");
        context.steering_queue.clear();
        context.follow_up_queue.clear();
    }

    pub(crate) fn drain_steering_queue(&mut self) -> Vec<AgentMessage> {
        self.context
            .as_mut()
            .expect("turn context is held")
            .steering_queue
            .drain(..)
            .collect()
    }

    pub(crate) fn drain_follow_up_queue(&mut self) -> Vec<AgentMessage> {
        self.context
            .as_mut()
            .expect("turn context is held")
            .follow_up_queue
            .drain(..)
            .collect()
    }

    pub(crate) fn abort(&mut self) {
        self.context
            .as_mut()
            .expect("turn context is held")
            .cancel_token
            .cancel();
    }

    pub(crate) fn set_provider_request_override(
        &mut self,
        context: ai_protocol::api::conversation::Context,
        stream_options: Option<ai_protocol::api::stream::StreamOptions>,
    ) {
        self.context
            .as_mut()
            .expect("turn context is held")
            .provider_request_override = Some(AgentTurnProviderRequestOverride {
            context,
            stream_options,
        });
    }

    /// Hands the working copy back to the actor for committing.
    pub(crate) fn into_context(self) -> AgentTurnContext {
        self.context.expect("turn context is held")
    }

    /// Returns `true` when a turn finished with `Continue` and the actor has
    /// not yet started the next turn.
    pub(crate) fn turn_continues(&self) -> bool {
        self.turn_continues
    }

    /// Clears the `turn_continues` flag so `next_event` will start the next
    /// turn on its next call.
    pub(crate) fn start_next_turn(&mut self) {
        self.turn_continues = false;
    }
}

async fn run_typed_turn(
    ctx: &mut AgentTurnContext,
    cancellation: tokio_util::sync::CancellationToken,
) -> Result<AgentTurnResult, AgentTurnError> {
    let mut state = AgentTurnState::Start;
    let mut state_visits = 0usize;
    loop {
        state_visits += 1;
        if state_visits > TURN_STATE_VISIT_FUSE {
            return Err(AgentTurnError::Invariant(format!(
                "typed AgentTurn exceeded the {TURN_STATE_VISIT_FUSE}-visit invariant fuse"
            )));
        }
        state = match state {
            AgentTurnState::Finish => return Ok(AgentTurnResult::Finish),
            AgentTurnState::Start => {
                let decision = nodes::start_turn(ctx)?;
                transition_from_decision(AgentTurnState::Start, decision)?
            }
            AgentTurnState::DrainQueuedInput => {
                nodes::drain_queued_input(ctx);
                AgentTurnState::CompactRuntimeContext
            }
            AgentTurnState::CompactRuntimeContext => {
                nodes::maybe_compact_runtime_context(ctx).await?;
                AgentTurnState::PrepareProviderRequest
            }
            AgentTurnState::PrepareProviderRequest => {
                let decision = nodes::prepare_provider_request(ctx).await?;
                transition_from_decision(AgentTurnState::PrepareProviderRequest, decision)?
            }
            AgentTurnState::ApplyProviderHook => {
                let decision = nodes::apply_before_provider_request_hook(ctx).await?;
                transition_from_decision(AgentTurnState::ApplyProviderHook, decision)?
            }
            AgentTurnState::ProviderStream => {
                let decision = nodes::stream_provider(ctx).await?;
                transition_from_decision(AgentTurnState::ProviderStream, decision)?
            }
            AgentTurnState::DecideAfterAssistant => {
                let decision = nodes::decide_after_assistant(ctx)?;
                transition_from_decision(AgentTurnState::DecideAfterAssistant, decision)?
            }
            AgentTurnState::ExecuteTools => {
                let decision = nodes::execute_tools(ctx).await?;
                transition_from_decision(AgentTurnState::ExecuteTools, decision)?
            }
            AgentTurnState::PrepareNextTurn => {
                let decision = nodes::maybe_prepare_next_turn(ctx).await?;
                return match decision {
                    AgentTurnDecision::Continue | AgentTurnDecision::ContinueProvider => {
                        Ok(AgentTurnResult::Continue)
                    }
                    AgentTurnDecision::Done
                    | AgentTurnDecision::Error
                    | AgentTurnDecision::Aborted => Ok(AgentTurnResult::Finish),
                    AgentTurnDecision::Next | AgentTurnDecision::Tools => {
                        Err(AgentTurnError::Invariant(format!(
                            "typed AgentTurn transition from PrepareNextTurn has unexpected decision {decision:?}"
                        )))
                    }
                };
            }
        };

        if cancellation.is_cancelled() {
            return Ok(AgentTurnResult::Finish);
        }
    }
}

fn transition_from_decision(
    state: AgentTurnState,
    decision: AgentTurnDecision,
) -> Result<AgentTurnState, AgentTurnError> {
    match state {
        AgentTurnState::Start => transition_from_start(decision),
        AgentTurnState::PrepareProviderRequest => transition_from_prepare_provider(decision),
        AgentTurnState::ApplyProviderHook => transition_from_provider_hook(decision),
        AgentTurnState::ProviderStream => transition_from_provider_stream(decision),
        AgentTurnState::DecideAfterAssistant => transition_from_assistant(decision),
        AgentTurnState::ExecuteTools => transition_from_tools(decision),
        AgentTurnState::Finish
        | AgentTurnState::DrainQueuedInput
        | AgentTurnState::CompactRuntimeContext
        | AgentTurnState::PrepareNextTurn => unexpected_decision(state, decision),
    }
}

fn transition_from_start(decision: AgentTurnDecision) -> Result<AgentTurnState, AgentTurnError> {
    match decision {
        AgentTurnDecision::Next => Ok(AgentTurnState::DrainQueuedInput),
        AgentTurnDecision::Error | AgentTurnDecision::Aborted => Ok(AgentTurnState::Finish),
        AgentTurnDecision::Continue
        | AgentTurnDecision::ContinueProvider
        | AgentTurnDecision::Tools
        | AgentTurnDecision::Done => unexpected_decision(AgentTurnState::Start, decision),
    }
}

fn transition_from_prepare_provider(
    decision: AgentTurnDecision,
) -> Result<AgentTurnState, AgentTurnError> {
    match decision {
        AgentTurnDecision::Next => Ok(AgentTurnState::ApplyProviderHook),
        AgentTurnDecision::Error | AgentTurnDecision::Aborted => Ok(AgentTurnState::Finish),
        AgentTurnDecision::Continue
        | AgentTurnDecision::ContinueProvider
        | AgentTurnDecision::Tools
        | AgentTurnDecision::Done => {
            unexpected_decision(AgentTurnState::PrepareProviderRequest, decision)
        }
    }
}

fn transition_from_provider_hook(
    decision: AgentTurnDecision,
) -> Result<AgentTurnState, AgentTurnError> {
    match decision {
        AgentTurnDecision::Next => Ok(AgentTurnState::ProviderStream),
        AgentTurnDecision::Error | AgentTurnDecision::Aborted => Ok(AgentTurnState::Finish),
        AgentTurnDecision::Continue
        | AgentTurnDecision::ContinueProvider
        | AgentTurnDecision::Tools
        | AgentTurnDecision::Done => {
            unexpected_decision(AgentTurnState::ApplyProviderHook, decision)
        }
    }
}

fn transition_from_provider_stream(
    decision: AgentTurnDecision,
) -> Result<AgentTurnState, AgentTurnError> {
    match decision {
        AgentTurnDecision::Next => Ok(AgentTurnState::DecideAfterAssistant),
        AgentTurnDecision::Error | AgentTurnDecision::Aborted => Ok(AgentTurnState::Finish),
        AgentTurnDecision::Continue
        | AgentTurnDecision::ContinueProvider
        | AgentTurnDecision::Tools
        | AgentTurnDecision::Done => unexpected_decision(AgentTurnState::ProviderStream, decision),
    }
}

fn transition_from_assistant(
    decision: AgentTurnDecision,
) -> Result<AgentTurnState, AgentTurnError> {
    match decision {
        AgentTurnDecision::Continue => Ok(AgentTurnState::PrepareNextTurn),
        AgentTurnDecision::Tools => Ok(AgentTurnState::ExecuteTools),
        AgentTurnDecision::Error | AgentTurnDecision::Aborted => Ok(AgentTurnState::Finish),
        AgentTurnDecision::Next | AgentTurnDecision::ContinueProvider | AgentTurnDecision::Done => {
            unexpected_decision(AgentTurnState::DecideAfterAssistant, decision)
        }
    }
}

fn transition_from_tools(decision: AgentTurnDecision) -> Result<AgentTurnState, AgentTurnError> {
    match decision {
        AgentTurnDecision::Continue | AgentTurnDecision::ContinueProvider => {
            Ok(AgentTurnState::PrepareNextTurn)
        }
        AgentTurnDecision::Error | AgentTurnDecision::Aborted => Ok(AgentTurnState::Finish),
        AgentTurnDecision::Next | AgentTurnDecision::Tools | AgentTurnDecision::Done => {
            unexpected_decision(AgentTurnState::ExecuteTools, decision)
        }
    }
}

fn unexpected_decision(
    state: AgentTurnState,
    decision: AgentTurnDecision,
) -> Result<AgentTurnState, AgentTurnError> {
    Err(AgentTurnError::Invariant(format!(
        "typed AgentTurn transition from {state:?} has unexpected decision {decision:?}"
    )))
}

#[cfg(test)]
mod transition_tests {
    use super::*;

    #[test]
    fn start_transitions() {
        assert_eq!(
            transition_from_start(AgentTurnDecision::Next).unwrap(),
            AgentTurnState::DrainQueuedInput
        );
        assert_eq!(
            transition_from_start(AgentTurnDecision::Error).unwrap(),
            AgentTurnState::Finish
        );
        assert_eq!(
            transition_from_start(AgentTurnDecision::Aborted).unwrap(),
            AgentTurnState::Finish
        );
    }

    #[test]
    fn assistant_transitions() {
        assert_eq!(
            transition_from_assistant(AgentTurnDecision::Continue).unwrap(),
            AgentTurnState::PrepareNextTurn
        );
        assert_eq!(
            transition_from_assistant(AgentTurnDecision::Tools).unwrap(),
            AgentTurnState::ExecuteTools
        );
        assert_eq!(
            transition_from_assistant(AgentTurnDecision::Error).unwrap(),
            AgentTurnState::Finish
        );
    }

    #[test]
    fn tools_transitions() {
        assert_eq!(
            transition_from_tools(AgentTurnDecision::Continue).unwrap(),
            AgentTurnState::PrepareNextTurn
        );
        assert_eq!(
            transition_from_tools(AgentTurnDecision::ContinueProvider).unwrap(),
            AgentTurnState::PrepareNextTurn
        );
    }

    #[test]
    fn illegal_transitions_fail_closed() {
        for (state, decision) in [
            (AgentTurnState::Start, AgentTurnDecision::Tools),
            (AgentTurnState::Start, AgentTurnDecision::Done),
            (AgentTurnState::Start, AgentTurnDecision::Continue),
            (AgentTurnState::ProviderStream, AgentTurnDecision::Tools),
            (
                AgentTurnState::DecideAfterAssistant,
                AgentTurnDecision::Next,
            ),
            (AgentTurnState::ExecuteTools, AgentTurnDecision::Done),
            (AgentTurnState::ExecuteTools, AgentTurnDecision::Tools),
            (AgentTurnState::Finish, AgentTurnDecision::Next),
            (AgentTurnState::DrainQueuedInput, AgentTurnDecision::Next),
            (
                AgentTurnState::CompactRuntimeContext,
                AgentTurnDecision::Next,
            ),
            (AgentTurnState::PrepareNextTurn, AgentTurnDecision::Next),
        ] {
            assert!(
                transition_from_decision(state, decision).is_err(),
                "expected {state:?} + {decision:?} to be rejected"
            );
        }
    }
}

#[cfg(all(test, feature = "test-support"))]
mod loop_tests {
    use crate::agent::Agent;
    use crate::agent::types::{AgentConfig, AgentEvent, AgentMessage};
    use ai::api::client::AiClient;
    use ai::api::provider::faux::{FauxCall, FauxProvider, FauxResponse, FauxToolCall};
    use ai_protocol::api::conversation::{ContentBlock, StopReason};
    use ai_protocol::api::model::{Model, ModelCost, ModelInput};
    use ai_protocol::api::stream::AssistantMessageEvent;
    use futures::StreamExt;
    use schemars::JsonSchema;
    use serde::Deserialize;
    use std::sync::Arc;
    use tool_contract::api::definition::{
        AuthorizationRisk, ToolBehaviorVersion, ToolCapabilities, ToolDefinition, ToolId, ToolKind,
    };
    use tool_contract::api::output::{ToolContent, ToolOutput};
    use tool_contract::api::schema::schema_for;
    use tool_runtime::api::{ToolRegistry, ToolRuntime, TypedTool};

    #[derive(Deserialize, JsonSchema)]
    struct RuntimeTestArgs {}

    fn test_model() -> Model {
        Model {
            id: "faux-model".into(),
            name: "Faux Model".into(),
            api: "faux-api".into(),
            provider: "faux".into(),
            base_url: String::new(),
            reasoning: false,
            thinking_level_map: None,
            input: vec![ModelInput::Text],
            cost: ModelCost {
                known: true,
                input: 0.0,
                output: 0.0,
                cache_read: 0.0,
                cache_write: 0.0,
            },
            context_window: 0,
            max_tokens: 0,
            headers: None,
            compat: None,
        }
    }

    fn test_agent(calls: Vec<FauxCall>) -> Agent {
        let provider = Arc::new(FauxProvider::with_call_queue(calls));
        let ai_client = Arc::new(AiClient::new());
        ai_client.register_provider("faux-api", provider);
        let mut config = AgentConfig::new(test_model());
        config.provider_streamer = Some(Arc::new({
            let ai_client = Arc::clone(&ai_client);
            move |model, context, options| ai_client.stream_model(model, context, options)
        }));
        Agent::new(config)
    }

    fn text_call(text: &str, stop_reason: StopReason) -> FauxCall {
        FauxProvider::text_call(text, stop_reason)
    }

    fn tool_call(text: &str) -> FauxCall {
        FauxProvider::single_call(
            vec![FauxResponse {
                text_deltas: vec![text.to_string()],
                thinking_deltas: vec![],
                tool_calls: vec![FauxToolCall {
                    id: "call_1".into(),
                    name: "test_tool".into(),
                    deltas: vec![],
                    final_arguments: serde_json::json!({}),
                }],
            }],
            StopReason::ToolUse,
        )
    }

    async fn install_test_tool(agent: &Agent) {
        let definition = ToolDefinition {
            id: ToolId::new("test_tool").unwrap(),
            kind: ToolKind::Function,
            description: "Typed test tool".into(),
            parameters: schema_for::<RuntimeTestArgs>().unwrap(),
            capabilities: ToolCapabilities::default(),
            behavior: ToolBehaviorVersion::V1,
            authorization_risk: AuthorizationRisk::None,
            requirements: Vec::new(),
        };
        let tool = TypedTool::<RuntimeTestArgs>::new(definition, |_context, _args| {
            Box::pin(async {
                Ok(ToolOutput {
                    content: vec![ToolContent::Text {
                        text: "typed result".into(),
                    }],
                    details: Some(serde_json::json!({"runtime": true})),
                    terminate: false,
                })
            })
        })
        .unwrap();
        let mut registry = ToolRegistry::default();
        registry.register(Arc::new(tool)).unwrap();
        agent
            .set_tool_runtime(ToolRuntime::new(registry).unwrap())
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn complete_consumption_yields_terminal_events() {
        let agent = test_agent(vec![text_call("answer is 42", StopReason::Stop)]);
        let mut stream = agent.prompt("hello");
        let mut turns = 0;
        let mut saw_done = false;
        while let Some(event) = stream.next().await {
            match &event {
                AgentEvent::TurnStart { .. } => turns += 1,
                AgentEvent::AgentDone { .. } => saw_done = true,
                _ => {}
            }
        }
        assert_eq!(turns, 1);
        assert!(saw_done);
        assert_eq!(agent.messages().await.len(), 2);
    }

    #[tokio::test]
    async fn typed_runtime_tools_are_declared_and_executed_without_legacy_registration() {
        let agent = test_agent(vec![
            tool_call("typed"),
            text_call("found", StopReason::Stop),
        ]);
        install_test_tool(&agent).await;

        let request = agent.provider_request_snapshot().await.0;
        assert!(request.tools.as_ref().is_some_and(|tools| {
            tools.iter().any(|tool| {
                tool.name == "test_tool" && tool.description.as_deref() == Some("Typed test tool")
            })
        }));

        let mut stream = agent.prompt("hello");
        let mut result = None;
        while let Some(event) = stream.next().await {
            if let AgentEvent::ToolCallEnd {
                result: tool_result,
                ..
            } = event
            {
                result = Some(tool_result);
            }
        }
        let result = result.expect("typed tool result event");
        assert!(matches!(
            result.content.as_slice(),
            [ContentBlock::Text { text, .. }] if text == "typed result"
        ));
        assert_eq!(result.details, Some(serde_json::json!({"runtime": true})));
    }

    #[tokio::test]
    async fn dropping_stream_mid_turn_commits_messages_and_releases_run() {
        let agent = test_agent(vec![
            text_call("I'll check.", StopReason::ToolUse),
            text_call("done", StopReason::Stop),
        ]);
        let mut stream = agent.prompt("hello");
        assert!(matches!(
            stream.next().await,
            Some(AgentEvent::TurnStart { .. })
        ));
        drop(stream);
        assert!(
            agent
                .messages()
                .await
                .iter()
                .any(|message| matches!(message, AgentMessage::UserText { .. })),
            "the user prompt must survive an early drop"
        );
        let mut second = agent.prompt("next question");
        assert!(
            matches!(second.next().await, Some(AgentEvent::TurnStart { .. })),
            "a new run must be admitted after the stream is dropped"
        );
    }

    #[tokio::test]
    async fn dropping_after_tool_turn_preserves_tool_results() {
        let agent = test_agent(vec![
            tool_call("searching"),
            text_call("found", StopReason::Stop),
        ]);
        install_test_tool(&agent).await;
        let mut stream = agent.prompt("hello");
        while let Some(event) = stream.next().await {
            if matches!(event, AgentEvent::ToolCallEnd { .. }) {
                break;
            }
        }
        drop(stream);
        // In the bounded actor model the turn runner may complete the next
        // turn before the consumer's drop is observed, so the exact count is
        // timing-dependent. The invariant that matters is that the tool
        // result survives the early drop.
        let messages = agent.messages().await;
        let has_tool_result = messages
            .iter()
            .any(|message| matches!(message, AgentMessage::ToolResult { .. }));
        assert!(has_tool_result, "tool result must survive an early drop");
    }

    #[tokio::test]
    async fn clear_queues_during_turn_empties_queued_input() {
        let agent = test_agent(vec![
            tool_call("searching"),
            text_call("found", StopReason::Stop),
        ]);
        install_test_tool(&agent).await;
        let mut stream = agent.prompt("hello");
        while let Some(event) = stream.next().await {
            if matches!(event, AgentEvent::ToolCallEnd { .. }) {
                break;
            }
        }
        agent.steer("late input").expect("queue accepts");
        agent.clear_queues();
        while stream.next().await.is_some() {}
        assert!(agent.drain_steering_queue().await.is_empty());
        assert!(
            !agent
                .messages()
                .await
                .iter()
                .any(|message| matches!(message, AgentMessage::UserText { text, .. } if text == "late input")),
            "cleared steering input must not reach the conversation"
        );
    }

    #[tokio::test]
    async fn steering_during_turn_is_consumed_by_the_current_turn() {
        let agent = test_agent(vec![
            tool_call("searching"),
            text_call("found", StopReason::Stop),
        ]);
        install_test_tool(&agent).await;
        let mut stream = agent.prompt("hello");
        while let Some(event) = stream.next().await {
            if matches!(event, AgentEvent::ToolCallEnd { .. }) {
                break;
            }
        }
        agent.steer("steer during turn").expect("queue accepts");
        while stream.next().await.is_some() {}
        assert!(
            agent
                .messages()
                .await
                .iter()
                .any(|message| matches!(message, AgentMessage::UserText { text, .. } if text == "steer during turn")),
            "steering input enqueued mid-turn must be consumed by the current turn"
        );
    }

    #[tokio::test]
    #[ignore = "release performance baseline"]
    async fn agent_core_release_faux_first_text_delta_baseline() {
        const FIRST_DELTA_BUDGET_MICROS: u128 = 50_000;

        let agent = test_agent(vec![text_call("first delta", StopReason::Stop)]);
        let started = std::time::Instant::now();
        let mut stream = agent.prompt("hello");
        let first_delta_micros = loop {
            let event = stream.next().await.expect("faux stream has a text delta");
            if matches!(
                event,
                AgentEvent::LlmEvent(AssistantMessageEvent::TextDelta { .. })
            ) {
                break started.elapsed().as_micros();
            }
        };

        println!("agent_perf\tfaux_first_text_delta_us={first_delta_micros}");
        assert!(
            first_delta_micros <= FIRST_DELTA_BUDGET_MICROS,
            "local agent pipeline first TextDelta exceeded 50 ms: {first_delta_micros} us"
        );
    }
}
