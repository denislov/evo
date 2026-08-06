use super::nodes::{AgentTurnDecision, AgentTurnError};
use super::runtime::AgentTurnState;

pub(crate) fn transition_from_decision(
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
