use tui::api::render::Tui;
use tui::api::terminal::Terminal;

use crate::interactive::TranscriptItem;
use crate::interactive::app::PromptContext;
use crate::interactive::error::CliError;
use crate::interactive::prompt_task::PromptTask;
use crate::interactive::root::{
    InteractiveStatus, PendingAgentInvocationRequest, PendingAgentTeamRequest,
    PendingBranchSummaryRequest, PendingDelegationConfirmationCommand,
    PendingDelegationConfirmationSelection, PendingForkRequest, PendingMergeReviewRequest,
    PendingSelfHealingEditRequest,
};
use crate::interactive::session_actions::SessionChoiceKind;
use coding_agent::api::operation::{
    BranchSummaryReusePolicy, CodingAgentOperation, PromptInvocation,
    SelfHealingEditModelRepairOptions, SelfHealingEditRequest,
};
use coding_agent::api::runtime::CodingAgentSession;

use super::{root_mut, set_terminal_progress};

pub(super) fn handle_delegation_confirmation_command<T: Terminal>(
    tui: &mut Tui<T>,
    root_id: usize,
    command: PendingDelegationConfirmationCommand,
    prompt_context: &PromptContext,
    running: &mut Option<PromptTask>,
    coding_session: &mut Option<CodingAgentSession>,
) -> Result<(), CliError> {
    match command {
        PendingDelegationConfirmationCommand::List => {
            show_pending_delegation_confirmations(tui, root_id, coding_session.as_ref())
        }
        PendingDelegationConfirmationCommand::Approve { selection } => {
            start_delegation_approval_task(
                tui,
                root_id,
                selection,
                prompt_context,
                running,
                coding_session,
            )
        }
        PendingDelegationConfirmationCommand::Reject { selection, reason } => {
            reject_pending_delegation_confirmation(
                tui,
                root_id,
                selection,
                reason,
                prompt_context,
                running,
                coding_session,
            )
        }
    }
}

fn show_pending_delegation_confirmations<T: Terminal>(
    tui: &mut Tui<T>,
    root_id: usize,
    coding_session: Option<&CodingAgentSession>,
) -> Result<(), CliError> {
    let Some(session) = coding_session else {
        root_mut(tui, root_id)?
            .transcript
            .push(TranscriptItem::system("No active coding session."));
        return Ok(());
    };
    let pending = session.pending_delegation_confirmations();
    if pending.is_empty() {
        root_mut(tui, root_id)?
            .transcript
            .push(TranscriptItem::system(
                "No pending delegation confirmations.",
            ));
        return Ok(());
    }
    root_mut(tui, root_id)?.open_delegation_confirmation_menu(pending);
    Ok(())
}

fn start_delegation_approval_task<T: Terminal>(
    tui: &mut Tui<T>,
    root_id: usize,
    selection: PendingDelegationConfirmationSelection,
    prompt_context: &PromptContext,
    running: &mut Option<PromptTask>,
    coding_session: &mut Option<CodingAgentSession>,
) -> Result<(), CliError> {
    let Some(session) = coding_session.as_ref() else {
        root_mut(tui, root_id)?
            .transcript
            .push(TranscriptItem::system("No active coding session."));
        return Ok(());
    };
    let (operation_id, tool_call_id) =
        match resolve_pending_delegation_confirmation(session, &selection) {
            Ok(resolved) => resolved,
            Err(message) => {
                root_mut(tui, root_id)?
                    .transcript
                    .push(TranscriptItem::system(message));
                return Ok(());
            }
        };

    let session = coding_session
        .take()
        .expect("coding session was checked before starting delegation approval");
    {
        let root = root_mut(tui, root_id)?;
        root.transcript.push(TranscriptItem::system(format!(
            "Approving delegation: {operation_id} {tool_call_id}"
        )));
        root.set_status(InteractiveStatus::Running);
    }
    *running = Some(PromptTask::spawn_delegation_approval(
        session,
        operation_id,
        tool_call_id,
    )?);
    if prompt_context.show_progress() {
        set_terminal_progress(tui, true)?;
    }
    Ok(())
}

pub(super) fn start_tree_label_task<T: Terminal>(
    tui: &mut Tui<T>,
    root_id: usize,
    entry_id: String,
    label: Option<String>,
    prompt_context: &PromptContext,
    running: &mut Option<PromptTask>,
    coding_session: &mut Option<CodingAgentSession>,
) -> Result<(), CliError> {
    let is_rust_native = root_mut(tui, root_id)?
        .active_session
        .as_ref()
        .is_some_and(|choice| choice.kind == SessionChoiceKind::Persistent);
    if !is_rust_native || coding_session.is_none() {
        root_mut(tui, root_id)?
            .transcript
            .push(TranscriptItem::system(
                "No active Rust-native session for tree label changes.",
            ));
        return Ok(());
    }
    let session = coding_session
        .take()
        .expect("coding session was checked before starting tree label mutation");
    root_mut(tui, root_id)?.set_status(InteractiveStatus::Running);
    *running = Some(PromptTask::spawn_session_tree_label(
        session, entry_id, label,
    )?);
    if prompt_context.show_progress() {
        set_terminal_progress(tui, true)?;
    }
    Ok(())
}

pub(super) fn start_merge_review_task<T: Terminal>(
    tui: &mut Tui<T>,
    root_id: usize,
    request: PendingMergeReviewRequest,
    prompt_context: &PromptContext,
    running: &mut Option<PromptTask>,
    coding_session: &mut Option<CodingAgentSession>,
) -> Result<(), CliError> {
    let Some(session) = coding_session.take() else {
        root_mut(tui, root_id)?
            .transcript
            .push(TranscriptItem::system("No active coding session."));
        return Ok(());
    };
    let operation = match request {
        PendingMergeReviewRequest::List => CodingAgentOperation::ListMergeProposals,
        PendingMergeReviewRequest::Merge(worktree_id) => {
            CodingAgentOperation::MergeChildWorktree { worktree_id }
        }
        PendingMergeReviewRequest::Discard(worktree_id) => {
            CodingAgentOperation::DiscardChildWorktree { worktree_id }
        }
    };
    root_mut(tui, root_id)?.set_status(InteractiveStatus::Running);
    *running = Some(PromptTask::spawn_merge_review(session, operation)?);
    if prompt_context.show_progress() {
        set_terminal_progress(tui, true)?;
    }
    Ok(())
}

fn reject_pending_delegation_confirmation<T: Terminal>(
    tui: &mut Tui<T>,
    root_id: usize,
    selection: PendingDelegationConfirmationSelection,
    reason: Option<String>,
    prompt_context: &PromptContext,
    running: &mut Option<PromptTask>,
    coding_session: &mut Option<CodingAgentSession>,
) -> Result<(), CliError> {
    let Some(session) = coding_session.as_ref() else {
        root_mut(tui, root_id)?
            .transcript
            .push(TranscriptItem::system("No active coding session."));
        return Ok(());
    };
    let (operation_id, tool_call_id) =
        match resolve_pending_delegation_confirmation(session, &selection) {
            Ok(resolved) => resolved,
            Err(message) => {
                root_mut(tui, root_id)?
                    .transcript
                    .push(TranscriptItem::system(message));
                return Ok(());
            }
        };

    let session = coding_session
        .take()
        .expect("coding session was checked before starting delegation rejection");
    root_mut(tui, root_id)?.set_status(InteractiveStatus::Running);
    *running = Some(PromptTask::spawn_delegation_rejection(
        session,
        operation_id,
        tool_call_id,
        reason.unwrap_or_else(|| "delegation rejected by user".to_string()),
    )?);
    if prompt_context.show_progress() {
        set_terminal_progress(tui, true)?;
    }
    Ok(())
}

fn resolve_pending_delegation_confirmation(
    session: &CodingAgentSession,
    selection: &PendingDelegationConfirmationSelection,
) -> Result<(String, String), String> {
    let pending = session.pending_delegation_confirmations();
    if pending.is_empty() {
        return Err("No pending delegation confirmations.".to_string());
    }
    if let Some(operation_id) = selection.operation_id.as_deref() {
        return pending
            .iter()
            .find(|pending| {
                pending.operation_id == operation_id
                    && pending.tool_call_id == selection.tool_call_id
            })
            .map(|pending| (pending.operation_id.clone(), pending.tool_call_id.clone()))
            .ok_or_else(|| {
                format!(
                    "Pending delegation confirmation not found: operation_id={operation_id}, tool_call_id={}",
                    selection.tool_call_id
                )
            });
    }

    let matches = pending
        .iter()
        .filter(|pending| pending.tool_call_id == selection.tool_call_id)
        .collect::<Vec<_>>();
    match matches.as_slice() {
        [pending] => Ok((pending.operation_id.clone(), pending.tool_call_id.clone())),
        [] => Err(format!(
            "Pending delegation confirmation not found: tool_call_id={}",
            selection.tool_call_id
        )),
        _ => Err(format!(
            "Multiple pending delegation confirmations match tool_call_id={}; include the operation id.",
            selection.tool_call_id
        )),
    }
}

pub(super) fn start_prompt_task<T: Terminal>(
    tui: &mut Tui<T>,
    root_id: usize,
    prompt: String,
    resource_invocation: Option<PromptInvocation>,
    prompt_context: &PromptContext,
    coding_session: &mut Option<CodingAgentSession>,
) -> Result<PromptTask, CliError> {
    let (operation, task_prompt) = match resource_invocation {
        Some(invocation) => (
            prompt_context.resource_prompt_operation(invocation),
            String::new(),
        ),
        None => {
            let prepared = prompt_context.prepare_prompt(&prompt)?;
            let task_prompt = prepared.display_text().to_string();
            (
                prompt_context.prepared_prompt_operation(prepared),
                task_prompt,
            )
        }
    };

    {
        let root = root_mut(tui, root_id)?;
        root.push_user(prompt.clone());
        root.set_status(InteractiveStatus::Running);
    }

    let bootstrap = prompt_context.session_bootstrap();
    let existing_session = coding_session.take();
    let task = PromptTask::spawn_prompt(operation, task_prompt, bootstrap, existing_session)?;
    if prompt_context.show_progress() {
        set_terminal_progress(tui, true)?;
    }
    Ok(task)
}

pub(super) fn start_agent_invocation_task<T: Terminal>(
    tui: &mut Tui<T>,
    root_id: usize,
    request: PendingAgentInvocationRequest,
    prompt_context: &PromptContext,
    coding_session: &mut Option<CodingAgentSession>,
) -> Result<PromptTask, CliError> {
    {
        let root = root_mut(tui, root_id)?;
        root.push_user(format!("/agent:{} {}", request.profile_id, request.task));
        root.set_status(InteractiveStatus::Running);
    }

    let operation = prompt_context.agent_invocation_operation(request.profile_id, request.task);
    let bootstrap = prompt_context.session_bootstrap();
    let task = PromptTask::spawn_agent_invocation(operation, bootstrap, coding_session.take())?;
    if prompt_context.show_progress() {
        set_terminal_progress(tui, true)?;
    }
    Ok(task)
}

pub(super) fn start_agent_team_task<T: Terminal>(
    tui: &mut Tui<T>,
    root_id: usize,
    request: PendingAgentTeamRequest,
    prompt_context: &PromptContext,
    coding_session: &mut Option<CodingAgentSession>,
) -> Result<PromptTask, CliError> {
    {
        let root = root_mut(tui, root_id)?;
        root.push_user(format!("/team:{} {}", request.team_id, request.task));
        root.set_status(InteractiveStatus::Running);
    }

    let operation = prompt_context.team_invocation_operation(request.team_id, request.task);
    let bootstrap = prompt_context.session_bootstrap();
    let task = PromptTask::spawn_agent_team(operation, bootstrap, coding_session.take())?;
    if prompt_context.show_progress() {
        set_terminal_progress(tui, true)?;
    }
    Ok(task)
}

fn interactive_self_healing_model_repair_options(
    prompt_context: &PromptContext,
    max_attempts: usize,
) -> SelfHealingEditModelRepairOptions {
    prompt_context.model_repair_options(max_attempts)
}

pub(super) fn start_self_healing_edit_task<T: Terminal>(
    tui: &mut Tui<T>,
    root_id: usize,
    request: PendingSelfHealingEditRequest,
    prompt_context: &PromptContext,
    coding_session: &mut Option<CodingAgentSession>,
) -> Result<PromptTask, CliError> {
    {
        let root = root_mut(tui, root_id)?;
        root.transcript.push(TranscriptItem::system(format!(
            "Applying self-healing edit: {}",
            request.path
        )));
        root.set_status(InteractiveStatus::Running);
    }

    let mut edit_request = SelfHealingEditRequest::new(request.path, request.replacements);
    if let Some(command) = request.check_command {
        edit_request = edit_request.with_check_command(command);
    }
    if let Some(model_repair) = request.model_repair {
        edit_request =
            edit_request.with_model_repair(interactive_self_healing_model_repair_options(
                prompt_context,
                model_repair.max_attempts,
            ));
    }
    let operation = prompt_context.self_healing_edit_operation(edit_request);
    let bootstrap = prompt_context.session_bootstrap();
    let task = PromptTask::spawn_self_healing_edit(operation, bootstrap, coding_session.take())?;
    if prompt_context.show_progress() {
        set_terminal_progress(tui, true)?;
    }
    Ok(task)
}

pub(super) fn start_compact_task<T: Terminal>(
    tui: &mut Tui<T>,
    root_id: usize,
    custom_instructions: Option<String>,
    prompt_context: &PromptContext,
    coding_session: &mut Option<CodingAgentSession>,
) -> Result<PromptTask, CliError> {
    let use_rust_native = {
        let root = root_mut(tui, root_id)?;
        matches!(
            root.active_session.as_ref().map(|choice| choice.kind),
            Some(SessionChoiceKind::Persistent)
        )
    };

    {
        let root = root_mut(tui, root_id)?;
        root.transcript
            .push(TranscriptItem::system("Compacting session..."));
        root.set_status(InteractiveStatus::Running);
    }

    if !use_rust_native {
        return Err(CliError::UnsupportedMode(
            "manual compaction requires an active Rust-native session".into(),
        ));
    }
    let operation = prompt_context.compact_operation(custom_instructions);
    let bootstrap = prompt_context.session_bootstrap();
    let task = PromptTask::spawn_compact(operation, bootstrap, coding_session.take())?;
    if prompt_context.show_progress() {
        set_terminal_progress(tui, true)?;
    }
    Ok(task)
}

pub(super) fn start_fork_task<T: Terminal>(
    tui: &mut Tui<T>,
    root_id: usize,
    request: PendingForkRequest,
    prompt_context: &PromptContext,
    running: &mut Option<PromptTask>,
    coding_session: &mut Option<CodingAgentSession>,
) -> Result<(), CliError> {
    if coding_session.is_none() {
        root_mut(tui, root_id)?
            .transcript
            .push(TranscriptItem::system("No active coding session."));
        return Ok(());
    }
    root_mut(tui, root_id)?.set_status(InteractiveStatus::Running);
    let operation = prompt_context.fork_session_operation(request.target_leaf_id);
    let bootstrap = prompt_context.session_bootstrap();
    *running = Some(PromptTask::spawn_fork_session(
        operation,
        bootstrap,
        coding_session.take(),
        Some("Forked to new session".to_string()),
    )?);
    if prompt_context.show_progress() {
        set_terminal_progress(tui, true)?;
    }
    Ok(())
}

pub(super) fn start_tree_navigation_fork_task<T: Terminal>(
    tui: &mut Tui<T>,
    root_id: usize,
    target_leaf_id: String,
    prompt_context: &PromptContext,
    running: &mut Option<PromptTask>,
    coding_session: &mut Option<CodingAgentSession>,
) -> Result<(), CliError> {
    root_mut(tui, root_id)?.set_status(InteractiveStatus::Running);
    let operation = prompt_context.fork_session_operation(Some(target_leaf_id));
    let bootstrap = prompt_context.session_bootstrap();
    *running = Some(PromptTask::spawn_fork_session(
        operation,
        bootstrap,
        coding_session.take(),
        Some("Navigated to selected point".to_string()),
    )?);
    if prompt_context.show_progress() {
        set_terminal_progress(tui, true)?;
    }
    Ok(())
}

pub(super) fn start_branch_summary_navigation_task<T: Terminal>(
    tui: &mut Tui<T>,
    root_id: usize,
    source_leaf_id: String,
    target_leaf_id: String,
    prompt_context: &PromptContext,
    coding_session: &mut Option<CodingAgentSession>,
) -> Result<PromptTask, CliError> {
    {
        let root = root_mut(tui, root_id)?;
        root.transcript.push(TranscriptItem::system(
            "Summarizing branch before navigation...",
        ));
        root.set_status(InteractiveStatus::Running);
    }

    let operation = prompt_context.branch_summary_operation(
        source_leaf_id,
        target_leaf_id.clone(),
        None,
        BranchSummaryReusePolicy::ReuseExisting,
    );
    let bootstrap = prompt_context.session_bootstrap();
    let task = PromptTask::spawn_branch_summary_navigation(
        operation,
        bootstrap,
        coding_session.take(),
        target_leaf_id,
    )?;
    if prompt_context.show_progress() {
        set_terminal_progress(tui, true)?;
    }
    Ok(task)
}

pub(super) fn start_branch_summary_task<T: Terminal>(
    tui: &mut Tui<T>,
    root_id: usize,
    request: PendingBranchSummaryRequest,
    prompt_context: &PromptContext,
    coding_session: &mut Option<CodingAgentSession>,
) -> Result<PromptTask, CliError> {
    let use_rust_native = {
        let root = root_mut(tui, root_id)?;
        matches!(
            root.active_session.as_ref().map(|choice| choice.kind),
            Some(SessionChoiceKind::Persistent)
        )
    };

    {
        let root = root_mut(tui, root_id)?;
        root.transcript
            .push(TranscriptItem::system("Summarizing branch..."));
        root.set_status(InteractiveStatus::Running);
    }

    if !use_rust_native {
        return Err(CliError::UnsupportedMode(
            "branch summary requires an active Rust-native session".into(),
        ));
    }
    let operation = prompt_context.branch_summary_operation(
        request.source_leaf_id,
        request.target_leaf_id,
        request.custom_instructions,
        BranchSummaryReusePolicy::AlwaysCreate,
    );
    let bootstrap = prompt_context.session_bootstrap();
    let task = PromptTask::spawn_branch_summary(operation, bootstrap, coding_session.take())?;
    if prompt_context.show_progress() {
        set_terminal_progress(tui, true)?;
    }
    Ok(task)
}
