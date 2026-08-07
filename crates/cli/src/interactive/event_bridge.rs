use std::collections::{HashMap, VecDeque};
use std::time::{Duration, Instant};

use coding_agent::api::client::{
    CodingAgentClientProjection, CodingAgentClientProjectionApply, CodingAgentContextSnapshot,
    CodingAgentOperationSnapshot, CodingAgentOperationStatus, CodingAgentSnapshot,
};
use coding_agent::api::event::{
    CodingAgentAgentProductEvent, CodingAgentDelegationProductEvent, CodingAgentImageContent,
    CodingAgentMessageProductEvent, CodingAgentProductEvent as ProductEvent,
    CodingAgentProductEventKind, CodingAgentProductEventProfileKind, CodingAgentProductEventUsage,
    CodingAgentRuntimeProductEvent, CodingAgentSessionProductEvent, CodingAgentToolProductEvent,
    CodingAgentWorkflowProductEvent,
};
use coding_agent::api::operation::PendingDelegationConfirmation;
use coding_agent::api::view::{
    CodingAgentCapabilities, CodingAgentSessionView, ProfileId, ProfileKind,
};

mod delegation;

use delegation::{
    delegation_block_from_tool_result, delegation_block_from_tool_start,
    delegation_tool_kind_label, is_delegation_tool, parse_tool_arguments, profile_kind_label,
};

pub(super) const MAX_CHILD_CONVERSATIONS: usize = 32;
const MAX_CHILD_UI_EVENTS: usize = 2_048;

#[derive(Debug, Clone, PartialEq)]
pub enum UiEvent {
    TurnStarted,
    AssistantDelta {
        text: String,
    },
    ThinkingDelta {
        text: String,
    },
    AssistantDone,
    AssistantImages {
        images: Vec<CodingAgentImageContent>,
    },
    ToolStarted {
        call_id: String,
        name: String,
        args: serde_json::Value,
    },
    ToolFinished {
        call_id: String,
        result: String,
        is_error: bool,
    },
    ToolUpdated {
        call_id: String,
        result: String,
    },
    ToolAuthorizationRequired {
        request: coding_agent::api::authorization::ToolAuthorizationRequest,
    },
    ToolAuthorizationResolved {
        authorization_id: String,
    },
    AgentError {
        error: String,
    },
    SystemNotice {
        text: String,
    },
    DelegationBlock {
        call_id: String,
        target_kind: String,
        target_id: String,
        task: String,
        status: String,
        child_operation_id: Option<String>,
        summary: Option<String>,
        is_error: bool,
    },
    DelegationConfirmationRequired {
        pending: PendingDelegationConfirmation,
    },
    DelegationConfirmationResolved {
        operation_id: String,
        tool_call_id: String,
    },
    CompactionNotice {
        summary: String,
    },
    UsageUpdate {
        input: u32,
        output: u32,
        cache_read: u32,
        cache_write: u32,
        cost: f64,
        /// Estimated context tokens from the last assistant usage;
        /// `None` means unknown (e.g. right after compaction).
        context_tokens: Option<u32>,
    },
}

#[derive(Debug, Clone)]
pub(crate) struct UiProjection {
    bridge: CodingEventBridge,
    product: Option<CodingAgentClientProjection>,
    unbound_last_sequence: u64,
    resync_notified: bool,
    pending: Vec<UiEvent>,
    context: CodingAgentContextSnapshot,
    operation_timings: HashMap<String, UiOperationTiming>,
    child_pending: HashMap<String, VecDeque<UiEvent>>,
    child_order: VecDeque<String>,
    child_summaries: HashMap<String, ChildDelegationSummary>,
}

#[derive(Debug, Clone)]
struct ChildDelegationSummary {
    call_id: String,
    target_kind: String,
    target_id: String,
    task: String,
    child_operation_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct UiOperationTiming {
    first_seen: Instant,
    terminal_elapsed: Option<Duration>,
}

impl Default for UiProjection {
    fn default() -> Self {
        Self::new()
    }
}

impl UiProjection {
    pub(crate) fn new() -> Self {
        Self {
            bridge: CodingEventBridge::new(),
            product: None,
            unbound_last_sequence: 0,
            resync_notified: false,
            pending: Vec::new(),
            context: CodingAgentContextSnapshot::default(),
            operation_timings: HashMap::new(),
            child_pending: HashMap::new(),
            child_order: VecDeque::new(),
            child_summaries: HashMap::new(),
        }
    }

    pub(crate) fn from_snapshot(snapshot: CodingAgentSnapshot) -> Self {
        let product = CodingAgentClientProjection::new(snapshot)
            .expect("product-owned snapshot must satisfy the shared projection contract");
        let context = product.snapshot().context.clone();
        let mut projection = Self {
            bridge: CodingEventBridge::new(),
            product: Some(product),
            unbound_last_sequence: 0,
            resync_notified: false,
            pending: Vec::new(),
            context,
            operation_timings: HashMap::new(),
            child_pending: HashMap::new(),
            child_order: VecDeque::new(),
            child_summaries: HashMap::new(),
        };
        projection.replace_product_context(projection.context.clone());
        projection.hydrate_child_delegations();
        let mut pending_authorizations = projection
            .product
            .as_ref()
            .map(|product| product.snapshot().pending_authorizations.clone())
            .unwrap_or_default();
        pending_authorizations.sort_by(|left, right| {
            left.requested_at
                .cmp(&right.requested_at)
                .then_with(|| left.authorization_id.cmp(&right.authorization_id))
        });
        for request in pending_authorizations {
            let event = UiEvent::ToolAuthorizationRequired {
                request: request.clone(),
            };
            if let Some(conversation_id) =
                projection.conversation_operation_id(&request.operation_id)
            {
                projection.push_child_events(&conversation_id, vec![event]);
            } else {
                projection.pending.push(event);
            }
        }
        projection
    }

    pub(crate) fn apply_product_event(&mut self, event: &ProductEvent) {
        if let Some(product) = self.product.as_mut() {
            match product.apply(event) {
                CodingAgentClientProjectionApply::Applied(_) => {
                    let context = product.snapshot().context.clone();
                    self.replace_product_context(context);
                }
                CodingAgentClientProjectionApply::IgnoredDuplicate => return,
                CodingAgentClientProjectionApply::NeedsResync(issue) => {
                    if !self.resync_notified {
                        self.pending.push(UiEvent::SystemNotice {
                            text: format!(
                                "Interactive projection requires a fresh snapshot ({})",
                                issue.code
                            ),
                        });
                        self.resync_notified = true;
                    }
                    return;
                }
            }
        } else {
            if event.sequence() <= self.unbound_last_sequence {
                return;
            }
            self.unbound_last_sequence = event.sequence();
        }
        let ui_events = self.bridge.push_product_event(event);
        self.remember_child_summaries(&ui_events);
        if let Some(child_operation_id) = self.child_operation_id(event) {
            if ui_events
                .iter()
                .any(|event| matches!(event, UiEvent::ToolAuthorizationRequired { .. }))
                && let Some(summary) =
                    self.child_status_event(&child_operation_id, "waiting_permission")
            {
                self.pending.push(summary);
            }
            if ui_events
                .iter()
                .any(|event| matches!(event, UiEvent::ToolAuthorizationResolved { .. }))
                && let Some(summary) = self.child_status_event(&child_operation_id, "running")
            {
                self.pending.push(summary);
            }
            self.push_child_events(&child_operation_id, ui_events);
        } else {
            self.pending.extend(ui_events);
        }
    }

    pub(crate) fn drain(&mut self) -> Vec<UiEvent> {
        self.pending.drain(..).collect()
    }

    pub(crate) fn drain_children(&mut self) -> Vec<(String, Vec<UiEvent>)> {
        let operation_ids = self.child_order.iter().cloned().collect::<Vec<_>>();
        operation_ids
            .into_iter()
            .filter_map(|operation_id| {
                let events = self
                    .child_pending
                    .get_mut(&operation_id)?
                    .drain(..)
                    .collect::<Vec<_>>();
                (!events.is_empty()).then_some((operation_id, events))
            })
            .collect()
    }

    fn child_operation_id(&self, event: &ProductEvent) -> Option<String> {
        let operation_id = event.operation_id()?;
        if event.parent_operation_id().is_none() && !self.is_child_operation(operation_id) {
            return None;
        }
        self.conversation_operation_id(operation_id)
            .or_else(|| Some(operation_id.to_owned()))
    }

    fn conversation_operation_id(&self, operation_id: &str) -> Option<String> {
        let mut conversation_id = operation_id;
        while let Some(operation) = self
            .context
            .operations
            .iter()
            .find(|operation| operation.operation_id == conversation_id)
        {
            let Some(parent_id) = operation.parent_operation_id.as_deref() else {
                return (conversation_id != operation_id).then(|| conversation_id.to_owned());
            };
            let parent_is_root = self
                .context
                .operations
                .iter()
                .find(|operation| operation.operation_id == parent_id)
                .is_none_or(|parent| parent.parent_operation_id.is_none());
            if parent_is_root {
                return Some(conversation_id.to_owned());
            }
            conversation_id = parent_id;
        }
        self.is_child_operation(operation_id)
            .then(|| operation_id.to_owned())
    }

    fn is_child_operation(&self, operation_id: &str) -> bool {
        self.context.operations.iter().any(|operation| {
            operation.operation_id == operation_id && operation.parent_operation_id.is_some()
        })
    }

    fn push_child_events(&mut self, operation_id: &str, events: Vec<UiEvent>) {
        if events.is_empty() {
            return;
        }
        if !self.child_pending.contains_key(operation_id) {
            while self.child_order.len() >= MAX_CHILD_CONVERSATIONS {
                if let Some(evicted) = self.child_order.pop_front() {
                    self.child_pending.remove(&evicted);
                }
            }
            self.child_order.push_back(operation_id.to_owned());
        }
        let pending = self
            .child_pending
            .entry(operation_id.to_owned())
            .or_default();
        pending.extend(events);
        while pending.len() > MAX_CHILD_UI_EVENTS {
            pending.pop_front();
        }
    }

    fn remember_child_summaries(&mut self, events: &[UiEvent]) {
        for event in events {
            let UiEvent::DelegationBlock {
                call_id,
                target_kind,
                target_id,
                task,
                child_operation_id: Some(child_operation_id),
                ..
            } = event
            else {
                continue;
            };
            self.child_summaries.insert(
                child_operation_id.clone(),
                ChildDelegationSummary {
                    call_id: call_id.clone(),
                    target_kind: target_kind.clone(),
                    target_id: target_id.clone(),
                    task: task.clone(),
                    child_operation_id: child_operation_id.clone(),
                },
            );
        }
    }

    fn hydrate_child_delegations(&mut self) {
        let delegations = self.context.delegations.clone();
        for delegation in delegations {
            let Some(child_operation_id) = delegation.child_operation_id else {
                continue;
            };
            self.child_summaries.insert(
                child_operation_id.clone(),
                ChildDelegationSummary {
                    call_id: delegation.tool_call_id,
                    target_kind: delegation.target_kind,
                    target_id: delegation.target_id,
                    task: delegation.task,
                    child_operation_id: child_operation_id.clone(),
                },
            );
            let event = delegation
                .failure
                .map(|error| UiEvent::AgentError { error })
                .or_else(|| {
                    delegation
                        .summary
                        .map(|text| UiEvent::SystemNotice { text })
                });
            if let Some(event) = event {
                self.push_child_events(&child_operation_id, vec![event]);
            }
        }
    }

    fn replace_product_context(&mut self, context: CodingAgentContextSnapshot) {
        let now = Instant::now();
        for operation in &context.operations {
            let timing = self
                .operation_timings
                .entry(operation.operation_id.clone())
                .or_insert(UiOperationTiming {
                    first_seen: now,
                    terminal_elapsed: None,
                });
            if operation.status != CodingAgentOperationStatus::Running
                && timing.terminal_elapsed.is_none()
            {
                timing.terminal_elapsed = Some(now.saturating_duration_since(timing.first_seen));
            }
        }
        self.operation_timings.retain(|operation_id, _| {
            context
                .operations
                .iter()
                .any(|operation| operation.operation_id == *operation_id)
        });
        self.context = context;
    }

    fn child_status_event(&self, operation_id: &str, status: &str) -> Option<UiEvent> {
        let summary = self.child_summaries.get(operation_id)?;
        Some(UiEvent::DelegationBlock {
            call_id: summary.call_id.clone(),
            target_kind: summary.target_kind.clone(),
            target_id: summary.target_id.clone(),
            task: summary.task.clone(),
            status: status.to_owned(),
            child_operation_id: Some(summary.child_operation_id.clone()),
            summary: Some(status.replace('_', " ")),
            is_error: false,
        })
    }

    pub(crate) fn context(&self) -> &CodingAgentContextSnapshot {
        &self.context
    }

    pub(crate) fn operation_elapsed(
        &self,
        operation: &CodingAgentOperationSnapshot,
    ) -> Option<Duration> {
        let timing = self.operation_timings.get(&operation.operation_id)?;
        if operation.status == CodingAgentOperationStatus::Running {
            Some(Instant::now().saturating_duration_since(timing.first_seen))
        } else {
            timing.terminal_elapsed
        }
    }

    pub(crate) fn capabilities(&self) -> Option<&CodingAgentCapabilities> {
        self.product
            .as_ref()
            .map(|product| &product.snapshot().capabilities)
    }

    pub(crate) fn session(&self) -> Option<&CodingAgentSessionView> {
        self.product
            .as_ref()
            .map(|product| &product.snapshot().session)
    }

    #[cfg(test)]
    pub(crate) fn product_for_tests(&self) -> &CodingAgentClientProjection {
        self.product
            .as_ref()
            .expect("test projection must be initialized from a product snapshot")
    }
}

/// Stateless event bridge: converts typed product events to `Vec<UiEvent>`.
///
/// No longer accumulates tokens — `UiEvent::UsageUpdate` carries per-event
/// delta values. The receiver (`InteractiveRoot::apply_events`) accumulates
/// them into `FooterStats`.
#[derive(Debug, Clone)]
pub struct CodingEventBridge;

/// Estimate current context size from a usage snapshot.
/// Mirrors `agent-core::compaction::estimate::calculate_context_tokens`
/// and the TS `getContextUsage` use of the latest assistant usage.
fn calculate_context_tokens(usage: &CodingAgentProductEventUsage) -> u32 {
    if usage.total_tokens > 0 {
        usage.total_tokens
    } else {
        usage
            .input
            .saturating_add(usage.output)
            .saturating_add(usage.cache_read)
            .saturating_add(usage.cache_write)
    }
}

impl Default for CodingEventBridge {
    fn default() -> Self {
        Self::new()
    }
}

impl CodingEventBridge {
    pub fn new() -> Self {
        Self
    }

    pub(crate) fn push_product_event(&mut self, event: &ProductEvent) -> Vec<UiEvent> {
        self.handle_typed(event.event())
    }

    fn handle_typed(&mut self, event: &CodingAgentProductEventKind) -> Vec<UiEvent> {
        match event {
            CodingAgentProductEventKind::Agent(CodingAgentAgentProductEvent::TurnStarted {
                ..
            }) => {
                vec![UiEvent::TurnStarted]
            }
            CodingAgentProductEventKind::Message(CodingAgentMessageProductEvent::Delta {
                text,
                ..
            }) => {
                vec![UiEvent::AssistantDelta { text: text.clone() }]
            }
            CodingAgentProductEventKind::Message(
                CodingAgentMessageProductEvent::ThinkingDelta { text, .. },
            ) => {
                vec![UiEvent::ThinkingDelta { text: text.clone() }]
            }
            CodingAgentProductEventKind::Message(CodingAgentMessageProductEvent::Completed {
                images,
                usage,
                ..
            }) => {
                let context_tokens = match calculate_context_tokens(usage) {
                    0 => None,
                    tokens => Some(tokens),
                };
                let mut events = vec![UiEvent::AssistantDone];
                if !images.is_empty() {
                    events.push(UiEvent::AssistantImages {
                        images: images.clone(),
                    });
                }
                events.push(UiEvent::UsageUpdate {
                    input: usage.input,
                    output: usage.output,
                    cache_read: usage.cache_read,
                    cache_write: usage.cache_write,
                    cost: usage.input_cost
                        + usage.output_cost
                        + usage.cache_read_cost
                        + usage.cache_write_cost,
                    context_tokens,
                });
                events
            }
            CodingAgentProductEventKind::Tool(CodingAgentToolProductEvent::Started {
                tool_call_id,
                name,
                arguments_json,
                ..
            }) => delegation_block_from_tool_start(tool_call_id, name, arguments_json).map_or_else(
                || {
                    vec![UiEvent::ToolStarted {
                        call_id: tool_call_id.clone(),
                        name: name.clone(),
                        args: parse_tool_arguments(arguments_json),
                    }]
                },
                |event| vec![event],
            ),
            CodingAgentProductEventKind::Tool(
                CodingAgentToolProductEvent::AuthorizationRequired { request },
            ) => vec![UiEvent::ToolAuthorizationRequired {
                request: request.clone(),
            }],
            CodingAgentProductEventKind::Tool(
                CodingAgentToolProductEvent::AuthorizationApproved {
                    authorization_id, ..
                }
                | CodingAgentToolProductEvent::AuthorizationDenied {
                    authorization_id, ..
                }
                | CodingAgentToolProductEvent::AuthorizationCancelled {
                    authorization_id, ..
                },
            ) => vec![UiEvent::ToolAuthorizationResolved {
                authorization_id: authorization_id.clone(),
            }],
            CodingAgentProductEventKind::Tool(CodingAgentToolProductEvent::Updated {
                tool_call_id,
                message,
                ..
            }) => {
                vec![UiEvent::ToolUpdated {
                    call_id: tool_call_id.clone(),
                    result: message.clone(),
                }]
            }
            CodingAgentProductEventKind::Tool(CodingAgentToolProductEvent::Completed {
                tool_call_id,
                name,
                summary,
                ..
            }) => delegation_block_from_tool_result(tool_call_id, name, summary).map_or_else(
                || {
                    vec![UiEvent::ToolFinished {
                        call_id: tool_call_id.clone(),
                        result: summary.clone(),
                        is_error: false,
                    }]
                },
                |event| vec![event],
            ),
            CodingAgentProductEventKind::Tool(CodingAgentToolProductEvent::Failed {
                tool_call_id,
                name,
                message,
                ..
            }) => {
                if is_delegation_tool(name) {
                    delegation_block_from_tool_result(tool_call_id, name, message).map_or_else(
                        || {
                            vec![UiEvent::DelegationBlock {
                                call_id: tool_call_id.clone(),
                                target_kind: delegation_tool_kind_label(name)
                                    .unwrap_or("agent")
                                    .to_string(),
                                target_id: String::new(),
                                task: String::new(),
                                status: "failed".into(),
                                child_operation_id: None,
                                summary: Some(format!("failed: {message}")),
                                is_error: true,
                            }]
                        },
                        |event| vec![event],
                    )
                } else {
                    vec![UiEvent::ToolFinished {
                        call_id: tool_call_id.clone(),
                        result: message.clone(),
                        is_error: true,
                    }]
                }
            }
            CodingAgentProductEventKind::Runtime(
                CodingAgentRuntimeProductEvent::CompactionCompleted { summary, .. },
            )
            | CodingAgentProductEventKind::Session(
                CodingAgentSessionProductEvent::CompactionCompleted { summary, .. },
            ) => vec![
                UiEvent::CompactionNotice {
                    summary: summary.clone(),
                },
                UiEvent::UsageUpdate {
                    input: 0,
                    output: 0,
                    cache_read: 0,
                    cache_write: 0,
                    cost: 0.0,
                    context_tokens: None,
                },
            ],
            CodingAgentProductEventKind::Runtime(CodingAgentRuntimeProductEvent::ShutDown) => {
                Vec::new()
            }
            CodingAgentProductEventKind::Workflow(
                CodingAgentWorkflowProductEvent::PromptFailed { error, .. },
            ) => vec![UiEvent::AgentError {
                error: error.summary.clone(),
            }],
            CodingAgentProductEventKind::Workflow(
                CodingAgentWorkflowProductEvent::PromptAborted { reason, .. },
            ) => vec![UiEvent::AgentError {
                error: format!("prompt aborted: {reason}"),
            }],
            CodingAgentProductEventKind::Delegation(payload) => {
                let confirmation_required = match payload {
                    CodingAgentDelegationProductEvent::ConfirmationRequired { context, reason } => {
                        Some(UiEvent::DelegationConfirmationRequired {
                            pending: PendingDelegationConfirmation {
                                operation_id: context.operation_id.clone(),
                                turn_id: context.turn_id.clone(),
                                tool_call_id: context.tool_call_id.clone(),
                                requesting_profile_id: ProfileId::from(
                                    context.requesting_profile_id.as_str(),
                                ),
                                target_kind: match context.target_kind {
                                    CodingAgentProductEventProfileKind::Agent => ProfileKind::Agent,
                                    CodingAgentProductEventProfileKind::Team => ProfileKind::Team,
                                },
                                target_id: ProfileId::from(context.target_id.as_str()),
                                task: context.task.clone(),
                                reason: reason.clone(),
                            },
                        })
                    }
                    _ => None,
                };
                let confirmation_resolved = match payload {
                    CodingAgentDelegationProductEvent::Approved { context }
                    | CodingAgentDelegationProductEvent::Rejected { context, .. } => {
                        Some(UiEvent::DelegationConfirmationResolved {
                            operation_id: context.operation_id.clone(),
                            tool_call_id: context.tool_call_id.clone(),
                        })
                    }
                    _ => None,
                };
                let (ctx, status, summary, child, is_error) = match payload {
                    CodingAgentDelegationProductEvent::Requested { context } => {
                        (context, "requested", Some("requested".into()), None, false)
                    }
                    CodingAgentDelegationProductEvent::ConfirmationRequired { context, reason } => {
                        (
                            context,
                            "confirmation_required",
                            Some(format!("confirmation required: {reason}")),
                            None,
                            false,
                        )
                    }
                    CodingAgentDelegationProductEvent::Approved { context } => {
                        (context, "approved", Some("approved".into()), None, false)
                    }
                    CodingAgentDelegationProductEvent::Rejected { context, reason } => (
                        context,
                        "rejected",
                        Some(format!("rejected: {reason}")),
                        None,
                        true,
                    ),
                    CodingAgentDelegationProductEvent::Started {
                        context,
                        child_operation_id,
                    } => (context, "running", None, Some(child_operation_id), false),
                    CodingAgentDelegationProductEvent::Completed {
                        context,
                        child_operation_id,
                        final_text,
                    } => (
                        context,
                        "completed",
                        Some(format!("completed: {final_text}")),
                        Some(child_operation_id),
                        false,
                    ),
                    CodingAgentDelegationProductEvent::Failed {
                        context,
                        child_operation_id,
                        error,
                    } => (
                        context,
                        "failed",
                        Some(format!("failed: {}", error.summary)),
                        Some(child_operation_id),
                        true,
                    ),
                };
                let mut events = vec![UiEvent::DelegationBlock {
                    call_id: ctx.tool_call_id.clone(),
                    target_kind: profile_kind_label(match ctx.target_kind {
                        CodingAgentProductEventProfileKind::Agent => ProfileKind::Agent,
                        CodingAgentProductEventProfileKind::Team => ProfileKind::Team,
                    })
                    .into(),
                    target_id: ctx.target_id.clone(),
                    task: ctx.task.clone(),
                    status: status.into(),
                    child_operation_id: child.cloned(),
                    summary,
                    is_error,
                }];
                events.extend(confirmation_required);
                events.extend(confirmation_resolved);
                events
            }
            CodingAgentProductEventKind::Workflow(
                CodingAgentWorkflowProductEvent::SelfHealingEditStarted {
                    path, replacements, ..
                },
            ) => vec![UiEvent::SystemNotice {
                text: format!(
                    "Self-healing edit started for {} ({}).",
                    path,
                    replacement_count_label(*replacements)
                ),
            }],
            CodingAgentProductEventKind::Workflow(
                CodingAgentWorkflowProductEvent::OperationRecoveryResolved {
                    operation_id,
                    resolution,
                    reason,
                    ..
                },
            ) => vec![UiEvent::SystemNotice {
                text: format!(
                    "Operation {operation_id} recovery resolved as {resolution:?}: {reason}"
                ),
            }],
            CodingAgentProductEventKind::Workflow(
                CodingAgentWorkflowProductEvent::SelfHealingEditRepairAttempted {
                    path,
                    attempt,
                    replacements,
                    check_output,
                    ..
                },
            ) => vec![UiEvent::SystemNotice {
                text: format!(
                    "Self-healing edit repair attempt {} for {}: {}, {}.",
                    attempt,
                    path,
                    replacement_count_label(replacements.len()),
                    check_output
                        .as_ref()
                        .map(|o| format!("check exit {}", o.exit_code))
                        .unwrap_or_else(|| "no check output".into())
                ),
            }],
            CodingAgentProductEventKind::Workflow(
                CodingAgentWorkflowProductEvent::SelfHealingEditCompleted {
                    path,
                    attempts,
                    first_changed_line,
                    ..
                },
            ) => vec![UiEvent::SystemNotice {
                text: format!(
                    "Self-healing edit completed for {} after {}{}.",
                    path,
                    attempt_count_label(*attempts),
                    first_changed_line_label(*first_changed_line)
                ),
            }],
            CodingAgentProductEventKind::Workflow(
                CodingAgentWorkflowProductEvent::SelfHealingEditFailed { path, error, .. },
            ) => vec![UiEvent::SystemNotice {
                text: format!("Self-healing edit failed for {}: {}", path, error.summary),
            }],
            CodingAgentProductEventKind::Workflow(
                CodingAgentWorkflowProductEvent::SelfHealingEditAborted { path, reason, .. },
            ) => vec![UiEvent::SystemNotice {
                text: format!("Self-healing edit cancelled for {path}: {reason}"),
            }],
            CodingAgentProductEventKind::Workflow(
                CodingAgentWorkflowProductEvent::OperationRecoveryPending {
                    operation_id,
                    recovery_id,
                    reason,
                    ..
                },
            ) => vec![UiEvent::SystemNotice {
                text: format!(
                    "Operation {operation_id} requires recovery ({recovery_id}): {reason}"
                ),
            }],
            CodingAgentProductEventKind::Workflow(
                CodingAgentWorkflowProductEvent::OperationRecovered {
                    operation_id,
                    reason,
                    ..
                },
            ) => vec![UiEvent::SystemNotice {
                text: format!("Recovered incomplete operation {operation_id}: {reason}"),
            }],
            _ => Vec::new(),
        }
    }
}

fn replacement_count_label(replacements: usize) -> String {
    match replacements {
        1 => "1 replacement".to_string(),
        count => format!("{count} replacements"),
    }
}

fn attempt_count_label(attempts: usize) -> String {
    match attempts {
        1 => "1 attempt".to_string(),
        count => format!("{count} attempts"),
    }
}

fn first_changed_line_label(first_changed_line: Option<usize>) -> String {
    first_changed_line
        .map(|line| format!(", first changed line {line}"))
        .unwrap_or_default()
}
