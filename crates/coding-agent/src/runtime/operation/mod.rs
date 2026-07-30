pub(crate) mod admission;
pub(crate) mod contract;
pub(crate) mod control;
mod dispatch;
#[cfg(test)]
mod dispatch_tests;
pub(crate) mod execution;
pub(crate) mod finalize;
pub(crate) mod permit;
pub(crate) mod submission;
#[cfg(test)]
mod tests;

use crate::operations::agent_invocation::runner::AgentInvocationOutcome;
use crate::operations::export::runner::ExportOutcome;
use crate::operations::prompt::context::InternalPromptTurnOutcome;
use crate::operations::self_healing_edit::runner::SelfHealingEditOutcome;
use crate::operations::team_invocation::runner::AgentTeamOutcome;
use crate::runtime::capability::{CapabilityGeneration, OperationCapabilitySnapshot};
use crate::runtime::error::CodingSessionError;
use control::OperationKind;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct OperationExecution {
    pub(crate) kind: OperationKind,
    pub(crate) descriptor: crate::runtime::operation::contract::OperationDescriptor,
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
        descriptor: crate::runtime::operation::contract::OperationDescriptor,
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
        descriptor: crate::runtime::operation::contract::OperationDescriptor,
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
        if self.descriptor.revision
            != crate::runtime::operation::contract::OPERATION_DESCRIPTOR_REVISION
        {
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
                crate::runtime::operation::contract::OperationLineage::Root,
                crate::runtime::capability::ActorId::Client,
                None,
                Some(root),
            ) if root == self.operation_id => Ok(()),
            (
                OperationOrigin::ParentChild,
                crate::runtime::operation::contract::OperationLineage::Child,
                crate::runtime::capability::ActorId::ChildOperation(actor_parent),
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
