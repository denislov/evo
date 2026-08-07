use tui::api::render::Tui;
use tui::api::terminal::Terminal;

use crate::interactive::error::CliError;
use crate::interactive::prompt_task::{PromptTaskCompletion, PromptTaskFailure, PromptTaskResult};
use crate::interactive::root::{InteractiveRoot, InteractiveStatus};
use crate::interactive::session_actions::hydrated_session_from_snapshot;
use crate::interactive::{TranscriptItem, UiEvent};
use coding_agent::api::operation::PromptTurnOutcome;
use coding_agent::api::runtime::{CodingAgentSession, CodingAgentSessionBootstrap};

use super::{public_cli_error_message, root_mut, set_terminal_progress};

pub(super) fn finish_prompt<T: Terminal>(
    tui: &mut Tui<T>,
    root_id: usize,
    result: PromptTaskCompletion,
    coding_session: &mut Option<CodingAgentSession>,
    session_bootstrap: &mut CodingAgentSessionBootstrap,
) -> Result<(), CliError> {
    set_terminal_progress(tui, false)?;
    let root = root_mut(tui, root_id)?;
    match result {
        PromptTaskCompletion::Completed(PromptTaskResult::Coding(result)) => {
            if let Some(session_id) = result.replacement_session_id.clone() {
                *session_bootstrap = session_bootstrap.clone().with_session_id(session_id);
            }
            let completion_notice = result.completion_notice.clone();
            if result.hydrate_transcript {
                if let Ok(Some(hydration)) = result.session.current_session_snapshot() {
                    root.apply_hydrated_session(
                        hydrated_session_from_snapshot(hydration),
                        completion_notice,
                    );
                } else {
                    finish_coding_prompt(root, &result.session, result.outcome)?;
                    if let Some(notice) = completion_notice {
                        root.transcript.push(TranscriptItem::system(notice));
                    }
                }
            } else {
                finish_coding_prompt(root, &result.session, result.outcome)?;
                if let Some(notice) = completion_notice {
                    root.transcript.push(TranscriptItem::system(notice));
                }
            }
            *coding_session = Some(result.session);
        }
        PromptTaskCompletion::Completed(PromptTaskResult::AgentInvocation(result)) => {
            root.set_default_agent_profile_id(
                result.session.view()?.default_agent_profile_id.clone(),
            );
            if let Ok(Some(hydration)) = result.session.current_session_snapshot() {
                let hydrated = hydrated_session_from_snapshot(hydration);
                let mut choice = hydrated.choice;
                if choice.active_leaf_id.is_none() {
                    choice.active_leaf_id = root.active_leaf_id.clone();
                }
                root.set_active_session_choice(choice);
            }
            *coding_session = Some(result.session);
        }
        PromptTaskCompletion::Completed(PromptTaskResult::AgentTeam(result)) => {
            let _final_text = &result.outcome.final_text;
            root.set_default_agent_profile_id(
                result.session.view()?.default_agent_profile_id.clone(),
            );
            if let Ok(Some(hydration)) = result.session.current_session_snapshot() {
                let hydrated = hydrated_session_from_snapshot(hydration);
                let mut choice = hydrated.choice;
                if choice.active_leaf_id.is_none() {
                    choice.active_leaf_id = root.active_leaf_id.clone();
                }
                root.set_active_session_choice(choice);
            }
            *coding_session = Some(result.session);
        }
        PromptTaskCompletion::Completed(PromptTaskResult::DelegationApproval(result)) => {
            root.set_default_agent_profile_id(
                result.session.view()?.default_agent_profile_id.clone(),
            );
            if let Ok(Some(hydration)) = result.session.current_session_snapshot() {
                let hydrated = hydrated_session_from_snapshot(hydration);
                let mut choice = hydrated.choice;
                if choice.active_leaf_id.is_none() {
                    choice.active_leaf_id = root.active_leaf_id.clone();
                }
                root.set_active_session_choice(choice);
            }
            *coding_session = Some(result.session);
        }
        PromptTaskCompletion::Completed(PromptTaskResult::SessionTreeLabel(result)) => {
            let notice = match result.label.as_deref() {
                Some(label) => format!("Tree label updated: {label}"),
                None => "Tree label cleared".to_string(),
            };
            root.apply_tree_label_update(&result.entry_id, result.label, result.updated_at);
            root.transcript.push(TranscriptItem::system(notice));
            *coding_session = Some(result.session);
        }
        PromptTaskCompletion::Completed(PromptTaskResult::MergeReview(result)) => {
            root.transcript.push(TranscriptItem::system(result.message));
            *coding_session = Some(result.session);
        }
        PromptTaskCompletion::Completed(PromptTaskResult::DelegationRejection(result)) => {
            root.set_default_agent_profile_id(
                result.session.view()?.default_agent_profile_id.clone(),
            );
            if let Ok(Some(hydration)) = result.session.current_session_snapshot() {
                let hydrated = hydrated_session_from_snapshot(hydration);
                let mut choice = hydrated.choice;
                if choice.active_leaf_id.is_none() {
                    choice.active_leaf_id = root.active_leaf_id.clone();
                }
                root.set_active_session_choice(choice);
            }
            *coding_session = Some(result.session);
        }
        PromptTaskCompletion::Completed(PromptTaskResult::SelfHealingEdit(result)) => {
            root.transcript
                .push(TranscriptItem::system(result.outcome.message.clone()));
            for diagnostic in &result.outcome.diagnostics {
                root.transcript
                    .push(TranscriptItem::system(diagnostic.message.clone()));
            }
            root.set_default_agent_profile_id(
                result.session.view()?.default_agent_profile_id.clone(),
            );
            if let Ok(Some(hydration)) = result.session.current_session_snapshot() {
                let hydrated = hydrated_session_from_snapshot(hydration);
                let mut choice = hydrated.choice;
                if choice.active_leaf_id.is_none() {
                    choice.active_leaf_id = root.active_leaf_id.clone();
                }
                root.set_active_session_choice(choice);
            }
            *coding_session = Some(result.session);
        }
        PromptTaskCompletion::Completed(PromptTaskResult::ForkSession(result)) => {
            *session_bootstrap = session_bootstrap
                .clone()
                .with_session_id(result.replacement_session_id.clone());
            let completion_notice = result.completion_notice.clone();
            if result.hydrate_transcript {
                if let Ok(Some(hydration)) = result.session.current_session_snapshot() {
                    root.apply_hydrated_session(
                        hydrated_session_from_snapshot(hydration),
                        completion_notice,
                    );
                } else if let Some(notice) = completion_notice {
                    root.transcript.push(TranscriptItem::system(notice));
                }
            } else if let Some(notice) = completion_notice {
                root.transcript.push(TranscriptItem::system(notice));
            }
            root.set_default_agent_profile_id(
                result.session.view()?.default_agent_profile_id.clone(),
            );
            *coding_session = Some(result.session);
        }
        PromptTaskCompletion::Failed(PromptTaskFailure { session, error }) => {
            *coding_session = Some(session);
            root.apply_events(vec![UiEvent::AgentError {
                error: public_cli_error_message(&error),
            }]);
        }
        PromptTaskCompletion::SetupFailed(error) => {
            root.apply_events(vec![UiEvent::AgentError {
                error: public_cli_error_message(&error),
            }]);
        }
    }
    root.set_status(InteractiveStatus::Idle);
    Ok(())
}

fn finish_coding_prompt(
    root: &mut InteractiveRoot,
    session: &CodingAgentSession,
    outcome: PromptTurnOutcome,
) -> Result<(), CliError> {
    root.set_default_agent_profile_id(session.view()?.default_agent_profile_id.clone());
    root.clear_active_session();
    match outcome {
        PromptTurnOutcome::Success {
            session_id,
            leaf_id,
            ..
        } => {
            if let Some(session_id) = session_id {
                root.session_label = session_id;
                root.active_leaf_id = leaf_id;
            }
        }
        PromptTurnOutcome::Aborted { session_id, .. } => {
            if let Some(session_id) = session_id {
                root.session_label = session_id;
            }
        }
        PromptTurnOutcome::Failed { .. } => {}
    }
    if let Ok(Some(hydration)) = session.current_session_snapshot() {
        let hydrated = hydrated_session_from_snapshot(hydration);
        let mut choice = hydrated.choice;
        if choice.active_leaf_id.is_none() {
            choice.active_leaf_id = root.active_leaf_id.clone();
        }
        root.set_active_session_choice(choice);
    }
    Ok(())
}
