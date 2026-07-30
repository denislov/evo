use super::capability::{ActorId, OperationCapabilitySnapshot};
use super::control::{OperationControl, OperationKind};
use super::intent::{OperationPermit, QueryIntent, QueryIntentMetadata};
use super::operation::{OperationClass, OperationDispatchMode, OperationExecution};
use crate::runtime::facade::CodingSessionError;
use crate::session::id::{IdGenerator, SystemIdGenerator};

/// Typed admission owner for runtime-affecting operations.
///
/// The scheduler intentionally delegates guard ownership to `OperationControl`
/// during the first migration slice. This keeps cancellation and prompt-control
/// lifetimes stable while making admission policy explicit and testable.
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
        let descriptor = crate::runtime::outcome::descriptor_for_child_kind(kind)
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
