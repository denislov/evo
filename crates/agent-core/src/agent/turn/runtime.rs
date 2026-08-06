use std::collections::VecDeque;
use std::pin::Pin;

use ai_protocol::api::conversation::ContentBlock;
use futures::channel::mpsc;
use futures::{FutureExt, StreamExt};
use tokio_util::sync::CancellationToken;

use super::context::{AgentTurnContext, AgentTurnProviderRequestOverride};
use super::nodes;
use super::nodes::{AgentTurnDecision, AgentTurnError};
use crate::agent::queue::{
    AgentInputQueue, PromptQueueEntry, edit_entry, enqueue_message, remove_entry,
};
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
    cancel_token: CancellationToken,
    pending_steering: VecDeque<PromptQueueEntry>,
    pending_follow_up: VecDeque<PromptQueueEntry>,
    pending_interjection: VecDeque<PromptQueueEntry>,
    pending_counter: u64,
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
            cancel_token: CancellationToken::new(),
            pending_steering: VecDeque::new(),
            pending_follow_up: VecDeque::new(),
            pending_interjection: VecDeque::new(),
            pending_counter: 0,
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
                self.flush_pending();
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
                    self.flush_pending();
                    self.finish_outcome(outcome.0);
                }
            }
        }
    }

    fn start_turn(&mut self) {
        let mut context = self.context.take().expect("turn context is held");
        context.turn = self.turn;
        self.cancel_token = context.cancel_token.clone();
        let cancel = self.cancel_token.clone();
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

    fn flush_pending(&mut self) {
        let Self {
            context,
            pending_steering,
            pending_follow_up,
            pending_interjection,
            ..
        } = self;
        if let Some(context) = context {
            while let Some(entry) = pending_steering.pop_front() {
                if enqueue_message(
                    &mut context.steering_queue,
                    AgentInputQueue::Steering,
                    entry,
                )
                .is_err()
                {
                    break;
                }
            }
            while let Some(entry) = pending_follow_up.pop_front() {
                if enqueue_message(
                    &mut context.follow_up_queue,
                    AgentInputQueue::FollowUp,
                    entry,
                )
                .is_err()
                {
                    break;
                }
            }
            while let Some(entry) = pending_interjection.pop_front() {
                if enqueue_message(
                    &mut context.interjection_queue,
                    AgentInputQueue::Interjection,
                    entry,
                )
                .is_err()
                {
                    break;
                }
            }
        }
    }

    fn next_pending_id(&mut self, prefix: &str) -> String {
        let id = format!("pending_{}_{}", prefix, self.pending_counter);
        self.pending_counter += 1;
        id
    }

    /// Enqueues steering input into the turn's working copy. Called by the
    /// actor when a `Steer` command arrives while this runner owns the turn.
    pub(crate) fn steer(&mut self, text: String) -> Result<(), AgentQueueError> {
        if let Some(context) = &mut self.context {
            let message_id = next_message_id(
                &context.messages,
                &context.steering_queue,
                &context.follow_up_queue,
                &context.interjection_queue,
                "steer",
            );
            enqueue_message(
                &mut context.steering_queue,
                AgentInputQueue::Steering,
                PromptQueueEntry {
                    id: message_id.clone(),
                    version: 0,
                    message: AgentMessage::UserText { message_id, text },
                },
            )
        } else {
            let id = self.next_pending_id("steer");
            self.pending_steering.push_back(PromptQueueEntry {
                id: id.clone(),
                version: 0,
                message: AgentMessage::UserText {
                    message_id: id,
                    text,
                },
            });
            Ok(())
        }
    }

    pub(crate) fn steer_content(
        &mut self,
        content: Vec<ContentBlock>,
    ) -> Result<(), AgentQueueError> {
        if let Some(context) = &mut self.context {
            let message_id = next_message_id(
                &context.messages,
                &context.steering_queue,
                &context.follow_up_queue,
                &context.interjection_queue,
                "steer",
            );
            enqueue_message(
                &mut context.steering_queue,
                AgentInputQueue::Steering,
                PromptQueueEntry {
                    id: message_id.clone(),
                    version: 0,
                    message: AgentMessage::Custom {
                        message_id,
                        custom_type: "input".into(),
                        content,
                        display: true,
                        details: None,
                        timestamp: 0,
                    },
                },
            )
        } else {
            let id = self.next_pending_id("steer");
            self.pending_steering.push_back(PromptQueueEntry {
                id: id.clone(),
                version: 0,
                message: AgentMessage::Custom {
                    message_id: id,
                    custom_type: "input".into(),
                    content,
                    display: true,
                    details: None,
                    timestamp: 0,
                },
            });
            Ok(())
        }
    }

    pub(crate) fn follow_up(&mut self, text: String) -> Result<(), AgentQueueError> {
        if let Some(context) = &mut self.context {
            let message_id = next_message_id(
                &context.messages,
                &context.steering_queue,
                &context.follow_up_queue,
                &context.interjection_queue,
                "followup",
            );
            enqueue_message(
                &mut context.follow_up_queue,
                AgentInputQueue::FollowUp,
                PromptQueueEntry {
                    id: message_id.clone(),
                    version: 0,
                    message: AgentMessage::UserText { message_id, text },
                },
            )
        } else {
            let id = self.next_pending_id("followup");
            self.pending_follow_up.push_back(PromptQueueEntry {
                id: id.clone(),
                version: 0,
                message: AgentMessage::UserText {
                    message_id: id,
                    text,
                },
            });
            Ok(())
        }
    }

    pub(crate) fn follow_up_content(
        &mut self,
        content: Vec<ContentBlock>,
    ) -> Result<(), AgentQueueError> {
        if let Some(context) = &mut self.context {
            let message_id = next_message_id(
                &context.messages,
                &context.steering_queue,
                &context.follow_up_queue,
                &context.interjection_queue,
                "followup",
            );
            enqueue_message(
                &mut context.follow_up_queue,
                AgentInputQueue::FollowUp,
                PromptQueueEntry {
                    id: message_id.clone(),
                    version: 0,
                    message: AgentMessage::Custom {
                        message_id,
                        custom_type: "input".into(),
                        content,
                        display: true,
                        details: None,
                        timestamp: 0,
                    },
                },
            )
        } else {
            let id = self.next_pending_id("followup");
            self.pending_follow_up.push_back(PromptQueueEntry {
                id: id.clone(),
                version: 0,
                message: AgentMessage::Custom {
                    message_id: id,
                    custom_type: "input".into(),
                    content,
                    display: true,
                    details: None,
                    timestamp: 0,
                },
            });
            Ok(())
        }
    }

    pub(crate) fn interject(&mut self, text: String) -> Result<(), AgentQueueError> {
        if let Some(context) = &mut self.context {
            let message_id = next_message_id(
                &context.messages,
                &context.steering_queue,
                &context.follow_up_queue,
                &context.interjection_queue,
                "interject",
            );
            enqueue_message(
                &mut context.interjection_queue,
                AgentInputQueue::Interjection,
                PromptQueueEntry {
                    id: message_id.clone(),
                    version: 0,
                    message: AgentMessage::UserText { message_id, text },
                },
            )
        } else {
            let id = self.next_pending_id("interject");
            self.pending_interjection.push_back(PromptQueueEntry {
                id: id.clone(),
                version: 0,
                message: AgentMessage::UserText {
                    message_id: id,
                    text,
                },
            });
            Ok(())
        }
    }

    pub(crate) fn interject_content(
        &mut self,
        content: Vec<ContentBlock>,
    ) -> Result<(), AgentQueueError> {
        if let Some(context) = &mut self.context {
            let message_id = next_message_id(
                &context.messages,
                &context.steering_queue,
                &context.follow_up_queue,
                &context.interjection_queue,
                "interject",
            );
            enqueue_message(
                &mut context.interjection_queue,
                AgentInputQueue::Interjection,
                PromptQueueEntry {
                    id: message_id.clone(),
                    version: 0,
                    message: AgentMessage::Custom {
                        message_id,
                        custom_type: "input".into(),
                        content,
                        display: true,
                        details: None,
                        timestamp: 0,
                    },
                },
            )
        } else {
            let id = self.next_pending_id("interject");
            self.pending_interjection.push_back(PromptQueueEntry {
                id: id.clone(),
                version: 0,
                message: AgentMessage::Custom {
                    message_id: id,
                    custom_type: "input".into(),
                    content,
                    display: true,
                    details: None,
                    timestamp: 0,
                },
            });
            Ok(())
        }
    }

    pub(crate) fn edit_queue_entry(
        &mut self,
        entry_id: &str,
        expected_version: u32,
        new_message: AgentMessage,
    ) -> Result<(), AgentQueueError> {
        let Self {
            context,
            pending_steering,
            pending_follow_up,
            pending_interjection,
            ..
        } = self;
        match context {
            Some(context) => edit_entry(
                &mut [
                    &mut context.steering_queue,
                    &mut context.follow_up_queue,
                    &mut context.interjection_queue,
                    pending_steering,
                    pending_follow_up,
                    pending_interjection,
                ],
                entry_id,
                expected_version,
                new_message,
            ),
            None => edit_entry(
                &mut [pending_steering, pending_follow_up, pending_interjection],
                entry_id,
                expected_version,
                new_message,
            ),
        }
    }

    pub(crate) fn remove_queue_entry(
        &mut self,
        entry_id: &str,
        expected_version: u32,
    ) -> Result<(), AgentQueueError> {
        let Self {
            context,
            pending_steering,
            pending_follow_up,
            pending_interjection,
            ..
        } = self;
        match context {
            Some(context) => remove_entry(
                &mut [
                    &mut context.steering_queue,
                    &mut context.follow_up_queue,
                    &mut context.interjection_queue,
                    pending_steering,
                    pending_follow_up,
                    pending_interjection,
                ],
                entry_id,
                expected_version,
            ),
            None => remove_entry(
                &mut [pending_steering, pending_follow_up, pending_interjection],
                entry_id,
                expected_version,
            ),
        }
    }

    pub(crate) fn clear_queues(&mut self) {
        if let Some(context) = &mut self.context {
            context.steering_queue.clear();
            context.follow_up_queue.clear();
            context.interjection_queue.clear();
        }
        self.pending_steering.clear();
        self.pending_follow_up.clear();
        self.pending_interjection.clear();
    }

    pub(crate) fn drain_steering_queue(&mut self) -> Vec<AgentMessage> {
        let mut result: Vec<AgentMessage> = match &mut self.context {
            Some(context) => context
                .steering_queue
                .drain(..)
                .map(|entry| entry.message)
                .collect(),
            None => Vec::new(),
        };
        result.extend(self.pending_steering.drain(..).map(|entry| entry.message));
        result
    }

    pub(crate) fn drain_follow_up_queue(&mut self) -> Vec<AgentMessage> {
        let mut result: Vec<AgentMessage> = match &mut self.context {
            Some(context) => context
                .follow_up_queue
                .drain(..)
                .map(|entry| entry.message)
                .collect(),
            None => Vec::new(),
        };
        result.extend(self.pending_follow_up.drain(..).map(|entry| entry.message));
        result
    }

    pub(crate) fn abort(&mut self) {
        self.cancel_token.cancel();
    }

    pub(crate) fn set_provider_request_override(
        &mut self,
        context: ai_protocol::api::conversation::Context,
        stream_options: Option<ai_protocol::api::stream::StreamOptions>,
    ) {
        if let Some(ctx) = &mut self.context {
            ctx.provider_request_override = Some(AgentTurnProviderRequestOverride {
                context,
                stream_options,
            });
        }
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
