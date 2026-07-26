use async_stream::stream;
use futures::channel::mpsc;
use futures::{FutureExt, StreamExt};
use std::sync::{Arc, RwLock};

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
    pub(crate) fn run_state(state: Arc<RwLock<AgentState>>) -> AgentStream {
        Box::pin(stream! {
            let mut turn: u32 = 0;

            loop {
                let mut context = {
                    let mut state = state.write().unwrap();
                    let context = AgentTurnContext::from_state(&state);
                    state.steering_queue.clear();
                    state.follow_up_queue.clear();
                    context
                };
                context.turn = turn;
                let cancel = context.cancel_token.clone();
                let (event_sender, mut event_receiver) = mpsc::unbounded();
                context.attach_runtime(Arc::clone(&state), event_sender);

                let mut run = Box::pin(run_typed_turn(&mut context, cancel)).fuse();
                let outcome_result = loop {
                    futures::select! {
                        event = event_receiver.next().fuse() => {
                            if let Some(event) = event {
                                yield event;
                            }
                        }
                        outcome = &mut run => break outcome,
                    }
                };
                drop(run);
                while let Some(Some(event)) = event_receiver.next().now_or_never() {
                    yield event;
                }

                let outcome = match outcome_result {
                    Ok(outcome) => outcome,
                    Err(error) => {
                        {
                            let mut state = state.write().unwrap();
                            context.apply_to_state(&mut state);
                        }
                        yield AgentEvent::AgentError {
                            error: if context.cancel_token.is_cancelled() {
                                "aborted".into()
                            } else {
                                error.to_string()
                            },
                        };
                        return;
                    }
                };

                turn = context.turn;

                {
                    let mut state = state.write().unwrap();
                    context.apply_to_state(&mut state);
                }

                match outcome {
                    AgentTurnResult::Continue => continue,
                    AgentTurnResult::Finish => return,
                }
            }
        })
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
mod tests {
    use super::*;

    const ALL_STATES: [AgentTurnState; 10] = [
        AgentTurnState::Finish,
        AgentTurnState::Start,
        AgentTurnState::DrainQueuedInput,
        AgentTurnState::CompactRuntimeContext,
        AgentTurnState::PrepareProviderRequest,
        AgentTurnState::ApplyProviderHook,
        AgentTurnState::ProviderStream,
        AgentTurnState::DecideAfterAssistant,
        AgentTurnState::ExecuteTools,
        AgentTurnState::PrepareNextTurn,
    ];
    const ALL_DECISIONS: [AgentTurnDecision; 7] = [
        AgentTurnDecision::Next,
        AgentTurnDecision::Continue,
        AgentTurnDecision::ContinueProvider,
        AgentTurnDecision::Tools,
        AgentTurnDecision::Done,
        AgentTurnDecision::Error,
        AgentTurnDecision::Aborted,
    ];
    const FIXED_TRANSITIONS: [(AgentTurnState, AgentTurnState); 2] = [
        (
            AgentTurnState::DrainQueuedInput,
            AgentTurnState::CompactRuntimeContext,
        ),
        (
            AgentTurnState::CompactRuntimeContext,
            AgentTurnState::PrepareProviderRequest,
        ),
    ];

    fn state_rank(state: AgentTurnState) -> usize {
        match state {
            AgentTurnState::Start => 0,
            AgentTurnState::DrainQueuedInput => 1,
            AgentTurnState::CompactRuntimeContext => 2,
            AgentTurnState::PrepareProviderRequest => 3,
            AgentTurnState::ApplyProviderHook => 4,
            AgentTurnState::ProviderStream => 5,
            AgentTurnState::DecideAfterAssistant => 6,
            AgentTurnState::ExecuteTools => 7,
            AgentTurnState::PrepareNextTurn => 8,
            AgentTurnState::Finish => 9,
        }
    }

    fn expected_decision_successor(
        state: AgentTurnState,
        decision: AgentTurnDecision,
    ) -> Option<AgentTurnState> {
        use AgentTurnDecision::{Aborted, Continue, ContinueProvider, Error, Next, Tools};
        use AgentTurnState::{
            ApplyProviderHook, DecideAfterAssistant, DrainQueuedInput, ExecuteTools, Finish,
            PrepareNextTurn, PrepareProviderRequest, ProviderStream, Start,
        };

        match (state, decision) {
            (Start, Next) => Some(DrainQueuedInput),
            (PrepareProviderRequest, Next) => Some(ApplyProviderHook),
            (ApplyProviderHook, Next) => Some(ProviderStream),
            (ProviderStream, Next) => Some(DecideAfterAssistant),
            (DecideAfterAssistant, Continue) => Some(PrepareNextTurn),
            (DecideAfterAssistant, Tools) => Some(ExecuteTools),
            (ExecuteTools, Continue | ContinueProvider) => Some(PrepareNextTurn),
            (
                Start
                | PrepareProviderRequest
                | ApplyProviderHook
                | ProviderStream
                | DecideAfterAssistant
                | ExecuteTools,
                Error | Aborted,
            ) => Some(Finish),
            _ => None,
        }
    }

    fn successors(state: AgentTurnState) -> Vec<AgentTurnState> {
        let mut successors: Vec<_> = FIXED_TRANSITIONS
            .iter()
            .filter_map(move |(from, to)| (*from == state).then_some(*to))
            .collect();
        for decision in ALL_DECISIONS {
            if let Ok(successor) = transition_from_decision(state, decision)
                && !successors.contains(&successor)
            {
                successors.push(successor);
            }
        }
        successors
    }

    fn longest_path_visits(state: AgentTurnState) -> usize {
        1 + successors(state)
            .into_iter()
            .map(longest_path_visits)
            .max()
            .unwrap_or(0)
    }

    #[test]
    fn decision_transition_table_is_exhaustive() {
        for state in ALL_STATES {
            for decision in ALL_DECISIONS {
                let actual = transition_from_decision(state, decision);
                let expected = expected_decision_successor(state, decision);
                match expected {
                    Some(expected) => assert_eq!(
                        actual.unwrap_or_else(|error| {
                            panic!("{state:?} + {decision:?} failed unexpectedly: {error}")
                        }),
                        expected
                    ),
                    None => assert!(
                        matches!(actual, Err(AgentTurnError::Invariant(_))),
                        "{state:?} + {decision:?} unexpectedly produced {actual:?}"
                    ),
                }
            }
        }
    }

    #[test]
    fn legal_turn_state_graph_is_acyclic_and_fuse_has_headroom() {
        for state in ALL_STATES {
            for successor in successors(state) {
                assert!(
                    state_rank(state) < state_rank(successor),
                    "state graph contains a non-forward edge: {state:?} -> {successor:?}"
                );
            }
        }

        let longest = longest_path_visits(AgentTurnState::Start);
        assert_eq!(longest, MAX_LEGAL_TURN_STATE_VISITS);
        assert!(
            TURN_STATE_VISIT_FUSE > longest,
            "invariant fuse must remain above the longest legal path"
        );
    }
}
