use tui::api::input::{InputEvent, is_key_release};
use tui::api::render::Tui;
use tui::api::terminal::Terminal;

use crate::interactive::TranscriptItem;
use crate::interactive::app::PromptContext;
use crate::interactive::error::CliError;
use crate::interactive::prompt_task::PromptTask;
use crate::interactive::root::{InteractiveAction, PendingInteractiveCommand};
use crate::interactive::session_actions::{SessionChoiceKind, hydrate_existing_session_target};
use coding_agent::api::embedding::CodingAgentAuthMutation;
use coding_agent::api::runtime::CodingAgentSession;

use super::effects::{
    handle_delegation_confirmation_command, start_agent_invocation_task, start_agent_team_task,
    start_branch_summary_navigation_task, start_branch_summary_task, start_compact_task,
    start_fork_task, start_merge_review_task, start_prompt_task, start_self_healing_edit_task,
    start_tree_label_task, start_tree_navigation_fork_task,
};
use super::{
    LoopControl, RenderRequest, root_mut, root_ref, set_terminal_progress, sync_transient_overlays,
};

pub(super) async fn handle_input_event<T: Terminal>(
    tui: &mut Tui<T>,
    root_id: usize,
    event: InputEvent,
    prompt_context: &mut PromptContext,
    running: &mut Option<PromptTask>,
    coding_session: &mut Option<CodingAgentSession>,
) -> Result<LoopControl, CliError> {
    if is_key_release(&event) {
        return Ok(LoopControl::Continue(RenderRequest::NONE));
    }

    let before = root_ref(tui, root_id)?.render_state();
    tui.dispatch_input(&event);
    root_mut(tui, root_id)?.drain_modal_overlay_input();

    let (
        action,
        prompt,
        prompt_invocation,
        selected_model,
        selected_thinking_level,
        selected_agent_profile_id,
        selected_session,
        selected_session_hydrate,
        settings_command,
        auth_command,
        compact_instructions,
        branch_summary_request,
        agent_invocation_request,
        agent_team_request,
        delegation_confirmation_command,
        tool_authorization_decision,
        self_healing_edit_request,
        fork_request,
        merge_review_request,
        mut render_request,
    ) = {
        let root = root_mut(tui, root_id)?;
        let action = root.take_action();
        let mut prompt = None;
        let mut prompt_invocation = None;
        let mut selected_agent_profile_id = None;
        let mut compact_instructions = None;
        let mut branch_summary_request = None;
        let mut agent_invocation_request = None;
        let mut agent_team_request = None;
        let mut self_healing_edit_request = None;
        let mut fork_request = None;
        let mut merge_review_request = None;
        if let Some(command) = root.take_pending_command() {
            debug_assert_eq!(action, command.action());
            match command {
                PendingInteractiveCommand::Submit(text)
                | PendingInteractiveCommand::FollowUp(text) => prompt = Some(text),
                PendingInteractiveCommand::SubmitResource {
                    display_text,
                    invocation,
                } => {
                    prompt = Some(display_text);
                    prompt_invocation = Some(invocation);
                }
                PendingInteractiveCommand::Compact { instructions } => {
                    compact_instructions = instructions;
                }
                PendingInteractiveCommand::BranchSummary(request) => {
                    branch_summary_request = Some(request);
                }
                PendingInteractiveCommand::Fork(request) => fork_request = Some(request),
                PendingInteractiveCommand::AgentInvocation(request) => {
                    agent_invocation_request = Some(request);
                }
                PendingInteractiveCommand::AgentTeam(request) => {
                    agent_team_request = Some(request);
                }
                PendingInteractiveCommand::SelfHealingEdit(request) => {
                    self_healing_edit_request = Some(request);
                }
                PendingInteractiveCommand::MergeReview(request) => {
                    merge_review_request = Some(request);
                }
                PendingInteractiveCommand::UseAgentProfile(profile_id) => {
                    selected_agent_profile_id = Some(profile_id);
                }
            }
        }
        let selected_model = root.take_selected_model();
        let selected_thinking_level = root.take_selected_thinking_level();
        let selected_session = root.take_selected_session();
        let selected_session_hydrate = root.take_selected_session_hydrate();
        let settings_command = root.take_settings_command();
        let auth_command = root.take_auth_command();
        let delegation_confirmation_command = if action == InteractiveAction::DelegationConfirmation
        {
            root.take_pending_delegation_confirmation_command()
        } else {
            None
        };
        let tool_authorization_decision = if action == InteractiveAction::ToolAuthorization {
            root.take_pending_tool_authorization_decision()
        } else {
            None
        };
        let after = root.render_state();
        (
            action,
            prompt,
            prompt_invocation,
            selected_model,
            selected_thinking_level,
            selected_agent_profile_id,
            selected_session,
            selected_session_hydrate,
            settings_command,
            auth_command,
            compact_instructions,
            branch_summary_request,
            agent_invocation_request,
            agent_team_request,
            delegation_confirmation_command,
            tool_authorization_decision,
            self_healing_edit_request,
            fork_request,
            merge_review_request,
            RenderRequest::changed(before != after),
        )
    };
    sync_transient_overlays(tui, root_id)?;

    if let Some(model) = selected_model {
        let diagnostic_text = prompt_context.select_model(&model)?;
        if !diagnostic_text.is_empty() {
            eprint!("{diagnostic_text}");
        }
        let root = root_mut(tui, root_id)?;
        root.available_models = prompt_context.model_choices.clone();
        root.auth_snapshot = prompt_context.auth_controller.snapshot();
    }
    if let Some(thinking_level) = selected_thinking_level {
        prompt_context.thinking_level = Some(thinking_level);
    }
    if let Some(profile_id) = selected_agent_profile_id {
        if coding_session.is_some() {
            let root = root_mut(tui, root_id)?;
            root.transcript.push(TranscriptItem::system(
                "The session profile is locked to the choice made at session creation. Start a new session to use a different agent profile.",
            ));
            return Ok(LoopControl::Continue(RenderRequest::FORCE));
        } else {
            prompt_context.default_agent_profile_id = profile_id.clone();
            prompt_context
                .profile_catalog
                .sync_default_agent_profile(&profile_id);
            prompt_context.session_bootstrap = prompt_context
                .session_bootstrap
                .clone()
                .with_default_agent_profile_id(profile_id);
        }
    }
    if let Some(session) = selected_session {
        *coding_session = None;
        prompt_context.session_bootstrap = prompt_context
            .session_bootstrap
            .clone()
            .with_session_id(session.id.clone());
        prompt_context
            .operation_factory
            .bind_session_bootstrap(&prompt_context.session_bootstrap);
        if selected_session_hydrate
            && let Some(hydrated) =
                hydrate_existing_session_target(&prompt_context.session_bootstrap)?
        {
            let root = root_mut(tui, root_id)?;
            root.apply_hydrated_session(
                hydrated,
                Some(format!("Session selected: {}", session.display_name())),
            );
        }
    }
    if let Some(command) = settings_command {
        match prompt_context.apply_settings_command(command) {
            Ok(outcome) => {
                let clear_on_shrink = outcome.snapshot.presentation.clear_on_shrink;
                let show_progress = outcome.snapshot.presentation.show_progress;
                root_mut(tui, root_id)?.settings = outcome.snapshot;
                tui.set_clear_on_shrink(clear_on_shrink);
                set_terminal_progress(tui, running.is_some() && show_progress)?;
            }
            Err(error) => {
                let root = root_mut(tui, root_id)?;
                root.apply_prompt_context(prompt_context);
                root.transcript.push(TranscriptItem::system(format!(
                    "Failed to update settings: {}",
                    error.summary
                )));
            }
        }
        render_request = RenderRequest::FORCE;
    }
    if let Some(command) = auth_command {
        let root = root_mut(tui, root_id)?;
        match prompt_context.apply_auth_command(command) {
            Ok(outcome) => {
                root.auth_snapshot = outcome.snapshot;
                root.available_models = prompt_context.model_choices.clone();
                let notice = match outcome.mutation {
                    CodingAgentAuthMutation::Stored => {
                        format!("Saved API key for {}", outcome.provider)
                    }
                    CodingAgentAuthMutation::Removed => {
                        format!("Removed stored auth for {}", outcome.provider)
                    }
                    CodingAgentAuthMutation::NotFound => {
                        format!("No stored auth found for {}", outcome.provider)
                    }
                };
                root.transcript.push(TranscriptItem::system(notice));
            }
            Err(error) => {
                root.transcript.push(TranscriptItem::system(format!(
                    "Failed to update provider authentication: {}",
                    error.summary
                )));
            }
        }
        render_request = RenderRequest::FORCE;
    }

    let tree_label_change = if running.is_none() {
        root_mut(tui, root_id)?.take_pending_tree_label_change()
    } else {
        None
    };
    if let Some((entry_id, label)) = tree_label_change {
        start_tree_label_task(
            tui,
            root_id,
            entry_id,
            label,
            prompt_context,
            running,
            coding_session,
        )?;
        return Ok(LoopControl::Continue(RenderRequest::FORCE));
    }

    // Process tree navigation.
    let mut tree_navigation_summary: Option<(String, String)> = None;
    let mut tree_navigation_fork: Option<String> = None;
    {
        let root = root_mut(tui, root_id)?;
        if let Some(target_id) = root.take_selected_tree_entry_id() {
            if let Some(choice) = root
                .active_session
                .as_ref()
                .filter(|choice| choice.kind == SessionChoiceKind::Persistent)
                .cloned()
            {
                let current_leaf_id = choice
                    .active_leaf_id
                    .clone()
                    .or_else(|| root.active_leaf_id.clone());
                if current_leaf_id.as_deref() == Some(target_id.as_str()) {
                    root.transcript
                        .push(TranscriptItem::system("Already at this point".to_string()));
                } else if let Some(source_leaf_id) = current_leaf_id {
                    tree_navigation_summary = Some((source_leaf_id, target_id));
                } else {
                    tree_navigation_fork = Some(target_id);
                }
            } else {
                root.transcript.push(TranscriptItem::system(
                    "No active Rust-native session for tree navigation".to_string(),
                ));
            }
        }
    }
    if let Some((source_leaf_id, target_leaf_id)) = tree_navigation_summary {
        if running.is_some() {
            let root = root_mut(tui, root_id)?;
            root.transcript.push(TranscriptItem::system(
                "Wait for the current run to finish before navigating the session tree.",
            ));
            return Ok(LoopControl::Continue(RenderRequest::FORCE));
        }
        *running = Some(start_branch_summary_navigation_task(
            tui,
            root_id,
            source_leaf_id,
            target_leaf_id,
            prompt_context,
            coding_session,
        )?);
        return Ok(LoopControl::Continue(RenderRequest::FORCE));
    }
    if let Some(target_id) = tree_navigation_fork {
        if running.is_some() {
            let root = root_mut(tui, root_id)?;
            root.transcript.push(TranscriptItem::system(
                "Wait for the current run to finish before navigating the session tree.",
            ));
            return Ok(LoopControl::Continue(RenderRequest::FORCE));
        }
        start_tree_navigation_fork_task(
            tui,
            root_id,
            target_id,
            prompt_context,
            running,
            coding_session,
        )?;
        return Ok(LoopControl::Continue(RenderRequest::FORCE));
    }

    match action {
        InteractiveAction::None => Ok(LoopControl::Continue(render_request)),
        InteractiveAction::Exit => {
            set_terminal_progress(tui, false)?;
            Ok(LoopControl::Exit)
        }
        InteractiveAction::AbortRunning => {
            if let Some(task) = running.as_mut() {
                task.abort_once().await;
            }
            Ok(LoopControl::Continue(RenderRequest::FORCE))
        }
        InteractiveAction::ToolAuthorization => {
            let Some((request, decision)) = tool_authorization_decision else {
                return Ok(LoopControl::Continue(render_request));
            };
            let accepted = match running.as_ref() {
                Some(task) => {
                    task.decide_tool_authorization(request.identity(), decision)
                        .await
                }
                None => false,
            };
            if !accepted {
                let root = root_mut(tui, root_id)?;
                root.restore_tool_authorization(request);
                root.transcript.push(TranscriptItem::system(
                    "Tool authorization decision could not be delivered to the active operation.",
                ));
            }
            Ok(LoopControl::Continue(RenderRequest::FORCE))
        }
        InteractiveAction::NewSession => {
            if prompt_context.session_bootstrap.is_persistent() {
                *coding_session = None;
                prompt_context.session_bootstrap = prompt_context
                    .session_bootstrap
                    .clone()
                    .with_fresh_session();
                prompt_context
                    .operation_factory
                    .bind_session_bootstrap(&prompt_context.session_bootstrap);
            }
            Ok(LoopControl::Continue(RenderRequest::FORCE))
        }
        InteractiveAction::ReloadResources => {
            if running.is_some() {
                let root = root_mut(tui, root_id)?;
                root.transcript.push(TranscriptItem::system(
                    "Wait for the current run to finish before reloading local resources.",
                ));
                return Ok(LoopControl::Continue(RenderRequest::FORCE));
            }
            match prompt_context.reload() {
                Ok(mut reloaded) => {
                    reloaded.default_agent_profile_id =
                        prompt_context.default_agent_profile_id.clone();
                    reloaded
                        .profile_catalog
                        .sync_default_agent_profile(&prompt_context.default_agent_profile_id);
                    reloaded.session_bootstrap = reloaded
                        .session_bootstrap
                        .inherit_initial_session_name_from(&prompt_context.session_bootstrap)
                        .with_default_agent_profile_id(
                            prompt_context.default_agent_profile_id.clone(),
                        );
                    reloaded
                        .operation_factory
                        .bind_session_bootstrap(&reloaded.session_bootstrap);
                    *prompt_context = reloaded;
                    let root = root_mut(tui, root_id)?;
                    root.apply_prompt_context(prompt_context);
                    root.transcript.push(TranscriptItem::system(
                        "Reloaded local configuration and resources",
                    ));
                }
                Err(error) => {
                    let root = root_mut(tui, root_id)?;
                    root.transcript
                        .push(TranscriptItem::system(format!("Reload failed: {error}")));
                }
            }
            Ok(LoopControl::Continue(RenderRequest::FORCE))
        }
        InteractiveAction::AgentProfileUse => Ok(LoopControl::Continue(RenderRequest::FORCE)),
        InteractiveAction::AgentInvocation => {
            if running.is_some() {
                return Ok(LoopControl::Continue(render_request));
            }
            let Some(request) = agent_invocation_request else {
                return Ok(LoopControl::Continue(render_request));
            };
            *running = Some(start_agent_invocation_task(
                tui,
                root_id,
                request,
                prompt_context,
                coding_session,
            )?);
            Ok(LoopControl::Continue(RenderRequest::FORCE))
        }
        InteractiveAction::AgentTeam => {
            if running.is_some() {
                return Ok(LoopControl::Continue(render_request));
            }
            let Some(request) = agent_team_request else {
                return Ok(LoopControl::Continue(render_request));
            };
            *running = Some(start_agent_team_task(
                tui,
                root_id,
                request,
                prompt_context,
                coding_session,
            )?);
            Ok(LoopControl::Continue(RenderRequest::FORCE))
        }
        InteractiveAction::MergeReview => {
            if running.is_some() {
                return Ok(LoopControl::Continue(render_request));
            }
            let Some(request) = merge_review_request else {
                return Ok(LoopControl::Continue(render_request));
            };
            start_merge_review_task(
                tui,
                root_id,
                request,
                prompt_context,
                running,
                coding_session,
            )?;
            Ok(LoopControl::Continue(RenderRequest::FORCE))
        }
        InteractiveAction::DelegationConfirmation => {
            if running.is_some() {
                return Ok(LoopControl::Continue(render_request));
            }
            let Some(command) = delegation_confirmation_command else {
                return Ok(LoopControl::Continue(render_request));
            };
            handle_delegation_confirmation_command(
                tui,
                root_id,
                command,
                prompt_context,
                running,
                coding_session,
            )?;
            Ok(LoopControl::Continue(RenderRequest::FORCE))
        }
        InteractiveAction::Submit => {
            let Some(prompt) = prompt else {
                return Ok(LoopControl::Continue(render_request));
            };
            if prompt.trim().is_empty() {
                return Ok(LoopControl::Continue(render_request));
            }
            if let Some(task) = running.as_ref() {
                if prompt_invocation.is_some() {
                    let root = root_mut(tui, root_id)?;
                    root.transcript.push(TranscriptItem::system(
                        "Wait for the current run to finish before invoking a skill or prompt template.",
                    ));
                    return Ok(LoopControl::Continue(RenderRequest::FORCE));
                }
                if task.steer(prompt).await {
                    return Ok(LoopControl::Continue(RenderRequest::FORCE));
                }
                return Ok(LoopControl::Continue(render_request));
            }
            *running = Some(start_prompt_task(
                tui,
                root_id,
                prompt,
                prompt_invocation,
                prompt_context,
                coding_session,
            )?);
            Ok(LoopControl::Continue(RenderRequest::FORCE))
        }
        InteractiveAction::FollowUp => {
            let Some(prompt) = prompt else {
                return Ok(LoopControl::Continue(render_request));
            };
            if prompt.trim().is_empty() {
                return Ok(LoopControl::Continue(render_request));
            }
            if let Some(task) = running.as_ref()
                && task.follow_up(prompt).await
            {
                return Ok(LoopControl::Continue(RenderRequest::FORCE));
            }
            Ok(LoopControl::Continue(render_request))
        }
        InteractiveAction::CompactSession => {
            if running.is_some() {
                return Ok(LoopControl::Continue(render_request));
            }
            *running = Some(start_compact_task(
                tui,
                root_id,
                compact_instructions,
                prompt_context,
                coding_session,
            )?);
            Ok(LoopControl::Continue(RenderRequest::FORCE))
        }
        InteractiveAction::BranchSummary => {
            if running.is_some() {
                return Ok(LoopControl::Continue(render_request));
            }
            let Some(request) = branch_summary_request else {
                return Ok(LoopControl::Continue(render_request));
            };
            *running = Some(start_branch_summary_task(
                tui,
                root_id,
                request,
                prompt_context,
                coding_session,
            )?);
            Ok(LoopControl::Continue(RenderRequest::FORCE))
        }
        InteractiveAction::SelfHealingEdit => {
            if running.is_some() {
                return Ok(LoopControl::Continue(render_request));
            }
            let Some(request) = self_healing_edit_request else {
                return Ok(LoopControl::Continue(render_request));
            };
            *running = Some(start_self_healing_edit_task(
                tui,
                root_id,
                request,
                prompt_context,
                coding_session,
            )?);
            Ok(LoopControl::Continue(RenderRequest::FORCE))
        }
        InteractiveAction::Fork => {
            if running.is_some() {
                return Ok(LoopControl::Continue(render_request));
            }
            let Some(request) = fork_request else {
                return Ok(LoopControl::Continue(render_request));
            };
            start_fork_task(
                tui,
                root_id,
                request,
                prompt_context,
                running,
                coding_session,
            )?;
            Ok(LoopControl::Continue(RenderRequest::FORCE))
        }
    }
}
