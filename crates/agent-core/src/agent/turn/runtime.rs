use futures::channel::mpsc;
use futures::Stream;
use std::pin::Pin;
use std::sync::{
    Arc, RwLock,
    atomic::{AtomicBool, Ordering},
};
use std::task::{Context as TaskContext, Poll};

use super::nodes::{AgentTurnDecision, AgentTurnError};
use super::{context::AgentTurnContext, nodes};
use crate::agent::AgentState;
use crate::agent::types::{AgentEvent, AgentStream};

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

pub struct AgentTurnRunner;

impl AgentTurnRunner {
    pub(crate) fn run_state(
        state: Arc<RwLock<AgentState>>,
        queues_cleared: Arc<AtomicBool>,
    ) -> AgentStream {
        Box::pin(TurnLoopStream::new(state, queues_cleared))
    }
}

type TurnRunOutcome = (
    Result<AgentTurnResult, AgentTurnError>,
    AgentTurnContext,
);

/// One multi-turn agent loop, implemented as a hand-written `Stream` so the
/// in-flight `AgentTurnContext` is committed back to the shared `AgentState`
/// even when the consumer drops the stream early (e.g. UI cancellation
/// without `abort()`). Dropping mid-turn used to discard every message the
/// turn had produced along with the queued inputs.
struct TurnLoopStream {
    state: Arc<RwLock<AgentState>>,
    queues_cleared: Arc<AtomicBool>,
    turn: u32,
    context: Option<AgentTurnContext>,
    event_rx: Option<mpsc::UnboundedReceiver<AgentEvent>>,
    run_fut: Option<Pin<Box<dyn Future<Output = TurnRunOutcome> + Send>>>,
    /// Error event produced by the completed turn, yielded after any events
    /// still buffered in `event_rx`.
    pending_error: Option<AgentEvent>,
    committed: bool,
    done: bool,
}

impl TurnLoopStream {
    fn new(state: Arc<RwLock<AgentState>>, queues_cleared: Arc<AtomicBool>) -> Self {
        Self {
            state,
            queues_cleared,
            turn: 0,
            context: None,
            event_rx: None,
            run_fut: None,
            pending_error: None,
            committed: false,
            done: false,
        }
    }

    fn commit(&mut self) {
        let Some(context) = &mut self.context else {
            self.committed = true;
            return;
        };
        let discard_queues = self.queues_cleared.swap(false, Ordering::SeqCst);
        commit_context(context, &self.state, discard_queues);
        self.committed = true;
    }

    fn start_turn(&mut self) {
        let mut context = {
            let mut state = self.state.write().unwrap();
            let mut context = AgentTurnContext::from_state(&state);
            // A turn takes ownership of the live queues: they are cloned into
            // the context and the live copies cleared so later enqueues stay
            // distinct. If the caller cleared the queues mid-turn, the cloned
            // (already-synced) messages are dropped here as well.
            if self.queues_cleared.swap(false, Ordering::SeqCst) {
                context.steering_queue.clear();
                context.follow_up_queue.clear();
            }
            state.steering_queue.clear();
            state.follow_up_queue.clear();
            context
        };
        context.turn = self.turn;
        let cancel = context.cancel_token.clone();
        let (event_sender, event_rx) = mpsc::unbounded();
        context.attach_runtime(Arc::clone(&self.state), event_sender);
        // The turn future owns the context. A drop guard inside the future
        // commits it back to the shared state even when the stream itself is
        // dropped mid-poll; on normal completion the future returns the
        // context so this stream commits it at the turn boundary instead.
        let run = {
            let state = Arc::clone(&self.state);
            let queues_cleared = Arc::clone(&self.queues_cleared);
            async move {
                let mut guard = TurnRunDropGuard {
                    context: Some(context),
                    state,
                    queues_cleared,
                };
                let outcome = run_typed_turn(
                    guard.context.as_mut().expect("turn context is held"),
                    cancel,
                )
                .await;
                (outcome, guard.context.take().expect("turn context is held"))
            }
        };
        self.event_rx = Some(event_rx);
        self.run_fut = Some(Box::pin(run));
        self.committed = false;
    }

    fn finish_outcome(&mut self, outcome: Result<AgentTurnResult, AgentTurnError>) {
        let cancelled = self
            .context
            .as_ref()
            .is_some_and(|ctx| ctx.cancel_token.is_cancelled());
        self.turn = self.context.as_ref().map_or(self.turn, |ctx| ctx.turn);
        self.commit();
        match outcome {
            Ok(AgentTurnResult::Continue) => {}
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
}

fn commit_context(
    context: &mut AgentTurnContext,
    state: &Arc<RwLock<AgentState>>,
    discard_queues: bool,
) {
    let mut state = state.write().unwrap();
    context.apply_to_state(&mut state, discard_queues);
}

/// Commits the in-flight turn context when the turn future is dropped before
/// completion (the consumer dropped the stream mid-poll). On the normal path
/// the context is taken out before this guard drops, so no second commit
/// happens.
struct TurnRunDropGuard {
    context: Option<AgentTurnContext>,
    state: Arc<RwLock<AgentState>>,
    queues_cleared: Arc<AtomicBool>,
}

impl Drop for TurnRunDropGuard {
    fn drop(&mut self) {
        if let Some(context) = &mut self.context {
            let discard_queues = self.queues_cleared.swap(false, Ordering::SeqCst);
            commit_context(context, &self.state, discard_queues);
        }
    }
}

impl Drop for TurnLoopStream {
    fn drop(&mut self) {
        if !self.committed {
            self.commit();
        }
    }
}

impl Stream for TurnLoopStream {
    type Item = AgentEvent;

    fn poll_next(self: Pin<&mut Self>, cx: &mut TaskContext<'_>) -> Poll<Option<AgentEvent>> {
        let this = self.get_mut();
        loop {
            if let Some(rx) = &mut this.event_rx {
                match Pin::new(rx).poll_next(cx) {
                    Poll::Ready(Some(event)) => return Poll::Ready(Some(event)),
                    Poll::Ready(None) => this.event_rx = None,
                    Poll::Pending => {}
                }
            }
            if let Some(event) = this.pending_error.take() {
                return Poll::Ready(Some(event));
            }

            if this.run_fut.is_none() {
                if this.done {
                    return Poll::Ready(None);
                }
                this.start_turn();
                continue;
            }

            let run_fut = this.run_fut.as_mut().expect("run future is set");
            match run_fut.as_mut().poll(cx) {
                Poll::Pending => return Poll::Pending,
                Poll::Ready((outcome, context)) => {
                    this.run_fut = None;
                    this.context = Some(context);
                    this.finish_outcome(outcome);
                    continue;
                }
            }
        }
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
            (AgentTurnState::DecideAfterAssistant, AgentTurnDecision::Next),
            (AgentTurnState::ExecuteTools, AgentTurnDecision::Done),
            (AgentTurnState::ExecuteTools, AgentTurnDecision::Tools),
            (AgentTurnState::Finish, AgentTurnDecision::Next),
            (AgentTurnState::DrainQueuedInput, AgentTurnDecision::Next),
            (AgentTurnState::CompactRuntimeContext, AgentTurnDecision::Next),
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
    use super::*;
    use crate::agent::types::{AgentConfig, AgentEvent, AgentMessage};
    use crate::agent::Agent;
    use ai::api::client::AiClient;
    use ai::api::conversation::{ContentBlock, StopReason};
    use ai::api::model::{Model, ModelCost, ModelInput};
    use ai::api::provider::faux::{FauxCall, FauxProvider, FauxResponse, FauxToolCall};
    use futures::StreamExt;
    use std::sync::Arc;

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
        assert_eq!(agent.messages().len(), 2);
    }

    #[tokio::test]
    async fn dropping_stream_mid_turn_commits_messages_and_releases_run() {
        let agent = test_agent(vec![
            text_call("I'll check.", StopReason::ToolUse),
            text_call("done", StopReason::Stop),
        ]);
        let mut stream = agent.prompt("hello");
        assert!(matches!(stream.next().await, Some(AgentEvent::TurnStart { .. })));
        drop(stream);
        assert!(
            agent
                .messages()
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
        let agent = test_agent(vec![tool_call("searching"), text_call("found", StopReason::Stop)]);
        agent
            .add_tool(crate::agent::types::AgentTool {
                name: "test_tool".into(),
                description: "A test tool".into(),
                parameters: serde_json::json!({"type": "object"}),
                execution_mode: None,
                execute: Arc::new(|_context, _args, _on_update| {
                    Box::pin(async move {
                        Ok(crate::agent::types::AgentToolOutput::new(vec![
                            ContentBlock::Text {
                                text: "result".into(),
                                text_signature: None,
                            },
                        ]))
                    })
                }),
            })
            .expect("valid tool");
        let mut stream = agent.prompt("hello");
        while let Some(event) = stream.next().await {
            if matches!(event, AgentEvent::ToolCallEnd { .. }) {
                break;
            }
        }
        drop(stream);
        assert_eq!(agent.messages().len(), 3);
        let has_tool_result = agent.messages().iter().any(|message| {
            matches!(message, AgentMessage::ToolResult { .. })
        });
        assert!(has_tool_result, "tool result must survive an early drop");
    }

    #[tokio::test]
    async fn clear_queues_during_turn_empties_queued_input() {
        let agent = test_agent(vec![tool_call("searching"), text_call("found", StopReason::Stop)]);
        agent
            .add_tool(crate::agent::types::AgentTool {
                name: "test_tool".into(),
                description: "A test tool".into(),
                parameters: serde_json::json!({"type": "object"}),
                execution_mode: None,
                execute: Arc::new(|_context, _args, _on_update| {
                    Box::pin(async move {
                        Ok(crate::agent::types::AgentToolOutput::new(vec![
                            ContentBlock::Text {
                                text: "result".into(),
                                text_signature: None,
                            },
                        ]))
                    })
                }),
            })
            .expect("valid tool");
        let mut stream = agent.prompt("hello");
        while let Some(event) = stream.next().await {
            if matches!(event, AgentEvent::ToolCallEnd { .. }) {
                break;
            }
        }
        agent.steer("late input").expect("queue accepts");
        agent.clear_queues();
        while stream.next().await.is_some() {}
        assert!(agent.drain_steering_queue().is_empty());
        assert!(
            !agent
                .messages()
                .iter()
                .any(|message| matches!(message, AgentMessage::UserText { text, .. } if text == "late input")),
            "cleared steering input must not reach the conversation"
        );
    }
}
