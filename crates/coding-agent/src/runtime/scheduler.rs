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

    #[cfg(test)]
    pub(crate) fn classify(
        kind: OperationKind,
        class: OperationClass,
        dispatch: OperationDispatchMode,
        capability_snapshot: OperationCapabilitySnapshot,
    ) -> OperationExecution {
        OperationExecution::root(
            kind,
            crate::runtime::outcome::descriptor_for_test_admission(kind, class, dispatch),
            super::operation::OperationOrigin::ClientRoot,
            None,
            Some("scheduler-test-session".into()),
            capability_snapshot,
        )
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::operation::OperationOrigin;

    fn admission(class: OperationClass, dispatch: OperationDispatchMode) -> OperationExecution {
        OperationScheduler::classify(
            OperationKind::Export,
            class,
            dispatch,
            OperationCapabilitySnapshot::permissive("scheduler-test"),
        )
    }

    #[test]
    fn unguarded_classes_bypass_busy_root_guard() {
        let control = OperationControl::new();
        let root = control.begin(OperationKind::Prompt, "root".into()).unwrap();
        let permit = OperationScheduler::admit(
            &control,
            &admission(
                OperationClass::ReadOnly,
                OperationDispatchMode::SyncReadOnly,
            ),
            OperationDispatchMode::SyncReadOnly,
        )
        .expect("read-only operations should bypass the root guard");
        assert!(!permit.is_guarded());
        assert_eq!(permit.class(), OperationClass::ReadOnly);
        assert_eq!(control.active(), Some(OperationKind::Prompt));
        drop(root);
    }

    #[test]
    fn session_writer_and_non_session_root_use_independent_slots() {
        let control = OperationControl::new();
        let root = control.begin(OperationKind::Prompt, "root".into()).unwrap();

        let non_session = OperationScheduler::admit(
            &control,
            &admission(OperationClass::NonSessionRoot, OperationDispatchMode::Async),
            OperationDispatchMode::Async,
        )
        .expect("a non-session root may coexist with the session writer");
        assert_eq!(control.active(), Some(OperationKind::Prompt));

        let rejection = OperationScheduler::admit(
            &control,
            &admission(
                OperationClass::SessionWriteRoot,
                OperationDispatchMode::Async,
            ),
            OperationDispatchMode::Async,
        )
        .expect_err("a second session writer must be rejected");
        assert!(matches!(
            rejection,
            AdmissionRejection::Control(CodingSessionError::Busy { .. })
        ));

        drop(non_session);
        drop(root);
    }

    #[test]
    fn non_session_roots_obey_the_explicit_runtime_limit() {
        let control = OperationControl::with_non_session_root_limit(2);
        let first = control
            .begin_root(
                OperationClass::NonSessionRoot,
                OperationKind::AgentInvocation,
                "root-1".into(),
            )
            .unwrap();
        let second = control
            .begin_root(
                OperationClass::NonSessionRoot,
                OperationKind::AgentTeam,
                "root-2".into(),
            )
            .unwrap();

        assert_eq!(
            control
                .begin_root(
                    OperationClass::NonSessionRoot,
                    OperationKind::AgentInvocation,
                    "root-3".into(),
                )
                .unwrap_err(),
            CodingSessionError::Busy {
                operation: "agent_invocation".into(),
            }
        );

        drop(first);
        assert!(
            control
                .begin_root(
                    OperationClass::NonSessionRoot,
                    OperationKind::AgentInvocation,
                    "root-3".into(),
                )
                .is_ok()
        );
        drop(second);
    }

    #[test]
    fn runtime_write_is_exclusive_in_both_directions() {
        let control = OperationControl::new();
        let session_write = control
            .begin(OperationKind::Prompt, "session".into())
            .unwrap();
        let rejection = control
            .begin_root(
                OperationClass::RuntimeWrite,
                OperationKind::SetDefaultAgentProfile,
                "runtime".into(),
            )
            .unwrap_err();
        assert_eq!(
            rejection,
            CodingSessionError::Busy {
                operation: "prompt".into(),
            }
        );
        drop(session_write);

        let runtime_write = control
            .begin_root(
                OperationClass::RuntimeWrite,
                OperationKind::SetDefaultAgentProfile,
                "runtime".into(),
            )
            .unwrap();
        for (class, kind) in [
            (OperationClass::SessionWriteRoot, OperationKind::Prompt),
            (
                OperationClass::NonSessionRoot,
                OperationKind::AgentInvocation,
            ),
            (
                OperationClass::RuntimeWrite,
                OperationKind::SetDefaultAgentProfile,
            ),
        ] {
            assert!(matches!(
                control.begin_root(class, kind, "blocked".into()),
                Err(CodingSessionError::Busy { operation })
                    if operation == "set_default_agent_profile"
            ));
        }
        drop(runtime_write);
    }

    #[test]
    fn child_class_requires_the_dedicated_admission_path() {
        let control = OperationControl::new();
        let mut snapshot = OperationCapabilitySnapshot::permissive("child-op");
        snapshot.actor = ActorId::ChildOperation("parent-op".into());
        let child_execution = OperationExecution::child(
            OperationKind::Prompt,
            crate::runtime::outcome::descriptor_for_child_kind(OperationKind::Prompt).unwrap(),
            snapshot,
            "parent-op".into(),
            "parent-op".into(),
        );
        let rejection =
            OperationScheduler::admit(&control, &child_execution, OperationDispatchMode::Async)
                .expect_err("child class must not bypass dedicated admission");
        assert!(matches!(
            rejection,
            AdmissionRejection::DedicatedPathRequired {
                kind: OperationKind::Prompt,
                class: OperationClass::Child,
            }
        ));
        assert_eq!(control.active(), None);
    }

    #[test]
    fn dispatch_mismatch_is_rejected_before_control_mutation() {
        let control = OperationControl::new();
        let rejection = OperationScheduler::admit(
            &control,
            &admission(
                OperationClass::SessionWriteRoot,
                OperationDispatchMode::Async,
            ),
            OperationDispatchMode::SyncMutable,
        )
        .expect_err("dispatch mismatch must fail closed");
        assert!(matches!(
            rejection,
            AdmissionRejection::DispatchMismatch { .. }
        ));
        assert_eq!(control.active(), None);
    }

    #[test]
    fn malformed_root_identity_is_rejected_before_control_mutation() {
        let control = OperationControl::new();
        let mut malformed = admission(
            OperationClass::SessionWriteRoot,
            OperationDispatchMode::Async,
        );
        malformed.capability_snapshot.actor = ActorId::ChildOperation("parent".into());

        let rejection =
            OperationScheduler::admit(&control, &malformed, OperationDispatchMode::Async)
                .expect_err("child actor must not enter root admission");

        assert!(matches!(
            rejection,
            AdmissionRejection::InvalidExecution(CodingSessionError::UnsupportedCapability { .. })
        ));
        assert_eq!(control.active(), None);
    }

    #[test]
    fn classified_admission_keeps_client_root_origin() {
        let admission = admission(OperationClass::RuntimeWrite, OperationDispatchMode::Async);
        assert_eq!(admission.origin, OperationOrigin::ClientRoot);
        assert_eq!(
            admission.descriptor.admission_class(),
            OperationClass::RuntimeWrite
        );
    }

    #[test]
    fn child_admission_accepts_only_nonempty_parent_lineage() {
        let control = OperationControl::new();
        let _root = control
            .begin(OperationKind::AgentInvocation, "parent-op".into())
            .unwrap();
        let mut child = OperationCapabilitySnapshot::permissive("child-op");
        child.actor = ActorId::ChildOperation("parent-op".into());
        let permit = OperationScheduler::admit_child(&control, OperationKind::Prompt, child)
            .expect("child actor with active parent lineage should be admitted");
        assert!(permit.is_guarded());
        assert_eq!(permit.class(), OperationClass::Child);
        assert_eq!(permit.execution().operation_id, "child-op");
        assert_eq!(
            permit.execution().parent_operation_id.as_deref(),
            Some("parent-op")
        );
        assert_eq!(
            permit.execution().root_operation_id.as_deref(),
            Some("parent-op")
        );
        assert_eq!(control.child_count(), 1);
        drop(permit);
        assert_eq!(control.child_count(), 0);

        let root = OperationCapabilitySnapshot::permissive("root-op");
        let rejection =
            OperationScheduler::admit_child(&control, OperationKind::AgentInvocation, root)
                .expect_err("root actor must not enter the child admission path");
        assert!(matches!(
            rejection,
            AdmissionRejection::ChildLineageMissing {
                kind: OperationKind::AgentInvocation
            }
        ));

        let mut forbidden = OperationCapabilitySnapshot::permissive("forbidden-child");
        forbidden.actor = ActorId::ChildOperation("parent-op".into());
        assert!(matches!(
            OperationScheduler::admit_child(&control, OperationKind::Export, forbidden),
            Err(AdmissionRejection::ChildKindNotPermitted {
                kind: OperationKind::Export
            })
        ));
    }
}
