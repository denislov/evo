use super::capability::{
    CapabilityGeneration, OperationCapabilitySnapshot, SessionCapabilityAccess,
};
use super::control::OperationKind;
use crate::operations::agent_invocation::runner::{AgentInvocationOptions, AgentInvocationOutcome};
use crate::operations::export::runner::{ExportOptions, ExportOutcome};
use crate::operations::prompt::context::{
    InternalPromptTurnOutcome, PromptTurnOptions, RuntimeSnapshot,
};
use crate::operations::self_healing_edit::runner::{
    SelfHealingEditOutcome, SelfHealingEditRequest,
};
use crate::operations::team_invocation::runner::{AgentTeamOptions, AgentTeamOutcome};
use crate::profiles::ProfileId;
use crate::runtime::error::CodingSessionError;

#[derive(Debug)]
pub(crate) enum Operation {
    Prompt(PromptTurnOptions),
    ManualCompaction(PromptTurnOptions),
    ApproveDelegationConfirmation {
        operation_id: String,
        tool_call_id: String,
    },
    RejectDelegationConfirmation {
        operation_id: String,
        tool_call_id: String,
        reason: String,
    },
    BranchSummary {
        options: PromptTurnOptions,
        source_leaf_id: String,
        target_leaf_id: String,
        custom_instructions: Option<String>,
        reuse_existing: bool,
    },
    SelfHealingEdit(SelfHealingEditRequest),
    AgentInvocation(AgentInvocationOptions),
    AgentTeam(AgentTeamOptions),
    ForkSession {
        target_leaf_id: Option<String>,
    },
    SwitchActiveLeaf {
        target_leaf_id: String,
    },
    SetSessionTreeLabel {
        entry_id: String,
        label: Option<String>,
    },
    SetSessionName {
        name: Option<String>,
    },
    SetDefaultAgentProfile {
        profile_id: ProfileId,
    },
    Export(ExportOptions),
}

impl Operation {
    pub(crate) fn runtime(&self) -> Option<&RuntimeSnapshot> {
        match self {
            Self::Prompt(options)
            | Self::ManualCompaction(options)
            | Self::BranchSummary { options, .. } => options.runtime(),
            Self::AgentInvocation(options) => options.prompt_options().runtime(),
            Self::AgentTeam(options) => options.prompt_options().runtime(),
            Self::SelfHealingEdit(request) => request
                .model_repair()
                .and_then(|repair| repair.prompt_options().runtime()),
            Self::ApproveDelegationConfirmation { .. }
            | Self::RejectDelegationConfirmation { .. }
            | Self::ForkSession { .. }
            | Self::SwitchActiveLeaf { .. }
            | Self::SetSessionTreeLabel { .. }
            | Self::SetSessionName { .. }
            | Self::SetDefaultAgentProfile { .. }
            | Self::Export(_) => None,
        }
    }

    pub(crate) fn session_access(&self) -> SessionCapabilityAccess {
        match crate::runtime::outcome::descriptor_for_internal_operation(self).session_access {
            crate::runtime::outcome::OperationSessionAccess::None => SessionCapabilityAccess::None,
            crate::runtime::outcome::OperationSessionAccess::Read => SessionCapabilityAccess::Read,
            crate::runtime::outcome::OperationSessionAccess::Write => {
                SessionCapabilityAccess::Write
            }
        }
    }

    pub(crate) fn prompt_options_mut(&mut self) -> Option<&mut PromptTurnOptions> {
        match self {
            Self::Prompt(options) | Self::ManualCompaction(options) => Some(options),
            Self::BranchSummary { options, .. } => Some(options),
            Self::SelfHealingEdit(request) => request
                .model_repair_mut()
                .map(|repair| repair.prompt_options_mut()),
            Self::AgentInvocation(options) => Some(options.prompt_options_mut()),
            Self::AgentTeam(options) => Some(options.prompt_options_mut()),
            Self::ApproveDelegationConfirmation { .. }
            | Self::RejectDelegationConfirmation { .. }
            | Self::ForkSession { .. }
            | Self::SwitchActiveLeaf { .. }
            | Self::SetSessionTreeLabel { .. }
            | Self::SetSessionName { .. }
            | Self::SetDefaultAgentProfile { .. }
            | Self::Export(_) => None,
        }
    }

    #[cfg(test)]
    pub(crate) fn kind(&self) -> OperationKind {
        self.static_kind()
            .expect("dynamic operation does not have a static kind")
    }

    pub(crate) fn static_kind(&self) -> Option<OperationKind> {
        (!matches!(self, Self::ApproveDelegationConfirmation { .. }))
            .then_some(self.descriptor().submitted_kind)
    }

    #[cfg(test)]
    pub(crate) fn origin(&self) -> OperationOrigin {
        OperationOrigin::ClientRoot
    }

    #[cfg(test)]
    pub(crate) fn class(&self) -> OperationClass {
        self.descriptor().admission_class()
    }

    pub(crate) fn descriptor(&self) -> crate::runtime::outcome::OperationDescriptor {
        crate::runtime::outcome::descriptor_for_internal_operation(self)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct OperationExecution {
    pub(crate) kind: OperationKind,
    pub(crate) descriptor: crate::runtime::outcome::OperationDescriptor,
    pub(crate) origin: OperationOrigin,
    pub(crate) admitted_at: Option<String>,
    pub(crate) session_identity: Option<String>,
    pub(crate) capability_snapshot: OperationCapabilitySnapshot,
    pub(crate) operation_id: String,
    pub(crate) capability_generation: CapabilityGeneration,
    pub(crate) parent_operation_id: Option<String>,
    pub(crate) root_operation_id: Option<String>,
}

impl OperationExecution {
    pub(crate) fn root(
        kind: OperationKind,
        descriptor: crate::runtime::outcome::OperationDescriptor,
        origin: OperationOrigin,
        admitted_at: Option<String>,
        session_identity: Option<String>,
        capability_snapshot: OperationCapabilitySnapshot,
    ) -> Self {
        let operation_id = capability_snapshot.operation_id.clone();
        let capability_generation = capability_snapshot.generation;
        Self {
            kind,
            descriptor,
            origin,
            admitted_at,
            session_identity,
            capability_snapshot,
            operation_id: operation_id.clone(),
            capability_generation,
            parent_operation_id: None,
            root_operation_id: Some(operation_id),
        }
    }

    pub(crate) fn child(
        kind: OperationKind,
        descriptor: crate::runtime::outcome::OperationDescriptor,
        capability_snapshot: OperationCapabilitySnapshot,
        parent_operation_id: String,
        root_operation_id: String,
    ) -> Self {
        let operation_id = capability_snapshot.operation_id.clone();
        let capability_generation = capability_snapshot.generation;
        Self {
            kind,
            descriptor,
            origin: OperationOrigin::ParentChild,
            admitted_at: None,
            session_identity: None,
            capability_snapshot,
            operation_id,
            capability_generation,
            parent_operation_id: Some(parent_operation_id),
            root_operation_id: Some(root_operation_id),
        }
    }

    pub(crate) fn validate(&self) -> Result<(), CodingSessionError> {
        let invalid = |message: String| CodingSessionError::UnsupportedCapability {
            capability: format!("invalid operation admission: {message}"),
        };
        self.descriptor
            .validate()
            .map_err(|message| invalid(message.into()))?;
        if self.descriptor.revision != crate::runtime::outcome::OPERATION_DESCRIPTOR_REVISION {
            return Err(invalid("unsupported descriptor revision".into()));
        }
        if self.operation_id.is_empty()
            || self.operation_id != self.capability_snapshot.operation_id
        {
            return Err(invalid(
                "execution identity does not match its capability snapshot".into(),
            ));
        }
        if self.capability_generation != self.capability_snapshot.generation {
            return Err(invalid(
                "execution generation does not match its capability snapshot".into(),
            ));
        }
        match (
            self.origin,
            self.descriptor.lineage,
            &self.capability_snapshot.actor,
            self.parent_operation_id.as_deref(),
            self.root_operation_id.as_deref(),
        ) {
            (
                OperationOrigin::ClientRoot,
                crate::runtime::outcome::OperationLineage::Root,
                super::capability::ActorId::Client,
                None,
                Some(root),
            ) if root == self.operation_id => Ok(()),
            (
                OperationOrigin::ParentChild,
                crate::runtime::outcome::OperationLineage::Child,
                super::capability::ActorId::ChildOperation(actor_parent),
                Some(parent),
                Some(root),
            ) if !parent.is_empty()
                && !root.is_empty()
                && !actor_parent.is_empty()
                && actor_parent == parent =>
            {
                Ok(())
            }
            _ => Err(invalid(
                "origin, lineage, actor, parent, and root identities disagree".into(),
            )),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OperationDispatchMode {
    Async,
    SyncReadOnly,
    SyncMutable,
}

impl OperationDispatchMode {
    pub(crate) fn dispatcher_label(self) -> &'static str {
        match self {
            Self::Async => "async",
            Self::SyncReadOnly => "read-only sync",
            Self::SyncMutable => "sync mutable",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OperationOrigin {
    ClientRoot,
    ParentChild,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OperationClass {
    Query,
    ReadOnly,
    SessionWriteRoot,
    NonSessionRoot,
    RuntimeWrite,
    Child,
}

#[derive(Debug)]
pub(crate) enum OperationOutcome {
    Prompt(InternalPromptTurnOutcome),
    ManualCompaction(InternalPromptTurnOutcome),
    DelegationApproval,
    DelegationRejection,
    BranchSummary(InternalPromptTurnOutcome),
    SelfHealingEdit(SelfHealingEditOutcome),
    AgentInvocation(AgentInvocationOutcome),
    AgentTeam(AgentTeamOutcome),
    SetDefaultAgentProfile,
    ForkSession,
    SwitchActiveLeaf,
    SessionTreeLabelChanged {
        entry_id: String,
        label: Option<String>,
        updated_at: String,
    },
    SessionNameChanged {
        name: Option<String>,
        updated_at: String,
    },
    Export(ExportOutcome),
}

#[cfg(test)]
mod tests {
    use super::super::intent::IntentRouter;
    use super::*;
    use crate::app::bootstrap::PromptInvocation;
    use crate::operations::self_healing_edit::runner::{
        SelfHealingEditReplacement, SelfHealingEditRequest,
    };
    use crate::runtime::facade::CodingSessionError;

    #[test]
    fn prompt_operation_declares_root_session_write_metadata() {
        let operation = Operation::Prompt(PromptTurnOptions::new(PromptInvocation::Text(
            "hello".into(),
        )));

        assert_eq!(operation.kind(), OperationKind::Prompt);
        assert_eq!(operation.origin(), OperationOrigin::ClientRoot);
        assert_eq!(operation.class(), OperationClass::SessionWriteRoot);
    }

    #[test]
    fn admission_carries_root_identity_and_capability_generation() {
        let snapshot = OperationCapabilitySnapshot::permissive("op-root");
        let admission = OperationExecution::root(
            OperationKind::Prompt,
            Operation::Prompt(PromptTurnOptions::new(PromptInvocation::Text(
                "hello".into(),
            )))
            .descriptor(),
            OperationOrigin::ClientRoot,
            None,
            Some("session-root".into()),
            snapshot,
        );

        assert_eq!(admission.operation_id, "op-root");
        assert_eq!(admission.descriptor.revision, 1);
        assert_eq!(admission.session_identity.as_deref(), Some("session-root"));
        assert_eq!(admission.capability_generation.get(), 1);
        assert_eq!(admission.parent_operation_id, None);
        assert_eq!(admission.root_operation_id.as_deref(), Some("op-root"));
    }

    #[test]
    fn child_actor_admission_preserves_parent_lineage() {
        let mut snapshot = OperationCapabilitySnapshot::permissive("op-child");
        snapshot.actor = super::super::capability::ActorId::ChildOperation("op-parent".into());
        let admission = OperationExecution::child(
            OperationKind::AgentInvocation,
            crate::runtime::outcome::descriptor_for_child_kind(OperationKind::AgentInvocation)
                .unwrap(),
            snapshot,
            "op-parent".into(),
            "op-root".into(),
        );

        assert_eq!(admission.parent_operation_id.as_deref(), Some("op-parent"));
        assert_eq!(admission.root_operation_id.as_deref(), Some("op-root"));
    }

    #[test]
    fn manual_compaction_operation_declares_root_session_write_metadata() {
        let operation =
            Operation::ManualCompaction(PromptTurnOptions::new(PromptInvocation::Compact {
                custom_instructions: None,
            }));

        assert_eq!(operation.kind(), OperationKind::Compact);
        assert_eq!(operation.origin(), OperationOrigin::ClientRoot);
        assert_eq!(operation.class(), OperationClass::SessionWriteRoot);
    }

    #[test]
    fn delegation_approval_operation_declares_dynamic_session_write_metadata() {
        let operation = Operation::ApproveDelegationConfirmation {
            operation_id: "op_parent".into(),
            tool_call_id: "tool_delegate".into(),
        };

        assert_eq!(operation.static_kind(), None);
        assert_eq!(operation.origin(), OperationOrigin::ClientRoot);
        assert_eq!(operation.class(), OperationClass::SessionWriteRoot);
    }

    #[test]
    fn operation_descriptor_exposes_static_contract_and_dispatch_mode() {
        let operation = Operation::Export(ExportOptions::view());

        let descriptor = operation.descriptor();

        assert_eq!(operation.static_kind(), Some(OperationKind::Export));
        assert_eq!(operation.origin(), OperationOrigin::ClientRoot);
        assert_eq!(descriptor.admission_class(), OperationClass::ReadOnly);
        assert_eq!(
            descriptor.dispatch_mode,
            OperationDispatchMode::SyncReadOnly
        );
    }

    #[test]
    fn dynamic_operation_descriptor_exposes_dispatch_without_static_kind() {
        let operation = Operation::ApproveDelegationConfirmation {
            operation_id: "op_parent".into(),
            tool_call_id: "tool_delegate".into(),
        };

        let descriptor = operation.descriptor();

        assert_eq!(operation.static_kind(), None);
        assert_eq!(operation.origin(), OperationOrigin::ClientRoot);
        assert_eq!(
            descriptor.admission_class(),
            OperationClass::SessionWriteRoot
        );
        assert_eq!(descriptor.dispatch_mode, OperationDispatchMode::Async);
    }

    #[test]
    fn delegation_rejection_operation_declares_session_write_metadata() {
        let operation = Operation::RejectDelegationConfirmation {
            operation_id: "op_parent".into(),
            tool_call_id: "tool_delegate".into(),
            reason: "not needed".into(),
        };

        assert_eq!(operation.kind(), OperationKind::DelegationConfirmation);
        assert_eq!(operation.origin(), OperationOrigin::ClientRoot);
        assert_eq!(operation.class(), OperationClass::SessionWriteRoot);
    }

    #[test]
    fn session_access_follows_operation_side_effects_instead_of_dynamic_kind() {
        let approval = Operation::ApproveDelegationConfirmation {
            operation_id: "op_parent".into(),
            tool_call_id: "tool_delegate".into(),
        };
        let rejection = Operation::RejectDelegationConfirmation {
            operation_id: "op_parent".into(),
            tool_call_id: "tool_delegate".into(),
            reason: "not needed".into(),
        };

        assert_eq!(approval.static_kind(), None);
        assert_eq!(approval.session_access(), SessionCapabilityAccess::Write);
        assert_eq!(rejection.session_access(), SessionCapabilityAccess::Write);
        assert_eq!(
            Operation::Export(ExportOptions::view()).session_access(),
            SessionCapabilityAccess::Read
        );
    }

    #[test]
    fn branch_summary_operation_declares_root_session_write_metadata() {
        let operation = Operation::BranchSummary {
            options: PromptTurnOptions::new(PromptInvocation::Text("summarize".into())),
            source_leaf_id: "source_leaf".into(),
            target_leaf_id: "target_leaf".into(),
            custom_instructions: Some("keep details".into()),
            reuse_existing: false,
        };

        assert_eq!(operation.kind(), OperationKind::BranchSummary);
        assert_eq!(operation.origin(), OperationOrigin::ClientRoot);
        assert_eq!(operation.class(), OperationClass::SessionWriteRoot);
    }

    #[test]
    fn self_healing_edit_operation_declares_root_session_write_metadata() {
        let operation = Operation::SelfHealingEdit(SelfHealingEditRequest::new(
            "src/lib.rs",
            vec![SelfHealingEditReplacement::new("old", "new")],
        ));

        assert_eq!(operation.kind(), OperationKind::SelfHealingEdit);
        assert_eq!(operation.origin(), OperationOrigin::ClientRoot);
        assert_eq!(operation.class(), OperationClass::SessionWriteRoot);
    }

    #[test]
    fn agent_invocation_operation_declares_root_non_session_metadata() {
        let operation = Operation::AgentInvocation(AgentInvocationOptions::new(
            "helper",
            "summarize this",
            PromptTurnOptions::new(PromptInvocation::Text("task".into())),
        ));

        assert_eq!(operation.kind(), OperationKind::AgentInvocation);
        assert_eq!(operation.origin(), OperationOrigin::ClientRoot);
        assert_eq!(operation.class(), OperationClass::NonSessionRoot);
    }

    #[test]
    fn agent_team_operation_declares_root_non_session_metadata() {
        let operation = Operation::AgentTeam(AgentTeamOptions::new(
            "team",
            "summarize this",
            PromptTurnOptions::new(PromptInvocation::Text("task".into())),
        ));

        assert_eq!(operation.kind(), OperationKind::AgentTeam);
        assert_eq!(operation.origin(), OperationOrigin::ClientRoot);
        assert_eq!(operation.class(), OperationClass::NonSessionRoot);
    }

    #[test]
    fn export_operation_declares_root_read_only_metadata() {
        let operation = Operation::Export(ExportOptions::view());

        assert_eq!(operation.kind(), OperationKind::Export);
        assert_eq!(operation.origin(), OperationOrigin::ClientRoot);
        assert_eq!(operation.class(), OperationClass::ReadOnly);
    }

    #[test]
    fn set_default_agent_profile_operation_declares_runtime_write_metadata() {
        let profile_id = ProfileId::new("agent-main").expect("valid profile id");
        let operation = Operation::SetDefaultAgentProfile { profile_id };

        assert_eq!(operation.kind(), OperationKind::SetDefaultAgentProfile);
        assert_eq!(operation.origin(), OperationOrigin::ClientRoot);
        assert_eq!(operation.class(), OperationClass::RuntimeWrite);
        assert_eq!(
            operation.descriptor().dispatch_mode,
            OperationDispatchMode::SyncMutable
        );
    }

    #[test]
    fn fork_session_operation_declares_root_session_write_metadata() {
        let operation = Operation::ForkSession {
            target_leaf_id: Some("leaf_1".into()),
        };

        assert_eq!(operation.kind(), OperationKind::ForkSession);
        assert_eq!(operation.origin(), OperationOrigin::ClientRoot);
        assert_eq!(operation.class(), OperationClass::SessionWriteRoot);
        assert_eq!(
            operation.descriptor().dispatch_mode,
            OperationDispatchMode::SyncMutable
        );
    }

    #[test]
    fn intent_router_rejects_dynamic_operation_without_owner_resolution() {
        let operation = Operation::ApproveDelegationConfirmation {
            operation_id: "op_parent".into(),
            tool_call_id: "tool_delegate".into(),
        };

        let error = IntentRouter::static_admission(&operation).unwrap_err();

        assert_eq!(
            error,
            CodingSessionError::UnsupportedCapability {
                capability: "dynamic operation requires async dispatcher".into(),
            }
        );
    }

    #[test]
    fn operation_admission_carries_frozen_capability_snapshot() {
        use crate::runtime::capability::{
            ActorId, CapabilityGeneration, ModelCapability, OperationCapabilitySnapshot,
            ToolCapabilitySet,
        };

        let descriptor = Operation::Prompt(PromptTurnOptions::new(PromptInvocation::Text(
            "hello".into(),
        )))
        .descriptor();
        let snapshot = OperationCapabilitySnapshot {
            generation: CapabilityGeneration::new(7),
            operation_id: "op_admitted".into(),
            actor: ActorId::Client,
            model: Some(ModelCapability { profile_id: None }),
            tools: ToolCapabilitySet::from_names(["read".to_string()]),
            commands: Default::default(),
            filesystem: None,
            shell: None,
            session_read: None,
            session_write: None,
            ui: None,
        };

        let admission = OperationExecution::root(
            OperationKind::Prompt,
            descriptor,
            OperationOrigin::ClientRoot,
            Some("2026-07-09T00:00:00Z".into()),
            Some("session-test".into()),
            snapshot.clone(),
        );

        assert_eq!(admission.capability_snapshot, snapshot);
    }

    #[test]
    fn prompt_operation_outcome_exposes_prompt_payload() {
        let outcome = OperationOutcome::Prompt(InternalPromptTurnOutcome::Aborted {
            operation_id: "op_test".into(),
            turn_id: Some("turn_test".into()),
            reason: "user cancelled".into(),
            session_id: None,
        });

        assert!(matches!(
            outcome,
            OperationOutcome::Prompt(InternalPromptTurnOutcome::Aborted { reason, .. })
                if reason == "user cancelled"
        ));
    }
}
