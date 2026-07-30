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

    pub(crate) fn static_kind(&self) -> Option<OperationKind> {
        (!matches!(self, Self::ApproveDelegationConfirmation { .. }))
            .then_some(self.descriptor().submitted_kind)
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
