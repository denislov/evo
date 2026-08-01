use super::contract::CodingAgentOperation;
use super::control::OperationControl;
use super::permit::OperationPermit;
use super::{OperationClass, OperationDispatchMode, OperationExecution, OperationOrigin};
use crate::runtime::capability::{ActorId, CapabilitySnapshotInput, OperationCapabilitySnapshot};
use crate::runtime::facade::{
    AgentProfile, CodingAgentSession, CodingSessionError, OperationKind, ProfileKind,
};
use crate::runtime::intent::{QueryIntent, QueryIntentMetadata};
use crate::session::id::{Clock, IdGenerator, SystemClock, SystemIdGenerator};
use crate::session::service::{SessionPersistence, session_cwd};
use std::path::PathBuf;

impl CodingAgentSession {
    pub(super) fn prepare_operation_for_admission(
        &self,
        operation: &mut CodingAgentOperation,
    ) -> Result<(), CodingSessionError> {
        match operation {
            CodingAgentOperation::Prompt(options)
            | CodingAgentOperation::Compact(options)
            | CodingAgentOperation::BranchSummary { options, .. } => {
                if options.runtime().is_some() {
                    *options = crate::operations::prompt::apply_default_agent_profile(
                        &self.runtime_host.session_coordinator.persistence,
                        &self.runtime_host.profile_registry,
                        options.clone(),
                    )?;
                }
            }
            CodingAgentOperation::SelfHealingEdit(request) => {
                if let Some(repair) = request.model_repair_mut()
                    && repair.prompt_options().runtime().is_some()
                {
                    let resolved = crate::operations::prompt::apply_default_agent_profile(
                        &self.runtime_host.session_coordinator.persistence,
                        &self.runtime_host.profile_registry,
                        repair.prompt_options().clone(),
                    )?;
                    *repair.prompt_options_mut() = resolved;
                }
            }
            CodingAgentOperation::ApproveDelegation { .. }
            | CodingAgentOperation::RejectDelegation { .. }
            | CodingAgentOperation::InvokeAgent(_)
            | CodingAgentOperation::InvokeTeam(_)
            | CodingAgentOperation::ForkSession { .. }
            | CodingAgentOperation::SwitchActiveLeaf { .. }
            | CodingAgentOperation::SetSessionTreeLabel { .. }
            | CodingAgentOperation::SetSessionName { .. }
            | CodingAgentOperation::ExportCurrent
            | CodingAgentOperation::ExportCurrentHtml(_) => {}
        }
        Ok(())
    }

    pub(super) fn resolve_operation_admission_with_id(
        &self,
        operation: &CodingAgentOperation,
        reserved_operation_id: Option<&str>,
    ) -> Result<OperationExecution, CodingSessionError> {
        let descriptor = operation.descriptor();
        let (kind, admitted_at, operation_runtime) = match operation {
            CodingAgentOperation::ApproveDelegation {
                operation_id,
                tool_call_id,
            } => {
                let now = SystemClock.now_rfc3339();
                let pending = crate::operations::delegation::confirmation::active_pending(
                    &self
                        .runtime_host
                        .session_coordinator
                        .pending_delegation_confirmations,
                    operation_id.as_str(),
                    tool_call_id.as_str(),
                    &now,
                )?;
                let kind = match pending.request.target_kind {
                    ProfileKind::Agent => OperationKind::AgentInvocation,
                    ProfileKind::Team => OperationKind::AgentTeam,
                };
                (kind, Some(now), pending.prompt_options.runtime().cloned())
            }
            _ => (
                operation.static_kind().ok_or_else(|| {
                    CodingSessionError::UnsupportedCapability {
                        capability: "dynamic operation requires async dispatcher".into(),
                    }
                })?,
                None,
                operation.runtime().cloned(),
            ),
        };
        let operation_id = reserved_operation_id
            .map(str::to_owned)
            .unwrap_or_else(|| self.next_operation_admission_id());
        let snapshot = self
            .runtime_host
            .operation_supervisor
            .capabilities
            .snapshot(self.snapshot_input_for_operation(
                operation_id,
                kind,
                operation,
                operation_runtime.as_ref(),
            ))?;
        let session_identity = Some(match &self.runtime_host.session_coordinator.persistence {
            SessionPersistence::Persistent(service) => service.session_id().to_owned(),
            SessionPersistence::NonPersistent(state) => state.runtime_id.clone(),
        });
        Ok(OperationExecution::root(
            kind,
            descriptor,
            OperationOrigin::ClientRoot,
            admitted_at,
            session_identity,
            snapshot,
        ))
    }

    pub(super) fn next_operation_admission_id(&self) -> String {
        let mut ids = SystemIdGenerator;
        ids.next_root_operation_id()
    }

    fn snapshot_input_for_operation(
        &self,
        operation_id: String,
        kind: OperationKind,
        operation: &CodingAgentOperation,
        operation_runtime: Option<&crate::operations::prompt::context::RuntimeSnapshot>,
    ) -> CapabilitySnapshotInput {
        let runtime_tools = self.operation_runtime_tool_names(operation_runtime);
        let profile_tools = match self.active_agent_profile() {
            Some(profile) if !profile.tools.is_empty() => profile.tools.clone(),
            _ => runtime_tools.clone(),
        };
        CapabilitySnapshotInput {
            operation_id,
            operation_kind: kind,
            session_access: operation.session_access(),
            actor: ActorId::Client,
            uses_model: operation_runtime.is_some(),
            model_profile_id: operation_runtime.and_then(|runtime| runtime.profile_id().cloned()),
            persistent_session: matches!(
                self.runtime_host.session_coordinator.persistence,
                SessionPersistence::Persistent(_)
            ),
            cwd: operation_runtime
                .and_then(|runtime| runtime.cwd().map(PathBuf::from))
                .or_else(|| self.cwd()),
            shell_path: operation_runtime
                .and_then(|runtime| runtime.settings())
                .and_then(|settings| settings.shell_path.clone()),
            shell_command_prefix: operation_runtime
                .and_then(|runtime| runtime.settings())
                .and_then(|settings| settings.shell_command_prefix.clone()),
            runtime_tools,
            profile_tools,
        }
    }

    fn operation_runtime_tool_names(
        &self,
        operation_runtime: Option<&crate::operations::prompt::context::RuntimeSnapshot>,
    ) -> Vec<String> {
        let mut names = self.current_runtime_tool_names();
        if let Some(runtime) = operation_runtime {
            names.extend(runtime.tools().iter().map(|tool| tool.name.clone()));
        }
        if let Some(profile) = self.active_agent_profile() {
            names.extend(
                crate::operations::delegation::delegation_tool_names(&profile.delegation)
                    .map(str::to_owned),
            );
        }
        names.sort();
        names.dedup();
        names
    }

    fn cwd(&self) -> Option<PathBuf> {
        match &self.runtime_host.session_coordinator.persistence {
            SessionPersistence::Persistent(session_service) => session_cwd(session_service),
            SessionPersistence::NonPersistent(_) => None,
        }
    }

    fn active_agent_profile(&self) -> Option<&AgentProfile> {
        let id = self.default_agent_profile_id();
        self.runtime_host.profile_registry.agent(id.as_str())
    }

    fn current_runtime_tool_names(&self) -> Vec<String> {
        crate::tools::PRODUCT_TOOL_NAMES
            .into_iter()
            .map(str::to_owned)
            .collect()
    }
}

/// Owns admission policy for runtime-affecting operations.
pub(crate) struct OperationScheduler;

impl OperationScheduler {
    pub(crate) fn allocate_child_operation_id() -> String {
        let mut ids = SystemIdGenerator;
        ids.next_child_operation_id()
    }

    pub(crate) fn admit(
        control: &OperationControl,
        admission: &OperationExecution,
        expected_dispatch: OperationDispatchMode,
    ) -> Result<OperationPermit, AdmissionRejection> {
        admission
            .validate()
            .map_err(AdmissionRejection::InvalidExecution)?;
        if admission.descriptor.dispatch_mode != expected_dispatch {
            return Err(AdmissionRejection::DispatchMismatch {
                kind: admission.kind,
                expected: expected_dispatch,
                actual: admission.descriptor.dispatch_mode,
            });
        }

        let class = admission.descriptor.admission_class();
        match class {
            OperationClass::Child => {
                return Err(AdmissionRejection::DedicatedPathRequired {
                    kind: admission.kind,
                    class,
                });
            }
            OperationClass::ReadOnly => {
                return Ok(OperationPermit::unguarded(
                    admission.kind,
                    class,
                    admission.clone(),
                ));
            }
            OperationClass::SessionWriteRoot
            | OperationClass::NonSessionRoot
            | OperationClass::RuntimeWrite => {}
            OperationClass::Query => unreachable!("queries do not create OperationExecution"),
        }

        control
            .begin_root_with_capability_generation(
                class,
                admission.kind,
                admission.capability_snapshot.operation_id.clone(),
                admission.capability_snapshot.generation,
            )
            .map(|guard| OperationPermit::guarded(admission.kind, class, guard, admission.clone()))
            .map_err(AdmissionRejection::Control)
    }

    pub(crate) fn admit_query(
        _control: &OperationControl,
        intent: QueryIntent,
    ) -> QueryIntentMetadata {
        intent.metadata()
    }

    pub(crate) fn admit_child(
        control: &OperationControl,
        kind: OperationKind,
        capability_snapshot: OperationCapabilitySnapshot,
    ) -> Result<OperationPermit, AdmissionRejection> {
        let descriptor = super::contract::descriptor_for_child_kind(kind)
            .ok_or(AdmissionRejection::ChildKindNotPermitted { kind })?;
        match &capability_snapshot.actor {
            ActorId::ChildOperation(parent_id) if !parent_id.is_empty() => {
                let parent_id = parent_id.clone();
                let guard = control
                    .begin_child_with_capability_generation(
                        kind,
                        capability_snapshot.operation_id.clone(),
                        parent_id,
                        capability_snapshot.generation,
                    )
                    .map_err(AdmissionRejection::Control)?;
                let execution = OperationExecution::child(
                    kind,
                    descriptor,
                    capability_snapshot,
                    guard.parent_operation_id().to_owned(),
                    guard.root_operation_id().to_owned(),
                );
                execution
                    .validate()
                    .map_err(AdmissionRejection::InvalidExecution)?;
                Ok(OperationPermit::child(kind, execution, guard))
            }
            _ => Err(AdmissionRejection::ChildLineageMissing { kind }),
        }
    }
}

#[derive(Debug)]
pub(crate) enum AdmissionRejection {
    DispatchMismatch {
        kind: OperationKind,
        expected: OperationDispatchMode,
        actual: OperationDispatchMode,
    },
    Control(CodingSessionError),
    InvalidExecution(CodingSessionError),
    ChildLineageMissing {
        kind: OperationKind,
    },
    ChildKindNotPermitted {
        kind: OperationKind,
    },
    DedicatedPathRequired {
        kind: OperationKind,
        class: OperationClass,
    },
}

impl AdmissionRejection {
    pub(crate) fn into_error(self) -> CodingSessionError {
        match self {
            Self::DispatchMismatch {
                kind,
                expected,
                actual,
            } => CodingSessionError::UnsupportedCapability {
                capability: format!(
                    "{} operation was sent to the wrong dispatcher (requires {}, received {})",
                    kind.as_str(),
                    expected.dispatcher_label(),
                    actual.dispatcher_label(),
                ),
            },
            Self::Control(error) => error,
            Self::InvalidExecution(error) => error,
            Self::ChildLineageMissing { kind } => CodingSessionError::UnsupportedCapability {
                capability: format!(
                    "{} child operation is missing a valid parent lineage",
                    kind.as_str()
                ),
            },
            Self::ChildKindNotPermitted { kind } => CodingSessionError::UnsupportedCapability {
                capability: format!(
                    "{} operation does not permit structured children",
                    kind.as_str()
                ),
            },
            Self::DedicatedPathRequired { kind, class } => {
                CodingSessionError::UnsupportedCapability {
                    capability: format!(
                        "{} {:?} operation requires its dedicated admission path",
                        kind.as_str(),
                        class,
                    ),
                }
            }
        }
    }
}
