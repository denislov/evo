use crate::events::CodingAgentProductEvent;
use crate::kernel::operation::OperationKind;
use crate::runtime::client::connection::{CodingAgentContextSnapshot, CodingAgentOperationStatus};
use crate::runtime::client::context_fold::{ProductContextPendingState, fold_product_context};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum UiOperationStatus {
    Running,
    Completed,
    Failed,
    Aborted,
    Recovered,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct UiOperationProjection {
    pub(crate) operation_id: String,
    pub(crate) kind: String,
    pub(crate) parent_operation_id: Option<String>,
    pub(crate) root_operation_id: Option<String>,
    pub(crate) status: UiOperationStatus,
    pub(crate) started_sequence: u64,
    pub(crate) updated_sequence: u64,
    pub(crate) diagnostics: Vec<String>,
    pub(crate) failure: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct UiFileChangeProjection {
    pub(crate) path: String,
    pub(crate) mutation_kind: String,
    pub(crate) source: String,
    pub(crate) operation_id: String,
    pub(crate) tool_call_id: Option<String>,
    pub(crate) session_id: Option<String>,
    pub(crate) turn_id: Option<String>,
    pub(crate) updated_sequence: u64,
    pub(crate) before_revision: Option<String>,
    pub(crate) after_revision: String,
    pub(crate) after_exists: bool,
    pub(crate) first_changed_line: Option<usize>,
    pub(crate) added_lines: Option<usize>,
    pub(crate) removed_lines: Option<usize>,
    pub(crate) diff: Option<String>,
    pub(crate) hunks: Vec<crate::runtime::client::connection::CodingAgentHunkChangeSnapshot>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct UiDelegationProjection {
    pub(crate) tool_call_id: String,
    pub(crate) child_operation_id: Option<String>,
    pub(crate) target_kind: String,
    pub(crate) target_id: String,
    pub(crate) task: String,
    pub(crate) status: String,
    pub(crate) updated_sequence: u64,
    pub(crate) summary: Option<String>,
    pub(crate) failure: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct UiTurnUsageProjection {
    pub(crate) turn_id: String,
    pub(crate) input: u32,
    pub(crate) output: u32,
    pub(crate) cache_read: u32,
    pub(crate) cache_write: u32,
    pub(crate) context_tokens: Option<u32>,
    pub(crate) cost: Option<f64>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct UiUsageProjection {
    pub(crate) input: u64,
    pub(crate) output: u64,
    pub(crate) cache_read: u64,
    pub(crate) cache_write: u64,
    pub(crate) cost: Option<f64>,
    pub(crate) latest_turn: Option<UiTurnUsageProjection>,
    pub(crate) model_id: Option<String>,
    pub(crate) context_window: Option<u32>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct UiContextProjection {
    pub(crate) operations: Vec<UiOperationProjection>,
    pub(crate) changes: Vec<UiFileChangeProjection>,
    pub(crate) delegations: Vec<UiDelegationProjection>,
    pub(crate) usage: UiUsageProjection,
    context_pending: ProductContextPendingState,
}

impl UiContextProjection {
    pub(crate) fn apply_product_event(
        &mut self,
        event: &CodingAgentProductEvent,
        operation_kind: Option<OperationKind>,
    ) {
        let mut context: CodingAgentContextSnapshot = self.clone().into();
        let _ = fold_product_context(
            &mut context,
            &mut self.context_pending,
            event,
            operation_kind.map(OperationKind::as_str),
        );
        self.replace_product_context(context);
    }

    fn replace_product_context(&mut self, context: CodingAgentContextSnapshot) {
        self.operations = context
            .operations
            .into_iter()
            .map(|operation| UiOperationProjection {
                operation_id: operation.operation_id,
                kind: operation.kind,
                parent_operation_id: operation.parent_operation_id,
                root_operation_id: operation.root_operation_id,
                status: ui_operation_status(operation.status),
                started_sequence: operation.started_sequence,
                updated_sequence: operation.updated_sequence,
                diagnostics: operation.diagnostics,
                failure: operation.failure,
            })
            .collect();
        self.delegations = context
            .delegations
            .into_iter()
            .map(|delegation| UiDelegationProjection {
                tool_call_id: delegation.tool_call_id,
                child_operation_id: delegation.child_operation_id,
                target_kind: delegation.target_kind,
                target_id: delegation.target_id,
                task: delegation.task,
                status: delegation.status,
                updated_sequence: delegation.updated_sequence,
                summary: delegation.summary,
                failure: delegation.failure,
            })
            .collect();
        self.usage = UiUsageProjection {
            input: context.usage.input,
            output: context.usage.output,
            cache_read: context.usage.cache_read,
            cache_write: context.usage.cache_write,
            cost: context.usage.cost,
            latest_turn: context
                .usage
                .latest_turn
                .map(|usage| UiTurnUsageProjection {
                    turn_id: usage.turn_id,
                    input: usage.input,
                    output: usage.output,
                    cache_read: usage.cache_read,
                    cache_write: usage.cache_write,
                    context_tokens: usage.context_tokens,
                    cost: usage.cost,
                }),
            model_id: context.usage.model_id,
            context_window: context.usage.context_window,
        };
    }

    pub(crate) fn replace_review_changes(&mut self, changes: Vec<UiFileChangeProjection>) {
        self.changes = changes;
    }
}

const fn ui_operation_status(status: CodingAgentOperationStatus) -> UiOperationStatus {
    match status {
        CodingAgentOperationStatus::Running => UiOperationStatus::Running,
        CodingAgentOperationStatus::Completed => UiOperationStatus::Completed,
        CodingAgentOperationStatus::Failed => UiOperationStatus::Failed,
        CodingAgentOperationStatus::Aborted => UiOperationStatus::Aborted,
        CodingAgentOperationStatus::Recovered => UiOperationStatus::Recovered,
    }
}
