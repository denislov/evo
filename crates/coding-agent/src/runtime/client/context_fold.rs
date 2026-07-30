use std::collections::HashMap;

use serde_json::Value;

use crate::events::{
    CodingAgentAgentProductEvent, CodingAgentDelegationProductEvent,
    CodingAgentDiagnosticProductEvent, CodingAgentMessageProductEvent, CodingAgentProductEvent,
    CodingAgentProductEventKind, CodingAgentProductEventProfileKind,
    CodingAgentProductEventTerminalStatus, CodingAgentTeamProductEvent,
    CodingAgentToolProductEvent, CodingAgentWorkflowProductEvent,
};
use crate::runtime::client::connection::{
    CodingAgentContextSnapshot, CodingAgentDelegationSnapshot, CodingAgentFileChangeSnapshot,
    CodingAgentOperationSnapshot, CodingAgentOperationStatus, CodingAgentTurnUsageSnapshot,
};

pub(crate) const MAX_CONTEXT_OPERATIONS: usize = 32;
pub(crate) const MAX_CONTEXT_CHANGES: usize = 64;
pub(crate) const MAX_CONTEXT_DELEGATIONS: usize = 32;
pub(crate) const MAX_CONTEXT_OPERATION_DIAGNOSTICS: usize = 4;
const MAX_PENDING_MUTATIONS: usize = 128;
const MAX_ID_BYTES: usize = 256;
const MAX_MESSAGE_BYTES: usize = 1024 * 1024;
const MAX_TOOL_BYTES: usize = 256 * 1024;
const MAX_DIAGNOSTIC_BYTES: usize = 32 * 1024;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct ProductContextPendingState {
    mutations: HashMap<String, ProductPendingMutation>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ProductPendingMutation {
    operation_id: String,
    tool_call_id: String,
    path: String,
    mutation_kind: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ProductContextFoldChange {
    Operations,
    Changes,
    Delegations,
    Usage,
}

pub(crate) fn fold_product_context(
    context: &mut CodingAgentContextSnapshot,
    pending: &mut ProductContextPendingState,
    event: &CodingAgentProductEvent,
    operation_kind: Option<&str>,
) -> Vec<ProductContextFoldChange> {
    let mut changes = Vec::with_capacity(4);
    if fold_operation(context, event, operation_kind) {
        changes.push(ProductContextFoldChange::Operations);
    }
    if fold_change(context, pending, event) {
        changes.push(ProductContextFoldChange::Changes);
    }
    if fold_delegation(context, event) {
        changes.push(ProductContextFoldChange::Delegations);
    }
    if fold_usage(context, event) {
        changes.push(ProductContextFoldChange::Usage);
    }
    changes
}

fn fold_operation(
    context: &mut CodingAgentContextSnapshot,
    event: &CodingAgentProductEvent,
    operation_kind: Option<&str>,
) -> bool {
    let Some(operation_id) = event.operation_id() else {
        return false;
    };
    let sequence = event.sequence();
    let inferred_kind = operation_kind
        .or_else(|| {
            event
                .terminal_operation()
                .map(|terminal| terminal.kind.as_str())
        })
        .unwrap_or_else(|| inferred_operation_kind(event.event()));
    let existing = context
        .operations
        .iter()
        .position(|operation| operation.operation_id == operation_id);
    let index = match existing {
        Some(index) => index,
        None => {
            let is_root_evidence = operation_kind.is_some()
                || event.terminal_operation().is_some()
                || is_root_start_event(event.event())
                || event.root_operation_id() == Some(operation_id);
            if !is_root_evidence {
                return false;
            }
            let operation = CodingAgentOperationSnapshot {
                operation_id: bounded_text(operation_id, MAX_ID_BYTES),
                kind: bounded_text(inferred_kind, MAX_ID_BYTES),
                parent_operation_id: event
                    .parent_operation_id()
                    .map(|value| bounded_text(value, MAX_ID_BYTES)),
                root_operation_id: event
                    .root_operation_id()
                    .map(|value| bounded_text(value, MAX_ID_BYTES)),
                status: CodingAgentOperationStatus::Running,
                started_sequence: sequence,
                updated_sequence: sequence,
                diagnostics: Vec::new(),
                failure: None,
            };
            let index = operation_insertion_index(&context.operations, &operation);
            context.operations.insert(index, operation);
            index
        }
    };

    let operation = &mut context.operations[index];
    operation.updated_sequence = sequence;
    if operation_kind.is_some() || event.terminal_operation().is_some() {
        operation.kind = bounded_text(inferred_kind, MAX_ID_BYTES);
    }
    if operation.parent_operation_id.is_none() {
        operation.parent_operation_id = event
            .parent_operation_id()
            .map(|value| bounded_text(value, MAX_ID_BYTES));
    }
    if operation.root_operation_id.is_none() {
        operation.root_operation_id = event
            .root_operation_id()
            .map(|value| bounded_text(value, MAX_ID_BYTES));
    }
    if let Some(terminal) = event.terminal_operation() {
        operation.status = terminal_status(terminal.status);
    }
    if let Some(failure) = event_failure(event.event()) {
        operation.failure = Some(bounded_text(&failure, MAX_DIAGNOSTIC_BYTES));
    }
    if let CodingAgentProductEventKind::Diagnostic(
        CodingAgentDiagnosticProductEvent::Diagnostic { diagnostic },
    ) = event.event()
    {
        operation
            .diagnostics
            .push(bounded_text(&diagnostic.summary, MAX_DIAGNOSTIC_BYTES));
        if operation.diagnostics.len() > MAX_CONTEXT_OPERATION_DIAGNOSTICS {
            operation.diagnostics.remove(0);
        }
    }
    if let Some(terminal) = event.terminal_operation()
        && context.operations[index].parent_operation_id.is_none()
    {
        let status = terminal_status(terminal.status);
        for descendant in context.operations.iter_mut().filter(|candidate| {
            candidate.operation_id != operation_id
                && candidate.root_operation_id.as_deref() == Some(operation_id)
                && candidate.status == CodingAgentOperationStatus::Running
        }) {
            descendant.status = status;
            descendant.updated_sequence = sequence;
        }
    }
    trim_context_operations(&mut context.operations);
    true
}

fn fold_change(
    context: &mut CodingAgentContextSnapshot,
    pending: &mut ProductContextPendingState,
    event: &CodingAgentProductEvent,
) -> bool {
    let next = match event.event() {
        CodingAgentProductEventKind::Tool(CodingAgentToolProductEvent::Started {
            operation_id,
            tool_call_id,
            name,
            arguments_json,
            ..
        }) if matches!(name.as_str(), "edit" | "write") => {
            if let Some(path) = mutation_path(arguments_json)
                && (pending.mutations.len() < MAX_PENDING_MUTATIONS
                    || pending.mutations.contains_key(tool_call_id))
            {
                pending.mutations.insert(
                    tool_call_id.clone(),
                    ProductPendingMutation {
                        operation_id: operation_id.clone(),
                        tool_call_id: tool_call_id.clone(),
                        path,
                        mutation_kind: name.clone(),
                    },
                );
            }
            return true;
        }
        CodingAgentProductEventKind::Tool(CodingAgentToolProductEvent::Completed {
            tool_call_id,
            name,
            ..
        }) if matches!(name.as_str(), "edit" | "write") => pending
            .mutations
            .remove(tool_call_id)
            .map(|pending| CodingAgentFileChangeSnapshot {
                path: pending.path,
                mutation_kind: pending.mutation_kind,
                operation_id: pending.operation_id,
                tool_call_id: Some(pending.tool_call_id),
                updated_sequence: event.sequence(),
                first_changed_line: None,
                added_lines: None,
                removed_lines: None,
                diff: None,
            }),
        CodingAgentProductEventKind::Tool(CodingAgentToolProductEvent::Failed {
            tool_call_id,
            name,
            ..
        }) if matches!(name.as_str(), "edit" | "write") => {
            pending.mutations.remove(tool_call_id);
            return true;
        }
        CodingAgentProductEventKind::Workflow(
            CodingAgentWorkflowProductEvent::SelfHealingEditCompleted {
                operation_id,
                path,
                first_changed_line,
                ..
            },
        ) => Some(CodingAgentFileChangeSnapshot {
            path: bounded_text(path, MAX_TOOL_BYTES),
            mutation_kind: "self_healing_edit".into(),
            operation_id: bounded_text(operation_id, MAX_ID_BYTES),
            tool_call_id: None,
            updated_sequence: event.sequence(),
            first_changed_line: *first_changed_line,
            added_lines: None,
            removed_lines: None,
            diff: None,
        }),
        _ => return false,
    };
    if let Some(next) = next {
        context.changes.retain(|current| current.path != next.path);
        context.changes.insert(0, next);
        context.changes.truncate(MAX_CONTEXT_CHANGES);
    }
    true
}

fn fold_delegation(
    context: &mut CodingAgentContextSnapshot,
    event: &CodingAgentProductEvent,
) -> bool {
    let CodingAgentProductEventKind::Delegation(delegation) = event.event() else {
        return false;
    };
    let (event_context, child_operation_id, status, summary, failure) = match delegation {
        CodingAgentDelegationProductEvent::Requested { context } => {
            (context, None, "requested", None, None)
        }
        CodingAgentDelegationProductEvent::Rejected { context, reason } => (
            context,
            None,
            "rejected",
            None,
            Some(bounded_text(reason, MAX_DIAGNOSTIC_BYTES)),
        ),
        CodingAgentDelegationProductEvent::Approved { context } => {
            (context, None, "approved", None, None)
        }
        CodingAgentDelegationProductEvent::ConfirmationRequired { context, reason } => (
            context,
            None,
            "confirmation_required",
            None,
            Some(bounded_text(reason, MAX_DIAGNOSTIC_BYTES)),
        ),
        CodingAgentDelegationProductEvent::Started {
            context,
            child_operation_id,
        } => (
            context,
            Some(bounded_text(child_operation_id, MAX_ID_BYTES)),
            "running",
            None,
            None,
        ),
        CodingAgentDelegationProductEvent::Completed {
            context,
            child_operation_id,
            final_text,
        } => (
            context,
            Some(bounded_text(child_operation_id, MAX_ID_BYTES)),
            "completed",
            Some(bounded_text(final_text, MAX_MESSAGE_BYTES)),
            None,
        ),
        CodingAgentDelegationProductEvent::Failed {
            context,
            child_operation_id,
            error,
        } => (
            context,
            Some(bounded_text(child_operation_id, MAX_ID_BYTES)),
            "failed",
            None,
            Some(bounded_text(&error.summary, MAX_DIAGNOSTIC_BYTES)),
        ),
    };
    let target_kind = match event_context.target_kind {
        CodingAgentProductEventProfileKind::Agent => "agent",
        CodingAgentProductEventProfileKind::Team => "team",
    };
    let next = CodingAgentDelegationSnapshot {
        tool_call_id: bounded_text(&event_context.tool_call_id, MAX_ID_BYTES),
        child_operation_id,
        target_kind: target_kind.into(),
        target_id: bounded_text(&event_context.target_id, MAX_ID_BYTES),
        task: bounded_text(&event_context.task, MAX_MESSAGE_BYTES),
        status: status.into(),
        updated_sequence: event.sequence(),
        summary,
        failure,
    };
    context
        .delegations
        .retain(|current| current.tool_call_id != next.tool_call_id);
    context.delegations.insert(0, next);
    context.delegations.truncate(MAX_CONTEXT_DELEGATIONS);
    true
}

fn fold_usage(context: &mut CodingAgentContextSnapshot, event: &CodingAgentProductEvent) -> bool {
    if let CodingAgentProductEventKind::Agent(
        CodingAgentAgentProductEvent::ProviderRequestStarted {
            model,
            context_window,
            ..
        },
    ) = event.event()
    {
        context.usage.model_id = Some(bounded_text(model, MAX_ID_BYTES));
        context.usage.context_window = *context_window;
        return true;
    }
    let CodingAgentProductEventKind::Message(CodingAgentMessageProductEvent::Completed {
        turn_id,
        usage,
        ..
    }) = event.event()
    else {
        return false;
    };
    let projected = &mut context.usage;
    projected.input = projected.input.saturating_add(usage.input as u64);
    projected.output = projected.output.saturating_add(usage.output as u64);
    projected.cache_read = projected.cache_read.saturating_add(usage.cache_read as u64);
    projected.cache_write = projected
        .cache_write
        .saturating_add(usage.cache_write as u64);
    let cost =
        usage.input_cost + usage.output_cost + usage.cache_read_cost + usage.cache_write_cost;
    let priced = (usage.cost_known && cost.is_finite() && cost > 0.0).then_some(cost);
    if let Some(cost) = priced {
        projected.cost = Some(projected.cost.unwrap_or(0.0) + cost);
    }
    let component_total = usage
        .input
        .saturating_add(usage.output)
        .saturating_add(usage.cache_read)
        .saturating_add(usage.cache_write);
    projected.latest_turn = Some(CodingAgentTurnUsageSnapshot {
        turn_id: bounded_text(turn_id, MAX_ID_BYTES),
        input: usage.input,
        output: usage.output,
        cache_read: usage.cache_read,
        cache_write: usage.cache_write,
        context_tokens: Some(if usage.total_tokens > 0 {
            usage.total_tokens
        } else {
            component_total
        }),
        cost: priced,
    });
    true
}

fn operation_insertion_index(
    operations: &[CodingAgentOperationSnapshot],
    operation: &CodingAgentOperationSnapshot,
) -> usize {
    let Some(parent_operation_id) = operation.parent_operation_id.as_deref() else {
        return 0;
    };
    let root_operation_id = operation
        .root_operation_id
        .as_deref()
        .unwrap_or(parent_operation_id);
    operations
        .iter()
        .rposition(|current| {
            current.operation_id == root_operation_id
                || current.root_operation_id.as_deref() == Some(root_operation_id)
        })
        .map_or(0, |index| index + 1)
}

pub(crate) fn trim_context_operations(operations: &mut Vec<CodingAgentOperationSnapshot>) {
    while operations.len() > MAX_CONTEXT_OPERATIONS {
        let remove_index = operations
            .iter()
            .rposition(|operation| operation.status != CodingAgentOperationStatus::Running)
            .unwrap_or(operations.len() - 1);
        operations.remove(remove_index);
    }
}

fn mutation_path(arguments_json: &str) -> Option<String> {
    let value: Value = serde_json::from_str(arguments_json).ok()?;
    ["path", "file_path", "filePath"]
        .into_iter()
        .find_map(|key| value.get(key).and_then(Value::as_str))
        .filter(|path| !path.trim().is_empty())
        .map(|path| bounded_text(path, MAX_TOOL_BYTES))
}

fn terminal_status(status: CodingAgentProductEventTerminalStatus) -> CodingAgentOperationStatus {
    match status {
        CodingAgentProductEventTerminalStatus::Completed => CodingAgentOperationStatus::Completed,
        CodingAgentProductEventTerminalStatus::Failed => CodingAgentOperationStatus::Failed,
        CodingAgentProductEventTerminalStatus::Aborted => CodingAgentOperationStatus::Aborted,
        CodingAgentProductEventTerminalStatus::Recovered => CodingAgentOperationStatus::Recovered,
    }
}

fn is_root_start_event(event: &CodingAgentProductEventKind) -> bool {
    matches!(
        event,
        CodingAgentProductEventKind::Agent(CodingAgentAgentProductEvent::InvocationStarted { .. })
            | CodingAgentProductEventKind::Team(CodingAgentTeamProductEvent::Started { .. })
            | CodingAgentProductEventKind::Workflow(
                CodingAgentWorkflowProductEvent::PromptStarted { .. }
                    | CodingAgentWorkflowProductEvent::SelfHealingEditStarted { .. }
            )
    )
}

fn inferred_operation_kind(event: &CodingAgentProductEventKind) -> &'static str {
    match event {
        CodingAgentProductEventKind::Agent(
            CodingAgentAgentProductEvent::InvocationStarted { .. }
            | CodingAgentAgentProductEvent::InvocationCompleted { .. }
            | CodingAgentAgentProductEvent::InvocationFailed { .. }
            | CodingAgentAgentProductEvent::InvocationAborted { .. },
        ) => "agent_invocation",
        CodingAgentProductEventKind::Agent(
            CodingAgentAgentProductEvent::TurnStarted { .. }
            | CodingAgentAgentProductEvent::ProviderRequestStarted { .. },
        )
        | CodingAgentProductEventKind::Message(_)
        | CodingAgentProductEventKind::Workflow(
            CodingAgentWorkflowProductEvent::PromptStarted { .. }
            | CodingAgentWorkflowProductEvent::PromptCompleted { .. }
            | CodingAgentWorkflowProductEvent::PromptFailed { .. }
            | CodingAgentWorkflowProductEvent::PromptAborted { .. },
        ) => "prompt",
        CodingAgentProductEventKind::Team(_) => "agent_team",
        CodingAgentProductEventKind::Workflow(
            CodingAgentWorkflowProductEvent::SelfHealingEditStarted { .. }
            | CodingAgentWorkflowProductEvent::SelfHealingEditRepairAttempted { .. }
            | CodingAgentWorkflowProductEvent::SelfHealingEditCompleted { .. }
            | CodingAgentWorkflowProductEvent::SelfHealingEditFailed { .. }
            | CodingAgentWorkflowProductEvent::SelfHealingEditAborted { .. },
        ) => "self_healing_edit",
        CodingAgentProductEventKind::Workflow(
            CodingAgentWorkflowProductEvent::OperationRecoveryPending { .. }
            | CodingAgentWorkflowProductEvent::OperationRecoveryResolved { .. }
            | CodingAgentWorkflowProductEvent::OperationRecovered { .. },
        ) => "recovery",
        CodingAgentProductEventKind::Delegation(_) => "delegation",
        CodingAgentProductEventKind::Tool(_) => "tool",
        _ => event.family().as_str(),
    }
}

fn event_failure(event: &CodingAgentProductEventKind) -> Option<String> {
    match event {
        CodingAgentProductEventKind::Agent(CodingAgentAgentProductEvent::InvocationFailed {
            error,
            ..
        })
        | CodingAgentProductEventKind::Team(CodingAgentTeamProductEvent::Failed {
            error, ..
        })
        | CodingAgentProductEventKind::Workflow(
            CodingAgentWorkflowProductEvent::SelfHealingEditFailed { error, .. }
            | CodingAgentWorkflowProductEvent::PromptFailed { error, .. },
        ) => Some(error.summary.clone()),
        CodingAgentProductEventKind::Agent(CodingAgentAgentProductEvent::InvocationAborted {
            reason,
            ..
        })
        | CodingAgentProductEventKind::Team(CodingAgentTeamProductEvent::Aborted {
            reason, ..
        })
        | CodingAgentProductEventKind::Workflow(
            CodingAgentWorkflowProductEvent::SelfHealingEditAborted { reason, .. }
            | CodingAgentWorkflowProductEvent::PromptAborted { reason, .. },
        ) => Some(reason.clone()),
        _ => None,
    }
}

fn bounded_text(value: &str, max_bytes: usize) -> String {
    if value.len() <= max_bytes {
        return value.to_owned();
    }
    let mut end = max_bytes;
    while end > 0 && !value.is_char_boundary(end) {
        end -= 1;
    }
    value[..end].to_owned()
}
