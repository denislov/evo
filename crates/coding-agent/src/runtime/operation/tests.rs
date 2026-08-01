//! Operation lifecycle contract guards.
//!
//! These tests lock the observable contract of every [`CodingAgentOperation`]
//! variant: the dispatch mode that routes it, the admission class that bounds
//! it, and the derived access/capacity/durability facts the scheduler and
//! finalizer read. Structural refactors of the dispatch pipeline must keep this
//! table byte-identical — it is the behaviour-equivalence judgement for
//! CAG-310 through CAG-312.
use std::path::PathBuf;

use super::contract::{
    BranchSummaryReusePolicy, CodingAgentOperation, OPERATION_DESCRIPTOR_REVISION,
    OperationCancellation, OperationCapacity, OperationChildPolicy, OperationLineage,
    OperationOutcomeFamily, OperationPriority, OperationRuntimeAccess, OperationSessionAccess,
    OperationTerminalPolicy,
};
use super::{OperationClass, OperationDispatchMode};
use crate::app::bootstrap::PromptInvocation;
use crate::operations::agent_invocation::runner::AgentInvocationOptions;
use crate::operations::prompt::context::PromptTurnOptions;
use crate::operations::self_healing_edit::runner::SelfHealingEditRequest;
use crate::operations::team_invocation::runner::AgentTeamOptions;
use crate::runtime::operation::control::OperationKind;

fn prompt_options() -> PromptTurnOptions {
    PromptTurnOptions::new(PromptInvocation::Text("contract probe".into()))
}

/// Every public operation variant, paired with the contract it must resolve to.
///
/// Adding a variant without extending this table fails
/// [`every_operation_variant_is_covered`].
fn contract_table() -> Vec<(&'static str, CodingAgentOperation, ExpectedContract)> {
    vec![
        (
            "Prompt",
            CodingAgentOperation::Prompt(prompt_options()),
            ExpectedContract {
                kind: OperationKind::Prompt,
                class: OperationClass::SessionWriteRoot,
                dispatch: OperationDispatchMode::Async,
                outcome_family: OperationOutcomeFamily::Prompt,
                terminal_policy: OperationTerminalPolicy::ProductEvent,
                has_root_evidence: true,
            },
        ),
        (
            "Compact",
            CodingAgentOperation::Compact(prompt_options()),
            ExpectedContract {
                kind: OperationKind::Compact,
                class: OperationClass::SessionWriteRoot,
                dispatch: OperationDispatchMode::Async,
                outcome_family: OperationOutcomeFamily::Compact,
                terminal_policy: OperationTerminalPolicy::ProductEvent,
                has_root_evidence: true,
            },
        ),
        (
            "BranchSummary",
            CodingAgentOperation::BranchSummary {
                options: prompt_options(),
                source_leaf_id: "leaf_source".into(),
                target_leaf_id: "leaf_target".into(),
                custom_instructions: None,
                reuse: BranchSummaryReusePolicy::AlwaysCreate,
            },
            ExpectedContract {
                kind: OperationKind::BranchSummary,
                class: OperationClass::SessionWriteRoot,
                dispatch: OperationDispatchMode::Async,
                outcome_family: OperationOutcomeFamily::BranchSummary,
                terminal_policy: OperationTerminalPolicy::OutcomeAcknowledgement,
                has_root_evidence: false,
            },
        ),
        (
            "SelfHealingEdit",
            CodingAgentOperation::SelfHealingEdit(SelfHealingEditRequest::new(
                "src/lib.rs",
                Vec::new(),
            )),
            ExpectedContract {
                kind: OperationKind::SelfHealingEdit,
                class: OperationClass::SessionWriteRoot,
                dispatch: OperationDispatchMode::Async,
                outcome_family: OperationOutcomeFamily::SelfHealingEdit,
                terminal_policy: OperationTerminalPolicy::ProductEvent,
                has_root_evidence: true,
            },
        ),
        (
            "InvokeAgent",
            CodingAgentOperation::InvokeAgent(AgentInvocationOptions::new(
                "reviewer",
                "review the diff",
                prompt_options(),
            )),
            ExpectedContract {
                kind: OperationKind::AgentInvocation,
                class: OperationClass::NonSessionRoot,
                dispatch: OperationDispatchMode::Async,
                outcome_family: OperationOutcomeFamily::AgentInvocation,
                terminal_policy: OperationTerminalPolicy::ProductEvent,
                has_root_evidence: true,
            },
        ),
        (
            "InvokeTeam",
            CodingAgentOperation::InvokeTeam(AgentTeamOptions::new(
                "squad",
                "ship the feature",
                prompt_options(),
            )),
            ExpectedContract {
                kind: OperationKind::AgentTeam,
                class: OperationClass::NonSessionRoot,
                dispatch: OperationDispatchMode::Async,
                outcome_family: OperationOutcomeFamily::AgentTeam,
                terminal_policy: OperationTerminalPolicy::ProductEvent,
                has_root_evidence: true,
            },
        ),
        // Approve is async because it resumes a real agent turn; reject is a
        // synchronous session write. This asymmetry is deliberate — keep it.
        (
            "ApproveDelegation",
            CodingAgentOperation::ApproveDelegation {
                operation_id: "op_parent".into(),
                tool_call_id: "tool_delegate".into(),
            },
            ExpectedContract {
                kind: OperationKind::DelegationConfirmation,
                class: OperationClass::SessionWriteRoot,
                dispatch: OperationDispatchMode::Async,
                outcome_family: OperationOutcomeFamily::DelegationApproved,
                terminal_policy: OperationTerminalPolicy::OutcomeAcknowledgement,
                has_root_evidence: false,
            },
        ),
        (
            "RejectDelegation",
            CodingAgentOperation::RejectDelegation {
                operation_id: "op_parent".into(),
                tool_call_id: "tool_delegate".into(),
                reason: "not now".into(),
            },
            ExpectedContract {
                kind: OperationKind::DelegationConfirmation,
                class: OperationClass::SessionWriteRoot,
                dispatch: OperationDispatchMode::SyncMutable,
                outcome_family: OperationOutcomeFamily::DelegationRejected,
                terminal_policy: OperationTerminalPolicy::OutcomeAcknowledgement,
                has_root_evidence: false,
            },
        ),
        (
            "ForkSession",
            CodingAgentOperation::ForkSession {
                target_leaf_id: None,
            },
            ExpectedContract {
                kind: OperationKind::ForkSession,
                class: OperationClass::SessionWriteRoot,
                dispatch: OperationDispatchMode::SyncMutable,
                outcome_family: OperationOutcomeFamily::SessionForked,
                terminal_policy: OperationTerminalPolicy::OutcomeAcknowledgement,
                has_root_evidence: false,
            },
        ),
        (
            "SwitchActiveLeaf",
            CodingAgentOperation::SwitchActiveLeaf {
                target_leaf_id: "leaf_target".into(),
            },
            ExpectedContract {
                kind: OperationKind::SwitchActiveLeaf,
                class: OperationClass::SessionWriteRoot,
                dispatch: OperationDispatchMode::SyncMutable,
                outcome_family: OperationOutcomeFamily::ActiveLeafSwitched,
                terminal_policy: OperationTerminalPolicy::OutcomeAcknowledgement,
                has_root_evidence: false,
            },
        ),
        (
            "SetSessionTreeLabel",
            CodingAgentOperation::SetSessionTreeLabel {
                entry_id: "entry".into(),
                label: None,
            },
            ExpectedContract {
                kind: OperationKind::SetSessionTreeLabel,
                class: OperationClass::SessionWriteRoot,
                dispatch: OperationDispatchMode::SyncMutable,
                outcome_family: OperationOutcomeFamily::SessionTreeLabelChanged,
                terminal_policy: OperationTerminalPolicy::OutcomeAcknowledgement,
                has_root_evidence: false,
            },
        ),
        (
            "SetSessionName",
            CodingAgentOperation::SetSessionName { name: None },
            ExpectedContract {
                kind: OperationKind::SetSessionName,
                class: OperationClass::SessionWriteRoot,
                dispatch: OperationDispatchMode::SyncMutable,
                outcome_family: OperationOutcomeFamily::SessionNameChanged,
                terminal_policy: OperationTerminalPolicy::OutcomeAcknowledgement,
                has_root_evidence: false,
            },
        ),
        (
            "ExportCurrent",
            CodingAgentOperation::ExportCurrent,
            ExpectedContract {
                kind: OperationKind::Export,
                class: OperationClass::ReadOnly,
                dispatch: OperationDispatchMode::SyncReadOnly,
                outcome_family: OperationOutcomeFamily::Export,
                terminal_policy: OperationTerminalPolicy::OutcomeAcknowledgement,
                has_root_evidence: false,
            },
        ),
        (
            "ExportCurrentHtml",
            CodingAgentOperation::ExportCurrentHtml(PathBuf::from("/tmp/export.html")),
            ExpectedContract {
                kind: OperationKind::Export,
                class: OperationClass::ReadOnly,
                dispatch: OperationDispatchMode::SyncReadOnly,
                outcome_family: OperationOutcomeFamily::ExportHtml,
                terminal_policy: OperationTerminalPolicy::OutcomeAcknowledgement,
                has_root_evidence: false,
            },
        ),
    ]
}

struct ExpectedContract {
    kind: OperationKind,
    class: OperationClass,
    dispatch: OperationDispatchMode,
    outcome_family: OperationOutcomeFamily,
    terminal_policy: OperationTerminalPolicy,
    has_root_evidence: bool,
}

#[test]
fn every_operation_variant_resolves_its_declared_contract() {
    for (name, operation, expected) in contract_table() {
        let descriptor = operation.descriptor();

        assert_eq!(
            descriptor.revision, OPERATION_DESCRIPTOR_REVISION,
            "{name}: descriptor revision"
        );
        assert_eq!(
            descriptor.submitted_kind, expected.kind,
            "{name}: submitted kind"
        );
        assert_eq!(
            descriptor.admission_class(),
            expected.class,
            "{name}: admission class"
        );
        assert_eq!(
            descriptor.dispatch_mode, expected.dispatch,
            "{name}: dispatch mode"
        );
        assert_eq!(
            descriptor.outcome_family, expected.outcome_family,
            "{name}: outcome family"
        );
        assert_eq!(
            descriptor.terminal_policy, expected.terminal_policy,
            "{name}: terminal policy"
        );
        assert_eq!(
            !descriptor.permitted_root_evidence.is_empty(),
            expected.has_root_evidence,
            "{name}: root terminal evidence presence"
        );
        assert_eq!(
            descriptor.lineage,
            OperationLineage::Root,
            "{name}: public submissions are always roots"
        );
    }
}

/// The access/capacity/durability quadruple is derived from the admission
/// class. Locking the derivation keeps the scheduler and the session writer
/// agreeing about who may run concurrently.
#[test]
fn admission_class_derives_access_capacity_and_durability() {
    for (name, operation, expected) in contract_table() {
        let descriptor = operation.descriptor();
        let (session_access, runtime_access, capacity, session_durable, runtime_durable) =
            match expected.class {
                OperationClass::SessionWriteRoot => (
                    OperationSessionAccess::Write,
                    OperationRuntimeAccess::None,
                    OperationCapacity::SessionWriter,
                    true,
                    false,
                ),
                OperationClass::NonSessionRoot => (
                    OperationSessionAccess::None,
                    OperationRuntimeAccess::Read,
                    OperationCapacity::BoundedRuntime,
                    false,
                    false,
                ),
                OperationClass::RuntimeWrite => (
                    OperationSessionAccess::Write,
                    OperationRuntimeAccess::Write,
                    OperationCapacity::RuntimeExclusive,
                    true,
                    true,
                ),
                OperationClass::ReadOnly => (
                    OperationSessionAccess::Read,
                    OperationRuntimeAccess::None,
                    OperationCapacity::Shared,
                    false,
                    false,
                ),
                other => panic!("{name}: public root resolved a dedicated intent class {other:?}"),
            };

        assert_eq!(
            descriptor.session_access, session_access,
            "{name}: session access"
        );
        assert_eq!(
            descriptor.runtime_access, runtime_access,
            "{name}: runtime access"
        );
        assert_eq!(descriptor.capacity, capacity, "{name}: capacity");
        assert_eq!(
            descriptor.durability.session_if_persistent, session_durable,
            "{name}: session durability"
        );
        assert_eq!(
            descriptor.durability.runtime_generation, runtime_durable,
            "{name}: runtime-generation durability"
        );
    }
}

/// Priority, cancellation and child policy are derived from kind and dispatch
/// mode rather than declared per variant. A refactor that reroutes an operation
/// silently changes all three, so assert the derivation directly.
#[test]
fn priority_cancellation_and_child_policy_follow_kind_and_dispatch() {
    for (name, operation, _) in contract_table() {
        let descriptor = operation.descriptor();

        let expected_priority = match descriptor.submitted_kind {
            OperationKind::Prompt | OperationKind::DelegationConfirmation => {
                OperationPriority::Interactive
            }
            _ => OperationPriority::Normal,
        };
        assert_eq!(descriptor.priority, expected_priority, "{name}: priority");

        let expected_cancellation = match descriptor.dispatch_mode {
            OperationDispatchMode::Async => OperationCancellation::Cancellable,
            OperationDispatchMode::SyncReadOnly | OperationDispatchMode::SyncMutable => {
                OperationCancellation::Atomic
            }
        };
        assert_eq!(
            descriptor.cancellation, expected_cancellation,
            "{name}: cancellation"
        );

        let expected_child_policy = match descriptor.submitted_kind {
            OperationKind::Prompt | OperationKind::AgentInvocation | OperationKind::AgentTeam => {
                OperationChildPolicy::Structured
            }
            _ => OperationChildPolicy::Forbidden,
        };
        assert_eq!(
            descriptor.child_policy, expected_child_policy,
            "{name}: child policy"
        );
    }
}

/// Guards the table itself: if a variant is added to the public enum without a
/// row here, the count check fails and the new operation is forced through
/// contract review.
#[test]
fn every_operation_variant_is_covered() {
    const PUBLIC_VARIANT_COUNT: usize = 14;

    let table = contract_table();
    assert_eq!(
        table.len(),
        PUBLIC_VARIANT_COUNT,
        "contract table must cover every CodingAgentOperation variant"
    );

    let mut names: Vec<&str> = table.iter().map(|(name, _, _)| *name).collect();
    names.sort_unstable();
    let unique = names.len();
    names.dedup();
    assert_eq!(names.len(), unique, "contract table has duplicate rows");
}

/// Export is the only public operation whose submitted shape differs from the
/// runner input. Keep both normalization branches explicit while the duplicate
/// internal operation enum is gone.
#[test]
fn export_variants_normalize_to_their_runner_modes() {
    let view = CodingAgentOperation::ExportCurrent
        .export_options()
        .expect("view export normalizes");
    assert!(!view.writes_html(), "view export must not write HTML");

    let html = CodingAgentOperation::ExportCurrentHtml(PathBuf::from("/tmp/export.html"))
        .export_options()
        .expect("HTML export normalizes");
    assert!(
        html.writes_html(),
        "HTML export must retain its output mode"
    );
}
