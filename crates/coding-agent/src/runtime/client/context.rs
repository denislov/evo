use crate::events::CodingAgentProductEvent;
use crate::runtime::client::context_fold::{ProductContextPendingState, fold_product_context};
use crate::runtime::client::projection::{CodingAgentContextSnapshot, CodingAgentOperationStatus};
use crate::runtime::control::OperationKind;

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
    pub(crate) operation_id: String,
    pub(crate) tool_call_id: Option<String>,
    pub(crate) updated_sequence: u64,
    pub(crate) first_changed_line: Option<usize>,
    pub(crate) added_lines: Option<usize>,
    pub(crate) removed_lines: Option<usize>,
    pub(crate) diff: Option<String>,
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
        self.changes = context
            .changes
            .into_iter()
            .map(|change| UiFileChangeProjection {
                path: change.path,
                mutation_kind: change.mutation_kind,
                operation_id: change.operation_id,
                tool_call_id: change.tool_call_id,
                updated_sequence: change.updated_sequence,
                first_changed_line: change.first_changed_line,
                added_lines: change.added_lines,
                removed_lines: change.removed_lines,
                diff: change.diff,
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

#[cfg(test)]
mod tests {
    use crate::events::emission::ProductEventDraft;
    use crate::events::{
        CodingAgentAgentProductEvent, CodingAgentDelegationEventContext,
        CodingAgentDelegationProductEvent, CodingAgentDiagnosticProductEvent,
        CodingAgentMessageProductEvent, CodingAgentProductEvent, CodingAgentProductEventDurability,
        CodingAgentProductEventKind, CodingAgentProductEventProfileKind,
        CodingAgentProductEventTerminalOperation, CodingAgentProductEventTerminalOperationKind,
        CodingAgentProductEventTerminalStatus, CodingAgentProductEventUsage,
        CodingAgentToolProductEvent, CodingAgentWorkflowProductEvent, ProductEventSequence,
    };
    use crate::runtime::client::context_fold::MAX_CONTEXT_OPERATIONS;

    use super::*;

    fn event(
        sequence: u64,
        operation_id: &str,
        event: CodingAgentProductEventKind,
        terminal: Option<CodingAgentProductEventTerminalOperation>,
    ) -> CodingAgentProductEvent {
        CodingAgentProductEvent::from_draft_for_tests(
            ProductEventSequence::new(sequence),
            ProductEventDraft {
                event,
                operation_id: Some(operation_id.into()),
                session_id: Some("session-1".into()),
                terminal_status: terminal.map(|terminal| terminal.status),
                durability: CodingAgentProductEventDurability::LiveOnly,
            },
            terminal,
        )
    }

    fn terminal_prompt(
        status: CodingAgentProductEventTerminalStatus,
    ) -> CodingAgentProductEventTerminalOperation {
        CodingAgentProductEventTerminalOperation {
            kind: CodingAgentProductEventTerminalOperationKind::Prompt,
            status,
        }
    }

    #[test]
    fn ignores_local_event_without_root_evidence_for_unknown_operation() {
        let mut projection = UiContextProjection::default();
        projection.apply_product_event(
            &event(
                1,
                "op-local-only",
                CodingAgentProductEventKind::Diagnostic(
                    CodingAgentDiagnosticProductEvent::Diagnostic {
                        diagnostic:
                            crate::runtime::public_error::CodingAgentPublicDiagnostic::new(
                                crate::runtime::public_error::CodingAgentPublicDiagnosticSeverity::Warning,
                                "child_diagnostic",
                                "child diagnostic",
                                crate::runtime::public_error::CodingAgentPublicDiagnosticOrigin::Runtime,
                                Some("op-local-only"),
                            ),
                    },
                ),
                None,
            ),
            None,
        );
        assert!(projection.operations.is_empty());
    }

    #[test]
    fn folds_typed_context_facts_without_reclassifying_the_root_operation() {
        let mut projection = UiContextProjection::default();
        projection.apply_product_event(
            &event(
                1,
                "op-1",
                CodingAgentProductEventKind::Workflow(
                    CodingAgentWorkflowProductEvent::PromptStarted {
                        operation_id: "op-1".into(),
                        turn_id: "turn-1".into(),
                    },
                ),
                None,
            ),
            Some(OperationKind::Prompt),
        );
        projection.apply_product_event(
            &event(
                2,
                "op-1",
                CodingAgentProductEventKind::Tool(CodingAgentToolProductEvent::Started {
                    operation_id: "op-1".into(),
                    turn_id: "turn-1".into(),
                    tool_call_id: "tool-1".into(),
                    name: "edit".into(),
                    arguments_json: r#"{"path":"src/lib.rs","oldText":"a","newText":"b"}"#.into(),
                }),
                None,
            ),
            None,
        );
        projection.apply_product_event(
            &event(
                3,
                "op-1",
                CodingAgentProductEventKind::Tool(CodingAgentToolProductEvent::Completed {
                    operation_id: "op-1".into(),
                    turn_id: "turn-1".into(),
                    tool_call_id: "tool-1".into(),
                    name: "edit".into(),
                    summary: "updated".into(),
                }),
                None,
            ),
            None,
        );
        projection.apply_product_event(
            &event(
                4,
                "op-1",
                CodingAgentProductEventKind::Delegation(
                    CodingAgentDelegationProductEvent::Started {
                        context: CodingAgentDelegationEventContext {
                            operation_id: "op-1".into(),
                            turn_id: "turn-1".into(),
                            tool_call_id: "delegate-1".into(),
                            requesting_profile_id: "default".into(),
                            target_kind: CodingAgentProductEventProfileKind::Agent,
                            target_id: "review".into(),
                            task: "review the change".into(),
                        },
                        child_operation_id: "child-1".into(),
                    },
                ),
                None,
            ),
            None,
        );
        projection.apply_product_event(
            &event(
                5,
                "op-1",
                CodingAgentProductEventKind::Agent(
                    CodingAgentAgentProductEvent::ProviderRequestStarted {
                        operation_id: "op-1".into(),
                        turn_id: "turn-1".into(),
                        provider: "test".into(),
                        model: "model-1".into(),
                        context_window: Some(128_000),
                    },
                ),
                None,
            ),
            None,
        );
        projection.apply_product_event(
            &event(
                6,
                "op-1",
                CodingAgentProductEventKind::Message(CodingAgentMessageProductEvent::Completed {
                    operation_id: "op-1".into(),
                    turn_id: "turn-1".into(),
                    message_id: Some("message-1".into()),
                    final_text: "done".into(),
                    images: Vec::new(),
                    reasoning_duration_millis: None,
                    usage: CodingAgentProductEventUsage {
                        input: 100,
                        output: 20,
                        cache_read: 5,
                        cache_write: 2,
                        total_tokens: 127,
                        cost_known: true,
                        input_cost: 0.001,
                        output_cost: 0.002,
                        cache_read_cost: 0.0,
                        cache_write_cost: 0.0,
                    },
                }),
                None,
            ),
            None,
        );
        projection.apply_product_event(
            &event(
                7,
                "op-1",
                CodingAgentProductEventKind::Workflow(
                    CodingAgentWorkflowProductEvent::PromptCompleted {
                        operation_id: "op-1".into(),
                        turn_id: "turn-1".into(),
                    },
                ),
                Some(terminal_prompt(
                    CodingAgentProductEventTerminalStatus::Completed,
                )),
            ),
            None,
        );

        assert_eq!(projection.operations.len(), 1);
        assert_eq!(projection.operations[0].kind, "prompt");
        assert_eq!(
            projection.operations[0].status,
            UiOperationStatus::Completed
        );
        assert_eq!(projection.changes.len(), 1);
        assert_eq!(projection.changes[0].path, "src/lib.rs");
        assert_eq!(projection.delegations.len(), 1);
        assert_eq!(projection.delegations[0].target_kind, "agent");
        assert_eq!(projection.delegations[0].status, "running");
        assert_eq!(projection.usage.input, 100);
        assert_eq!(
            projection
                .usage
                .latest_turn
                .as_ref()
                .unwrap()
                .context_tokens,
            Some(127)
        );
        assert_eq!(projection.usage.cost, Some(0.003));
        assert_eq!(projection.usage.model_id.as_deref(), Some("model-1"));
        assert_eq!(projection.usage.context_window, Some(128_000));
    }

    #[test]
    fn failed_mutations_and_unpriced_usage_remain_explicitly_unavailable() {
        let mut projection = UiContextProjection::default();
        projection.apply_product_event(
            &event(
                1,
                "op-1",
                CodingAgentProductEventKind::Tool(CodingAgentToolProductEvent::Started {
                    operation_id: "op-1".into(),
                    turn_id: "turn-1".into(),
                    tool_call_id: "tool-1".into(),
                    name: "write".into(),
                    arguments_json: r#"{"file_path":"README.md","content":"x"}"#.into(),
                }),
                None,
            ),
            Some(OperationKind::Prompt),
        );
        projection.apply_product_event(
            &event(
                2,
                "op-1",
                CodingAgentProductEventKind::Tool(CodingAgentToolProductEvent::Failed {
                    operation_id: "op-1".into(),
                    turn_id: "turn-1".into(),
                    tool_call_id: "tool-1".into(),
                    name: "write".into(),
                    message: "denied".into(),
                }),
                None,
            ),
            None,
        );
        projection.apply_product_event(
            &event(
                3,
                "op-1",
                CodingAgentProductEventKind::Message(CodingAgentMessageProductEvent::Completed {
                    operation_id: "op-1".into(),
                    turn_id: "turn-1".into(),
                    message_id: None,
                    final_text: String::new(),
                    images: Vec::new(),
                    reasoning_duration_millis: None,
                    usage: CodingAgentProductEventUsage {
                        input: 1,
                        output: 1,
                        cache_read: 0,
                        cache_write: 0,
                        total_tokens: 2,
                        cost_known: true,
                        input_cost: 0.0,
                        output_cost: 0.0,
                        cache_read_cost: 0.0,
                        cache_write_cost: 0.0,
                    },
                }),
                None,
            ),
            None,
        );

        assert!(projection.changes.is_empty());
        assert_eq!(projection.usage.cost, None);
        assert_eq!(projection.usage.latest_turn.as_ref().unwrap().cost, None);
    }

    #[test]
    fn operation_history_is_bounded_and_keeps_the_latest_entries() {
        let mut projection = UiContextProjection::default();
        for sequence in 1..=40 {
            let operation_id = format!("op-{sequence}");
            projection.apply_product_event(
                &event(
                    sequence,
                    &operation_id,
                    CodingAgentProductEventKind::Workflow(
                        CodingAgentWorkflowProductEvent::PromptStarted {
                            operation_id: operation_id.clone(),
                            turn_id: format!("turn-{sequence}"),
                        },
                    ),
                    None,
                ),
                Some(OperationKind::Prompt),
            );
        }

        assert_eq!(projection.operations.len(), MAX_CONTEXT_OPERATIONS);
        assert_eq!(projection.operations[0].operation_id, "op-40");
        assert!(
            projection
                .operations
                .iter()
                .all(|operation| operation.operation_id != "op-1")
        );
    }

    #[test]
    fn operation_lineage_order_stays_stable_across_terminal_updates() {
        let mut projection = UiContextProjection::default();
        projection.apply_product_event(
            &event(
                1,
                "op-root",
                CodingAgentProductEventKind::Workflow(
                    CodingAgentWorkflowProductEvent::PromptStarted {
                        operation_id: "op-root".into(),
                        turn_id: "turn-root".into(),
                    },
                ),
                None,
            ),
            Some(OperationKind::Prompt),
        );
        projection.apply_product_event(
            &event(
                2,
                "op-agent",
                CodingAgentProductEventKind::Agent(
                    CodingAgentAgentProductEvent::InvocationStarted {
                        operation_id: "op-agent".into(),
                        child_operation_id: "op-child-prompt".into(),
                        profile_id: "coder".into(),
                        task: "implement".into(),
                    },
                ),
                None,
            )
            .with_lineage_for_tests("op-root", "op-root"),
            Some(OperationKind::AgentInvocation),
        );
        projection.apply_product_event(
            &event(
                3,
                "op-child-prompt",
                CodingAgentProductEventKind::Workflow(
                    CodingAgentWorkflowProductEvent::PromptStarted {
                        operation_id: "op-child-prompt".into(),
                        turn_id: "turn-child".into(),
                    },
                ),
                None,
            )
            .with_lineage_for_tests("op-agent", "op-root"),
            Some(OperationKind::Prompt),
        );
        let before = projection
            .operations
            .iter()
            .map(|operation| operation.operation_id.clone())
            .collect::<Vec<_>>();
        assert_eq!(before, ["op-root", "op-agent", "op-child-prompt"]);

        projection.apply_product_event(
            &event(
                4,
                "op-child-prompt",
                CodingAgentProductEventKind::Workflow(
                    CodingAgentWorkflowProductEvent::PromptCompleted {
                        operation_id: "op-child-prompt".into(),
                        turn_id: "turn-child".into(),
                    },
                ),
                Some(terminal_prompt(
                    CodingAgentProductEventTerminalStatus::Completed,
                )),
            )
            .with_lineage_for_tests("op-agent", "op-root"),
            Some(OperationKind::Prompt),
        );

        let after = projection
            .operations
            .iter()
            .map(|operation| operation.operation_id.clone())
            .collect::<Vec<_>>();
        assert_eq!(after, before);
        assert_eq!(
            projection.operations[2].status,
            UiOperationStatus::Completed
        );
    }

    #[test]
    fn root_terminal_event_closes_running_descendants_after_cancellation() {
        let mut projection = UiContextProjection::default();
        projection.apply_product_event(
            &event(
                1,
                "op-root",
                CodingAgentProductEventKind::Workflow(
                    CodingAgentWorkflowProductEvent::PromptStarted {
                        operation_id: "op-root".into(),
                        turn_id: "turn-root".into(),
                    },
                ),
                None,
            ),
            Some(OperationKind::Prompt),
        );
        projection.apply_product_event(
            &event(
                2,
                "op-agent",
                CodingAgentProductEventKind::Agent(
                    CodingAgentAgentProductEvent::InvocationStarted {
                        operation_id: "op-agent".into(),
                        child_operation_id: "op-child-prompt".into(),
                        profile_id: "coder".into(),
                        task: "implement".into(),
                    },
                ),
                None,
            )
            .with_lineage_for_tests("op-root", "op-root"),
            Some(OperationKind::AgentInvocation),
        );
        projection.apply_product_event(
            &event(
                3,
                "op-child-prompt",
                CodingAgentProductEventKind::Workflow(
                    CodingAgentWorkflowProductEvent::PromptStarted {
                        operation_id: "op-child-prompt".into(),
                        turn_id: "turn-child".into(),
                    },
                ),
                None,
            )
            .with_lineage_for_tests("op-agent", "op-root"),
            Some(OperationKind::Prompt),
        );

        projection.apply_product_event(
            &event(
                4,
                "op-root",
                CodingAgentProductEventKind::Workflow(
                    CodingAgentWorkflowProductEvent::PromptAborted {
                        operation_id: "op-root".into(),
                        reason: "cancelled".into(),
                    },
                ),
                Some(terminal_prompt(
                    CodingAgentProductEventTerminalStatus::Aborted,
                )),
            ),
            Some(OperationKind::Prompt),
        );

        assert!(
            projection
                .operations
                .iter()
                .all(|operation| operation.status == UiOperationStatus::Aborted)
        );
    }
}
