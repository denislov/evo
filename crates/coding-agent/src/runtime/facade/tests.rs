#[cfg(test)]
mod cases {
    use crate::events as public_event;
    use crate::runtime::control as operation_control;
    use std::{
        fs,
        sync::{Arc, Mutex},
    };

    use agent_core::api::agent::AgentResources;
    use agent_core::api::tool::{AgentTool, AgentToolOutput};
    use ai::api::client::AiClient;
    use ai::api::conversation::{AssistantMessage, ContentBlock, Context, Message, StopReason};
    use ai::api::model::{Model, ModelCost, ModelInput};
    use ai::api::provider::ApiProvider;
    use ai::api::stream::{AssistantMessageEvent, EventStream, StreamOptions};
    use ai::api::testing::{FauxProvider, FauxResponse, FauxToolCall};
    use async_stream::stream;
    use tokio::sync::oneshot;

    use super::super::*;
    use crate::app::bootstrap::{PromptInvocation, SessionRunOptions};
    use crate::app::prompt_runtime::PromptRuntimeOptions;
    use crate::operations::delegation::delegation_runtime_seed_from_prompt_options;
    use crate::operations::prompt::context::{DelegationRequest, InternalPromptTurnOutcome};
    use crate::runtime::control::PromptControlCommand;
    use crate::runtime::finalization::OperationFinalizer;
    use crate::runtime::operation::{
        Operation, OperationExecution, OperationOrigin, OperationOutcome,
    };
    use crate::runtime::outcome as public_operation;
    use crate::runtime::submission::SubmissionCommitGuard;
    use crate::session::event::{PersistedContentBlock, SessionEventData, SessionEventEnvelope};
    use crate::session::id::{Clock, SystemClock};
    use crate::session::replay::{MessageStatus, TranscriptItem};
    use crate::session::repository::StoreFailurePoint;

    fn model(api: &str) -> Model {
        Model {
            id: "test-model".into(),
            name: "Test Model".into(),
            api: api.into(),
            provider: "test".into(),
            base_url: String::new(),
            reasoning: false,
            thinking_level_map: None,
            input: vec![ModelInput::Text],
            cost: ModelCost::default(),
            context_window: 0,
            max_tokens: 0,
            headers: None,
            compat: None,
        }
    }

    fn prompt_options(api: &str, prompt: &str) -> PromptTurnOptions {
        prompt_options_with_tools(api, prompt, Vec::new())
    }

    fn pending_delegation_confirmation_state(
        target_kind: ProfileKind,
    ) -> PendingDelegationConfirmationState {
        PendingDelegationConfirmationState {
            request: DelegationRequest {
                operation_id: "op_parent".into(),
                turn_id: "turn_parent".into(),
                tool_call_id: "tool_delegate".into(),
                requesting_profile_id: ProfileId::from("parent"),
                target_kind,
                target_id: ProfileId::from("target"),
                task: "delegate this".into(),
            },
            prompt_options: PromptTurnOptions::new(PromptInvocation::Text("delegated task".into())),
            reason: "requires confirmation".into(),
            requested_at: SystemClock.now_rfc3339(),
            child_delegation_depth: 1,
            delegation_lineage: Vec::new(),
        }
    }

    fn queue_persistent_delegation_confirmation(
        session: &mut CodingAgentSession,
        operation_id: &str,
        tool_call_id: &str,
        target_kind: ProfileKind,
    ) {
        let mut pending = pending_delegation_confirmation_state(target_kind);
        pending.request.operation_id = operation_id.into();
        pending.request.tool_call_id = tool_call_id.into();
        pending.request.target_id = ProfileId::from("default");
        pending.prompt_options = prompt_options(
            "coding-session-canonical-delegation-decision",
            "delegated task",
        );
        crate::operations::delegation::confirmation::queue_pending(
            &mut session.runtime_host.session_coordinator.persistence,
            &mut session
                .runtime_host
                .session_coordinator
                .pending_delegation_confirmations,
            &session.runtime_host.event_hub.service,
            pending,
            true,
        )
        .unwrap();
    }

    #[tokio::test]
    async fn interactive_store_and_pending_delegation_bridge_arms_real_fixtures() {
        let temp = tempfile::tempdir().unwrap();

        let append_options = CodingAgentSessionOptions::new()
            .with_session_id("sess_interactive_append_bridge")
            .with_session_log_root(temp.path());
        let mut append_session = CodingAgentSession::create_internal(append_options)
            .await
            .unwrap();
        append_session.queue_pending_delegation_for_tests("op_append", "tool_append");
        append_session.arm_append_events_failure_for_tests(0);
        let append_error = append_session
            .run_internal(CodingAgentOperation::RejectDelegation {
                operation_id: "op_append".into(),
                tool_call_id: "tool_append".into(),
                reason: "declined".into(),
            })
            .await
            .unwrap_err();
        assert_eq!(append_error.code(), "session_write_rejected");

        let manifest_options = CodingAgentSessionOptions::new()
            .with_session_id("sess_interactive_manifest_bridge")
            .with_session_log_root(temp.path());
        let mut manifest_session = CodingAgentSession::create_internal(manifest_options)
            .await
            .unwrap();
        manifest_session.queue_pending_delegation_for_tests("op_manifest", "tool_manifest");
        manifest_session.arm_update_manifest_failure_for_tests(0);
        let manifest_error = manifest_session
            .run_internal(CodingAgentOperation::RejectDelegation {
                operation_id: "op_manifest".into(),
                tool_call_id: "tool_manifest".into(),
                reason: "declined".into(),
            })
            .await
            .unwrap_err();
        assert_eq!(manifest_error.code(), "partial_commit");

        let pending_options = CodingAgentSessionOptions::new()
            .with_session_id("sess_interactive_pending_bridge")
            .with_session_log_root(temp.path());
        let mut pending_session = CodingAgentSession::create_internal(pending_options.clone())
            .await
            .unwrap();
        pending_session.queue_pending_delegation_for_tests("op_pending", "tool_pending");
        let pending = pending_session
            .runtime_host
            .session_coordinator
            .pending_delegation_confirmations();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].operation_id, "op_pending");
        assert_eq!(pending[0].tool_call_id, "tool_pending");

        let reopened = CodingAgentSession::open_internal(pending_options)
            .await
            .unwrap();
        let reopened_pending = reopened.pending_delegation_confirmations();
        assert_eq!(reopened_pending.len(), 1);
        assert_eq!(reopened_pending[0].operation_id, "op_pending");
        assert_eq!(reopened_pending[0].tool_call_id, "tool_pending");
    }

    fn prompt_options_with_tools(
        api: &str,
        prompt: &str,
        tools: Vec<AgentTool>,
    ) -> PromptTurnOptions {
        PromptTurnOptions::from_prompt_runtime_options(PromptRuntimeOptions {
            model: model(api),
            api_key: None,
            auth_diagnostics: Vec::new(),
            system_prompt: Some("system".into()),
            max_turns: Some(2),
            tools,
            register_builtins: false,
            ai_client: None,
            session: Some(SessionRunOptions::disabled(".".into())),
            session_target: None,
            session_name: None,
            thinking_level: None,
            tool_execution: None,
            resources: AgentResources::default(),
            settings: None,
            invocation: PromptInvocation::Text(prompt.into()),
        })
    }

    fn prompt_options_with_session_naming_model(
        api: &str,
        prompt: &str,
        model_id: &str,
    ) -> PromptTurnOptions {
        PromptTurnOptions::from_prompt_runtime_options(PromptRuntimeOptions {
            model: model(api),
            api_key: None,
            auth_diagnostics: Vec::new(),
            system_prompt: Some("system".into()),
            max_turns: Some(2),
            tools: Vec::new(),
            register_builtins: false,
            ai_client: None,
            session: Some(SessionRunOptions::disabled(".".into())),
            session_target: None,
            session_name: None,
            thinking_level: None,
            tool_execution: None,
            resources: AgentResources::default(),
            settings: Some(
                crate::config::settings::PartialSettings {
                    session_naming_model: Some(model_id.into()),
                    ..crate::config::settings::PartialSettings::default()
                }
                .resolve(),
            ),
            invocation: PromptInvocation::Text(prompt.into()),
        })
    }

    fn prompt_options_with_auto_naming(api: &str, prompt: &str) -> PromptTurnOptions {
        PromptTurnOptions::from_prompt_runtime_options(PromptRuntimeOptions {
            model: model(api),
            api_key: None,
            auth_diagnostics: Vec::new(),
            system_prompt: Some("system".into()),
            max_turns: Some(2),
            tools: Vec::new(),
            register_builtins: false,
            ai_client: None,
            session: Some(SessionRunOptions::disabled(".".into())),
            session_target: None,
            session_name: None,
            thinking_level: None,
            tool_execution: None,
            resources: AgentResources::default(),
            settings: Some(crate::config::settings::PartialSettings::default().resolve()),
            invocation: PromptInvocation::Text(prompt.into()),
        })
    }

    async fn wait_for_session_name(options: &CodingAgentSessionOptions) -> Option<String> {
        for _ in 0..100 {
            let hydration = CodingAgentSession::hydrate(options.clone()).unwrap();
            if hydration.summary.name.is_some() {
                return hydration.summary.name;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        None
    }

    async fn wait_for_session_event(
        root: &std::path::Path,
        session_id: &str,
        predicate: impl Fn(&SessionEventData) -> bool,
    ) -> Vec<SessionEventEnvelope> {
        for _ in 0..100 {
            let store = crate::session::repository::SessionLogStore::new(root);
            let handle = store.open_session_id(session_id).unwrap();
            let events = store.read_events(&handle).unwrap();
            if events.iter().any(|event| predicate(&event.data)) {
                return events;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        panic!("timed out waiting for session event")
    }

    #[tokio::test]
    async fn ui_snapshot_uses_session_view_capabilities_and_event_cursor() {
        let session = CodingAgentSession::non_persistent_internal(CodingAgentSessionOptions::new())
            .await
            .unwrap();
        let snapshot = session.ui_snapshot(Vec::new());

        assert_eq!(snapshot.session, session.view());
        assert_eq!(snapshot.capabilities, session.capabilities());
        assert_eq!(
            snapshot.cursor.last_event_sequence,
            session
                .runtime_host
                .event_hub
                .service
                .current_product_sequence()
        );
        assert_eq!(
            snapshot.cursor.capability_generation,
            session.current_capability_generation_for_tests()
        );
        assert_eq!(snapshot.active_operation, None);
    }

    #[tokio::test]
    async fn startup_recovery_product_event_is_visible_to_first_subscriber() {
        let temp = tempfile::tempdir().unwrap();
        let store = crate::session::repository::SessionLogStore::new(temp.path());
        let handle = store
            .create_session(crate::session::repository::CreateSessionOptions::new(
                "sess_startup_recovery_projection",
                "2026-07-09T00:00:00Z",
            ))
            .unwrap();
        let started = SessionEventEnvelope::new(
            "sess_startup_recovery_projection",
            "evt_started",
            "2026-07-09T00:00:01Z",
            SessionEventData::OperationStarted {
                operation: crate::session::event::OperationKind::Prompt,
                runtime_generation: crate::session::event::PersistedRuntimeGenerationRef {
                    profile_id: Some("default".into()),
                    capability_generation: Some(9),
                },
            },
        )
        .with_operation_id("op_in_doubt");
        store.append_events(&handle, &[started]).unwrap();

        let session = CodingAgentSession::open_internal(
            CodingAgentSessionOptions::new()
                .with_session_id("sess_startup_recovery_projection")
                .with_session_log_root(temp.path()),
        )
        .await
        .unwrap();
        assert_eq!(
            session.recovery_pending_internal().unwrap(),
            vec![crate::runtime::facade::CodingAgentRecoveryPending {
                operation_id: "op_in_doubt".into(),
                recovery_id: "recovery_pending:sess_startup_recovery_projection/op_in_doubt".into(),
                operation_kind: Some("prompt".into()),
                record_version: 1,
                descriptor_revision: 1,
                capability_generation: Some(9),
                attempt_count: 0,
                last_attempt_at: None,
                next_attempt_at: None,
            }]
        );
        let mut receiver = session.subscribe_product_events();

        let event = receiver
            .try_recv()
            .unwrap()
            .expect("startup recovery should be projected after subscription");
        assert!(matches!(
            event.event(),
            CodingAgentProductEventKind::Workflow(
                CodingAgentWorkflowProductEvent::OperationRecoveryPending { operation_id, .. }
            ) if operation_id == "op_in_doubt"
        ));
        assert_eq!(event.capability_generation(), Some(9));
        assert_eq!(event.root_operation_id(), Some("op_in_doubt"));
        assert_eq!(event.session_id(), Some("sess_startup_recovery_projection"));
        assert_eq!(event.terminal_operation(), None);
        assert_eq!(event.terminal_status(), None);
        let recovery_id = match event.event() {
            CodingAgentProductEventKind::Workflow(
                CodingAgentWorkflowProductEvent::OperationRecoveryPending { recovery_id, .. },
            ) => recovery_id.clone(),
            _ => unreachable!("recovery event checked above"),
        };
        assert_eq!(
            recovery_id,
            "recovery_pending:sess_startup_recovery_projection/op_in_doubt"
        );
        assert_eq!(
            event.durability(),
            &crate::events::CodingAgentProductEventDurability::DerivedFromSession {
                session_id: "sess_startup_recovery_projection".into(),
                source_operation_id: "op_in_doubt".into(),
                recovery_id,
            }
        );
    }

    #[tokio::test]
    async fn recovery_resolution_is_version_guarded_audited_terminal_and_restartable() {
        let temp = tempfile::tempdir().unwrap();
        let store = crate::session::repository::SessionLogStore::new(temp.path());
        let handle = store
            .create_session(crate::session::repository::CreateSessionOptions::new(
                "sess_recovery_resolve",
                "2026-07-19T00:00:00Z",
            ))
            .unwrap();
        store
            .append_events(
                &handle,
                &[SessionEventEnvelope::new(
                    "sess_recovery_resolve",
                    "evt_started",
                    "2026-07-19T00:00:01Z",
                    SessionEventData::OperationStarted {
                        operation: crate::session::event::OperationKind::Prompt,
                        runtime_generation: crate::session::event::PersistedRuntimeGenerationRef {
                            profile_id: Some("default".into()),
                            capability_generation: Some(11),
                        },
                    },
                )
                .with_operation_id("op_recovery_resolve")],
            )
            .unwrap();
        let options = CodingAgentSessionOptions::new()
            .with_session_id("sess_recovery_resolve")
            .with_session_log_root(temp.path());
        let mut session = CodingAgentSession::open_internal(options.clone())
            .await
            .unwrap();
        let pending = session.recovery_pending_internal().unwrap().pop().unwrap();
        let mut stale = crate::runtime::facade::CodingAgentRecoveryResolutionRequest::from_pending(
            &pending,
            crate::events::CodingAgentRecoveryResolution::Failed,
            "operator rejected uncertain commit",
        );
        stale.expected_record_version += 1;
        let error = session.resolve_recovery_internal(stale).unwrap_err();
        assert!(error.to_string().contains("version is stale"));
        assert_eq!(
            session.recovery_pending_internal().unwrap(),
            vec![pending.clone()]
        );
        let mut stale_attempt =
            crate::runtime::facade::CodingAgentRecoveryResolutionRequest::from_pending(
                &pending,
                crate::events::CodingAgentRecoveryResolution::Failed,
                "operator rejected uncertain commit",
            );
        stale_attempt.expected_attempt_count += 1;
        let error = session
            .resolve_recovery_internal(stale_attempt)
            .unwrap_err();
        assert!(error.to_string().contains("attempt count is stale"));
        assert_eq!(
            session.recovery_pending_internal().unwrap(),
            vec![pending.clone()]
        );

        let mut receiver = session.subscribe_product_events();
        let _pending_event = receiver.try_recv().unwrap().unwrap();
        let result = session
            .resolve_recovery_internal(
                crate::runtime::facade::CodingAgentRecoveryResolutionRequest::from_pending(
                    &pending,
                    crate::events::CodingAgentRecoveryResolution::Failed,
                    "token=super-secret-value operator rejected uncertain commit",
                ),
            )
            .unwrap();
        assert_eq!(result.operation_id, "op_recovery_resolve");
        assert_eq!(result.recovery_id, pending.recovery_id);
        assert_eq!(
            result.resolution,
            crate::events::CodingAgentRecoveryResolution::Failed
        );
        assert!(session.recovery_pending_internal().unwrap().is_empty());
        let terminal = receiver.try_recv().unwrap().unwrap();
        assert_eq!(
            terminal.terminal_status(),
            Some(crate::events::CodingAgentProductEventTerminalStatus::Failed)
        );
        assert_eq!(
            terminal.terminal_operation(),
            Some(crate::events::CodingAgentProductEventTerminalOperation {
                kind: crate::events::CodingAgentProductEventTerminalOperationKind::Prompt,
                status: crate::events::CodingAgentProductEventTerminalStatus::Failed,
            })
        );
        assert!(matches!(
            terminal.event(),
            CodingAgentProductEventKind::Workflow(
                CodingAgentWorkflowProductEvent::OperationRecoveryResolved {
                    reason,
                    descriptor_revision: 1,
                    capability_generation: Some(11),
                    ..
                }
            ) if reason.contains("token=<redacted>")
                && !reason.contains("super-secret-value")
        ));

        let durable_outbox = store.read_outbox(&handle).unwrap();
        assert!(durable_outbox.iter().any(|record| matches!(
            record.draft.event,
            CodingAgentProductEventKind::Workflow(
                CodingAgentWorkflowProductEvent::OperationRecoveryResolved { .. }
            )
        )));

        drop(session);
        let reopened = CodingAgentSession::open_internal(options).await.unwrap();
        assert!(reopened.recovery_pending_internal().unwrap().is_empty());
        let restarted_events = reopened
            .runtime_host
            .event_hub
            .service
            .product_events_after(crate::events::ProductEventSequence::default())
            .unwrap();
        let redelivered_terminal = restarted_events
            .into_iter()
            .find(|event| {
                matches!(
                    event.event(),
                    CodingAgentProductEventKind::Workflow(
                        CodingAgentWorkflowProductEvent::OperationRecoveryResolved { .. }
                    )
                )
            })
            .unwrap();
        assert_eq!(
            redelivered_terminal.terminal_operation(),
            Some(crate::events::CodingAgentProductEventTerminalOperation {
                kind: crate::events::CodingAgentProductEventTerminalOperationKind::Prompt,
                status: crate::events::CodingAgentProductEventTerminalStatus::Failed,
            })
        );
        assert_eq!(redelivered_terminal.capability_generation(), Some(11));

        let reopened_handle = store.open_session_id("sess_recovery_resolve").unwrap();
        let serialized = store
            .read_events(&reopened_handle)
            .unwrap()
            .into_iter()
            .map(|event| serde_json::to_string(&event).unwrap())
            .collect::<String>();
        assert!(serialized.contains("operation.recovery_resolved"));
        assert!(serialized.contains("operation.failed"));
        assert!(serialized.contains("operation.terminal.recorded"));
        assert!(serialized.contains("authorization_subject"));
        assert!(serialized.contains("trusted_host"));
        assert!(serialized.contains("token=<redacted>"));
        assert!(!serialized.contains("super-secret-value"));
    }

    #[tokio::test]
    async fn recovery_retry_is_bounded_durable_and_non_terminal() {
        let temp = tempfile::tempdir().unwrap();
        let store = crate::session::repository::SessionLogStore::new(temp.path());
        let handle = store
            .create_session(crate::session::repository::CreateSessionOptions::new(
                "sess_recovery_retry",
                "2026-07-19T00:00:00Z",
            ))
            .unwrap();
        store
            .append_events(
                &handle,
                &[SessionEventEnvelope::new(
                    "sess_recovery_retry",
                    "evt_started",
                    "2026-07-19T00:00:01Z",
                    SessionEventData::OperationStarted {
                        operation: crate::session::event::OperationKind::Prompt,
                        runtime_generation: crate::session::event::PersistedRuntimeGenerationRef {
                            capability_generation: Some(12),
                            ..Default::default()
                        },
                    },
                )
                .with_operation_id("op_recovery_retry")],
            )
            .unwrap();
        let options = CodingAgentSessionOptions::new()
            .with_session_id("sess_recovery_retry")
            .with_session_log_root(temp.path());
        let mut session = CodingAgentSession::open_internal(options.clone())
            .await
            .unwrap();
        let mut pending = session.recovery_pending_internal().unwrap().pop().unwrap();
        assert_eq!(pending.attempt_count, 0);
        for expected_attempt in 1..=3 {
            let request =
                crate::runtime::facade::CodingAgentRecoveryRetryRequest::from_pending(&pending);
            let request = if expected_attempt == 1 {
                request.with_backoff()
            } else {
                request
            };
            let stale_request = request.clone();
            let result = session.retry_recovery_internal(request).unwrap();
            assert_eq!(result.attempt_count, expected_attempt);
            assert_eq!(result.next_attempt_at.is_some(), expected_attempt == 1);
            if expected_attempt == 1 {
                let last = time::OffsetDateTime::parse(
                    &result.last_attempt_at,
                    &time::format_description::well_known::Rfc3339,
                )
                .unwrap();
                let next = time::OffsetDateTime::parse(
                    result.next_attempt_at.as_deref().unwrap(),
                    &time::format_description::well_known::Rfc3339,
                )
                .unwrap();
                assert_eq!(next - last, time::Duration::seconds(1));
            }
            pending = session.recovery_pending_internal().unwrap().pop().unwrap();
            assert_eq!(pending.attempt_count, expected_attempt);
            assert!(pending.last_attempt_at.is_some());
            assert_eq!(pending.next_attempt_at.is_some(), expected_attempt == 1);
            if expected_attempt == 1 {
                let error = session.retry_recovery_internal(stale_request).unwrap_err();
                assert!(error.to_string().contains("attempt count is stale"));
                assert_eq!(
                    session.recovery_pending_internal().unwrap().pop().unwrap(),
                    pending
                );
            }
        }
        let before = store.read_events(&handle).unwrap().len();
        let error = session
            .retry_recovery_internal(
                crate::runtime::facade::CodingAgentRecoveryRetryRequest::from_pending(&pending),
            )
            .unwrap_err();
        assert!(error.to_string().contains("retry limit reached"));
        assert_eq!(store.read_events(&handle).unwrap().len(), before);
        assert_eq!(
            session
                .runtime_host
                .event_hub
                .service
                .product_events_after(crate::events::ProductEventSequence::default())
                .unwrap()
                .iter()
                .filter(|event| event.terminal_status().is_some())
                .count(),
            0
        );
        drop(session);

        let reopened = CodingAgentSession::open_internal(options).await.unwrap();
        let reopened_pending = reopened.recovery_pending_internal().unwrap();
        assert_eq!(reopened_pending.len(), 1);
        assert_eq!(reopened_pending[0].attempt_count, 3);
        assert!(reopened_pending[0].last_attempt_at.is_some());
        assert!(reopened_pending[0].next_attempt_at.is_none());
        let events = store
            .read_events(&store.open_session_id("sess_recovery_retry").unwrap())
            .unwrap();
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(
                    event.data,
                    SessionEventData::OperationRecoveryPending {
                        attempt_count: 3,
                        ..
                    }
                ))
                .count(),
            1
        );
    }

    #[tokio::test]
    async fn due_recovery_retry_runs_once_on_session_open_without_terminal() {
        let temp = tempfile::tempdir().unwrap();
        let store = crate::session::repository::SessionLogStore::new(temp.path());
        let handle = store
            .create_session(crate::session::repository::CreateSessionOptions::new(
                "sess_recovery_due",
                "2026-07-19T00:00:00Z",
            ))
            .unwrap();
        store
            .append_events(
                &handle,
                &[SessionEventEnvelope::new(
                    "sess_recovery_due",
                    "evt_started",
                    "2026-07-19T00:00:01Z",
                    SessionEventData::OperationStarted {
                        operation: crate::session::event::OperationKind::Prompt,
                        runtime_generation: Default::default(),
                    },
                )
                .with_operation_id("op_recovery_due")],
            )
            .unwrap();
        let options = CodingAgentSessionOptions::new()
            .with_session_id("sess_recovery_due")
            .with_session_log_root(temp.path());
        let mut session = CodingAgentSession::open_internal(options.clone())
            .await
            .unwrap();
        let pending = session.recovery_pending_internal().unwrap().pop().unwrap();
        let scheduled = session
            .retry_recovery_internal(
                crate::runtime::facade::CodingAgentRecoveryRetryRequest::from_pending(&pending)
                    .with_backoff(),
            )
            .unwrap();
        assert!(scheduled.next_attempt_at.is_some());
        drop(session);

        tokio::time::sleep(std::time::Duration::from_millis(1_100)).await;
        let reopened = CodingAgentSession::open_internal(options).await.unwrap();
        let pending = reopened.recovery_pending_internal().unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].attempt_count, 2);
        assert!(pending[0].next_attempt_at.is_none());
        assert!(
            reopened
                .runtime_host
                .event_hub
                .service
                .product_events_after(crate::events::ProductEventSequence::default())
                .unwrap()
                .iter()
                .all(|event| event.terminal_status().is_none())
        );

        let events = store
            .read_events(&store.open_session_id("sess_recovery_due").unwrap())
            .unwrap();
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(
                    event.data,
                    SessionEventData::OperationRecoveryPending {
                        attempt_count: 2,
                        ..
                    }
                ))
                .count(),
            1
        );
    }

    #[tokio::test]
    async fn public_product_event_receiver_maps_internal_product_events() {
        let session = CodingAgentSession::non_persistent_internal(CodingAgentSessionOptions::new())
            .await
            .unwrap();
        let mut receiver = session.subscribe_product_events_public();
        session
            .runtime_host
            .event_hub
            .service
            .emit_diagnostic(None::<String>, "public event");

        let event = receiver.recv().await.unwrap();
        assert_eq!(event.sequence(), 1);
        assert!(matches!(
            event.event(),
            CodingAgentProductEventKind::Diagnostic(
                CodingAgentDiagnosticProductEvent::Diagnostic { diagnostic }
            ) if diagnostic.summary == "public event"
        ));
    }

    #[tokio::test]
    async fn public_product_event_receiver_supports_non_blocking_receive() {
        let session = CodingAgentSession::non_persistent_internal(CodingAgentSessionOptions::new())
            .await
            .unwrap();
        let mut receiver = session.subscribe_product_events_public();

        assert_eq!(receiver.try_recv().unwrap(), None);

        session
            .runtime_host
            .event_hub
            .service
            .emit_diagnostic(None::<String>, "public event");
        let event = receiver
            .try_recv()
            .unwrap()
            .expect("emitted event should be available without blocking");
        assert_eq!(event.sequence(), 1);
        assert!(matches!(
            event.event(),
            CodingAgentProductEventKind::Diagnostic(
                CodingAgentDiagnosticProductEvent::Diagnostic { diagnostic }
            ) if diagnostic.summary == "public event"
        ));
    }

    #[tokio::test]
    async fn stale_persistent_delegation_confirmation_is_not_restored_as_pending() {
        let temp = tempfile::tempdir().unwrap();
        let store = crate::session::repository::SessionLogStore::new(temp.path());
        let handle = store
            .create_session(crate::session::repository::CreateSessionOptions::new(
                "sess_stale_delegation_confirmation",
                "2026-01-01T00:00:00Z",
            ))
            .unwrap();
        let runtime_seed = delegation_runtime_seed_from_prompt_options(
            &prompt_options("stale-delegation-api", "plan feature"),
            1,
            &[],
        )
        .unwrap();
        store
            .append_events(
                &handle,
                &[
                    SessionEventEnvelope::new(
                        "sess_stale_delegation_confirmation",
                        "evt_1",
                        "2026-01-01T00:00:00Z",
                        SessionEventData::SessionCreated {
                            cwd: Some(".".to_string()),
                            workspace_scope: None,
                        },
                    ),
                    SessionEventEnvelope::new(
                        "sess_stale_delegation_confirmation",
                        "evt_2",
                        "2026-01-01T00:00:00Z",
                        SessionEventData::DelegationConfirmationRequested {
                            source_operation_id: "op_parent".to_string(),
                            turn_id: "turn_parent".to_string(),
                            tool_call_id: "tool_delegate_agent".to_string(),
                            requesting_profile_id: ProfileId::from("delegating-planner"),
                            target_kind: ProfileKind::Agent,
                            target_id: ProfileId::from("coder"),
                            task: "implement parser".to_string(),
                            reason: "delegation policy requires confirmation".to_string(),
                            runtime_seed,
                        },
                    )
                    .with_operation_id("op_parent")
                    .with_turn_id("turn_parent"),
                    SessionEventEnvelope::new(
                        "sess_stale_delegation_confirmation",
                        "evt_3",
                        "2026-01-01T00:00:01Z",
                        SessionEventData::OperationCommitted { new_leaf_id: None },
                    )
                    .with_operation_id("op_parent")
                    .with_turn_id("turn_parent"),
                ],
            )
            .unwrap();
        let replay = store.replay_session(&handle).unwrap();
        assert_eq!(replay.pending_delegation_confirmations.len(), 1);

        let mut session = CodingAgentSession::open_internal(
            CodingAgentSessionOptions::new()
                .with_session_id("sess_stale_delegation_confirmation")
                .with_session_log_root(temp.path()),
        )
        .await
        .unwrap();

        assert!(
            session
                .runtime_host
                .session_coordinator
                .pending_delegation_confirmations()
                .is_empty()
        );
        let error = session
            .run_internal(CodingAgentOperation::ApproveDelegation {
                operation_id: "op_parent".into(),
                tool_call_id: "tool_delegate_agent".into(),
            })
            .await
            .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("pending delegation confirmation not found"),
            "{error}"
        );
    }

    #[test]
    fn delegation_runtime_seed_strips_model_headers() {
        let mut runtime_model = model("delegation-seed-api");
        runtime_model.headers = Some(serde_json::json!({
            "authorization": "Bearer secret",
            "x-model": "metadata",
        }));
        let options = PromptTurnOptions::from_prompt_runtime_options(PromptRuntimeOptions {
            model: runtime_model,
            api_key: Some("secret-key".into()),
            auth_diagnostics: Vec::new(),
            system_prompt: Some("system".into()),
            max_turns: Some(2),
            tools: Vec::new(),
            register_builtins: false,
            ai_client: None,
            session: Some(SessionRunOptions::disabled(".".into())),
            session_target: None,
            session_name: None,
            thinking_level: None,
            tool_execution: None,
            resources: AgentResources::default(),
            settings: None,
            invocation: PromptInvocation::Text("plan".into()),
        });

        let seed = delegation_runtime_seed_from_prompt_options(&options, 1, &[]).unwrap();

        assert_eq!(seed.model.id, "test-model");
        assert!(seed.model.headers.is_none());
    }

    fn compact_options(api: &str, custom_instructions: Option<&str>) -> PromptTurnOptions {
        PromptTurnOptions::from_prompt_runtime_options(PromptRuntimeOptions {
            model: model(api),
            api_key: None,
            auth_diagnostics: Vec::new(),
            system_prompt: Some("system".into()),
            max_turns: Some(2),
            tools: Vec::new(),
            register_builtins: false,
            ai_client: None,
            session: Some(SessionRunOptions::disabled(".".into())),
            session_target: None,
            session_name: None,
            thinking_level: None,
            tool_execution: None,
            resources: AgentResources::default(),
            settings: None,
            invocation: PromptInvocation::Compact {
                custom_instructions: custom_instructions.map(str::to_owned),
            },
        })
    }

    fn prompt_outcome(outcome: CodingAgentOperationOutcome) -> PromptTurnOutcome {
        match outcome {
            CodingAgentOperationOutcome::Prompt(outcome) => outcome,
            other => panic!("expected prompt outcome, got {other:?}"),
        }
    }

    fn compact_outcome(outcome: CodingAgentOperationOutcome) -> PromptTurnOutcome {
        match outcome {
            CodingAgentOperationOutcome::Compact(outcome) => outcome,
            other => panic!("expected compaction outcome, got {other:?}"),
        }
    }

    fn echo_tool() -> AgentTool {
        AgentTool {
            name: "echo".into(),
            description: "echoes input".into(),
            parameters: serde_json::json!({
                "type": "object",
                "x-evo-authorization-risk": "workspace_local_read_only"
            }),
            execution_mode: None,
            execute: Arc::new(|_context, args, _on_update| {
                let text = args
                    .get("text")
                    .and_then(|value| value.as_str())
                    .unwrap_or("")
                    .to_owned();
                Box::pin(async move {
                    Ok(AgentToolOutput::new(vec![ContentBlock::Text {
                        text: format!("echo: {text}"),
                        text_signature: None,
                    }]))
                })
            }),
        }
    }

    struct RecordingProvider {
        contexts: Arc<Mutex<Vec<Context>>>,
        response: String,
    }

    impl RecordingProvider {
        fn new(contexts: Arc<Mutex<Vec<Context>>>, response: impl Into<String>) -> Self {
            Self {
                contexts,
                response: response.into(),
            }
        }
    }

    impl ApiProvider for RecordingProvider {
        fn stream(&self, model: &Model, ctx: Context, _opts: Option<StreamOptions>) -> EventStream {
            self.contexts.lock().unwrap().push(ctx);
            let model_id = model.id.clone();
            let response = self.response.clone();
            Box::pin(stream! {
                let mut message = AssistantMessage::empty("recording", &model_id);
                message.provider = Some("recording".into());
                message.content.push(ContentBlock::Text {
                    text: response,
                    text_signature: None,
                });
                yield AssistantMessageEvent::Done {
                    reason: StopReason::Stop,
                    message,
                };
            })
        }
    }

    struct BlockingTwoTurnProvider {
        contexts: Arc<Mutex<Vec<Context>>>,
        first_started: Mutex<Option<oneshot::Sender<()>>>,
        release_first: Mutex<Option<oneshot::Receiver<()>>>,
    }

    impl BlockingTwoTurnProvider {
        fn new(
            contexts: Arc<Mutex<Vec<Context>>>,
            first_started: oneshot::Sender<()>,
            release_first: oneshot::Receiver<()>,
        ) -> Self {
            Self {
                contexts,
                first_started: Mutex::new(Some(first_started)),
                release_first: Mutex::new(Some(release_first)),
            }
        }
    }

    impl ApiProvider for BlockingTwoTurnProvider {
        fn stream(&self, model: &Model, ctx: Context, _opts: Option<StreamOptions>) -> EventStream {
            let call_index = {
                let mut contexts = self.contexts.lock().unwrap();
                contexts.push(ctx);
                contexts.len()
            };
            let first_release = if call_index == 1 {
                if let Some(started) = self.first_started.lock().unwrap().take() {
                    let _ = started.send(());
                }
                self.release_first.lock().unwrap().take()
            } else {
                None
            };
            let model_id = model.id.clone();
            Box::pin(stream! {
                if let Some(release) = first_release {
                    let _ = release.await;
                }
                let text = if call_index == 1 { "first" } else { "second" };
                let mut message = AssistantMessage::empty("blocking", &model_id);
                message.provider = Some("blocking".into());
                message.content.push(ContentBlock::Text {
                    text: text.into(),
                    text_signature: None,
                });
                yield AssistantMessageEvent::Done {
                    reason: StopReason::Stop,
                    message,
                };
            })
        }
    }

    struct AbortableProvider {
        started: Mutex<Option<oneshot::Sender<()>>>,
    }

    impl AbortableProvider {
        fn new(started: oneshot::Sender<()>) -> Self {
            Self {
                started: Mutex::new(Some(started)),
            }
        }
    }

    impl ApiProvider for AbortableProvider {
        fn stream(&self, model: &Model, _ctx: Context, opts: Option<StreamOptions>) -> EventStream {
            if let Some(started) = self.started.lock().unwrap().take() {
                let _ = started.send(());
            }
            let model_id = model.id.clone();
            let cancel = opts.and_then(|opts| opts.cancel);
            Box::pin(stream! {
                if let Some(cancel) = cancel {
                    cancel.cancelled().await;
                }
                let mut message = AssistantMessage::empty("abortable", &model_id);
                message.provider = Some("abortable".into());
                message.stop_reason = StopReason::Aborted;
                yield AssistantMessageEvent::Done {
                    reason: StopReason::Aborted,
                    message,
                };
            })
        }
    }

    #[tokio::test]
    async fn set_default_profile_installs_future_capability_generation() {
        let temp = tempfile::tempdir().unwrap();
        let mut session = CodingAgentSession::create_internal(
            CodingAgentSessionOptions::new()
                .with_session_id("sess_generation_profile")
                .with_session_log_root(temp.path()),
        )
        .await
        .unwrap();
        let first = session.current_capability_generation_for_tests();

        assert!(matches!(
            session
                .run_internal(CodingAgentOperation::SetDefaultAgentProfile {
                    profile_id: ProfileId::from("reviewer"),
                })
                .await
                .unwrap(),
            CodingAgentOperationOutcome::DefaultAgentProfileChanged
        ));
        let second = session.current_capability_generation_for_tests();

        assert_eq!(first.get() + 1, second.get());
    }

    #[tokio::test]
    async fn capability_control_installs_revoking_generation_and_publishes_exact_outcome() {
        let session = CodingAgentSession::non_persistent_internal(CodingAgentSessionOptions::new())
            .await
            .unwrap();
        let mut receiver = session.subscribe_product_events();
        let first = session.current_capability_generation_for_tests();

        let outcome = session
            .capability_control()
            .revoke_older_operations()
            .unwrap();

        assert_eq!(outcome.generation, first.get() + 1);
        assert!(outcome.cancellation_requested_operation_ids.is_empty());
        let event = receiver.try_recv().unwrap().unwrap();
        assert!(matches!(
            event.event(),
            CodingAgentProductEventKind::Capability(CodingAgentCapabilityProductEvent::Changed {
                generation,
                revocation: CodingAgentProductEventCapabilityRevocation::RequestCancelOlderOperations,
                cancellation_requested_operation_ids,
            }) if *generation == outcome.generation
                && cancellation_requested_operation_ids.is_empty()
        ));
    }

    #[tokio::test]
    async fn capability_revocation_aborts_an_active_prompt_from_the_older_generation() {
        let api = "coding-session-capability-revocation";
        let (started_tx, started_rx) = oneshot::channel();
        let ai_client = AiClient::new();
        ai_client.register_provider(api, Arc::new(AbortableProvider::new(started_tx)));
        let mut session = CodingAgentSession::non_persistent_internal(
            CodingAgentSessionOptions::new().with_ai_client(ai_client),
        )
        .await
        .unwrap();
        let capability_control = session.capability_control();
        let mut events = session.subscribe_product_events();
        let mut prompt = Box::pin(
            session.run_internal(CodingAgentOperation::Prompt(prompt_options(api, "hello"))),
        );
        tokio::select! {
            started = started_rx => started.unwrap(),
            result = &mut prompt => panic!("prompt finished before revocation: {result:?}"),
        }

        let revocation = capability_control.revoke_older_operations().unwrap();
        let outcome = prompt.await.unwrap();
        let CodingAgentOperationOutcome::Prompt(PromptTurnOutcome::Aborted {
            operation_id, ..
        }) = outcome
        else {
            panic!("capability revocation must abort the active prompt")
        };

        assert_eq!(
            revocation.cancellation_requested_operation_ids.as_slice(),
            std::slice::from_ref(&operation_id)
        );
        let emitted = std::iter::from_fn(|| events.try_recv().ok().flatten()).collect::<Vec<_>>();
        assert!(emitted.iter().any(|event| matches!(
            event.event(),
            CodingAgentProductEventKind::Capability(CodingAgentCapabilityProductEvent::Changed {
                generation,
                revocation: CodingAgentProductEventCapabilityRevocation::RequestCancelOlderOperations,
                cancellation_requested_operation_ids,
            }) if *generation == revocation.generation
                && cancellation_requested_operation_ids == std::slice::from_ref(&operation_id)
        )));
    }

    #[tokio::test]
    async fn submitted_explicit_aborted_outcome_finishes_aborted_exactly_once() {
        let api = "coding-session-abort-control";
        let (started_tx, started_rx) = oneshot::channel();
        let ai_client = AiClient::new();
        ai_client.register_provider(api, Arc::new(AbortableProvider::new(started_tx)));
        let temp = tempfile::tempdir().unwrap();
        let mut session = CodingAgentSession::create_internal(
            CodingAgentSessionOptions::new()
                .with_session_id("sess_prompt_abort_control")
                .with_session_log_root(temp.path())
                .with_ai_client(ai_client),
        )
        .await
        .unwrap();
        let handle = session.prompt_control_handle().unwrap();

        let connection = session
            .connect(public_projection::CodingAgentClientId::new(
                "submitted-abort-client",
            ))
            .unwrap();
        let draft_id = public_projection::CodingAgentDraftId("submitted-abort-draft".into());
        connection
            .set_prompt_draft(draft_id.clone(), "hello")
            .unwrap();
        let operation = CodingAgentOperation::Prompt(prompt_options(api, "hello"));
        let lease = connection
            .prepare_submission(&mut session, draft_id, &operation)
            .unwrap();

        let mut prompt = Box::pin(session.run(operation));
        tokio::select! {
            started = started_rx => started.unwrap(),
            result = &mut prompt => panic!("prompt finished before provider blocked: {result:?}"),
        }
        handle.abort("user cancelled").unwrap();

        let outcome = prompt.await.unwrap();
        let CodingAgentOperationOutcome::Prompt(outcome) = outcome else {
            panic!("prompt operation returned another outcome")
        };

        assert!(
            matches!(
                outcome,
                PromptTurnOutcome::Aborted {
                    ref reason,
                    session_id: Some(ref session_id),
                    ..
                } if reason == "user cancelled" && session_id == "sess_prompt_abort_control"
            ),
            "got {outcome:?}"
        );
        let event_log = std::fs::read_to_string(
            temp.path()
                .join("sess_prompt_abort_control")
                .join("events.jsonl"),
        )
        .unwrap();
        assert!(event_log.contains("\"kind\":\"operation.aborted\""));
        assert!(event_log.contains("user cancelled"));
        drop(lease);

        let submitted = connection
            .state()
            .unwrap()
            .submitted_operation
            .expect("aborted submitted terminal state");
        assert!(matches!(
            submitted.status,
            public_projection::CodingAgentSubmittedOperationStatus::Terminal {
                status: public_event::CodingAgentProductEventTerminalStatus::Aborted,
                ..
            }
        ));
    }

    #[test]
    fn submitted_cancelled_error_finishes_aborted_exactly_once() {
        let result = Err(CodingSessionError::Cancelled);
        assert_eq!(
            OperationFinalizer::terminal_status(&result),
            public_event::ProductEventTerminalStatus::Aborted
        );
    }

    #[test]
    fn supervisor_finalization_decision_freezes_admitted_identity_and_payload() {
        let descriptor =
            CodingAgentOperation::Prompt(prompt_options("finalization-decision", "freeze"))
                .descriptor();
        let execution = OperationExecution::root(
            OperationKind::Prompt,
            descriptor,
            OperationOrigin::ClientRoot,
            Some("2026-07-19T00:00:00Z".into()),
            Some("session-finalization".into()),
            crate::runtime::capability::OperationCapabilitySnapshot::permissive("op-finalization"),
        );
        let decision = OperationFinalizer.freeze(&execution, &Err(CodingSessionError::Cancelled));

        assert_eq!(decision.operation_id, "op-finalization");
        assert_eq!(decision.root_operation_id, "op-finalization");
        assert_eq!(
            decision.session_identity.as_deref(),
            Some("session-finalization")
        );
        assert_eq!(decision.descriptor, descriptor);
        assert_eq!(
            decision.capability_generation,
            execution.capability_generation
        );
        assert_eq!(
            decision.semantic_event_id,
            "session-finalization/op-finalization/operation_terminal"
        );
        assert_eq!(
            decision.terminal_status,
            public_event::ProductEventTerminalStatus::Aborted
        );
        assert_eq!(
            decision.payload,
            crate::runtime::finalization::FinalizationPayload::Aborted {
                reason: "cancelled".into()
            }
        );
    }

    #[test]
    fn submitted_typed_prompt_failure_finishes_failed_not_completed() {
        let result = Ok(OperationOutcome::Prompt(
            InternalPromptTurnOutcome::Failed {
                operation_id: "op-typed-failure".into(),
                turn_id: Some("turn-typed-failure".into()),
                error: CodingSessionError::Provider {
                    message: "provider rejected request".into(),
                },
                diagnostics: Vec::new(),
            },
        ));

        assert_eq!(
            OperationFinalizer::terminal_status(&result),
            public_event::ProductEventTerminalStatus::Failed
        );
    }

    #[test]
    fn submitted_reused_branch_summary_finishes_completed_not_aborted() {
        let result = Ok(OperationOutcome::BranchSummary(
            InternalPromptTurnOutcome::Success {
                operation_id: "op-reused-summary".into(),
                turn_id: "turn-reused-summary".into(),
                session_id: Some("session-reused-summary".into()),
                leaf_id: Some("leaf-reused-summary".into()),
                final_text: "existing summary".into(),
                final_message: AssistantMessage::empty("test", "test-model"),
                diagnostics: Vec::new(),
            },
        ));
        assert_eq!(
            OperationFinalizer::terminal_status(&result),
            public_event::ProductEventTerminalStatus::Completed
        );
    }

    #[test]
    fn submitted_invalid_compact_options_finishes_failed_not_aborted() {
        let result = Err(CodingSessionError::Input {
            message: "invalid compact options".into(),
        });
        assert_eq!(
            OperationFinalizer::terminal_status(&result),
            public_event::ProductEventTerminalStatus::Failed
        );
    }

    #[test]
    fn submitted_non_persistent_operations_finish_failed_not_aborted() {
        let result = Err(CodingSessionError::UnsupportedCapability {
            capability: "persistent session required".into(),
        });
        assert_eq!(
            OperationFinalizer::terminal_status(&result),
            public_event::ProductEventTerminalStatus::Failed
        );
    }

    #[test]
    fn submitted_sync_mutable_failure_finishes_failed_not_aborted() {
        let result = Err(CodingSessionError::Session {
            message: "sync mutable persistence failure".into(),
        });
        assert_eq!(
            OperationFinalizer::terminal_status(&result),
            public_event::ProductEventTerminalStatus::Failed
        );
    }

    #[tokio::test]
    async fn public_reconnect_receiver_projects_live_lag_as_fresh_snapshot_recovery() {
        let session = CodingAgentSession::non_persistent_with_event_capacity_for_tests(
            CodingAgentSessionOptions::new(),
            1,
        )
        .await
        .unwrap();
        let connection = session
            .connect_internal(CodingAgentClientId::new("lag-client"))
            .unwrap();
        let CodingAgentReconnect::Replayed {
            mut receiver,
            cursor,
            ..
        } = connection.reconnect(0).unwrap()
        else {
            panic!("initial cursor must establish a replay/live boundary")
        };

        session
            .runtime_host
            .event_hub
            .service
            .emit_diagnostic(None::<String>, "one");
        session
            .runtime_host
            .event_hub
            .service
            .emit_diagnostic(None::<String>, "two");

        let Some(CodingAgentReconnectDelivery::FreshSnapshotRequired(recovery)) =
            receiver.try_recv().unwrap()
        else {
            panic!("lagged reconnect receiver must require a typed fresh snapshot")
        };
        assert_eq!(recovery.reason, CodingAgentRecoveryReason::LiveReceiverLag);
        assert_eq!(recovery.requested_sequence, cursor.last_event_sequence);
        assert_eq!(recovery.oldest_available_sequence, 2);
        assert_eq!(recovery.fresh_cursor.last_event_sequence, 2);
    }

    #[tokio::test]
    async fn public_scoped_control_receipts_are_idempotent_fifo_and_acceptance_clears_drafts() {
        let session = CodingAgentSession::non_persistent_internal(CodingAgentSessionOptions::new())
            .await
            .unwrap();
        let connection = session
            .connect_internal(CodingAgentClientId::new("receipt-client"))
            .unwrap();
        for (id, kind, text) in [
            ("steer-draft", CodingAgentDraftKind::Steer, "draft steer"),
            (
                "follow-draft",
                CodingAgentDraftKind::FollowUp,
                "draft follow",
            ),
        ] {
            connection
                .enqueue_control_draft(CodingAgentDraft {
                    id: CodingAgentDraftId(id.into()),
                    kind,
                    text: text.into(),
                })
                .unwrap();
        }
        let (sender, mut receiver) = operation_control::prompt_control_channel();
        session
            .runtime_host
            .client_projection
            .coordinator
            .bind_prompt_control(
                connection.handle(),
                "op-receipts".into(),
                operation_control::PromptControlGeneration(1),
                sender,
            );
        let control = connection.prompt_control("op-receipts");

        let abort = control
            .abort(CodingAgentControlId("abort-1".into()), "stop")
            .unwrap();
        assert_eq!(
            control
                .abort(CodingAgentControlId("abort-1".into()), "stop")
                .unwrap(),
            abort
        );
        assert_eq!(
            control
                .abort(CodingAgentControlId("abort-1".into()), "different")
                .unwrap_err()
                .reason,
            CodingAgentControlRejectionReason::PayloadConflict
        );
        control
            .steer(CodingAgentControlId("steer-1".into()), "direct steer")
            .unwrap();
        let image_control_id = CodingAgentControlId("steer-image-1".into());
        let image_content = vec![ContentBlock::Image {
            data: "c3RlZXI=".into(),
            mime_type: "image/png".into(),
        }];
        let image_receipt = control
            .steer_content(image_control_id.clone(), image_content.clone())
            .unwrap();
        assert_eq!(
            control
                .steer_content(image_control_id.clone(), image_content.clone())
                .unwrap(),
            image_receipt
        );
        assert_eq!(
            control
                .steer_content(
                    image_control_id,
                    vec![ContentBlock::Image {
                        data: "ZGlmZmVyZW50".into(),
                        mime_type: "image/png".into(),
                    }],
                )
                .unwrap_err()
                .reason,
            CodingAgentControlRejectionReason::PayloadConflict
        );
        control
            .steer_draft(CodingAgentDraftId("steer-draft".into()))
            .unwrap();
        control
            .follow_up_draft(CodingAgentDraftId("follow-draft".into()))
            .unwrap();

        assert_eq!(
            std::iter::from_fn(|| receiver.try_recv().ok()).collect::<Vec<_>>(),
            vec![
                PromptControlCommand::Abort {
                    reason: "stop".into()
                },
                PromptControlCommand::Steer {
                    text: "direct steer".into()
                },
                PromptControlCommand::SteerContent {
                    content: image_content
                },
                PromptControlCommand::Steer {
                    text: "draft steer".into()
                },
                PromptControlCommand::FollowUp {
                    text: "draft follow".into()
                },
            ]
        );
        assert!(connection.state().unwrap().drafts.is_empty());
    }

    #[tokio::test]
    async fn structured_prompt_draft_uses_content_fingerprint_without_exposing_image_data() {
        let mut session =
            CodingAgentSession::non_persistent_internal(CodingAgentSessionOptions::new())
                .await
                .unwrap();
        let connection = session
            .connect_internal(CodingAgentClientId::new("structured-draft-client"))
            .unwrap();
        let draft_id = CodingAgentDraftId("structured-draft".into());
        let image_data = "aW1hZ2UtZGF0YQ==";
        let operation =
            CodingAgentOperation::Prompt(PromptTurnOptions::new(PromptInvocation::Content(vec![
                ContentBlock::Text {
                    text: "describe image".into(),
                    text_signature: None,
                },
                ContentBlock::Image {
                    data: image_data.into(),
                    mime_type: "image/png".into(),
                },
            ])));
        let (_, fingerprint) = operation
            .submission_fingerprint()
            .expect("structured prompt must have submission provenance");
        assert_eq!(fingerprint.len(), 64);
        assert!(!fingerprint.contains(image_data));

        connection
            .set_prompt_operation_draft(
                draft_id.clone(),
                "describe image\n[image:image/png]",
                &operation,
            )
            .unwrap();

        let draft = connection.state().unwrap().drafts.remove(0);
        assert_eq!(draft.id, draft_id);
        assert_eq!(draft.text, "describe image\n[image:image/png]");
        assert!(!draft.text.contains(image_data));

        let lease = connection
            .prepare_submission(&mut session, draft_id, &operation)
            .unwrap();
        assert_eq!(
            *lease.shared.lock().unwrap(),
            SubmissionLeaseLifecycle::Prepared
        );
    }

    #[tokio::test]
    async fn prepared_prompt_exact_draft_commits_running_and_clears() {
        let api = "prepared-prompt-exact-draft";
        let _provider = crate::test_support::ProviderGuard::register(
            api,
            Arc::new(FauxProvider::with_call_queue(vec![
                FauxProvider::text_call("first answer", StopReason::Stop),
                FauxProvider::text_call("replacement answer", StopReason::Stop),
            ])),
        );
        let mut session = CodingAgentSession::non_persistent_internal(
            CodingAgentSessionOptions::new().with_ai_client(_provider.ai_client()),
        )
        .await
        .unwrap();
        let connection = session
            .connect(public_projection::CodingAgentClientId::new(
                "prepared-prompt-exact-client",
            ))
            .unwrap();

        let exact_id = public_projection::CodingAgentDraftId("exact-draft".into());
        connection
            .set_prompt_draft(exact_id.clone(), "exact prompt")
            .unwrap();
        let exact_operation = CodingAgentOperation::Prompt(prompt_options(api, "exact prompt"));
        let exact_lease = connection
            .prepare_submission(&mut session, exact_id, &exact_operation)
            .unwrap();

        let outcome = session.run(exact_operation).await.unwrap();

        assert!(matches!(outcome, CodingAgentOperationOutcome::Prompt(_)));
        assert!(connection.state().unwrap().drafts.is_empty());
        assert_eq!(
            *exact_lease.shared.lock().unwrap(),
            SubmissionLeaseLifecycle::Committed
        );
        drop(exact_lease);

        let replacement_connection = session
            .connect(public_projection::CodingAgentClientId::new(
                "prepared-prompt-replacement-client",
            ))
            .unwrap();
        let original_id = public_projection::CodingAgentDraftId("original-draft".into());
        replacement_connection
            .set_prompt_draft(original_id.clone(), "original prompt")
            .unwrap();
        let original_operation =
            CodingAgentOperation::Prompt(prompt_options(api, "original prompt"));
        let original_lease = replacement_connection
            .prepare_submission(&mut session, original_id, &original_operation)
            .unwrap();
        replacement_connection
            .set_prompt_draft(
                public_projection::CodingAgentDraftId("replacement-draft".into()),
                "replacement prompt",
            )
            .unwrap();

        assert!(matches!(
            session.run_internal(original_operation).await,
            Err(CodingSessionError::SubmissionDraftMismatch)
        ));
        assert_eq!(
            *original_lease.shared.lock().unwrap(),
            SubmissionLeaseLifecycle::Abandoned
        );
        let replacement = replacement_connection
            .state()
            .unwrap()
            .drafts
            .into_iter()
            .next()
            .expect("replacement draft must remain available");
        assert_eq!(replacement.id.0, "replacement-draft");
        assert_eq!(replacement.text, "replacement prompt");
    }

    #[tokio::test]
    async fn prepared_prompt_replacement_rejects_original_and_preserves_replacement() {
        let mut session =
            CodingAgentSession::non_persistent_internal(CodingAgentSessionOptions::new())
                .await
                .unwrap();
        let connection = session
            .connect(public_projection::CodingAgentClientId::new(
                "prepared-prompt-replacement-regression",
            ))
            .unwrap();
        let original_id = public_projection::CodingAgentDraftId("draft-a".into());
        connection
            .set_prompt_draft(original_id.clone(), "prompt A")
            .unwrap();
        let operation_a =
            CodingAgentOperation::Prompt(prompt_options("prepared-replacement", "prompt A"));
        let lease_a = connection
            .prepare_submission(&mut session, original_id, &operation_a)
            .unwrap();
        let replacement_id = public_projection::CodingAgentDraftId("draft-b".into());
        connection
            .set_prompt_draft(replacement_id.clone(), "prompt B")
            .unwrap();

        let error = session.run_internal(operation_a).await.unwrap_err();

        assert_eq!(error, CodingSessionError::SubmissionDraftMismatch);
        assert_eq!(error.code(), "submission_draft_mismatch");
        assert_eq!(
            *lease_a.shared.lock().unwrap(),
            SubmissionLeaseLifecycle::Abandoned
        );
        let state = connection.state().unwrap();
        assert!(state.submitted_operation.is_none());
        assert_eq!(state.drafts.len(), 1);
        assert_eq!(state.drafts[0].id, replacement_id);
        assert_eq!(state.drafts[0].text, "prompt B");

        let operation_b =
            CodingAgentOperation::Prompt(prompt_options("prepared-replacement", "prompt B"));
        let lease_b = connection
            .prepare_submission(&mut session, replacement_id, &operation_b)
            .unwrap();
        assert_eq!(
            *lease_b.shared.lock().unwrap(),
            SubmissionLeaseLifecycle::Prepared
        );
    }

    #[tokio::test]
    async fn prepared_prompt_same_text_different_id_is_identity_mismatch() {
        let mut session =
            CodingAgentSession::non_persistent_internal(CodingAgentSessionOptions::new())
                .await
                .unwrap();
        let connection = session
            .connect(public_projection::CodingAgentClientId::new(
                "prepared-prompt-identity-regression",
            ))
            .unwrap();
        let original_id = public_projection::CodingAgentDraftId("identity-a".into());
        connection
            .set_prompt_draft(original_id.clone(), "same prompt")
            .unwrap();
        let operation =
            CodingAgentOperation::Prompt(prompt_options("prepared-identity", "same prompt"));
        let lease = connection
            .prepare_submission(&mut session, original_id, &operation)
            .unwrap();
        let replacement_id = public_projection::CodingAgentDraftId("identity-b".into());
        connection
            .set_prompt_draft(replacement_id.clone(), "same prompt")
            .unwrap();

        let error = session.run_internal(operation).await.unwrap_err();

        assert_eq!(error, CodingSessionError::SubmissionDraftMismatch);
        assert_eq!(error.code(), "submission_draft_mismatch");
        assert_eq!(
            *lease.shared.lock().unwrap(),
            SubmissionLeaseLifecycle::Abandoned
        );
        let state = connection.state().unwrap();
        assert!(state.submitted_operation.is_none());
        assert_eq!(state.drafts.len(), 1);
        assert_eq!(state.drafts[0].id, replacement_id);
        assert_eq!(state.drafts[0].text, "same prompt");
    }

    #[test]
    fn atomic_submission_transition_commits_running_and_consumes_prompt_draft() {
        let coordinator = SnapshotCoordinator::new();
        let service = ClientService::new(coordinator.clone());
        let handle = service
            .connect_or_takeover(ClientConnectionId::new("atomic-submission-client"))
            .unwrap();
        service
            .set_prompt_draft(
                &handle,
                Some(snapshot_coordinator::DraftRecord {
                    id: "prompt-draft".into(),
                    kind: ClientDraftKind::Prompt,
                    text: "preserve or consume atomically".into(),
                    fingerprint: "preserve or consume atomically".into(),
                }),
            )
            .unwrap();
        let descriptor = CodingAgentOperation::Prompt(prompt_options(
            "atomic-submission",
            "preserve or consume atomically",
        ))
        .descriptor();

        let expected_prompt_draft = snapshot_coordinator::DraftRecord {
            id: "prompt-draft".into(),
            kind: ClientDraftKind::Prompt,
            text: "preserve or consume atomically".into(),
            fingerprint: "preserve or consume atomically".into(),
        };
        coordinator
            .register_prepared_submission(&handle, "op-atomic".into(), descriptor)
            .unwrap();
        service
            .commit_submission_running(
                &handle,
                "op-atomic".into(),
                descriptor,
                Some(&expected_prompt_draft),
            )
            .unwrap();

        let state = coordinator.state.lock().unwrap();
        let record = &state.clients[&handle.id];
        assert!(record.prompt_draft.is_none());
        assert_eq!(
            record.submitted_operation,
            Some(snapshot_coordinator::SubmittedOperationStatus::Running {
                operation_id: "op-atomic".into(),
                kind: OperationKind::Prompt,
                descriptor,
            })
        );
    }

    fn submission_guard_for_test(
        service: &ClientService,
        handle: snapshot_coordinator::ClientHandle,
        descriptor: public_operation::OperationDescriptor,
    ) -> SubmissionCommitGuard {
        let expected_prompt_draft = service.coordinator.state.lock().unwrap().clients[&handle.id]
            .prompt_draft
            .clone();
        SubmissionCommitGuard::for_tests(
            service.clone(),
            service.coordinator.clone(),
            handle,
            descriptor,
            expected_prompt_draft,
        )
    }

    #[test]
    fn committed_submission_guard_drop_aborts_exact_running_once() {
        let coordinator = SnapshotCoordinator::new();
        let service = ClientService::new(coordinator.clone());
        let handle = service
            .connect_or_takeover(ClientConnectionId::new("guard-drop-exact"))
            .unwrap();
        let descriptor = CodingAgentOperation::BranchSummary {
            options: prompt_options("guard-drop-exact", "summary"),
            source_leaf_id: "leaf-source".into(),
            target_leaf_id: "leaf-target".into(),
            custom_instructions: None,
            reuse: BranchSummaryReusePolicy::AlwaysCreate,
        }
        .descriptor();
        let mut guard = submission_guard_for_test(&service, handle.clone(), descriptor);
        guard.commit("op-guard-drop-exact".into()).unwrap();

        drop(guard);

        let state = coordinator.state.lock().unwrap();
        assert_eq!(
            state.clients[&handle.id].submitted_operation,
            Some(snapshot_coordinator::SubmittedOperationStatus::Terminal {
                operation_id: "op-guard-drop-exact".into(),
                kind: OperationKind::BranchSummary,
                descriptor,
                anchor: snapshot_coordinator::SubmittedTerminalAnchor::TerminalUncertain {
                    operation_id: "op-guard-drop-exact".into(),
                },
                status: public_event::ProductEventTerminalStatus::Aborted,
                root_count: 0,
            })
        );
    }

    #[test]
    fn submission_guard_drop_never_overwrites_terminal_or_nonmatching_state() {
        let coordinator = SnapshotCoordinator::new();
        let service = ClientService::new(coordinator.clone());
        let prompt_descriptor =
            CodingAgentOperation::Prompt(prompt_options("guard-drop-controls", "control prompt"))
                .descriptor();
        let outcome_descriptor = CodingAgentOperation::ExportCurrent.descriptor();

        let terminal_handle = service
            .connect_or_takeover(ClientConnectionId::new("guard-drop-terminal"))
            .unwrap();
        service
            .set_prompt_draft(
                &terminal_handle,
                Some(snapshot_coordinator::DraftRecord {
                    id: "terminal-draft".into(),
                    kind: ClientDraftKind::Prompt,
                    text: "control prompt".into(),
                    fingerprint: "control prompt".into(),
                }),
            )
            .unwrap();
        let mut terminal_guard =
            submission_guard_for_test(&service, terminal_handle.clone(), prompt_descriptor);
        terminal_guard.commit("op-terminal".into()).unwrap();
        coordinator
            .finalize_terminal_association(
                &terminal_handle,
                "op-terminal",
                prompt_descriptor,
                public_event::ProductEventTerminalStatus::Completed,
            )
            .unwrap();
        let terminal_before = coordinator.state.lock().unwrap().clients[&terminal_handle.id]
            .submitted_operation
            .clone();
        drop(terminal_guard);
        assert_eq!(
            coordinator.state.lock().unwrap().clients[&terminal_handle.id].submitted_operation,
            terminal_before
        );

        let mismatch_handle = service
            .connect_or_takeover(ClientConnectionId::new("guard-drop-mismatch"))
            .unwrap();
        let mut mismatch_guard =
            submission_guard_for_test(&service, mismatch_handle.clone(), outcome_descriptor);
        mismatch_guard.commit("op-original".into()).unwrap();
        let mismatch_execution = mismatch_guard.execution.as_mut().unwrap();
        mismatch_execution.operation_id = "op-stale".into();
        mismatch_execution.descriptor = prompt_descriptor;
        let running_before = coordinator.state.lock().unwrap().clients[&mismatch_handle.id]
            .submitted_operation
            .clone();
        drop(mismatch_guard);
        assert_eq!(
            coordinator.state.lock().unwrap().clients[&mismatch_handle.id].submitted_operation,
            running_before
        );

        let newer_handle = service
            .connect_or_takeover(ClientConnectionId::new("guard-drop-newer"))
            .unwrap();
        let mut stale_guard =
            submission_guard_for_test(&service, newer_handle.clone(), outcome_descriptor);
        stale_guard.commit("op-old".into()).unwrap();
        coordinator
            .state
            .lock()
            .unwrap()
            .clients
            .get_mut(&newer_handle.id)
            .unwrap()
            .submitted_operation = Some(snapshot_coordinator::SubmittedOperationStatus::Running {
            operation_id: "op-new".into(),
            kind: outcome_descriptor.submitted_kind,
            descriptor: outcome_descriptor,
        });
        drop(stale_guard);
        assert!(matches!(
            coordinator.state.lock().unwrap().clients[&newer_handle.id].submitted_operation,
            Some(snapshot_coordinator::SubmittedOperationStatus::Running {
                ref operation_id,
                ..
            }) if operation_id == "op-new"
        ));
    }

    #[test]
    fn precommit_submission_guard_drop_only_abandons_lease() {
        let coordinator = SnapshotCoordinator::new();
        let service = ClientService::new(coordinator.clone());
        let handle = service
            .connect_or_takeover(ClientConnectionId::new("guard-drop-precommit"))
            .unwrap();
        let descriptor = CodingAgentOperation::ExportCurrent.descriptor();
        let guard = submission_guard_for_test(&service, handle.clone(), descriptor);
        let lifecycle = guard.lifecycle.clone();

        drop(guard);

        assert_eq!(
            *lifecycle.lock().unwrap(),
            SubmissionLeaseLifecycle::Abandoned
        );
        assert!(
            coordinator.state.lock().unwrap().clients[&handle.id]
                .submitted_operation
                .is_none()
        );
    }

    #[test]
    fn outcome_only_committed_guard_drop_aborts_exact_running_once() {
        let coordinator = SnapshotCoordinator::new();
        let service = ClientService::new(coordinator.clone());
        let handle = service
            .connect_or_takeover(ClientConnectionId::new("guard-drop-outcome-only"))
            .unwrap();
        let descriptor = CodingAgentOperation::ExportCurrent.descriptor();
        assert_eq!(
            descriptor.terminal_policy,
            public_operation::OperationTerminalPolicy::OutcomeAcknowledgement
        );
        let mut guard = submission_guard_for_test(&service, handle.clone(), descriptor);
        guard.commit("op-outcome-only-drop".into()).unwrap();

        drop(guard);

        assert_eq!(
            coordinator.state.lock().unwrap().clients[&handle.id].submitted_operation,
            Some(snapshot_coordinator::SubmittedOperationStatus::Terminal {
                operation_id: "op-outcome-only-drop".into(),
                kind: OperationKind::Export,
                descriptor,
                anchor: snapshot_coordinator::SubmittedTerminalAnchor::TerminalUncertain {
                    operation_id: "op-outcome-only-drop".into(),
                },
                status: public_event::ProductEventTerminalStatus::Aborted,
                root_count: 0,
            })
        );
    }

    fn assert_public_drop_terminal(
        submitted: &public_projection::CodingAgentSubmittedOperation,
        operation_id: &str,
    ) {
        assert_eq!(submitted.operation_id, operation_id);
        assert_eq!(submitted.kind, OperationKind::Prompt.as_str());
        assert_eq!(
            submitted.status,
            public_projection::CodingAgentSubmittedOperationStatus::Terminal {
                status: public_event::CodingAgentProductEventTerminalStatus::Aborted,
                anchor: public_projection::CodingAgentSubmittedTerminalAnchor::TerminalUncertain {
                    operation_id: operation_id.into(),
                    recovery: public_projection::CodingAgentTerminalUncertainty::RecoveryRequired,
                },
            }
        );
    }

    #[tokio::test]
    async fn dropping_pending_public_prompt_run_terminalizes_submitted_aborted_once() {
        let api = "coding-session-public-prompt-drop";
        let contexts = Arc::new(Mutex::new(Vec::new()));
        let (started_tx, started_rx) = oneshot::channel();
        let (_release_tx, release_rx) = oneshot::channel();
        let _provider_guard = crate::test_support::ProviderGuard::register(
            api,
            Arc::new(BlockingTwoTurnProvider::new(
                contexts, started_tx, release_rx,
            )),
        );
        let mut session = CodingAgentSession::non_persistent_internal(
            CodingAgentSessionOptions::new().with_ai_client(_provider_guard.ai_client()),
        )
        .await
        .unwrap();
        let client_id = public_projection::CodingAgentClientId::new("public-prompt-drop-client");
        let connection = session.connect(client_id.clone()).unwrap();
        let draft_id = public_projection::CodingAgentDraftId("public-prompt-drop-draft".into());
        connection
            .set_prompt_draft(draft_id.clone(), "pending")
            .unwrap();
        let operation = CodingAgentOperation::Prompt(prompt_options(api, "pending"));
        let lease = connection
            .prepare_submission(&mut session, draft_id, &operation)
            .unwrap();
        let coordinator = session.runtime_host.client_projection.coordinator.clone();
        let mut run = Box::pin(session.run(operation));

        tokio::select! {
            started = started_rx => started.unwrap(),
            result = &mut run => panic!("prompt finished before provider gate: {result:?}"),
        }
        let running = connection
            .state()
            .unwrap()
            .submitted_operation
            .expect("provider gate must observe Running");
        assert!(matches!(
            running.status,
            public_projection::CodingAgentSubmittedOperationStatus::Running
        ));
        let operation_id = running.operation_id;
        drop(run);

        let reconnected = session.connect(client_id).unwrap();
        let submitted = reconnected
            .state()
            .unwrap()
            .submitted_operation
            .expect("drop must retain terminal submitted state");
        assert_public_drop_terminal(&submitted, &operation_id);
        let state = coordinator.state.lock().unwrap();
        assert!(matches!(
            state
                .clients
                .values()
                .find_map(|record| record.submitted_operation.as_ref()),
            Some(snapshot_coordinator::SubmittedOperationStatus::Terminal { root_count: 0, .. })
        ));
        assert_eq!(
            state
                .retained_product_events
                .iter()
                .filter(|event| event.operation_id() == Some(operation_id.as_str())
                    && event.terminal_status().is_some())
                .count(),
            0
        );
        drop(lease);
    }

    #[tokio::test]
    async fn dropping_prompt_a_allows_prompt_b_scoped_control_delivery() {
        let prompt_a_api = "coding-session-public-prompt-a-drop";
        let prompt_b_api = "coding-session-public-prompt-b-control";
        let (prompt_a_started_tx, prompt_a_started_rx) = oneshot::channel();
        let (_prompt_a_release_tx, prompt_a_release_rx) = oneshot::channel();
        let (prompt_b_started_tx, prompt_b_started_rx) = oneshot::channel();
        let (prompt_b_release_tx, prompt_b_release_rx) = oneshot::channel();
        let prompt_b_contexts = Arc::new(Mutex::new(Vec::new()));
        let _provider_guard = crate::test_support::ProviderGuard::register_many(vec![
            (
                prompt_a_api.into(),
                Arc::new(BlockingTwoTurnProvider::new(
                    Arc::new(Mutex::new(Vec::new())),
                    prompt_a_started_tx,
                    prompt_a_release_rx,
                )),
            ),
            (
                prompt_b_api.into(),
                Arc::new(BlockingTwoTurnProvider::new(
                    prompt_b_contexts.clone(),
                    prompt_b_started_tx,
                    prompt_b_release_rx,
                )),
            ),
        ]);
        let mut session = CodingAgentSession::non_persistent_internal(
            CodingAgentSessionOptions::new().with_ai_client(_provider_guard.ai_client()),
        )
        .await
        .unwrap();
        let connection_a = session
            .connect(public_projection::CodingAgentClientId::new(
                "public-prompt-a-client",
            ))
            .unwrap();
        let connection_b = session
            .connect(public_projection::CodingAgentClientId::new(
                "public-prompt-b-client",
            ))
            .unwrap();

        let draft_a = public_projection::CodingAgentDraftId("public-prompt-a-draft".into());
        connection_a
            .set_prompt_draft(draft_a.clone(), "prompt A")
            .unwrap();
        let operation_a = CodingAgentOperation::Prompt(prompt_options(prompt_a_api, "prompt A"));
        let lease_a = connection_a
            .prepare_submission(&mut session, draft_a, &operation_a)
            .unwrap();
        let mut run_a = Box::pin(session.run(operation_a));
        tokio::select! {
            started = prompt_a_started_rx => started.expect("Prompt A start gate closed"),
            result = &mut run_a => panic!("Prompt A finished before provider gate: {result:?}"),
        }
        let operation_a_id = connection_a
            .state()
            .unwrap()
            .submitted_operation
            .expect("Prompt A provider gate must observe Running")
            .operation_id;
        drop(run_a);
        let terminal_a = connection_a
            .state()
            .unwrap()
            .submitted_operation
            .expect("Prompt A drop must retain terminal submitted state");
        assert_public_drop_terminal(&terminal_a, &operation_a_id);

        let draft_b = public_projection::CodingAgentDraftId("public-prompt-b-draft".into());
        connection_b
            .set_prompt_draft(draft_b.clone(), "prompt B")
            .unwrap();
        let operation_b = CodingAgentOperation::Prompt(prompt_options(prompt_b_api, "prompt B"));
        let lease_b = connection_b
            .prepare_submission(&mut session, draft_b, &operation_b)
            .unwrap();
        let mut run_b = Box::pin(session.run(operation_b));
        tokio::select! {
            started = prompt_b_started_rx => started.expect("Prompt B start gate closed"),
            result = &mut run_b => panic!("Prompt B finished before provider gate: {result:?}"),
        }
        let operation_b_id = connection_b
            .state()
            .unwrap()
            .submitted_operation
            .expect("Prompt B provider gate must observe Running")
            .operation_id;

        let control_a_for_b = connection_a.prompt_control(operation_b_id.clone());
        let rejection = control_a_for_b
            .follow_up(
                public_projection::CodingAgentControlId("prompt-a-cross-client".into()),
                "must not cross client ownership",
            )
            .expect_err("Prompt A identity must not control Prompt B");
        assert_eq!(
            rejection.reason,
            public_projection::CodingAgentControlRejectionReason::NotOwner
        );

        let control_b = connection_b.prompt_control(operation_b_id.clone());
        match control_b.follow_up(
            public_projection::CodingAgentControlId("prompt-b-follow-up".into()),
            "continue Prompt B",
        ) {
            Ok(receipt) => assert_eq!(receipt.operation_id, operation_b_id),
            Err(rejection)
                if rejection.reason
                    == public_projection::CodingAgentControlRejectionReason::ControlChannelClosed =>
            {
                panic!(
                    "PROMPT_B_CONTROL_REUSED_STALE_PROMPT_A_CHANNEL: client B control hit stale Prompt A closed channel"
                )
            }
            Err(rejection) => panic!("Prompt B control failed unexpectedly: {rejection:?}"),
        }
        prompt_b_release_tx
            .send(())
            .expect("Prompt B release gate closed");

        let outcome_b = run_b.await.unwrap();
        assert!(matches!(
            outcome_b,
            CodingAgentOperationOutcome::Prompt(PromptTurnOutcome::Success { ref final_text, .. })
                if final_text == "second"
        ));
        let contexts = prompt_b_contexts.lock().unwrap();
        assert_eq!(contexts.len(), 2);
        assert!(contexts[1].messages.iter().any(|message| matches!(
            message,
            Message::User { content }
                if content.iter().any(|block| matches!(
                    block,
                    ContentBlock::Text { text, .. } if text == "continue Prompt B"
                ))
        )));
        assert_eq!(
            connection_a
                .state()
                .unwrap()
                .submitted_operation
                .expect("Prompt A terminal evidence must remain intact"),
            terminal_a
        );
        drop(lease_b);
        drop(lease_a);
    }

    #[tokio::test]
    async fn completed_public_prompt_run_is_not_overwritten_by_guard_drop() {
        let api = "coding-session-public-prompt-completed";
        let _provider_guard = crate::test_support::ProviderGuard::register(
            api,
            Arc::new(FauxProvider::simple_text("completed")),
        );
        let mut session = CodingAgentSession::non_persistent_internal(
            CodingAgentSessionOptions::new().with_ai_client(_provider_guard.ai_client()),
        )
        .await
        .unwrap();
        let connection = session
            .connect(public_projection::CodingAgentClientId::new(
                "public-prompt-completed-client",
            ))
            .unwrap();
        let draft_id = public_projection::CodingAgentDraftId("completed-draft".into());
        connection
            .set_prompt_draft(draft_id.clone(), "complete")
            .unwrap();
        let operation = CodingAgentOperation::Prompt(prompt_options(api, "complete"));
        let lease = connection
            .prepare_submission(&mut session, draft_id, &operation)
            .unwrap();

        let outcome = session.run(operation).await.unwrap();
        let CodingAgentOperationOutcome::Prompt(PromptTurnOutcome::Success {
            operation_id, ..
        }) = outcome
        else {
            panic!("expected successful Prompt outcome")
        };
        let submitted = connection
            .state()
            .unwrap()
            .submitted_operation
            .expect("completed submitted state");
        assert_eq!(submitted.operation_id, operation_id);
        assert!(matches!(
            submitted.status,
            public_projection::CodingAgentSubmittedOperationStatus::Terminal {
                status: public_event::CodingAgentProductEventTerminalStatus::Completed,
                anchor: public_projection::CodingAgentSubmittedTerminalAnchor::ProductEvent { .. },
            }
        ));
        assert!(matches!(
            session
                .runtime_host
                .client_projection
                .coordinator
                .state
                .lock()
                .unwrap()
                .clients
                .values()
                .find_map(|record| record.submitted_operation.as_ref()),
            Some(snapshot_coordinator::SubmittedOperationStatus::Terminal { root_count: 1, .. })
        ));
        drop(lease);
    }

    #[tokio::test]
    async fn detaching_during_pending_public_prompt_does_not_abort_until_run_future_drops() {
        let api = "coding-session-public-prompt-detach-drop";
        let contexts = Arc::new(Mutex::new(Vec::new()));
        let (started_tx, started_rx) = oneshot::channel();
        let (_release_tx, release_rx) = oneshot::channel();
        let _provider_guard = crate::test_support::ProviderGuard::register(
            api,
            Arc::new(BlockingTwoTurnProvider::new(
                contexts, started_tx, release_rx,
            )),
        );
        let mut session = CodingAgentSession::non_persistent_internal(
            CodingAgentSessionOptions::new().with_ai_client(_provider_guard.ai_client()),
        )
        .await
        .unwrap();
        let client_id = public_projection::CodingAgentClientId::new("public-prompt-detach-client");
        let connection = session.connect(client_id.clone()).unwrap();
        let draft_id = public_projection::CodingAgentDraftId("detach-drop-draft".into());
        connection
            .set_prompt_draft(draft_id.clone(), "pending")
            .unwrap();
        let operation = CodingAgentOperation::Prompt(prompt_options(api, "pending"));
        let lease = connection
            .prepare_submission(&mut session, draft_id, &operation)
            .unwrap();
        let coordinator = session.runtime_host.client_projection.coordinator.clone();
        let mut run = Box::pin(session.run(operation));

        tokio::select! {
            started = started_rx => started.unwrap(),
            result = &mut run => panic!("prompt finished before provider gate: {result:?}"),
        }
        let operation_id = connection
            .state()
            .unwrap()
            .submitted_operation
            .expect("provider gate must observe Running")
            .operation_id;
        assert_eq!(
            connection.detach().unwrap(),
            public_projection::CodingAgentDetachOutcome::Detached
        );
        assert!(matches!(
            coordinator
                .state
                .lock()
                .unwrap()
                .clients
                .values()
                .find_map(|record| record.submitted_operation.as_ref()),
            Some(snapshot_coordinator::SubmittedOperationStatus::Running { .. })
        ));

        drop(run);

        let reconnected = session.connect(client_id).unwrap();
        let submitted = reconnected
            .state()
            .unwrap()
            .submitted_operation
            .expect("drop after detach must terminalize retained state");
        assert_public_drop_terminal(&submitted, &operation_id);
        drop(lease);
    }

    #[test]
    fn submission_transition_preserves_prompt_draft_when_detach_wins() {
        let coordinator = SnapshotCoordinator::new();
        let service = ClientService::new(coordinator.clone());
        let handle = service
            .connect_or_takeover(ClientConnectionId::new("detach-first-submission"))
            .unwrap();
        service
            .set_prompt_draft(
                &handle,
                Some(snapshot_coordinator::DraftRecord {
                    id: "detach-draft".into(),
                    kind: ClientDraftKind::Prompt,
                    text: "detach keeps this exact draft".into(),
                    fingerprint: "detach keeps this exact draft".into(),
                }),
            )
            .unwrap();
        let descriptor = CodingAgentOperation::Prompt(prompt_options(
            "detach-first-submission",
            "detach keeps this exact draft",
        ))
        .descriptor();
        let mut guard = submission_guard_for_test(&service, handle.clone(), descriptor);

        assert_eq!(
            service.coordinator.detach(&handle),
            Ok(snapshot_coordinator::ClientDetachOutcome::Detached)
        );
        assert_eq!(
            guard.commit("op-detach-first".into()),
            Err(CodingSessionError::Lifecycle {
                reason: crate::runtime::error::CodingAgentLifecycleRejection::Detached,
            })
        );

        let state = coordinator.state.lock().unwrap();
        let record = &state.clients[&handle.id];
        assert!(record.submitted_operation.is_none());
        assert_eq!(
            record
                .prompt_draft
                .as_ref()
                .map(|draft| draft.text.as_str()),
            Some("detach keeps this exact draft")
        );
    }

    #[test]
    fn submission_transition_preserves_prompt_draft_when_shutdown_wins() {
        let coordinator = SnapshotCoordinator::new();
        let service = ClientService::new(coordinator.clone());
        let handle = service
            .connect_or_takeover(ClientConnectionId::new("shutdown-first-submission"))
            .unwrap();
        service
            .set_prompt_draft(
                &handle,
                Some(snapshot_coordinator::DraftRecord {
                    id: "shutdown-draft".into(),
                    kind: ClientDraftKind::Prompt,
                    text: "shutdown keeps this exact draft".into(),
                    fingerprint: "shutdown keeps this exact draft".into(),
                }),
            )
            .unwrap();
        let descriptor = CodingAgentOperation::Prompt(prompt_options(
            "shutdown-first-submission",
            "shutdown keeps this exact draft",
        ))
        .descriptor();
        let mut guard = submission_guard_for_test(&service, handle.clone(), descriptor);

        assert_eq!(
            coordinator.request_shutdown(),
            snapshot_coordinator::RuntimeLifecycle::Running
        );
        assert_eq!(
            guard.commit("op-shutdown-first".into()),
            Err(CodingSessionError::Lifecycle {
                reason: crate::runtime::error::CodingAgentLifecycleRejection::RuntimeShutDown,
            })
        );

        let state = coordinator.state.lock().unwrap();
        let record = &state.clients[&handle.id];
        assert!(record.submitted_operation.is_none());
        assert_eq!(
            record
                .prompt_draft
                .as_ref()
                .map(|draft| draft.text.as_str()),
            Some("shutdown keeps this exact draft")
        );
    }

    #[test]
    fn submission_transition_commits_running_before_lifecycle_revocation() {
        let coordinator = SnapshotCoordinator::new();
        let service = ClientService::new(coordinator.clone());
        let handle = service
            .connect_or_takeover(ClientConnectionId::new("submission-first-client"))
            .unwrap();
        service
            .set_prompt_draft(
                &handle,
                Some(snapshot_coordinator::DraftRecord {
                    id: "submission-first-draft".into(),
                    kind: ClientDraftKind::Prompt,
                    text: "submission consumes this exact draft".into(),
                    fingerprint: "submission consumes this exact draft".into(),
                }),
            )
            .unwrap();
        let descriptor = CodingAgentOperation::Prompt(prompt_options(
            "submission-first",
            "submission consumes this exact draft",
        ))
        .descriptor();
        let mut guard = submission_guard_for_test(&service, handle.clone(), descriptor);
        let (entered_rx, release_tx) = coordinator.install_submission_transition_probe_for_tests();

        let commit_thread = std::thread::spawn(move || {
            guard.commit("op-submission-first".into()).unwrap();
            guard
        });
        entered_rx
            .recv()
            .expect("submission transition must pause while holding coordinator state");

        let lifecycle_service = service.clone();
        let lifecycle_handle = handle.clone();
        let (started_tx, started_rx) = std::sync::mpsc::channel();
        let (detached_tx, detached_rx) = std::sync::mpsc::channel();
        let detach_thread = std::thread::spawn(move || {
            started_tx.send(()).unwrap();
            let outcome = lifecycle_service
                .coordinator
                .detach(&lifecycle_handle)
                .unwrap();
            detached_tx.send(outcome).unwrap();
        });
        started_rx.recv().unwrap();
        assert!(matches!(
            detached_rx.try_recv(),
            Err(std::sync::mpsc::TryRecvError::Empty)
        ));

        release_tx.send(()).unwrap();
        let _committed_guard = commit_thread.join().unwrap();
        assert_eq!(
            detached_rx.recv().unwrap(),
            snapshot_coordinator::ClientDetachOutcome::Detached
        );
        detach_thread.join().unwrap();

        let state = coordinator.state.lock().unwrap();
        let record = &state.clients[&handle.id];
        assert!(record.prompt_draft.is_none());
        assert_eq!(
            record.submitted_operation,
            Some(snapshot_coordinator::SubmittedOperationStatus::Running {
                operation_id: "op-submission-first".into(),
                kind: OperationKind::Prompt,
                descriptor,
            })
        );
    }

    #[tokio::test]
    async fn prompt_uses_owner_issued_follow_up_control_handle() {
        let api = "coding-session-follow-up-control";
        let contexts = Arc::new(Mutex::new(Vec::new()));
        let (started_tx, started_rx) = oneshot::channel();
        let (release_tx, release_rx) = oneshot::channel();
        let _provider_guard = crate::test_support::ProviderGuard::register(
            api,
            Arc::new(BlockingTwoTurnProvider::new(
                contexts.clone(),
                started_tx,
                release_rx,
            )),
        );
        let mut session = CodingAgentSession::non_persistent_internal(
            CodingAgentSessionOptions::new().with_ai_client(_provider_guard.ai_client()),
        )
        .await
        .unwrap();
        let handle = session.prompt_control_handle().unwrap();

        let mut prompt = Box::pin(
            session.run_internal(CodingAgentOperation::Prompt(prompt_options(api, "hello"))),
        );
        tokio::select! {
            started = started_rx => started.unwrap(),
            result = &mut prompt => panic!("prompt finished before provider blocked: {result:?}"),
        }
        handle.follow_up("continue from session owner").unwrap();
        release_tx.send(()).unwrap();

        let outcome = prompt.await.unwrap();
        let CodingAgentOperationOutcome::Prompt(outcome) = outcome else {
            panic!("prompt operation returned another outcome")
        };

        assert!(matches!(
            outcome,
            PromptTurnOutcome::Success { final_text, .. } if final_text == "second"
        ));
        let contexts = contexts.lock().unwrap();
        assert_eq!(contexts.len(), 2);
        assert!(
            contexts[1].messages.iter().any(|message| matches!(
                message,
                Message::User { content }
                    if content.iter().any(|block| matches!(
                        block,
                        ContentBlock::Text { text, .. } if text == "continue from session owner"
                    ))
            )),
            "{:#?}",
            contexts[1].messages
        );
    }

    #[tokio::test]
    async fn public_prompt_run_clears_pending_control_when_replay_fails_before_admission() {
        let api = "coding-session-pre-admission-control-cleanup";
        let _provider_guard = crate::test_support::ProviderGuard::register(
            api,
            Arc::new(FauxProvider::simple_text("unused")),
        );
        let temp = tempfile::tempdir().unwrap();
        let session_id = "sess_pre_admission_control_cleanup";
        let options = CodingAgentSessionOptions::new()
            .with_session_id(session_id)
            .with_session_log_root(temp.path())
            .with_ai_client(_provider_guard.ai_client());
        let mut session = CodingAgentSession::create_internal(options).await.unwrap();
        fs::write(temp.path().join(session_id).join("events.jsonl"), "{}\n").unwrap();
        let _handle = session.prompt_control_handle().unwrap();

        let error = session
            .run_internal(CodingAgentOperation::Prompt(prompt_options(
                api,
                "trigger replay",
            )))
            .await
            .unwrap_err();

        assert!(
            error
                .to_string()
                .contains("required v2 session event frame"),
            "unexpected error: {error}"
        );
        assert!(
            session.prompt_control_handle().is_ok(),
            "failed pre-admission prompt must release prompt-control ownership"
        );
    }

    #[tokio::test]
    async fn run_operation_agent_team_uses_guard_and_preserves_input_error() {
        let mut session =
            CodingAgentSession::non_persistent_internal(CodingAgentSessionOptions::new())
                .await
                .unwrap();
        let operation = Operation::AgentTeam(AgentTeamOptions::new(
            "team",
            "",
            PromptTurnOptions::new(PromptInvocation::Text("task".into())),
        ));
        let error = session.run_operation(operation, None).await.unwrap_err();

        assert_eq!(error.code(), "input");
        assert!(
            error
                .to_string()
                .contains("agent team invocation requires a non-empty task"),
            "{error}"
        );
        assert_eq!(
            session.runtime_host.operation_supervisor.control.active(),
            None
        );
    }

    #[tokio::test]
    async fn runtime_owned_agent_invocation_can_coexist_with_a_session_prompt() {
        let api = "coding-session-runtime-owned-concurrency";
        let contexts = Arc::new(Mutex::new(Vec::new()));
        let (started_tx, started_rx) = oneshot::channel();
        let (release_tx, release_rx) = oneshot::channel();
        let _provider_guard = crate::test_support::ProviderGuard::register(
            api,
            Arc::new(BlockingTwoTurnProvider::new(
                contexts.clone(),
                started_tx,
                release_rx,
            )),
        );
        let mut session = CodingAgentSession::non_persistent_internal(
            CodingAgentSessionOptions::new().with_ai_client(_provider_guard.ai_client()),
        )
        .await
        .unwrap();

        let invocation_control = session.prompt_control_handle().unwrap();
        let invocation = session
            .submit_internal(CodingAgentOperation::InvokeAgent(
                AgentInvocationOptions::new(
                    "default",
                    "background task",
                    prompt_options(api, "background task"),
                ),
            ))
            .unwrap();
        assert!(!invocation.operation_id().is_empty());
        tokio::time::timeout(std::time::Duration::from_secs(2), started_rx)
            .await
            .expect("runtime-owned invocation did not reach provider")
            .unwrap();
        assert_eq!(
            session.runtime_host.operation_supervisor.control.active(),
            Some(OperationKind::AgentInvocation)
        );
        let prompt_control = session
            .prompt_control_handle()
            .expect("runtime-owned invocation must transfer its control receiver before return");
        drop((invocation_control, prompt_control));

        let prompt = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            session.run_internal(CodingAgentOperation::Prompt(prompt_options(
                api,
                "foreground prompt",
            ))),
        )
        .await
        .expect("session prompt was blocked by independent non-session root")
        .unwrap();
        assert!(matches!(
            prompt,
            CodingAgentOperationOutcome::Prompt(PromptTurnOutcome::Success {
                final_text,
                ..
            }) if final_text == "second"
        ));
        assert_eq!(
            session.runtime_host.operation_supervisor.control.active(),
            Some(OperationKind::AgentInvocation)
        );

        release_tx.send(()).unwrap();
        let invocation = tokio::time::timeout(std::time::Duration::from_secs(2), invocation.join())
            .await
            .expect("runtime-owned invocation did not finish")
            .unwrap();
        assert!(matches!(
            invocation,
            CodingAgentOperationOutcome::AgentInvocation(AgentInvocationOutcome {
                final_text,
                ..
            }) if final_text == "first"
        ));
        assert_eq!(
            session.runtime_host.operation_supervisor.control.active(),
            None
        );
        assert_eq!(contexts.lock().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn runtime_submit_rejects_session_writes_without_consuming_a_root_slot() {
        let mut session =
            CodingAgentSession::non_persistent_internal(CodingAgentSessionOptions::new())
                .await
                .unwrap();

        let error = session
            .submit_internal(CodingAgentOperation::Prompt(PromptTurnOptions::new(
                PromptInvocation::Text("not detached".into()),
            )))
            .unwrap_err();

        assert_eq!(error.code(), "unsupported_capability");
        assert!(
            error
                .to_string()
                .contains("supported async non-session roots")
        );
        assert_eq!(
            session.runtime_host.operation_supervisor.control.active(),
            None
        );
    }

    #[tokio::test]
    async fn runtime_owned_submission_keeps_client_terminal_association() {
        let api = "coding-session-runtime-owned-submission";
        let _provider_guard = crate::test_support::ProviderGuard::register(
            api,
            Arc::new(FauxProvider::simple_text("runtime-owned result")),
        );
        let mut session = CodingAgentSession::non_persistent_internal(
            CodingAgentSessionOptions::new().with_ai_client(_provider_guard.ai_client()),
        )
        .await
        .unwrap();
        let connection = session
            .connect(public_projection::CodingAgentClientId::new(
                "runtime-owned-client",
            ))
            .unwrap();
        let mut events = session.subscribe_product_events();
        let operation = CodingAgentOperation::InvokeAgent(AgentInvocationOptions::new(
            "default",
            "detached task",
            prompt_options(api, "detached task"),
        ));
        let lease = connection
            .prepare_submission(
                &mut session,
                public_projection::CodingAgentDraftId("unused-for-non-prompt".into()),
                &operation,
            )
            .unwrap();

        let task = session.submit_internal(operation).unwrap();
        let operation_id = task.operation_id().to_owned();
        let running = connection
            .state()
            .unwrap()
            .submitted_operation
            .expect("submitted runtime task should be running");
        assert_eq!(running.operation_id, operation_id);
        assert!(matches!(
            running.status,
            public_projection::CodingAgentSubmittedOperationStatus::Running
        ));

        let outcome = task.join().await.unwrap();
        assert!(matches!(
            &outcome,
            CodingAgentOperationOutcome::AgentInvocation(AgentInvocationOutcome {
                operation_id: outcome_operation_id,
                final_text,
                ..
            }) if outcome_operation_id == &operation_id && final_text == "runtime-owned result"
        ));
        while let Ok(Some(event)) = events.try_recv() {
            if matches!(
                event.event(),
                public_event::CodingAgentProductEventKind::Agent(
                    public_event::CodingAgentAgentProductEvent::InvocationStarted { .. }
                        | public_event::CodingAgentAgentProductEvent::InvocationCompleted { .. }
                        | public_event::CodingAgentAgentProductEvent::InvocationFailed { .. }
                        | public_event::CodingAgentAgentProductEvent::InvocationAborted { .. }
                )
            ) && let Some(event_operation_id) = event.operation_id()
            {
                assert_eq!(event_operation_id, operation_id);
            }
        }
        let terminal = connection
            .state()
            .unwrap()
            .submitted_operation
            .expect("submitted runtime task should be terminal");
        assert_eq!(terminal.operation_id, operation_id);
        assert!(matches!(
            terminal.status,
            public_projection::CodingAgentSubmittedOperationStatus::Terminal {
                status: public_event::CodingAgentProductEventTerminalStatus::Completed,
                ..
            }
        ));
        drop(lease);
    }

    #[tokio::test]
    async fn runtime_owned_agent_invocation_abort_cancels_by_operation_identity() {
        let api = "coding-session-runtime-owned-cancellation";
        let contexts = Arc::new(Mutex::new(Vec::new()));
        let (started_tx, started_rx) = oneshot::channel();
        let (release_tx, release_rx) = oneshot::channel();
        let _provider_guard = crate::test_support::ProviderGuard::register(
            api,
            Arc::new(BlockingTwoTurnProvider::new(
                contexts, started_tx, release_rx,
            )),
        );
        let mut session = CodingAgentSession::non_persistent_internal(
            CodingAgentSessionOptions::new().with_ai_client(_provider_guard.ai_client()),
        )
        .await
        .unwrap();
        let connection = session
            .connect(public_projection::CodingAgentClientId::new(
                "runtime-owned-cancellation-client",
            ))
            .unwrap();
        let operation = CodingAgentOperation::InvokeAgent(AgentInvocationOptions::new(
            "default",
            "cancel detached task",
            prompt_options(api, "cancel detached task"),
        ));
        let lease = connection
            .prepare_submission(
                &mut session,
                public_projection::CodingAgentDraftId("unused-for-cancel".into()),
                &operation,
            )
            .unwrap();
        let task = session.submit(operation).unwrap();
        let operation_id = task.operation_id().to_owned();

        started_rx.await.unwrap();
        connection
            .operation_control(operation_id.clone())
            .abort(
                public_projection::CodingAgentControlId("abort-runtime-owned".into()),
                "stop detached invocation",
            )
            .unwrap();
        release_tx.send(()).unwrap();

        assert_eq!(
            task.join_internal().await.unwrap_err(),
            CodingSessionError::Cancelled
        );
        let terminal = connection
            .state()
            .unwrap()
            .submitted_operation
            .expect("cancelled runtime task should be terminal");
        assert_eq!(terminal.operation_id, operation_id);
        assert!(matches!(
            terminal.status,
            public_projection::CodingAgentSubmittedOperationStatus::Terminal {
                status: public_event::CodingAgentProductEventTerminalStatus::Aborted,
                ..
            }
        ));
        drop(lease);
    }

    #[tokio::test]
    async fn runtime_submit_executes_agent_team_inside_the_owned_task() {
        let mut session =
            CodingAgentSession::non_persistent_internal(CodingAgentSessionOptions::new())
                .await
                .unwrap();
        let mut events = session.subscribe_product_events();
        let task = session
            .submit_internal(CodingAgentOperation::InvokeTeam(AgentTeamOptions::new(
                "team",
                "",
                PromptTurnOptions::new(PromptInvocation::Text("team task".into())),
            )))
            .unwrap();
        let operation_id = task.operation_id().to_owned();
        assert_eq!(
            session.runtime_host.operation_supervisor.control.active(),
            Some(OperationKind::AgentTeam)
        );

        let error = task.join_internal().await.unwrap_err();

        assert_eq!(error.code(), "input");
        assert!(
            error
                .to_string()
                .contains("agent team invocation requires a non-empty task")
        );
        while let Ok(Some(event)) = events.try_recv() {
            if matches!(
                event.event(),
                public_event::CodingAgentProductEventKind::Team(_)
            ) && let Some(event_operation_id) = event.operation_id()
            {
                assert_eq!(event_operation_id, operation_id);
            }
        }
        assert_eq!(
            session.runtime_host.operation_supervisor.control.active(),
            None
        );
    }

    #[tokio::test]
    async fn run_sync_operation_export_preserves_persistence_error_without_active_operation() {
        let session = CodingAgentSession::non_persistent_internal(CodingAgentSessionOptions::new())
            .await
            .unwrap();
        let operation = Operation::Export(ExportOptions::view());

        let error = session.run_sync_operation(operation, None).unwrap_err();

        assert_eq!(error.code(), "unsupported_capability");
        assert_eq!(
            error.to_string(),
            "unsupported capability: export requires a persistent Rust-native session"
        );
        assert_eq!(
            session.runtime_host.operation_supervisor.control.active(),
            None
        );
    }

    #[tokio::test]
    async fn run_sync_operation_export_uses_read_only_admission_while_root_busy() {
        let session = CodingAgentSession::non_persistent_internal(CodingAgentSessionOptions::new())
            .await
            .unwrap();
        let _guard = session
            .runtime_host
            .operation_supervisor
            .control
            .begin(OperationKind::Prompt, "op_test".into())
            .unwrap();

        let error = session
            .run_sync_operation(Operation::Export(ExportOptions::view()), None)
            .unwrap_err();

        assert_eq!(error.code(), "unsupported_capability");
        assert_eq!(
            error.to_string(),
            "unsupported capability: export requires a persistent Rust-native session"
        );
        assert_eq!(
            session.runtime_host.operation_supervisor.control.active(),
            Some(OperationKind::Prompt)
        );
    }

    #[tokio::test]
    async fn canonical_run_uses_each_metadata_dispatch_family() {
        let api = "coding-session-canonical-dispatch-families";
        let _provider_guard = crate::test_support::ProviderGuard::register(
            api,
            Arc::new(FauxProvider::with_call_queue(vec![
                FauxProvider::text_call("async answer", StopReason::Stop),
            ])),
        );
        let temp = tempfile::tempdir().unwrap();
        let mut session = CodingAgentSession::create_internal(
            CodingAgentSessionOptions::new()
                .with_ai_client(_provider_guard.ai_client())
                .with_session_id("sess_canonical_dispatch_families")
                .with_session_log_root(temp.path()),
        )
        .await
        .unwrap();

        let async_descriptor = CodingAgentOperation::Prompt(prompt_options(api, "async prompt"))
            .into_internal()
            .descriptor();
        assert_eq!(
            async_descriptor.dispatch_mode,
            crate::runtime::operation::OperationDispatchMode::Async
        );
        let async_outcome = session
            .run_internal(CodingAgentOperation::Prompt(prompt_options(
                api,
                "async prompt",
            )))
            .await
            .unwrap();
        assert!(matches!(
            async_outcome,
            CodingAgentOperationOutcome::Prompt(PromptTurnOutcome::Success { .. })
        ));

        let read_only_descriptor = CodingAgentOperation::ExportCurrent
            .into_internal()
            .descriptor();
        assert_eq!(
            read_only_descriptor.dispatch_mode,
            crate::runtime::operation::OperationDispatchMode::SyncReadOnly
        );
        let read_only_outcome = session
            .run_internal(CodingAgentOperation::ExportCurrent)
            .await
            .unwrap();
        assert!(read_only_outcome.into_export().is_ok());

        let sync_mut_descriptor = CodingAgentOperation::SetDefaultAgentProfile {
            profile_id: ProfileId::from("reviewer"),
        }
        .into_internal()
        .descriptor();
        assert_eq!(
            sync_mut_descriptor.dispatch_mode,
            crate::runtime::operation::OperationDispatchMode::SyncMutable
        );
        let sync_mut_outcome = session
            .run_internal(CodingAgentOperation::SetDefaultAgentProfile {
                profile_id: ProfileId::from("reviewer"),
            })
            .await
            .unwrap();
        assert!(matches!(
            sync_mut_outcome,
            CodingAgentOperationOutcome::DefaultAgentProfileChanged
        ));
        assert_eq!(session.default_agent_profile_id().as_str(), "reviewer");
    }

    #[tokio::test]
    async fn set_default_agent_profile_rejects_while_operation_is_busy() {
        let mut session =
            CodingAgentSession::non_persistent_internal(CodingAgentSessionOptions::new())
                .await
                .unwrap();
        let _guard = session
            .runtime_host
            .operation_supervisor
            .control
            .begin(OperationKind::Prompt, "op_test".into())
            .unwrap();

        let error = session
            .run_internal(CodingAgentOperation::SetDefaultAgentProfile {
                profile_id: ProfileId::from("agent-main"),
            })
            .await
            .unwrap_err();

        assert_eq!(error.code(), "busy");
        assert_eq!(
            session.runtime_host.operation_supervisor.control.active(),
            Some(OperationKind::Prompt)
        );
    }

    #[tokio::test]
    async fn fork_current_session_rejects_while_operation_is_busy() {
        let mut session =
            CodingAgentSession::non_persistent_internal(CodingAgentSessionOptions::new())
                .await
                .unwrap();
        let _guard = session
            .runtime_host
            .operation_supervisor
            .control
            .begin(OperationKind::Prompt, "op_test".into())
            .unwrap();

        let error = session
            .run_internal(CodingAgentOperation::ForkSession {
                target_leaf_id: None,
            })
            .await
            .unwrap_err();

        assert_eq!(error.code(), "busy");
        assert_eq!(
            session.runtime_host.operation_supervisor.control.active(),
            Some(OperationKind::Prompt)
        );
    }

    #[tokio::test]
    async fn canonical_run_switches_active_leaf() {
        let api = "coding-session-canonical-switch-active-leaf";
        let _provider_guard = crate::test_support::ProviderGuard::register(
            api,
            Arc::new(FauxProvider::with_call_queue(vec![
                FauxProvider::text_call("root answer", StopReason::Stop),
                FauxProvider::text_call("branch answer", StopReason::Stop),
            ])),
        );
        let temp = tempfile::tempdir().unwrap();
        let mut session = CodingAgentSession::create_internal(
            CodingAgentSessionOptions::new()
                .with_ai_client(_provider_guard.ai_client())
                .with_session_id("sess_canonical_switch_active_leaf")
                .with_session_log_root(temp.path()),
        )
        .await
        .unwrap();
        let target_leaf_id = match prompt_outcome(
            session
                .run_internal(CodingAgentOperation::Prompt(prompt_options(
                    api,
                    "root question",
                )))
                .await
                .unwrap(),
        ) {
            PromptTurnOutcome::Success {
                leaf_id: Some(leaf_id),
                ..
            } => leaf_id,
            other => panic!("expected root prompt success, got {other:?}"),
        };
        prompt_outcome(
            session
                .run_internal(CodingAgentOperation::Prompt(prompt_options(
                    api,
                    "branch question",
                )))
                .await
                .unwrap(),
        );

        let outcome = session
            .run_internal(CodingAgentOperation::SwitchActiveLeaf {
                target_leaf_id: target_leaf_id.clone(),
            })
            .await
            .unwrap();

        assert!(matches!(
            outcome,
            CodingAgentOperationOutcome::ActiveLeafSwitched
        ));
        let hydrated = session.hydrate_current().unwrap().unwrap();
        assert_eq!(
            hydrated.summary.active_leaf_id.as_deref(),
            Some(target_leaf_id.as_str())
        );
        assert_eq!(
            session
                .persistent_session_service()
                .replay()
                .unwrap()
                .active_leaf_id
                .as_deref(),
            Some(target_leaf_id.as_str())
        );
    }

    #[tokio::test]
    async fn canonical_run_forks_current_session() {
        let api = "coding-session-canonical-fork-current-session";
        let _provider_guard = crate::test_support::ProviderGuard::register(
            api,
            Arc::new(FauxProvider::with_call_queue(vec![
                FauxProvider::text_call("keep answer", StopReason::Stop),
                FauxProvider::text_call("drop answer", StopReason::Stop),
            ])),
        );
        let temp = tempfile::tempdir().unwrap();
        let mut session = CodingAgentSession::create_internal(
            CodingAgentSessionOptions::new()
                .with_ai_client(_provider_guard.ai_client())
                .with_session_id("sess_canonical_fork_source")
                .with_session_log_root(temp.path()),
        )
        .await
        .unwrap();
        let target_leaf_id = match prompt_outcome(
            session
                .run_internal(CodingAgentOperation::Prompt(prompt_options(
                    api,
                    "keep prompt",
                )))
                .await
                .unwrap(),
        ) {
            PromptTurnOutcome::Success {
                leaf_id: Some(leaf_id),
                ..
            } => leaf_id,
            other => panic!("expected selected prompt success, got {other:?}"),
        };
        prompt_outcome(
            session
                .run_internal(CodingAgentOperation::Prompt(prompt_options(
                    api,
                    "drop prompt",
                )))
                .await
                .unwrap(),
        );
        let original_session_id = session.persistent_session_service().session_id().to_owned();

        let outcome = session
            .run_internal(CodingAgentOperation::ForkSession {
                target_leaf_id: Some(target_leaf_id.clone()),
            })
            .await
            .unwrap();

        assert!(matches!(
            outcome,
            CodingAgentOperationOutcome::SessionForked
        ));
        let hydrated = session.hydrate_current().unwrap().unwrap();
        assert_ne!(hydrated.summary.session_id, original_session_id);
        assert_eq!(
            hydrated.summary.active_leaf_id.as_deref(),
            Some(target_leaf_id.as_str())
        );
        assert!(hydrated.transcript.iter().any(|item| matches!(
            item,
            CodingAgentSessionTranscriptItem::User { text } if text == "keep prompt"
        )));
        assert!(!hydrated.transcript.iter().any(|item| matches!(
            item,
            CodingAgentSessionTranscriptItem::User { text } if text == "drop prompt"
        )));
        let replay = session.persistent_session_service().replay().unwrap();
        assert_eq!(
            replay.active_leaf_id.as_deref(),
            Some(target_leaf_id.as_str())
        );
        assert!(replay.transcript.iter().any(|item| matches!(
            item,
            TranscriptItem::UserInput { text, .. } if text == "keep prompt"
        )));
        assert!(!replay.transcript.iter().any(|item| matches!(
            item,
            TranscriptItem::UserInput { text, .. } if text == "drop prompt"
        )));
    }

    #[tokio::test]
    async fn canonical_fork_preserves_owner_runtime_and_event_stream() {
        let api = "coding-session-canonical-fork-owner-continuity";
        let _provider_guard = crate::test_support::ProviderGuard::register(
            api,
            Arc::new(FauxProvider::with_call_queue(vec![
                FauxProvider::text_call("keep answer", StopReason::Stop),
            ])),
        );
        let temp = tempfile::tempdir().unwrap();
        let mut session = CodingAgentSession::create_internal(
            CodingAgentSessionOptions::new()
                .with_ai_client(_provider_guard.ai_client())
                .with_session_id("sess_canonical_fork_owner_continuity")
                .with_session_log_root(temp.path()),
        )
        .await
        .unwrap();
        let target_leaf_id = match prompt_outcome(
            session
                .run_internal(CodingAgentOperation::Prompt(prompt_options(
                    api,
                    "keep prompt",
                )))
                .await
                .unwrap(),
        ) {
            PromptTurnOutcome::Success {
                leaf_id: Some(leaf_id),
                ..
            } => leaf_id,
            other => panic!("expected selected prompt success, got {other:?}"),
        };
        session
            .run_internal(CodingAgentOperation::SetDefaultAgentProfile {
                profile_id: ProfileId::from("reviewer"),
            })
            .await
            .unwrap();
        let capability_generation_before = session.current_capability_generation_for_tests();
        let mut events = session.subscribe_product_events();

        session
            .run_internal(CodingAgentOperation::ForkSession {
                target_leaf_id: Some(target_leaf_id),
            })
            .await
            .unwrap();

        assert_eq!(
            session.current_capability_generation_for_tests(),
            capability_generation_before
        );
        session
            .run_internal(CodingAgentOperation::SetDefaultAgentProfile {
                profile_id: ProfileId::from("default"),
            })
            .await
            .unwrap();

        let emitted = std::iter::from_fn(|| events.try_recv().unwrap()).collect::<Vec<_>>();
        assert!(
            emitted.iter().any(|event| matches!(
                event.event(),
                CodingAgentProductEventKind::Session(
                    CodingAgentSessionProductEvent::Opened { session_id }
                )
                    if session_id == &session.view().session_id
            )),
            "pre-fork receiver should observe the forked session transition: {emitted:#?}"
        );
        assert!(
            emitted.iter().any(|event| matches!(
                event.event(),
                CodingAgentProductEventKind::Profile(
                    CodingAgentProfileProductEvent::DefaultChanged { profile_id }
                ) if profile_id == "default"
            )),
            "pre-fork receiver should observe post-fork runtime events: {emitted:#?}"
        );
        assert!(
            emitted
                .windows(2)
                .all(|events| events[0].sequence() < events[1].sequence()),
            "product event sequence should stay monotonic across fork: {emitted:#?}"
        );
    }

    #[tokio::test]
    async fn canonical_switch_reports_partial_commit_after_durable_leaf_change() {
        let api = "coding-session-canonical-switch-partial-commit";
        let _provider_guard = crate::test_support::ProviderGuard::register(
            api,
            Arc::new(FauxProvider::with_call_queue(vec![
                FauxProvider::text_call("root answer", StopReason::Stop),
                FauxProvider::text_call("branch answer", StopReason::Stop),
            ])),
        );
        let temp = tempfile::tempdir().unwrap();
        let mut session = CodingAgentSession::create_internal(
            CodingAgentSessionOptions::new()
                .with_ai_client(_provider_guard.ai_client())
                .with_session_id("sess_canonical_switch_partial_commit")
                .with_session_log_root(temp.path()),
        )
        .await
        .unwrap();
        let target_leaf_id = match prompt_outcome(
            session
                .run_internal(CodingAgentOperation::Prompt(prompt_options(
                    api,
                    "root question",
                )))
                .await
                .unwrap(),
        ) {
            PromptTurnOutcome::Success {
                leaf_id: Some(leaf_id),
                ..
            } => leaf_id,
            other => panic!("expected root prompt success, got {other:?}"),
        };
        prompt_outcome(
            session
                .run_internal(CodingAgentOperation::Prompt(prompt_options(
                    api,
                    "branch question",
                )))
                .await
                .unwrap(),
        );
        session
            .persistent_session_service()
            .fail_store_after_for_tests(StoreFailurePoint::UpdateManifest, 0);

        let error = session
            .run_internal(CodingAgentOperation::SwitchActiveLeaf {
                target_leaf_id: target_leaf_id.clone(),
            })
            .await
            .unwrap_err();

        assert!(matches!(
            &error,
            CodingSessionError::PartialCommit { operation_id, .. }
                if operation_id.starts_with("op_")
        ));
        assert_eq!(error.code(), "partial_commit");
        assert_eq!(
            session
                .persistent_session_service()
                .replay()
                .unwrap()
                .active_leaf_id
                .as_deref(),
            Some(target_leaf_id.as_str())
        );
    }

    #[tokio::test]
    async fn delegation_approval_operation_kind_uses_pending_team_target() {
        let mut session =
            CodingAgentSession::non_persistent_internal(CodingAgentSessionOptions::new())
                .await
                .unwrap();
        session
            .runtime_host
            .session_coordinator
            .pending_delegation_confirmations
            .push(pending_delegation_confirmation_state(ProfileKind::Team));
        let now = SystemClock.now_rfc3339();

        let kind = session
            .delegation_approval_operation_kind("op_parent", "tool_delegate", &now)
            .unwrap();

        assert_eq!(kind, OperationKind::AgentTeam);
    }

    #[tokio::test]
    async fn resolve_operation_admission_returns_structured_dynamic_contract() {
        let mut session =
            CodingAgentSession::non_persistent_internal(CodingAgentSessionOptions::new())
                .await
                .unwrap();
        session
            .runtime_host
            .session_coordinator
            .pending_delegation_confirmations
            .push(pending_delegation_confirmation_state(ProfileKind::Team));
        let operation = Operation::ApproveDelegationConfirmation {
            operation_id: "op_parent".into(),
            tool_call_id: "tool_delegate".into(),
        };

        let admission = session.resolve_operation_admission(&operation).unwrap();

        assert_eq!(admission.kind, OperationKind::AgentTeam);
        assert_eq!(
            admission.descriptor.submitted_kind,
            OperationKind::DelegationConfirmation
        );
        assert_eq!(
            admission.descriptor.dispatch_mode,
            crate::runtime::operation::OperationDispatchMode::Async
        );
        assert_eq!(
            admission.descriptor.admission_class(),
            crate::runtime::operation::OperationClass::SessionWriteRoot
        );
        assert!(admission.capability_snapshot.session_write.is_some());
        assert!(admission.admitted_at.is_some());
    }

    #[tokio::test]
    async fn resolve_operation_admission_returns_structured_static_contract() {
        let session = CodingAgentSession::non_persistent_internal(CodingAgentSessionOptions::new())
            .await
            .unwrap();
        let operation = Operation::RejectDelegationConfirmation {
            operation_id: "op_parent".into(),
            tool_call_id: "tool_delegate".into(),
            reason: "not now".into(),
        };

        let admission = session.resolve_operation_admission(&operation).unwrap();

        assert_eq!(admission.kind, OperationKind::DelegationConfirmation);
        assert_eq!(
            admission.descriptor.submitted_kind,
            OperationKind::DelegationConfirmation
        );
        assert_eq!(
            admission.descriptor.dispatch_mode,
            crate::runtime::operation::OperationDispatchMode::SyncMutable
        );
        assert_eq!(
            admission.descriptor.admission_class(),
            crate::runtime::operation::OperationClass::SessionWriteRoot
        );
        assert!(admission.capability_snapshot.session_write.is_some());
        assert_eq!(admission.admitted_at, None);
    }

    #[tokio::test]
    async fn non_persistent_admission_freezes_operation_runtime_cwd_handles() {
        let cwd = tempfile::tempdir().unwrap();
        let session = CodingAgentSession::non_persistent_internal(CodingAgentSessionOptions::new())
            .await
            .unwrap();
        let options = PromptTurnOptions::from_prompt_runtime_options(PromptRuntimeOptions {
            model: model("runtime-cwd-capability"),
            api_key: None,
            auth_diagnostics: Vec::new(),
            system_prompt: None,
            max_turns: Some(1),
            tools: crate::tools::builtin_tools(cwd.path().to_path_buf()).unwrap(),
            register_builtins: false,
            ai_client: None,
            session: Some(SessionRunOptions::disabled(cwd.path().to_path_buf())),
            session_target: None,
            session_name: None,
            thinking_level: None,
            tool_execution: None,
            resources: AgentResources::default(),
            settings: None,
            invocation: PromptInvocation::Text("read".into()),
        });

        let admission = session
            .resolve_operation_admission(&Operation::Prompt(options))
            .unwrap();

        assert_eq!(
            admission
                .capability_snapshot
                .filesystem
                .as_ref()
                .map(|capability| capability.cwd.as_path()),
            Some(cwd.path())
        );
        assert_eq!(
            admission
                .capability_snapshot
                .shell
                .as_ref()
                .map(|capability| capability.cwd.as_path()),
            Some(cwd.path())
        );
    }

    #[tokio::test]
    async fn run_operation_delegation_approval_preserves_missing_pending_before_busy() {
        let mut session =
            CodingAgentSession::non_persistent_internal(CodingAgentSessionOptions::new())
                .await
                .unwrap();
        let _operation = session
            .runtime_host
            .operation_supervisor
            .control
            .begin(OperationKind::Prompt, "op_test".into())
            .unwrap();
        let operation = Operation::ApproveDelegationConfirmation {
            operation_id: "missing_op".into(),
            tool_call_id: "missing_tool".into(),
        };

        let error = session.run_operation(operation, None).await.unwrap_err();

        assert_eq!(error.code(), "input");
        assert!(
            error
                .to_string()
                .contains("pending delegation confirmation not found"),
            "{error}"
        );
        assert_eq!(
            session.runtime_host.operation_supervisor.control.active(),
            Some(OperationKind::Prompt)
        );
    }

    #[tokio::test]
    async fn delegation_approval_cannot_overlap_an_active_session_writer() {
        let mut session =
            CodingAgentSession::non_persistent_internal(CodingAgentSessionOptions::new())
                .await
                .unwrap();
        session
            .runtime_host
            .session_coordinator
            .pending_delegation_confirmations
            .push(pending_delegation_confirmation_state(ProfileKind::Agent));
        let _operation = session
            .runtime_host
            .operation_supervisor
            .control
            .begin(OperationKind::Prompt, "op_test".into())
            .unwrap();

        let error = session
            .run_operation(
                Operation::ApproveDelegationConfirmation {
                    operation_id: "op_parent".into(),
                    tool_call_id: "tool_delegate".into(),
                },
                None,
            )
            .await
            .unwrap_err();

        assert_eq!(
            error,
            CodingSessionError::Busy {
                operation: "prompt".into(),
            }
        );
        assert_eq!(
            session
                .runtime_host
                .session_coordinator
                .pending_delegation_confirmations()
                .len(),
            1
        );
        assert_eq!(
            session.runtime_host.operation_supervisor.control.active(),
            Some(OperationKind::Prompt)
        );
    }

    #[tokio::test]
    async fn reject_delegation_confirmation_cannot_overlap_an_active_session_writer() {
        let mut session =
            CodingAgentSession::non_persistent_internal(CodingAgentSessionOptions::new())
                .await
                .unwrap();
        session
            .runtime_host
            .session_coordinator
            .pending_delegation_confirmations
            .push(pending_delegation_confirmation_state(ProfileKind::Agent));
        let _operation = session
            .runtime_host
            .operation_supervisor
            .control
            .begin(OperationKind::Prompt, "op_test".into())
            .unwrap();

        let error = session
            .run_internal(CodingAgentOperation::RejectDelegation {
                operation_id: "op_parent".into(),
                tool_call_id: "tool_delegate".into(),
                reason: "not now".into(),
            })
            .await
            .unwrap_err();

        assert_eq!(
            error,
            CodingSessionError::Busy {
                operation: "prompt".into(),
            }
        );
        assert_eq!(
            session
                .runtime_host
                .session_coordinator
                .pending_delegation_confirmations()
                .len(),
            1
        );
        assert_eq!(
            session.runtime_host.operation_supervisor.control.active(),
            Some(OperationKind::Prompt)
        );
    }

    #[tokio::test]
    async fn run_operation_agent_invocation_uses_guard_and_preserves_input_error() {
        let mut session =
            CodingAgentSession::non_persistent_internal(CodingAgentSessionOptions::new())
                .await
                .unwrap();
        let _handle = session
            .runtime_host
            .operation_supervisor
            .control
            .prompt_control_handle()
            .unwrap();
        let operation = Operation::AgentInvocation(AgentInvocationOptions::new(
            "helper",
            "",
            PromptTurnOptions::new(PromptInvocation::Text("task".into())),
        ));
        let error = session.run_operation(operation, None).await.unwrap_err();

        assert_eq!(error.code(), "input");
        assert!(
            error
                .to_string()
                .contains("agent invocation requires a non-empty task"),
            "{error}"
        );
        assert_eq!(
            session.runtime_host.operation_supervisor.control.active(),
            None
        );
        assert!(
            session
                .runtime_host
                .operation_supervisor
                .control
                .prompt_control_handle()
                .is_ok()
        );
    }

    #[tokio::test]
    async fn run_operation_self_healing_edit_uses_guard_and_preserves_persistence_error() {
        let mut session =
            CodingAgentSession::non_persistent_internal(CodingAgentSessionOptions::new())
                .await
                .unwrap();
        let operation = Operation::SelfHealingEdit(SelfHealingEditRequest::new(
            "src/lib.rs",
            vec![SelfHealingEditReplacement::new("old", "new")],
        ));

        let error = session.run_operation(operation, None).await.unwrap_err();

        assert_eq!(error.code(), "unsupported_capability");
        assert!(
            error
                .to_string()
                .contains("self-healing edit requires a persistent Rust-native session"),
            "{error}"
        );
        assert_eq!(
            session.runtime_host.operation_supervisor.control.active(),
            None
        );
    }

    #[tokio::test]
    async fn self_healing_edit_terminals_persist_and_restart_with_typed_family() {
        let temp = tempfile::tempdir().unwrap();
        let workspace = temp.path().join("workspace");
        let sessions = temp.path().join("sessions");
        fs::create_dir_all(workspace.join("src")).unwrap();
        fs::write(workspace.join("src/app.txt"), "one\ntwo\n").unwrap();
        let session_id = "sess_self_healing_terminal_outbox";
        let mut session = CodingAgentSession::create_internal(
            CodingAgentSessionOptions::new()
                .with_session_id(session_id)
                .with_cwd(&workspace)
                .with_session_log_root(&sessions),
        )
        .await
        .unwrap();
        let mut product_events = session.subscribe_product_events();

        session
            .run_internal(CodingAgentOperation::SelfHealingEdit(
                SelfHealingEditRequest::new(
                    "src/app.txt",
                    vec![SelfHealingEditReplacement::new("two", "deux")],
                ),
            ))
            .await
            .unwrap();
        session
            .run_internal(CodingAgentOperation::SelfHealingEdit(
                SelfHealingEditRequest::new(
                    "src/app.txt",
                    vec![SelfHealingEditReplacement::new("", "invalid")],
                ),
            ))
            .await
            .unwrap_err();

        let emitted = std::iter::from_fn(|| product_events.try_recv().unwrap()).collect::<Vec<_>>();
        for expected in ["self_healing_edit_completed", "self_healing_edit_failed"] {
            assert!(emitted.iter().any(|event| {
                typed_event_kind(event.event()) == expected
                    && event.terminal_operation().is_some_and(|terminal| {
                        terminal.kind
                            == crate::events::CodingAgentProductEventTerminalOperationKind::SelfHealingEdit
                    })
            }));
        }
        let event_log = fs::read_to_string(sessions.join(session_id).join("events.jsonl")).unwrap();
        assert_eq!(event_log.matches("operation.terminal.recorded").count(), 2);
        let outbox = fs::read_to_string(sessions.join(session_id).join("outbox.jsonl")).unwrap();
        assert!(outbox.contains("self_healing_edit_completed"));
        assert!(outbox.contains("self_healing_edit_failed"));
        assert!(outbox.contains("\"operation_kind\":\"self_healing_edit\""));

        session.shutdown_internal().await.unwrap();
        drop(session);
        let reopened = CodingAgentSession::open_internal(
            CodingAgentSessionOptions::new()
                .with_session_id(session_id)
                .with_session_log_root(&sessions),
        )
        .await
        .unwrap();
        let connection = reopened
            .connect(public_projection::CodingAgentClientId::new(
                "self-healing-replay",
            ))
            .unwrap();
        let public_projection::CodingAgentReconnect::Replayed { events, .. } =
            connection.reconnect(0).unwrap()
        else {
            panic!("self-healing terminals must be retained for restart redelivery")
        };
        for expected in ["self_healing_edit_completed", "self_healing_edit_failed"] {
            assert!(events.iter().any(|event| {
                typed_event_kind(event.event()) == expected
                    && event.terminal_operation().is_some_and(|terminal| {
                        terminal.kind
                            == crate::events::CodingAgentProductEventTerminalOperationKind::SelfHealingEdit
                    })
            }));
        }
    }

    #[tokio::test]
    async fn run_operation_branch_summary_uses_branch_summary_guard_and_preserves_persistence_error()
     {
        let mut session =
            CodingAgentSession::non_persistent_internal(CodingAgentSessionOptions::new())
                .await
                .unwrap();
        let operation = Operation::BranchSummary {
            options: PromptTurnOptions::new(PromptInvocation::Text("summarize".into())),
            source_leaf_id: "source_leaf".into(),
            target_leaf_id: "target_leaf".into(),
            custom_instructions: None,
            reuse_existing: false,
        };

        let error = session.run_operation(operation, None).await.unwrap_err();

        assert_eq!(error.code(), "unsupported_capability");
        assert!(
            error
                .to_string()
                .contains("branch summary without persistent session"),
            "{error}"
        );
        assert_eq!(
            session.runtime_host.operation_supervisor.control.active(),
            None
        );
    }

    #[tokio::test]
    async fn run_operation_manual_compaction_uses_compact_guard_and_preserves_config_error() {
        let mut session =
            CodingAgentSession::non_persistent_internal(CodingAgentSessionOptions::new())
                .await
                .unwrap();
        let operation =
            Operation::ManualCompaction(PromptTurnOptions::new(PromptInvocation::Compact {
                custom_instructions: None,
            }));

        let error = session.run_operation(operation, None).await.unwrap_err();

        assert_eq!(error.code(), "config");
        assert!(
            error
                .to_string()
                .contains("compact operation options do not include a runtime snapshot"),
            "{error}"
        );
        assert_eq!(
            session.runtime_host.operation_supervisor.control.active(),
            None
        );
    }

    #[tokio::test]
    async fn run_operation_prompt_uses_prompt_guard_and_preserves_prompt_error() {
        let mut session =
            CodingAgentSession::non_persistent_internal(CodingAgentSessionOptions::new())
                .await
                .unwrap();
        let operation = Operation::Prompt(PromptTurnOptions::new(PromptInvocation::Text(
            "hello".into(),
        )));

        let error = session.run_operation(operation, None).await.unwrap_err();

        assert_eq!(error.code(), "config");
        assert!(error.to_string().contains("runtime snapshot"), "{error}");
        assert_eq!(
            session.runtime_host.operation_supervisor.control.active(),
            None
        );
    }

    #[tokio::test]
    async fn prompt_runs_flow_and_commits_session_events() {
        let api = "coding-session-prompt";
        let _provider_guard = crate::test_support::ProviderGuard::register(
            api,
            Arc::new(FauxProvider::simple_text("session answer")),
        );
        let temp = tempfile::tempdir().unwrap();
        let options = CodingAgentSessionOptions::new()
            .with_ai_client(_provider_guard.ai_client())
            .with_session_id("sess_prompt")
            .with_session_log_root(temp.path());
        let mut session = CodingAgentSession::create_internal(options.clone())
            .await
            .unwrap();
        let mut events = session.subscribe_product_events();

        let outcome = prompt_outcome(
            session
                .run_internal(CodingAgentOperation::Prompt(prompt_options(api, "hello")))
                .await
                .unwrap(),
        );

        let leaf_id = match &outcome {
            PromptTurnOutcome::Success {
                final_text,
                session_id: Some(session_id),
                leaf_id: Some(leaf_id),
                ..
            } if final_text == "session answer" && session_id == "sess_prompt" => leaf_id.clone(),
            other => panic!("expected successful prompt with committed leaf, got {other:?}"),
        };
        assert!(leaf_id.starts_with("leaf_"));
        assert!(matches!(
            events.try_recv().unwrap().as_ref().map(ProductEvent::event),
            Some(CodingAgentProductEventKind::Workflow(
                CodingAgentWorkflowProductEvent::PromptStarted { .. }
            ))
        ));
        assert!(matches!(
            events.try_recv().unwrap().as_ref().map(ProductEvent::event),
            Some(CodingAgentProductEventKind::Agent(
                CodingAgentAgentProductEvent::TurnStarted { .. }
            ))
        ));
        let remaining_events =
            std::iter::from_fn(|| events.try_recv().unwrap()).collect::<Vec<_>>();
        assert_event_order(
            &remaining_events,
            &[
                "session_write_pending",
                "session_write_committed",
                "prompt_completed",
            ],
        );
        assert_eq!(
            remaining_events
                .iter()
                .filter(|event| matches!(
                    event.event(),
                    CodingAgentProductEventKind::Workflow(
                        CodingAgentWorkflowProductEvent::PromptCompleted { .. }
                    )
                ))
                .count(),
            1
        );

        let replay = session.persistent_session_service().replay().unwrap();
        assert_eq!(replay.active_leaf_id.as_deref(), Some(leaf_id.as_str()));
        assert!(matches!(
            replay.transcript.as_slice(),
            [
                TranscriptItem::UserInput {
                    turn_id,
                    text,
                },
                TranscriptItem::AssistantMessage {
                    content,
                    status: MessageStatus::Completed,
                    ..
                },
            ] if turn_id == outcome_turn_id(&outcome)
                && text == "hello"
                && content == &vec![PersistedContentBlock::Text {
                    text: "session answer".into(),
                }]
        ));
        let event_log =
            std::fs::read_to_string(temp.path().join("sess_prompt/events.jsonl")).unwrap();
        assert!(!event_log.contains("\"message.delta\""));
        assert!(event_log.contains("\"kind\":\"message.completed\""));
        assert!(event_log.contains("\"content\""));
        let committed_leaf = event_log
            .lines()
            .filter_map(|line| {
                crate::session::repository::decode_durable_record_for_tests::<SessionEventEnvelope>(
                    line,
                )
                .ok()
            })
            .find_map(|event| match event.data {
                SessionEventData::OperationCommitted {
                    new_leaf_id: Some(leaf_id),
                } => Some(leaf_id),
                _ => None,
            })
            .unwrap();
        assert_eq!(committed_leaf, leaf_id);
        let hydrated = session.hydrate_current().unwrap().unwrap();
        assert_eq!(
            hydrated.summary.active_leaf_id.as_deref(),
            Some(leaf_id.as_str())
        );
        let summaries = CodingAgentSession::list_internal(options).unwrap();
        assert_eq!(
            summaries[0].active_leaf_id.as_deref(),
            Some(leaf_id.as_str())
        );
        assert_eq!(session.view().session_id, "sess_prompt");
    }

    #[tokio::test]
    async fn parallel_sessions_with_the_same_api_use_their_scoped_ai_clients() {
        let api = "coding-session-shared-scoped-api";
        let first_client = AiClient::new();
        first_client.register_provider(api, Arc::new(FauxProvider::simple_text("first session")));
        let second_client = AiClient::new();
        second_client.register_provider(api, Arc::new(FauxProvider::simple_text("second session")));
        let mut first = CodingAgentSession::non_persistent_internal(
            CodingAgentSessionOptions::new().with_ai_client(first_client),
        )
        .await
        .unwrap();
        let mut second = CodingAgentSession::non_persistent_internal(
            CodingAgentSessionOptions::new().with_ai_client(second_client),
        )
        .await
        .unwrap();

        let (first_outcome, second_outcome) = tokio::join!(
            first.run_internal(CodingAgentOperation::Prompt(prompt_options(api, "first"))),
            second.run_internal(CodingAgentOperation::Prompt(prompt_options(api, "second"))),
        );

        let final_text = |outcome| match prompt_outcome(outcome) {
            PromptTurnOutcome::Success { final_text, .. } => final_text,
            other => panic!("expected successful scoped prompt, got {other:?}"),
        };
        assert_eq!(final_text(first_outcome.unwrap()), "first session");
        assert_eq!(final_text(second_outcome.unwrap()), "second session");
    }

    #[tokio::test]
    async fn prompt_requires_runtime_backed_options() {
        let temp = tempfile::tempdir().unwrap();
        let mut session = CodingAgentSession::create_internal(
            CodingAgentSessionOptions::new()
                .with_session_id("sess_prompt_missing_runtime")
                .with_session_log_root(temp.path()),
        )
        .await
        .unwrap();

        let error = session
            .run_internal(CodingAgentOperation::Prompt(PromptTurnOptions::new(
                PromptInvocation::Text("hello".into()),
            )))
            .await
            .unwrap_err();

        assert_eq!(error.code(), "config");
        assert!(error.to_string().contains("runtime snapshot"));
        assert!(
            session
                .persistent_session_service()
                .replay()
                .unwrap()
                .transcript
                .is_empty()
        );
        assert!(
            session
                .hydrate_current()
                .unwrap()
                .unwrap()
                .summary
                .active_leaf_id
                .is_none()
        );
    }

    #[tokio::test]
    async fn non_persistent_constructor_does_not_create_session_files() {
        let temp = tempfile::tempdir().unwrap();
        let session = CodingAgentSession::non_persistent_internal(
            CodingAgentSessionOptions::new().with_session_log_root(temp.path()),
        )
        .await
        .unwrap();

        assert!(session.view().session_id.starts_with("runtime_sess_"));
        assert!(std::fs::read_dir(temp.path()).unwrap().next().is_none());
    }

    #[tokio::test]
    async fn non_persistent_prompt_emits_skipped_write_before_completion() {
        let api = "coding-session-non-persistent-prompt";
        let _provider_guard = crate::test_support::ProviderGuard::register(
            api,
            Arc::new(FauxProvider::simple_text("transient answer")),
        );
        let mut session = CodingAgentSession::non_persistent_internal(
            CodingAgentSessionOptions::new().with_ai_client(_provider_guard.ai_client()),
        )
        .await
        .unwrap();
        let mut events = session.subscribe_product_events();

        let outcome = prompt_outcome(
            session
                .run_internal(CodingAgentOperation::Prompt(prompt_options(api, "hello")))
                .await
                .unwrap(),
        );

        assert!(matches!(
            &outcome,
            PromptTurnOutcome::Success {
                final_text,
                session_id: None,
                leaf_id: None,
                ..
            } if final_text == "transient answer"
        ));
        let emitted_events = std::iter::from_fn(|| events.try_recv().unwrap()).collect::<Vec<_>>();
        assert_event_order(
            &emitted_events,
            &["session_write_skipped", "prompt_completed"],
        );
        assert!(emitted_events.iter().any(|event| matches!(
            event.event(),
            CodingAgentProductEventKind::Session(
                CodingAgentSessionProductEvent::WriteSkipped { reason, .. }
            ) if reason == "session persistence disabled"
        )));
    }

    #[tokio::test]
    async fn resolved_prompt_credentials_never_enter_product_projection_or_events() {
        const SECRET: &str = "resolved-product-credential-canary";
        let api = "coding-session-resolved-credential-canary";
        let provider_guard = crate::test_support::ProviderGuard::register(
            api,
            Arc::new(FauxProvider::simple_text("credential-safe answer")),
        );
        let mut session = CodingAgentSession::non_persistent_internal(
            CodingAgentSessionOptions::new().with_ai_client(provider_guard.ai_client()),
        )
        .await
        .unwrap();
        let mut events = session.subscribe_product_events();
        let operation = CodingAgentOperation::Prompt(
            PromptTurnOptions::from_prompt_runtime_options(PromptRuntimeOptions {
                model: model(api),
                api_key: Some(SECRET.into()),
                auth_diagnostics: Vec::new(),
                system_prompt: Some("system".into()),
                max_turns: Some(2),
                tools: Vec::new(),
                register_builtins: false,
                ai_client: Some(provider_guard.ai_client()),
                session: Some(SessionRunOptions::disabled(".".into())),
                session_target: None,
                session_name: None,
                thinking_level: None,
                tool_execution: None,
                resources: AgentResources::default(),
                settings: None,
                invocation: PromptInvocation::Text("verify credential boundary".into()),
            }),
        );
        let operation_debug = format!("{operation:?}");
        assert!(!operation_debug.contains(SECRET), "{operation_debug}");

        let outcome = session.run(operation).await.unwrap();
        let emitted_events = std::iter::from_fn(|| events.try_recv().unwrap()).collect::<Vec<_>>();
        let public_artifacts = format!(
            "{:?}\n{}\n{:?}\n{:?}",
            outcome,
            serde_json::to_string(&emitted_events).unwrap(),
            session.snapshot(),
            session.transcript_snapshot().unwrap()
        );
        assert!(!public_artifacts.contains(SECRET), "{public_artifacts}");
    }

    #[tokio::test]
    async fn persistent_prompt_terminal_is_recorded_in_operation_terminal_outbox() {
        let api = "coding-session-terminal-outbox-prompt";
        let _provider_guard = crate::test_support::ProviderGuard::register(
            api,
            Arc::new(FauxProvider::simple_text("durable terminal")),
        );
        let temp = tempfile::tempdir().unwrap();
        let session_id = "sess_terminal_operation_outbox";
        let mut session = CodingAgentSession::create_internal(
            CodingAgentSessionOptions::new()
                .with_session_id(session_id)
                .with_session_log_root(temp.path())
                .with_ai_client(_provider_guard.ai_client()),
        )
        .await
        .unwrap();

        let outcome = session
            .run_internal(CodingAgentOperation::Prompt(prompt_options(api, "hello")))
            .await
            .unwrap();
        assert!(matches!(
            prompt_outcome(outcome),
            PromptTurnOutcome::Success { .. }
        ));

        let records = std::fs::read_to_string(temp.path().join(session_id).join("outbox.jsonl"))
            .unwrap()
            .lines()
            .map(
                crate::session::repository::decode_durable_record_for_tests::<
                    crate::events::outbox::DurableOutboxRecord,
                >,
            )
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert!(records.iter().any(|record| {
            record.kind == crate::events::outbox::DurableOutboxRecordKind::OperationTerminal
                && record.record_id.ends_with("/operation_terminal")
                && record.operation_id.is_some()
        }));

        session.shutdown_internal().await.unwrap();
        drop(session);
        let reopened = CodingAgentSession::open_internal(
            CodingAgentSessionOptions::new()
                .with_session_id(session_id)
                .with_session_log_root(temp.path()),
        )
        .await
        .unwrap();
        let connection = reopened
            .connect(public_projection::CodingAgentClientId::new(
                "terminal-replay",
            ))
            .unwrap();
        let public_projection::CodingAgentReconnect::Replayed { events, .. } =
            connection.reconnect(0).unwrap()
        else {
            panic!("terminal outbox must be retained for restart redelivery")
        };
        assert!(events.iter().any(|event| {
            event.operation_id().is_some()
                && event.terminal_operation().is_some()
                && matches!(
                    event.event(),
                    CodingAgentProductEventKind::Workflow(
                        CodingAgentWorkflowProductEvent::PromptCompleted { .. }
                    )
                )
        }));
    }

    #[tokio::test]
    async fn first_successful_exchange_auto_names_once_and_accounts_usage() {
        let api = "coding-session-auto-name-once";
        let provider = Arc::new(FauxProvider::with_call_queue(vec![
            FauxProvider::text_call("first answer", StopReason::Stop),
            FauxProvider::text_call("First concise title", StopReason::Stop),
            FauxProvider::text_call("second answer", StopReason::Stop),
        ]));
        let provider_guard = crate::test_support::ProviderGuard::register(api, provider);
        let temp = tempfile::tempdir().unwrap();
        let session_id = "sess_auto_name_once";
        let options = CodingAgentSessionOptions::new()
            .with_ai_client(provider_guard.ai_client())
            .with_session_id(session_id)
            .with_session_log_root(temp.path());
        let mut session = CodingAgentSession::create_internal(options.clone())
            .await
            .unwrap();

        let first = session
            .run_internal(CodingAgentOperation::Prompt(
                prompt_options_with_auto_naming(api, "first question"),
            ))
            .await
            .unwrap();
        assert!(matches!(
            prompt_outcome(first),
            PromptTurnOutcome::Success { .. }
        ));
        assert_eq!(
            wait_for_session_name(&options).await.as_deref(),
            Some("First concise title")
        );
        let after_first = CodingAgentSession::hydrate(options.clone()).unwrap();
        assert_eq!(after_first.usage.input, 20);
        assert_eq!(after_first.usage.output, 40);

        let second = session
            .run_internal(CodingAgentOperation::Prompt(
                prompt_options_with_auto_naming(api, "second question"),
            ))
            .await
            .unwrap();
        assert!(matches!(
            prompt_outcome(second),
            PromptTurnOutcome::Success { .. }
        ));
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        let store = crate::session::repository::SessionLogStore::new(temp.path());
        let events = store
            .read_events(&store.open_session_id(session_id).unwrap())
            .unwrap();
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(event.data, SessionEventData::ModelUsageRecorded { .. }))
                .count(),
            1
        );
        assert!(events.iter().any(|event| matches!(
            &event.data,
            SessionEventData::ModelUsageRecorded { model_id, .. }
                if model_id == "test-model"
        )));
        let after_second = CodingAgentSession::hydrate(options).unwrap();
        assert_eq!(
            after_second.summary.name.as_deref(),
            Some("First concise title")
        );
        assert_eq!(after_second.usage.input, 30);
        assert_eq!(after_second.usage.output, 60);
    }

    #[tokio::test]
    async fn existing_session_name_skips_background_model_call() {
        let api = "coding-session-auto-name-existing";
        let provider_guard = crate::test_support::ProviderGuard::register(
            api,
            Arc::new(FauxProvider::with_call_queue(vec![
                FauxProvider::text_call("answer", StopReason::Stop),
            ])),
        );
        let temp = tempfile::tempdir().unwrap();
        let session_id = "sess_auto_name_existing";
        let options = CodingAgentSessionOptions::new()
            .with_ai_client(provider_guard.ai_client())
            .with_session_id(session_id)
            .with_session_name("Manual name")
            .with_session_log_root(temp.path());
        let mut session = CodingAgentSession::create_internal(options.clone())
            .await
            .unwrap();

        session
            .run_internal(CodingAgentOperation::Prompt(
                prompt_options_with_auto_naming(api, "question"),
            ))
            .await
            .unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        let hydration = CodingAgentSession::hydrate(options).unwrap();
        assert_eq!(hydration.summary.name.as_deref(), Some("Manual name"));
        assert_eq!(hydration.usage.input, 10);
        assert_eq!(hydration.usage.output, 20);
    }

    #[tokio::test]
    async fn failed_auto_name_persists_diagnostic_usage_and_keeps_session_usable() {
        let api = "coding-session-auto-name-failure";
        let provider_guard = crate::test_support::ProviderGuard::register(
            api,
            Arc::new(FauxProvider::with_call_queue(vec![
                FauxProvider::text_call("first answer", StopReason::Stop),
                FauxProvider::text_call("", StopReason::Stop),
                FauxProvider::text_call("second answer", StopReason::Stop),
            ])),
        );
        let temp = tempfile::tempdir().unwrap();
        let session_id = "sess_auto_name_failure";
        let options = CodingAgentSessionOptions::new()
            .with_ai_client(provider_guard.ai_client())
            .with_session_id(session_id)
            .with_session_log_root(temp.path());
        let mut session = CodingAgentSession::create_internal(options.clone())
            .await
            .unwrap();

        session
            .run_internal(CodingAgentOperation::Prompt(
                prompt_options_with_auto_naming(api, "first question"),
            ))
            .await
            .unwrap();
        let events = wait_for_session_event(temp.path(), session_id, |event| {
            matches!(
                event,
                SessionEventData::DiagnosticEmitted { message, .. }
                    if message.contains("automatic session naming returned an empty name")
            )
        })
        .await;
        assert!(
            events
                .iter()
                .any(|event| matches!(event.data, SessionEventData::ModelUsageRecorded { .. }))
        );
        let failed = CodingAgentSession::hydrate(options.clone()).unwrap();
        assert_eq!(failed.summary.name, None);
        assert!(failed.diagnostics.iter().any(|diagnostic| {
            diagnostic
                .message
                .contains("automatic session naming returned an empty name")
        }));
        assert_eq!(failed.usage.input, 20);
        assert_eq!(failed.usage.output, 40);

        let second = session
            .run_internal(CodingAgentOperation::Prompt(
                prompt_options_with_auto_naming(api, "second question"),
            ))
            .await
            .unwrap();
        assert!(matches!(
            prompt_outcome(second),
            PromptTurnOutcome::Success { .. }
        ));
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        let after_second = CodingAgentSession::hydrate(options).unwrap();
        assert_eq!(after_second.summary.name, None);
        assert_eq!(after_second.usage.input, 30);
        assert_eq!(after_second.usage.output, 60);
    }

    #[tokio::test]
    async fn configured_naming_model_overrides_current_model_and_default_follows_it() {
        let configured = ai::api::model::lookup_model("claude-haiku-4-5").unwrap();
        let current_api = "coding-session-auto-name-model-current";
        let provider = Arc::new(FauxProvider::with_call_queue(vec![
            FauxProvider::text_call("answer", StopReason::Stop),
            FauxProvider::text_call("Configured title", StopReason::Stop),
        ]));
        let provider_guard = crate::test_support::ProviderGuard::register_many(vec![
            (current_api.into(), provider.clone()),
            (configured.api.clone(), provider),
        ]);
        let temp = tempfile::tempdir().unwrap();
        let session_id = "sess_auto_name_configured_model";
        let options = CodingAgentSessionOptions::new()
            .with_ai_client(provider_guard.ai_client())
            .with_session_id(session_id)
            .with_session_log_root(temp.path());
        let mut session = CodingAgentSession::create_internal(options.clone())
            .await
            .unwrap();

        session
            .run_internal(CodingAgentOperation::Prompt(
                prompt_options_with_session_naming_model(current_api, "question", &configured.id),
            ))
            .await
            .unwrap();
        let events = wait_for_session_event(temp.path(), session_id, |event| {
            matches!(event, SessionEventData::ModelUsageRecorded { .. })
        })
        .await;
        assert!(events.iter().any(|event| matches!(
            &event.data,
            SessionEventData::ModelUsageRecorded { model_id, .. }
                if model_id == &configured.id
        )));
        assert_eq!(
            CodingAgentSession::hydrate(options)
                .unwrap()
                .summary
                .name
                .as_deref(),
            Some("Configured title")
        );
    }

    #[tokio::test]
    async fn non_persistent_prompt_hydrates_owner_lifetime_transcript() {
        let first_api = "coding-session-non-persistent-first";
        let second_api = "coding-session-non-persistent-second";
        let contexts = Arc::new(Mutex::new(Vec::new()));
        let _provider_guard = crate::test_support::ProviderGuard::register_many(vec![
            (
                first_api.to_string(),
                Arc::new(FauxProvider::simple_text("first answer")),
            ),
            (
                second_api.to_string(),
                Arc::new(RecordingProvider::new(
                    Arc::clone(&contexts),
                    "second answer",
                )),
            ),
        ]);
        let mut session = CodingAgentSession::non_persistent_internal(
            CodingAgentSessionOptions::new().with_ai_client(_provider_guard.ai_client()),
        )
        .await
        .unwrap();

        prompt_outcome(
            session
                .run_internal(CodingAgentOperation::Prompt(prompt_options(
                    first_api,
                    "first question",
                )))
                .await
                .unwrap(),
        );

        prompt_outcome(
            session
                .run_internal(CodingAgentOperation::Prompt(prompt_options(
                    second_api,
                    "second question",
                )))
                .await
                .unwrap(),
        );

        let contexts = contexts.lock().unwrap();
        assert_eq!(contexts.len(), 1);
        assert_eq!(contexts[0].messages.len(), 3);
        assert!(matches!(
            &contexts[0].messages[0],
            Message::User { content }
                if content == &vec![ContentBlock::Text {
                    text: "first question".into(),
                    text_signature: None,
                }]
        ));
        assert!(matches!(
            &contexts[0].messages[1],
            Message::Assistant { content }
                if content == &vec![ContentBlock::Text {
                    text: "first answer".into(),
                    text_signature: None,
                }]
        ));
        assert!(matches!(
            &contexts[0].messages[2],
            Message::User { content }
                if content == &vec![ContentBlock::Text {
                    text: "second question".into(),
                    text_signature: None,
                }]
        ));
    }

    #[tokio::test]
    async fn prompt_does_not_duplicate_failure_event_from_agent_error() {
        let api = "coding-session-prompt-error";
        let _provider_guard = crate::test_support::ProviderGuard::register(
            api,
            Arc::new(FauxProvider::with_call_queue(vec![
                FauxProvider::text_call("partial", StopReason::Error),
            ])),
        );
        let temp = tempfile::tempdir().unwrap();
        let mut session = CodingAgentSession::create_internal(
            CodingAgentSessionOptions::new()
                .with_ai_client(_provider_guard.ai_client())
                .with_session_id("sess_prompt_error")
                .with_session_log_root(temp.path()),
        )
        .await
        .unwrap();
        let mut events = session.subscribe_product_events();

        let outcome = prompt_outcome(
            session
                .run_internal(CodingAgentOperation::Prompt(prompt_options(api, "hello")))
                .await
                .unwrap(),
        );

        assert!(matches!(outcome, PromptTurnOutcome::Failed { .. }));
        let emitted_events = std::iter::from_fn(|| events.try_recv().unwrap()).collect::<Vec<_>>();
        assert_event_order(
            &emitted_events,
            &[
                "session_write_pending",
                "session_write_committed",
                "prompt_failed",
            ],
        );
        assert_eq!(
            emitted_events
                .iter()
                .filter(|event| matches!(
                    event.event(),
                    CodingAgentProductEventKind::Workflow(
                        CodingAgentWorkflowProductEvent::PromptFailed { .. }
                    )
                ))
                .count(),
            1
        );
        assert!(
            session
                .persistent_session_service()
                .replay()
                .unwrap()
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.message.contains("operation")
                    && diagnostic.message.contains("failed"))
        );
    }

    #[tokio::test]
    async fn branch_summary_persistent_session_records_model_summary() {
        let api = "coding-session-branch-summary";
        let _provider_guard = crate::test_support::ProviderGuard::register(
            api,
            Arc::new(FauxProvider::with_call_queue(vec![
                FauxProvider::text_call("root answer", StopReason::Stop),
                FauxProvider::text_call("branch answer", StopReason::Stop),
                FauxProvider::text_call("model branch summary", StopReason::Stop),
            ])),
        );
        let temp = tempfile::tempdir().unwrap();
        let mut session = CodingAgentSession::create_internal(
            CodingAgentSessionOptions::new()
                .with_ai_client(_provider_guard.ai_client())
                .with_session_id("sess_branch_summary_owner")
                .with_session_log_root(temp.path()),
        )
        .await
        .unwrap();
        let root_leaf = match prompt_outcome(
            session
                .run_internal(CodingAgentOperation::Prompt(prompt_options(
                    api,
                    "root question",
                )))
                .await
                .unwrap(),
        ) {
            PromptTurnOutcome::Success {
                leaf_id: Some(leaf_id),
                ..
            } => leaf_id,
            other => panic!("expected root prompt success, got {other:?}"),
        };
        let branch_leaf = match prompt_outcome(
            session
                .run_internal(CodingAgentOperation::Prompt(prompt_options(
                    api,
                    "branch question",
                )))
                .await
                .unwrap(),
        ) {
            PromptTurnOutcome::Success {
                leaf_id: Some(leaf_id),
                ..
            } => leaf_id,
            other => panic!("expected branch prompt success, got {other:?}"),
        };
        let mut events = session.subscribe_product_events();

        let outcome = session
            .run_internal(CodingAgentOperation::BranchSummary {
                options: prompt_options(api, ""),
                source_leaf_id: branch_leaf.clone(),
                target_leaf_id: root_leaf.clone(),
                custom_instructions: Some("keep branch decisions".into()),
                reuse: BranchSummaryReusePolicy::AlwaysCreate,
            })
            .await
            .unwrap();
        let outcome = match outcome {
            CodingAgentOperationOutcome::BranchSummary(outcome) => outcome,
            other => panic!("expected branch summary outcome, got {other:?}"),
        };

        assert!(matches!(
            &outcome,
            PromptTurnOutcome::Success {
                final_text,
                session_id: Some(session_id),
                leaf_id: Some(_),
                ..
            } if final_text.contains("model branch summary")
                && session_id == "sess_branch_summary_owner"
        ));
        let emitted_events = std::iter::from_fn(|| events.try_recv().unwrap()).collect::<Vec<_>>();
        assert_event_order(
            &emitted_events,
            &["session_write_pending", "session_write_committed"],
        );
        let replay = session.persistent_session_service().replay().unwrap();
        assert!(matches!(
            replay.transcript.last(),
            Some(TranscriptItem::BranchSummary {
                summary,
                source_leaf_id,
                target_leaf_id,
            }) if summary.contains("model branch summary")
                && source_leaf_id == &branch_leaf
                && target_leaf_id == &root_leaf
        ));
        let event_log =
            std::fs::read_to_string(temp.path().join("sess_branch_summary_owner/events.jsonl"))
                .unwrap();
        assert!(event_log.contains("branch.summary.created"));
    }

    #[tokio::test]
    async fn canonical_run_reuses_branch_summary_when_requested() {
        let api = "coding-session-branch-summary-navigation-reuse";
        let _provider_guard = crate::test_support::ProviderGuard::register(
            api,
            Arc::new(FauxProvider::with_call_queue(vec![
                FauxProvider::text_call("root answer", StopReason::Stop),
                FauxProvider::text_call("branch answer", StopReason::Stop),
                FauxProvider::text_call("model branch summary", StopReason::Stop),
            ])),
        );
        let temp = tempfile::tempdir().unwrap();
        let mut session = CodingAgentSession::create_internal(
            CodingAgentSessionOptions::new()
                .with_ai_client(_provider_guard.ai_client())
                .with_session_id("sess_branch_summary_navigation_reuse")
                .with_session_log_root(temp.path()),
        )
        .await
        .unwrap();
        let root_leaf = match prompt_outcome(
            session
                .run_internal(CodingAgentOperation::Prompt(prompt_options(
                    api,
                    "root question",
                )))
                .await
                .unwrap(),
        ) {
            PromptTurnOutcome::Success {
                leaf_id: Some(leaf_id),
                ..
            } => leaf_id,
            other => panic!("expected root prompt success, got {other:?}"),
        };
        let branch_leaf = match prompt_outcome(
            session
                .run_internal(CodingAgentOperation::Prompt(prompt_options(
                    api,
                    "branch question",
                )))
                .await
                .unwrap(),
        ) {
            PromptTurnOutcome::Success {
                leaf_id: Some(leaf_id),
                ..
            } => leaf_id,
            other => panic!("expected branch prompt success, got {other:?}"),
        };
        session
            .run_internal(CodingAgentOperation::BranchSummary {
                options: prompt_options(api, ""),
                source_leaf_id: branch_leaf.clone(),
                target_leaf_id: root_leaf.clone(),
                custom_instructions: None,
                reuse: BranchSummaryReusePolicy::AlwaysCreate,
            })
            .await
            .unwrap();
        let event_log_path = temp
            .path()
            .join("sess_branch_summary_navigation_reuse/events.jsonl");
        let event_log_before = std::fs::read(&event_log_path).unwrap();
        let event_count_before = event_log_before.split(|byte| *byte == b'\n').count();
        let event_log_text_before = String::from_utf8(event_log_before.clone()).unwrap();
        let summary_count_before = event_log_text_before
            .matches("branch.summary.created")
            .count();
        let mut events = session.subscribe_product_events_public();

        let outcome = session
            .run_internal(CodingAgentOperation::BranchSummary {
                options: prompt_options(api, ""),
                source_leaf_id: branch_leaf.clone(),
                target_leaf_id: root_leaf.clone(),
                custom_instructions: None,
                reuse: BranchSummaryReusePolicy::ReuseExisting,
            })
            .await
            .unwrap();

        assert!(matches!(
            &outcome,
            CodingAgentOperationOutcome::BranchSummary(PromptTurnOutcome::Success {
                final_text,
                session_id: Some(session_id),
                leaf_id: Some(active_leaf),
                ..
            }) if final_text.contains("model branch summary")
                && session_id == "sess_branch_summary_navigation_reuse"
                && active_leaf.as_str() == branch_leaf.as_str()
        ));
        let emitted_events = std::iter::from_fn(|| events.try_recv().unwrap()).collect::<Vec<_>>();
        assert!(emitted_events.is_empty(), "{emitted_events:#?}");
        let event_log_after = std::fs::read(&event_log_path).unwrap();
        assert_eq!(event_log_after, event_log_before);
        assert_eq!(
            event_log_after.split(|byte| *byte == b'\n').count(),
            event_count_before
        );
        assert_eq!(summary_count_before, 1);
        assert_eq!(
            String::from_utf8(event_log_after)
                .unwrap()
                .matches("branch.summary.created")
                .count(),
            1
        );
    }

    #[tokio::test]
    async fn canonical_run_preserves_navigation_and_branch_summary_durability() {
        let api = "coding-session-canonical-navigation-durability";
        let _provider_guard = crate::test_support::ProviderGuard::register(
            api,
            Arc::new(FauxProvider::with_call_queue(vec![
                FauxProvider::text_call("root answer", StopReason::Stop),
                FauxProvider::text_call("branch answer", StopReason::Stop),
                FauxProvider::text_call("durable branch summary", StopReason::Stop),
            ])),
        );
        let temp = tempfile::tempdir().unwrap();
        let source_options = CodingAgentSessionOptions::new()
            .with_ai_client(_provider_guard.ai_client())
            .with_session_id("sess_canonical_navigation_durability")
            .with_session_log_root(temp.path());
        let mut session = CodingAgentSession::create_internal(source_options.clone())
            .await
            .unwrap();
        let root_leaf = match session
            .run_internal(CodingAgentOperation::Prompt(prompt_options(
                api,
                "root question",
            )))
            .await
            .unwrap()
        {
            CodingAgentOperationOutcome::Prompt(PromptTurnOutcome::Success {
                leaf_id: Some(leaf_id),
                ..
            }) => leaf_id,
            other => panic!("expected root prompt success, got {other:?}"),
        };
        let branch_leaf = match session
            .run_internal(CodingAgentOperation::Prompt(prompt_options(
                api,
                "branch question",
            )))
            .await
            .unwrap()
        {
            CodingAgentOperationOutcome::Prompt(PromptTurnOutcome::Success {
                leaf_id: Some(leaf_id),
                ..
            }) => leaf_id,
            other => panic!("expected branch prompt success, got {other:?}"),
        };

        let switch = session
            .run_internal(CodingAgentOperation::SwitchActiveLeaf {
                target_leaf_id: root_leaf.clone(),
            })
            .await
            .unwrap();
        assert!(matches!(
            switch,
            CodingAgentOperationOutcome::ActiveLeafSwitched
        ));
        assert_eq!(
            session
                .hydrate_current()
                .unwrap()
                .unwrap()
                .summary
                .active_leaf_id,
            Some(root_leaf.clone())
        );
        let reopened = CodingAgentSession::open_internal(source_options.clone())
            .await
            .unwrap();
        assert_eq!(
            reopened
                .persistent_session_service()
                .replay()
                .unwrap()
                .active_leaf_id,
            Some(root_leaf.clone())
        );

        let generated = session
            .run_internal(CodingAgentOperation::BranchSummary {
                options: prompt_options(api, ""),
                source_leaf_id: branch_leaf.clone(),
                target_leaf_id: root_leaf.clone(),
                custom_instructions: None,
                reuse: BranchSummaryReusePolicy::AlwaysCreate,
            })
            .await
            .unwrap();
        let expected_summary = match generated {
            CodingAgentOperationOutcome::BranchSummary(PromptTurnOutcome::Success {
                final_text,
                ..
            }) => final_text,
            other => panic!("expected generated branch summary, got {other:?}"),
        };
        let event_log_path = temp
            .path()
            .join("sess_canonical_navigation_durability/events.jsonl");
        let event_log_before_reuse = std::fs::read(&event_log_path).unwrap();
        let mut reuse_events = session.subscribe_product_events_public();
        let reused = session
            .run_internal(CodingAgentOperation::BranchSummary {
                options: prompt_options(api, ""),
                source_leaf_id: branch_leaf.clone(),
                target_leaf_id: root_leaf.clone(),
                custom_instructions: None,
                reuse: BranchSummaryReusePolicy::ReuseExisting,
            })
            .await
            .unwrap();
        assert!(matches!(
            reused,
            CodingAgentOperationOutcome::BranchSummary(PromptTurnOutcome::Success {
                final_text,
                ..
            }) if final_text == expected_summary
        ));
        assert!(reuse_events.try_recv().unwrap().is_none());
        assert_eq!(
            std::fs::read(&event_log_path).unwrap(),
            event_log_before_reuse
        );
        let reopened = CodingAgentSession::open_internal(source_options)
            .await
            .unwrap();
        assert_eq!(
            reopened
                .persistent_session_service()
                .branch_summary_for(&branch_leaf, &root_leaf)
                .unwrap()
                .as_deref(),
            Some(expected_summary.as_str())
        );

        let capability_generation = session.current_capability_generation_for_tests();
        let mut fork_events = session.subscribe_product_events();
        let source_session_id = session.view().session_id;
        let forked = session
            .run_internal(CodingAgentOperation::ForkSession {
                target_leaf_id: Some(root_leaf.clone()),
            })
            .await
            .unwrap();
        assert!(matches!(forked, CodingAgentOperationOutcome::SessionForked));
        assert_ne!(session.view().session_id, source_session_id);
        assert_eq!(
            session.view().session_id,
            session
                .hydrate_current()
                .unwrap()
                .unwrap()
                .summary
                .session_id
        );
        assert_eq!(
            session.current_capability_generation_for_tests(),
            capability_generation
        );
        let emitted = std::iter::from_fn(|| fork_events.try_recv().unwrap()).collect::<Vec<_>>();
        assert!(emitted.iter().any(|event| matches!(
            event.event(),
            CodingAgentProductEventKind::Session(
                CodingAgentSessionProductEvent::Opened { session_id }
            )
                if session_id == &session.view().session_id
        )));
        assert!(
            emitted
                .windows(2)
                .all(|pair| pair[0].sequence() < pair[1].sequence())
        );
    }

    #[tokio::test]
    async fn canonical_durable_mutations_distinguish_no_commit_partial_commit_and_replay() {
        let api = "coding-session-canonical-mutation-boundaries";
        let _provider_guard = crate::test_support::ProviderGuard::register(
            api,
            Arc::new(FauxProvider::with_call_queue(vec![
                FauxProvider::text_call("root answer", StopReason::Stop),
                FauxProvider::text_call("branch answer", StopReason::Stop),
            ])),
        );
        let temp = tempfile::tempdir().unwrap();
        let options = CodingAgentSessionOptions::new()
            .with_ai_client(_provider_guard.ai_client())
            .with_session_id("sess_canonical_mutation_boundaries")
            .with_session_log_root(temp.path());
        let mut session = CodingAgentSession::create_internal(options.clone())
            .await
            .unwrap();
        let root_leaf = match session
            .run_internal(CodingAgentOperation::Prompt(prompt_options(
                api,
                "root question",
            )))
            .await
            .unwrap()
        {
            CodingAgentOperationOutcome::Prompt(PromptTurnOutcome::Success {
                leaf_id: Some(leaf_id),
                ..
            }) => leaf_id,
            other => panic!("expected root prompt success, got {other:?}"),
        };
        let branch_leaf = match session
            .run_internal(CodingAgentOperation::Prompt(prompt_options(
                api,
                "branch question",
            )))
            .await
            .unwrap()
        {
            CodingAgentOperationOutcome::Prompt(PromptTurnOutcome::Success {
                leaf_id: Some(leaf_id),
                ..
            }) => leaf_id,
            other => panic!("expected branch prompt success, got {other:?}"),
        };
        let event_log_path = temp
            .path()
            .join("sess_canonical_mutation_boundaries/events.jsonl");
        let manifest_path = temp
            .path()
            .join("sess_canonical_mutation_boundaries/session.json");
        let events_before = std::fs::read(&event_log_path).unwrap();
        let manifest_before = std::fs::read(&manifest_path).unwrap();

        session
            .persistent_session_service()
            .fail_store_after_for_tests(StoreFailurePoint::AppendEvents, 0);
        let error = session
            .run_internal(CodingAgentOperation::SwitchActiveLeaf {
                target_leaf_id: root_leaf.clone(),
            })
            .await
            .unwrap_err();
        assert_eq!(error.code(), "session_write_rejected");
        assert_eq!(std::fs::read(&event_log_path).unwrap(), events_before);
        assert_eq!(std::fs::read(&manifest_path).unwrap(), manifest_before);
        assert_eq!(
            session.view().session_id,
            "sess_canonical_mutation_boundaries"
        );
        let reopened = CodingAgentSession::open_internal(options.clone())
            .await
            .unwrap();
        assert_eq!(
            reopened
                .persistent_session_service()
                .replay()
                .unwrap()
                .active_leaf_id,
            Some(branch_leaf.clone())
        );

        session
            .persistent_session_service()
            .fail_store_after_for_tests(StoreFailurePoint::UpdateManifest, 0);
        let error = session
            .run_internal(CodingAgentOperation::SwitchActiveLeaf {
                target_leaf_id: root_leaf.clone(),
            })
            .await
            .unwrap_err();
        let operation_id = match &error {
            CodingSessionError::PartialCommit { operation_id, .. } => operation_id,
            other => panic!("expected partial commit, got {other:?}"),
        };
        assert!(!operation_id.is_empty());
        assert_eq!(error.code(), "partial_commit");
        assert_eq!(
            session
                .persistent_session_service()
                .replay()
                .unwrap()
                .active_leaf_id,
            Some(root_leaf.clone())
        );
        let reopened = CodingAgentSession::open_internal(options).await.unwrap();
        assert_eq!(
            reopened
                .persistent_session_service()
                .replay()
                .unwrap()
                .active_leaf_id,
            Some(root_leaf)
        );
    }

    #[tokio::test]
    async fn canonical_tree_label_operation_persists_and_replays() {
        let api = "coding-session-tree-label-operation";
        let _provider_guard = crate::test_support::ProviderGuard::register(
            api,
            Arc::new(FauxProvider::simple_text("answer")),
        );
        let temp = tempfile::tempdir().unwrap();
        let options = CodingAgentSessionOptions::new()
            .with_ai_client(_provider_guard.ai_client())
            .with_session_id("sess_tree_label_operation")
            .with_session_log_root(temp.path());
        let mut session = CodingAgentSession::create_internal(options.clone())
            .await
            .unwrap();
        let leaf_id = match session
            .run_internal(CodingAgentOperation::Prompt(prompt_options(
                api, "label me",
            )))
            .await
            .unwrap()
        {
            CodingAgentOperationOutcome::Prompt(PromptTurnOutcome::Success {
                leaf_id: Some(leaf_id),
                ..
            }) => leaf_id,
            other => panic!("expected prompt success, got {other:?}"),
        };

        let outcome = session
            .run_internal(CodingAgentOperation::SetSessionTreeLabel {
                entry_id: leaf_id.clone(),
                label: Some(" checkpoint ".into()),
            })
            .await
            .unwrap();
        let updated_at = match outcome {
            CodingAgentOperationOutcome::SessionTreeLabelChanged {
                entry_id,
                label,
                updated_at,
            } => {
                assert_eq!(entry_id, leaf_id);
                assert_eq!(label.as_deref(), Some("checkpoint"));
                updated_at
            }
            other => panic!("expected tree label outcome, got {other:?}"),
        };

        drop(session);
        let reopened = CodingAgentSession::open_internal(options.clone())
            .await
            .unwrap();
        let tree = CodingAgentSession::tree_view(options).unwrap();
        assert_eq!(reopened.view().session_id, "sess_tree_label_operation");
        assert_eq!(tree.tree[0].label.as_deref(), Some("checkpoint"));
        assert_eq!(
            tree.tree[0].label_timestamp.as_deref(),
            Some(updated_at.as_str())
        );
    }

    #[tokio::test]
    async fn canonical_session_name_operation_persists_and_hydrates() {
        let temp = tempfile::tempdir().unwrap();
        let options = CodingAgentSessionOptions::new()
            .with_session_id("sess_name_operation")
            .with_session_name(" Initial name ")
            .with_session_log_root(temp.path());
        let mut session = CodingAgentSession::create_internal(options.clone())
            .await
            .unwrap();
        assert_eq!(
            session
                .current_session_snapshot()
                .unwrap()
                .unwrap()
                .choice
                .name
                .as_deref(),
            Some("Initial name")
        );

        let requested_name = format!("  {}  ", "会".repeat(240));
        let outcome = session
            .run_internal(CodingAgentOperation::SetSessionName {
                name: Some(requested_name),
            })
            .await
            .unwrap();
        let (name, _) = outcome.into_session_name_changed().unwrap();
        assert_eq!(name.as_deref().unwrap().chars().count(), 200);

        drop(session);
        let hydration = CodingAgentSession::hydrate(options).unwrap();
        assert_eq!(hydration.summary.name, name);
    }

    #[tokio::test]
    async fn canonical_run_preserves_profile_and_delegation_contracts() {
        let api = "coding-session-canonical-delegation-decision";
        let _provider_guard = crate::test_support::ProviderGuard::register(
            api,
            Arc::new(FauxProvider::with_call_queue(vec![
                FauxProvider::text_call("delegated result", StopReason::Stop),
            ])),
        );
        let temp = tempfile::tempdir().unwrap();
        let options = CodingAgentSessionOptions::new()
            .with_ai_client(_provider_guard.ai_client())
            .with_session_id("sess_canonical_profile_delegation")
            .with_session_log_root(temp.path());
        let mut session = CodingAgentSession::create_internal(options.clone())
            .await
            .unwrap();
        let mut events = session.subscribe_product_events();

        let profile = session
            .run_internal(CodingAgentOperation::SetDefaultAgentProfile {
                profile_id: ProfileId::from("reviewer"),
            })
            .await
            .unwrap();
        assert!(matches!(
            profile,
            CodingAgentOperationOutcome::DefaultAgentProfileChanged
        ));
        assert_eq!(session.view().default_agent_profile_id.as_str(), "reviewer");
        let reopened = CodingAgentSession::open_internal(options.clone())
            .await
            .unwrap();
        assert_eq!(
            reopened.view().default_agent_profile_id.as_str(),
            "reviewer"
        );

        queue_persistent_delegation_confirmation(
            &mut session,
            "op_reject_contract",
            "tool_reject_contract",
            ProfileKind::Agent,
        );
        let rejected = session
            .run_internal(CodingAgentOperation::RejectDelegation {
                operation_id: "op_reject_contract".into(),
                tool_call_id: "tool_reject_contract".into(),
                reason: "not now".into(),
            })
            .await
            .unwrap();
        assert!(matches!(
            rejected,
            CodingAgentOperationOutcome::DelegationRejected
        ));
        assert!(
            session
                .runtime_host
                .session_coordinator
                .pending_delegation_confirmations()
                .is_empty()
        );

        queue_persistent_delegation_confirmation(
            &mut session,
            "op_approve_contract",
            "tool_approve_contract",
            ProfileKind::Agent,
        );
        let approved = session
            .run_internal(CodingAgentOperation::ApproveDelegation {
                operation_id: "op_approve_contract".into(),
                tool_call_id: "tool_approve_contract".into(),
            })
            .await
            .unwrap();
        assert!(matches!(
            approved,
            CodingAgentOperationOutcome::DelegationApproved
        ));
        assert!(
            session
                .runtime_host
                .session_coordinator
                .pending_delegation_confirmations()
                .is_empty()
        );
        let emitted = std::iter::from_fn(|| events.try_recv().unwrap()).collect::<Vec<_>>();
        assert!(emitted.iter().any(|event| matches!(
            event.event(),
            CodingAgentProductEventKind::Delegation(
                CodingAgentDelegationProductEvent::Rejected { reason, .. }
            ) if reason == "not now"
        )));
        assert!(emitted.iter().any(|event| matches!(
            event.event(),
            CodingAgentProductEventKind::Delegation(
                CodingAgentDelegationProductEvent::Approved { .. }
            )
        )));
        assert!(
            emitted
                .windows(2)
                .all(|pair| pair[0].sequence() < pair[1].sequence())
        );
    }

    #[tokio::test]
    async fn canonical_delegation_decisions_distinguish_no_commit_partial_commit_and_replay() {
        async fn session_with_pending(
            root: &Path,
            session_id: &str,
            operation_id: &str,
            tool_call_id: &str,
        ) -> (CodingAgentSession, CodingAgentSessionOptions) {
            let options = CodingAgentSessionOptions::new()
                .with_session_id(session_id)
                .with_session_log_root(root);
            let mut session = CodingAgentSession::create_internal(options.clone())
                .await
                .unwrap();
            queue_persistent_delegation_confirmation(
                &mut session,
                operation_id,
                tool_call_id,
                ProfileKind::Agent,
            );
            (session, options)
        }

        for (decision, failure_point) in [
            ("reject_pre_append", StoreFailurePoint::AppendEvents),
            ("approve_pre_append", StoreFailurePoint::AppendEvents),
        ] {
            let temp = tempfile::tempdir().unwrap();
            let operation_id = format!("op_{decision}");
            let tool_call_id = format!("tool_{decision}");
            let (mut session, options) = session_with_pending(
                temp.path(),
                &format!("sess_{decision}"),
                &operation_id,
                &tool_call_id,
            )
            .await;
            let event_log_path = temp.path().join(format!("sess_{decision}/events.jsonl"));
            let manifest_path = temp.path().join(format!("sess_{decision}/session.json"));
            let events_before = std::fs::read(&event_log_path).unwrap();
            let manifest_before = std::fs::read(&manifest_path).unwrap();
            session
                .persistent_session_service()
                .fail_store_after_for_tests(failure_point, 0);
            let error = if decision.starts_with("reject") {
                session
                    .run_internal(CodingAgentOperation::RejectDelegation {
                        operation_id: operation_id.clone(),
                        tool_call_id: tool_call_id.clone(),
                        reason: "declined".into(),
                    })
                    .await
                    .unwrap_err()
            } else {
                session
                    .run_internal(CodingAgentOperation::ApproveDelegation {
                        operation_id: operation_id.clone(),
                        tool_call_id: tool_call_id.clone(),
                    })
                    .await
                    .unwrap_err()
            };
            assert_eq!(error.code(), "session_write_rejected");
            assert_eq!(
                session
                    .runtime_host
                    .session_coordinator
                    .pending_delegation_confirmations()
                    .len(),
                1
            );
            assert_eq!(std::fs::read(&event_log_path).unwrap(), events_before);
            assert_eq!(std::fs::read(&manifest_path).unwrap(), manifest_before);
            assert_eq!(
                CodingAgentSession::open_internal(options)
                    .await
                    .unwrap()
                    .pending_delegation_confirmations()
                    .len(),
                1
            );
        }

        for decision in ["reject_partial_commit", "approve_partial_commit"] {
            let temp = tempfile::tempdir().unwrap();
            let operation_id = format!("op_{decision}");
            let tool_call_id = format!("tool_{decision}");
            let (mut session, options) = session_with_pending(
                temp.path(),
                &format!("sess_{decision}"),
                &operation_id,
                &tool_call_id,
            )
            .await;
            session
                .persistent_session_service()
                .fail_store_after_for_tests(StoreFailurePoint::UpdateManifest, 0);
            let error = if decision.starts_with("reject") {
                session
                    .run_internal(CodingAgentOperation::RejectDelegation {
                        operation_id: operation_id.clone(),
                        tool_call_id: tool_call_id.clone(),
                        reason: "declined".into(),
                    })
                    .await
                    .unwrap_err()
            } else {
                session
                    .run_internal(CodingAgentOperation::ApproveDelegation {
                        operation_id: operation_id.clone(),
                        tool_call_id: tool_call_id.clone(),
                    })
                    .await
                    .unwrap_err()
            };
            assert!(matches!(
                &error,
                CodingSessionError::PartialCommit {
                    operation_id: durable_operation_id,
                    ..
                } if durable_operation_id == &operation_id
            ));
            assert_eq!(error.code(), "partial_commit");
            assert_eq!(
                session
                    .runtime_host
                    .session_coordinator
                    .pending_delegation_confirmations()
                    .len(),
                1
            );
            let reopened = CodingAgentSession::open_internal(options).await.unwrap();
            assert!(reopened.pending_delegation_confirmations().is_empty());
            assert!(
                reopened
                    .persistent_session_service()
                    .replay()
                    .unwrap()
                    .pending_delegation_confirmations
                    .is_empty()
            );
        }
    }

    #[tokio::test]
    async fn automatic_delegation_terminal_write_distinguishes_definite_failure_and_uncertainty() {
        fn delegation_provider() -> FauxProvider {
            FauxProvider::with_call_queue(vec![
                FauxProvider::single_call(
                    vec![FauxResponse {
                        text_deltas: Vec::new(),
                        thinking_deltas: Vec::new(),
                        tool_calls: vec![FauxToolCall {
                            id: "tool_delegate_fault".into(),
                            name: "delegate_agent".into(),
                            deltas: Vec::new(),
                            final_arguments: serde_json::json!({
                                "agent_id": "explore",
                                "task": "inspect durable fault"
                            }),
                        }],
                    }],
                    StopReason::ToolUse,
                ),
                FauxProvider::text_call("child durable result", StopReason::Stop),
                FauxProvider::text_call("parent durable result", StopReason::Stop),
            ])
        }

        let definite_temp = tempfile::tempdir().unwrap();
        let definite_api = "automatic-delegation-definite-write-failure";
        let definite_guard = crate::test_support::ProviderGuard::register(
            definite_api,
            Arc::new(delegation_provider()),
        );
        let definite_options = CodingAgentSessionOptions::new()
            .with_ai_client(definite_guard.ai_client())
            .with_session_id("sess_automatic_delegation_definite")
            .with_session_log_root(definite_temp.path());
        let mut definite = CodingAgentSession::create_internal(definite_options.clone())
            .await
            .unwrap();
        let definite_event_path = definite_temp
            .path()
            .join("sess_automatic_delegation_definite/events.jsonl");
        definite
            .persistent_session_service()
            .fail_store_after_for_tests(StoreFailurePoint::AppendEvents, 0);
        let outcome = definite
            .run_internal(CodingAgentOperation::Prompt(prompt_options(
                definite_api,
                "delegate before definite failure",
            )))
            .await
            .unwrap();
        let definite_operation_id = match outcome {
            CodingAgentOperationOutcome::Prompt(PromptTurnOutcome::Failed {
                operation_id,
                error,
                ..
            }) => {
                assert_ne!(error.code(), "partial_commit");
                assert_eq!(error.code(), "session_write_rejected");
                operation_id
            }
            other => panic!("expected definite prompt failure, got {other:?}"),
        };
        assert!(std::fs::metadata(&definite_event_path).unwrap().len() > 0);
        let reopened = CodingAgentSession::open_internal(definite_options)
            .await
            .unwrap();
        let replay = reopened.persistent_session_service().replay().unwrap();
        assert_eq!(
            replay.operation_status(&definite_operation_id),
            Some(crate::session::replay::OperationReplayStatus::Failed)
        );
        assert!(
            reopened
                .persistent_session_service()
                .recovery_summary()
                .unwrap()
                .in_doubt_operations
                .is_empty()
        );

        let uncertain_temp = tempfile::tempdir().unwrap();
        let uncertain_api = "automatic-delegation-manifest-uncertainty";
        let uncertain_guard = crate::test_support::ProviderGuard::register(
            uncertain_api,
            Arc::new(delegation_provider()),
        );
        let uncertain_options = CodingAgentSessionOptions::new()
            .with_ai_client(uncertain_guard.ai_client())
            .with_session_id("sess_automatic_delegation_uncertain")
            .with_session_log_root(uncertain_temp.path());
        let mut uncertain = CodingAgentSession::create_internal(uncertain_options.clone())
            .await
            .unwrap();
        uncertain
            .persistent_session_service()
            .fail_store_after_for_tests(StoreFailurePoint::UpdateManifest, 0);
        let outcome = uncertain
            .run_internal(CodingAgentOperation::Prompt(prompt_options(
                uncertain_api,
                "delegate before uncertain commit",
            )))
            .await
            .unwrap();
        let operation_id = match outcome {
            CodingAgentOperationOutcome::Prompt(PromptTurnOutcome::Failed { error, .. })
                if error.code() == "partial_commit" =>
            {
                let CodingAgentErrorContext::Operation { operation_id } = error.context else {
                    panic!("partial-commit public error must retain its operation id");
                };
                operation_id
            }
            other => panic!("expected partial-commit prompt failure, got {other:?}"),
        };
        drop(uncertain);
        let reopened = CodingAgentSession::open_internal(uncertain_options)
            .await
            .unwrap();
        let replay = reopened.persistent_session_service().replay().unwrap();
        assert_eq!(
            replay.operation_status(&operation_id),
            Some(crate::session::replay::OperationReplayStatus::Committed),
            "authoritative terminal facts resolve manifest uncertainty on reopen"
        );
        assert!(
            reopened
                .persistent_session_service()
                .recovery_summary()
                .unwrap()
                .in_doubt_operations
                .is_empty(),
            "resolved committed work must not retain recovery-pending residue"
        );
        let outbox = std::fs::read_to_string(
            uncertain_temp
                .path()
                .join("sess_automatic_delegation_uncertain/outbox.jsonl"),
        )
        .unwrap();
        assert!(outbox.contains(&operation_id));
    }

    #[tokio::test]
    async fn compact_persistent_session_records_events_and_replays_summary() {
        let api = "coding-session-compact";
        let _provider_guard = crate::test_support::ProviderGuard::register(
            api,
            Arc::new(FauxProvider::with_call_queue(vec![
                FauxProvider::text_call("first answer", StopReason::Stop),
                FauxProvider::text_call("summary from compact", StopReason::Stop),
            ])),
        );
        let temp = tempfile::tempdir().unwrap();
        let mut session = CodingAgentSession::create_internal(
            CodingAgentSessionOptions::new()
                .with_ai_client(_provider_guard.ai_client())
                .with_session_id("sess_compact")
                .with_session_log_root(temp.path()),
        )
        .await
        .unwrap();
        prompt_outcome(
            session
                .run_internal(CodingAgentOperation::Prompt(prompt_options(
                    api,
                    "first question",
                )))
                .await
                .unwrap(),
        );
        let mut events = session.subscribe_product_events();

        let outcome = compact_outcome(
            session
                .run_internal(CodingAgentOperation::Compact(compact_options(
                    api,
                    Some("keep decisions"),
                )))
                .await
                .unwrap(),
        );

        assert!(matches!(
            &outcome,
            PromptTurnOutcome::Success {
                final_text,
                session_id: Some(session_id),
                leaf_id: Some(_),
                ..
            } if final_text == "summary from compact" && session_id == "sess_compact"
        ));
        let emitted_events = std::iter::from_fn(|| events.try_recv().unwrap()).collect::<Vec<_>>();
        assert_event_order(
            &emitted_events,
            &[
                "session_write_pending",
                "session_write_committed",
                "session_compaction_completed",
                "prompt_completed",
            ],
        );
        assert!(emitted_events.iter().any(|event| matches!(
            event.event(),
            CodingAgentProductEventKind::Session(
                CodingAgentSessionProductEvent::CompactionCompleted {
                    summary,
                    tokens_before,
                    ..
                }
            ) if summary == "summary from compact" && *tokens_before > 0
        )));

        let replay = session.persistent_session_service().replay().unwrap();
        assert!(matches!(
            replay.transcript.as_slice(),
            [
                TranscriptItem::CompactionSummary {
                    summary,
                    first_kept_message_id,
                    tokens_before,
                },
                TranscriptItem::AssistantMessage {
                    content,
                    status: MessageStatus::Completed,
                    ..
                },
            ] if summary == "summary from compact"
                && first_kept_message_id.starts_with("msg_")
                && *tokens_before > 0
                && content == &vec![PersistedContentBlock::Text {
                    text: "first answer".into(),
                }]
        ));
        let event_log =
            std::fs::read_to_string(temp.path().join("sess_compact/events.jsonl")).unwrap();
        assert!(event_log.contains("session.compaction.started"));
        assert!(event_log.contains("session.compaction.completed"));
        assert!(event_log.contains("operation.terminal.recorded"));
        let outbox =
            std::fs::read_to_string(temp.path().join("sess_compact/outbox.jsonl")).unwrap();
        assert!(outbox.contains("operation_terminal"));
        assert!(outbox.contains("summary from compact"));

        session.shutdown_internal().await.unwrap();
        drop(session);
        let reopened = CodingAgentSession::open_internal(
            CodingAgentSessionOptions::new()
                .with_session_id("sess_compact")
                .with_session_log_root(temp.path()),
        )
        .await
        .unwrap();
        let connection = reopened
            .connect(public_projection::CodingAgentClientId::new(
                "compact-replay",
            ))
            .unwrap();
        let public_projection::CodingAgentReconnect::Replayed { events, .. } =
            connection.reconnect(0).unwrap()
        else {
            panic!("compact terminal outbox must be retained for restart redelivery")
        };
        assert!(events.iter().any(|event| {
            matches!(
                event.event(),
                CodingAgentProductEventKind::Session(
                    CodingAgentSessionProductEvent::CompactionCompleted { .. }
                )
            ) && event.terminal_operation().is_some_and(|terminal| {
                terminal.kind
                    == crate::events::CodingAgentProductEventTerminalOperationKind::Compact
            })
        }));
    }

    #[tokio::test]
    async fn failed_transaction_store_fixture_enters_durable_recovery_pending_without_terminal() {
        let prompt_api = "coding-session-store-failure-prompt";
        let prompt_provider = crate::test_support::ProviderGuard::register(
            prompt_api,
            Arc::new(FauxProvider::with_call_queue(vec![
                FauxProvider::text_call("seed answer", StopReason::Stop),
                FauxProvider::text_call("partial", StopReason::Error),
            ])),
        );
        let prompt_temp = tempfile::tempdir().unwrap();
        let mut prompt_session = CodingAgentSession::create_internal(
            CodingAgentSessionOptions::new()
                .with_session_id("sess_store_failure_prompt")
                .with_session_log_root(prompt_temp.path()),
        )
        .await
        .unwrap();
        prompt_outcome(
            prompt_session
                .run_internal(CodingAgentOperation::Prompt(prompt_options(
                    prompt_api,
                    "seed question",
                )))
                .await
                .unwrap(),
        );
        let prompt_connection = prompt_session
            .connect(public_projection::CodingAgentClientId::new(
                "store-failure-prompt-client",
            ))
            .unwrap();
        let prompt_draft =
            public_projection::CodingAgentDraftId("store-failure-prompt-draft".into());
        prompt_connection
            .set_prompt_draft(prompt_draft.clone(), "fail after durable append")
            .unwrap();
        let prompt_operation =
            CodingAgentOperation::Prompt(prompt_options(prompt_api, "fail after durable append"));
        let prompt_lease = prompt_connection
            .prepare_submission(&mut prompt_session, prompt_draft, &prompt_operation)
            .unwrap();
        prompt_session.arm_update_manifest_failure_for_tests(0);

        let prompt_operation_id = match prompt_session.run(prompt_operation).await.unwrap() {
            CodingAgentOperationOutcome::Prompt(PromptTurnOutcome::Failed {
                operation_id,
                error,
                ..
            }) if error.code() == "partial_commit" => {
                let CodingAgentErrorContext::Operation {
                    operation_id: partial_operation_id,
                } = error.context
                else {
                    panic!("partial-commit public error must retain its operation id");
                };
                assert_eq!(operation_id, partial_operation_id);
                operation_id
            }
            other => panic!("expected failed Prompt PartialCommit outcome, got {other:?}"),
        };
        drop(prompt_lease);

        let prompt_submitted = prompt_connection
            .state()
            .unwrap()
            .submitted_operation
            .expect("failed Prompt uncertain submitted state");
        assert_eq!(prompt_submitted.operation_id, prompt_operation_id);
        let prompt_recovery_id = match prompt_submitted.status {
            public_projection::CodingAgentSubmittedOperationStatus::RecoveryPending {
                recovery_id,
            } => recovery_id,
            other => panic!("unexpected failed Prompt recovery state: {other:?}"),
        };
        assert!(prompt_recovery_id.contains(&prompt_operation_id));
        assert!(prompt_recovery_id.ends_with("/session_write_committed"));
        let public_projection::CodingAgentReconnect::Replayed {
            events: prompt_events,
            ..
        } = prompt_connection.reconnect(0).unwrap()
        else {
            panic!("failed Prompt events should be retained")
        };
        assert_eq!(
            prompt_events
                .iter()
                .filter(|event| {
                    event.operation_id() == Some(prompt_operation_id.as_str())
                        && event.terminal_operation().is_some()
                })
                .count(),
            0
        );
        assert!(prompt_events.iter().any(|event| {
            event.operation_id() == Some(prompt_operation_id.as_str())
                && matches!(
                    event.event(),
                    CodingAgentProductEventKind::Session(
                        CodingAgentSessionProductEvent::WriteFailed {
                            status: CodingAgentSessionWriteFailureStatus::Uncertain,
                            ..
                        }
                    )
                )
                && matches!(
                    event.durability(),
                    CodingAgentProductEventDurability::PersistenceUncertain { operation_id }
                        if operation_id == &prompt_operation_id
                )
        }));
        drop(prompt_provider);

        let compact_api = "coding-session-store-failure-compact";
        let _compact_provider = crate::test_support::ProviderGuard::register(
            compact_api,
            Arc::new(FauxProvider::with_call_queue(vec![
                FauxProvider::text_call("seed answer", StopReason::Stop),
                FauxProvider::text_call("compact summary", StopReason::Stop),
            ])),
        );
        let compact_temp = tempfile::tempdir().unwrap();
        let mut compact_session = CodingAgentSession::create_internal(
            CodingAgentSessionOptions::new()
                .with_session_id("sess_store_failure_compact")
                .with_session_log_root(compact_temp.path()),
        )
        .await
        .unwrap();
        prompt_outcome(
            compact_session
                .run_internal(CodingAgentOperation::Prompt(prompt_options(
                    compact_api,
                    "seed question",
                )))
                .await
                .unwrap(),
        );
        let compact_connection = compact_session
            .connect(public_projection::CodingAgentClientId::new(
                "store-failure-compact-client",
            ))
            .unwrap();
        let compact_operation = CodingAgentOperation::Compact(compact_options(compact_api, None));
        let compact_lease = compact_connection
            .prepare_submission(
                &mut compact_session,
                public_projection::CodingAgentDraftId("unused".into()),
                &compact_operation,
            )
            .unwrap();
        compact_session.arm_update_manifest_failure_for_tests(0);

        let compact_operation_id = match compact_session
            .run_internal(compact_operation)
            .await
            .expect_err("Compact manifest update")
        {
            CodingSessionError::PartialCommit { operation_id, .. } => operation_id,
            other => panic!("expected Compact PartialCommit, got {other:?}"),
        };
        drop(compact_lease);

        let compact_submitted = compact_connection
            .state()
            .unwrap()
            .submitted_operation
            .expect("Compact uncertain submitted state");
        assert_eq!(compact_submitted.operation_id, compact_operation_id);
        assert!(matches!(
            compact_submitted.status,
            public_projection::CodingAgentSubmittedOperationStatus::RecoveryPending {
                recovery_id: ref pending_id,
            } if pending_id.contains(&compact_operation_id)
                && pending_id.ends_with("/session_write_committed")
        ));
        let public_projection::CodingAgentReconnect::Replayed {
            events: compact_events,
            ..
        } = compact_connection.reconnect(0).unwrap()
        else {
            panic!("Compact events should be retained")
        };
        assert_eq!(
            compact_events
                .iter()
                .filter(|event| {
                    event.operation_id() == Some(compact_operation_id.as_str())
                        && matches!(
                            event.event(),
                            CodingAgentProductEventKind::Session(
                                CodingAgentSessionProductEvent::CompactionCompleted { .. }
                            ) | CodingAgentProductEventKind::Workflow(
                                CodingAgentWorkflowProductEvent::PromptFailed { .. }
                            )
                        )
                })
                .count(),
            0
        );
    }

    #[tokio::test]
    async fn compact_cancellation_reaches_canonical_run_and_preserves_operation_id() {
        let seed_api = "coding-session-compact-cancellation-seed";
        let ai_client = AiClient::new();
        ai_client.register_provider(seed_api, Arc::new(FauxProvider::simple_text("seed answer")));
        let temp = tempfile::tempdir().unwrap();
        let mut session = CodingAgentSession::create_internal(
            CodingAgentSessionOptions::new()
                .with_session_id("sess_compact_cancellation")
                .with_session_log_root(temp.path())
                .with_ai_client(ai_client.clone()),
        )
        .await
        .unwrap();
        prompt_outcome(
            session
                .run_internal(CodingAgentOperation::Prompt(prompt_options(
                    seed_api,
                    "seed question",
                )))
                .await
                .unwrap(),
        );
        ai_client.unregister_provider(seed_api);

        let compact_api = "coding-session-compact-cancellation";
        let contexts = Arc::new(Mutex::new(Vec::new()));
        let (started_tx, started_rx) = oneshot::channel();
        let (release_tx, release_rx) = oneshot::channel();
        ai_client.register_provider(
            compact_api,
            Arc::new(BlockingTwoTurnProvider::new(
                contexts, started_tx, release_rx,
            )),
        );
        let connection = session
            .connect(public_projection::CodingAgentClientId::new(
                "compact-cancellation-client",
            ))
            .unwrap();
        let operation = CodingAgentOperation::Compact(compact_options(compact_api, None));
        let lease = connection
            .prepare_submission(
                &mut session,
                public_projection::CodingAgentDraftId("unused".into()),
                &operation,
            )
            .unwrap();
        let task = tokio::spawn(async move {
            let result = session.run(operation).await;
            (session, result)
        });

        started_rx.await.unwrap();
        let submitted = connection
            .state()
            .unwrap()
            .submitted_operation
            .expect("running Compact submission");
        assert!(matches!(
            submitted.status,
            public_projection::CodingAgentSubmittedOperationStatus::Running
        ));
        let stale = connection
            .operation_control("stale-operation")
            .abort(
                public_projection::CodingAgentControlId("abort-stale".into()),
                "stop stale compact",
            )
            .unwrap_err();
        assert_eq!(
            stale.reason,
            public_projection::CodingAgentControlRejectionReason::TargetMismatch
        );
        connection
            .operation_control(submitted.operation_id.clone())
            .abort(
                public_projection::CodingAgentControlId("abort-compact".into()),
                "stop compact",
            )
            .unwrap();
        release_tx.send(()).unwrap();

        let (_session, result) = task.await.unwrap();
        let outcome = compact_outcome(result.unwrap());
        assert!(matches!(
            &outcome,
            PromptTurnOutcome::Failed {
                operation_id,
                error,
                ..
            } if operation_id == &submitted.operation_id && error.code() == "cancelled"
        ));
        drop(lease);
        let terminal = connection
            .state()
            .unwrap()
            .submitted_operation
            .expect("cancelled Compact terminal state");
        assert_eq!(terminal.operation_id, submitted.operation_id);
        assert!(matches!(
            terminal.status,
            public_projection::CodingAgentSubmittedOperationStatus::Terminal {
                status: public_event::CodingAgentProductEventTerminalStatus::Failed,
                anchor: public_projection::CodingAgentSubmittedTerminalAnchor::ProductEvent { .. },
            }
        ));
        let completed = connection
            .operation_control(submitted.operation_id)
            .abort(
                public_projection::CodingAgentControlId("abort-completed".into()),
                "too late",
            )
            .unwrap_err();
        assert_eq!(
            completed.reason,
            public_projection::CodingAgentControlRejectionReason::TargetNotRunning
        );
    }

    #[tokio::test]
    async fn compact_summary_failure_records_failure_without_folding_replay() {
        let api = "coding-session-compact-summary-failure";
        let _provider_guard = crate::test_support::ProviderGuard::register(
            api,
            Arc::new(FauxProvider::with_call_queue(vec![
                FauxProvider::text_call("first answer", StopReason::Stop),
                FauxProvider::text_call("", StopReason::Stop),
            ])),
        );
        let temp = tempfile::tempdir().unwrap();
        let mut session = CodingAgentSession::create_internal(
            CodingAgentSessionOptions::new()
                .with_ai_client(_provider_guard.ai_client())
                .with_session_id("sess_compact_failure")
                .with_session_log_root(temp.path()),
        )
        .await
        .unwrap();
        let prompt_outcome = prompt_outcome(
            session
                .run_internal(CodingAgentOperation::Prompt(prompt_options(
                    api,
                    "first question",
                )))
                .await
                .unwrap(),
        );
        let active_leaf_before = match prompt_outcome {
            PromptTurnOutcome::Success {
                leaf_id: Some(leaf_id),
                ..
            } => leaf_id,
            other => panic!("expected prompt success, got {other:?}"),
        };
        let mut events = session.subscribe_product_events();

        let outcome = compact_outcome(
            session
                .run_internal(CodingAgentOperation::Compact(compact_options(api, None)))
                .await
                .unwrap(),
        );

        assert!(matches!(
            &outcome,
            PromptTurnOutcome::Failed { error, .. }
                if error.code() == "provider"
                    && error.category == crate::runtime::public_error::CodingAgentErrorCategory::Provider
                    && error.summary == "The model provider request failed."
                    && !error.to_string().contains("empty summary")
        ));
        let emitted_events = std::iter::from_fn(|| events.try_recv().unwrap()).collect::<Vec<_>>();
        assert_event_order(
            &emitted_events,
            &[
                "session_write_pending",
                "session_write_committed",
                "prompt_failed",
            ],
        );
        let replay = session.persistent_session_service().replay().unwrap();
        assert_eq!(
            replay.active_leaf_id.as_deref(),
            Some(active_leaf_before.as_str())
        );
        assert!(
            replay
                .transcript
                .iter()
                .all(|item| !matches!(item, TranscriptItem::CompactionSummary { .. }))
        );
        assert!(matches!(
            replay.transcript.as_slice(),
            [
                TranscriptItem::UserInput { text, .. },
                TranscriptItem::AssistantMessage { content, .. },
                TranscriptItem::Diagnostic { message, .. },
            ] if text == "first question"
                && content == &vec![PersistedContentBlock::Text {
                    text: "first answer".into(),
                }]
                && message.contains("empty summary")
        ));
        let event_log =
            std::fs::read_to_string(temp.path().join("sess_compact_failure/events.jsonl")).unwrap();
        assert!(event_log.contains("session.compaction.started"));
        assert!(event_log.contains("operation.failed"));
        assert!(event_log.contains("operation.terminal.recorded"));
        assert!(!event_log.contains("session.compaction.completed"));
        let outbox =
            std::fs::read_to_string(temp.path().join("sess_compact_failure/outbox.jsonl")).unwrap();
        assert!(outbox.contains("operation_terminal"));
        assert!(outbox.contains("\"operation_kind\":\"compact\""));

        session.shutdown_internal().await.unwrap();
        drop(session);
        let reopened = CodingAgentSession::open_internal(
            CodingAgentSessionOptions::new()
                .with_session_id("sess_compact_failure")
                .with_session_log_root(temp.path()),
        )
        .await
        .unwrap();
        let connection = reopened
            .connect(public_projection::CodingAgentClientId::new(
                "compact-failure-replay",
            ))
            .unwrap();
        let public_projection::CodingAgentReconnect::Replayed { events, .. } =
            connection.reconnect(0).unwrap()
        else {
            panic!("compact failure terminal must be retained for restart redelivery")
        };
        assert!(events.iter().any(|event| {
            matches!(
                event.event(),
                CodingAgentProductEventKind::Workflow(
                    CodingAgentWorkflowProductEvent::PromptFailed { .. }
                )
            ) && event.terminal_operation().is_some_and(|terminal| {
                terminal.kind
                    == crate::events::CodingAgentProductEventTerminalOperationKind::Compact
            })
        }));
    }

    #[tokio::test]
    async fn prompt_hydrates_replayed_transcript_when_opening_session() {
        let first_api = "coding-session-hydrate-first";
        let second_api = "coding-session-hydrate-second";
        let contexts = Arc::new(Mutex::new(Vec::new()));
        let _provider_guard = crate::test_support::ProviderGuard::register_many(vec![
            (
                first_api.to_string(),
                Arc::new(FauxProvider::simple_text("first answer")),
            ),
            (
                second_api.to_string(),
                Arc::new(RecordingProvider::new(
                    Arc::clone(&contexts),
                    "second answer",
                )),
            ),
        ]);
        let temp = tempfile::tempdir().unwrap();
        let options = CodingAgentSessionOptions::new()
            .with_ai_client(_provider_guard.ai_client())
            .with_session_id("sess_hydrate")
            .with_session_log_root(temp.path());
        let mut created = CodingAgentSession::create_internal(options.clone())
            .await
            .unwrap();
        prompt_outcome(
            created
                .run_internal(CodingAgentOperation::Prompt(prompt_options(
                    first_api,
                    "first question",
                )))
                .await
                .unwrap(),
        );
        let mut opened = CodingAgentSession::open_internal(options).await.unwrap();

        let outcome = prompt_outcome(
            opened
                .run_internal(CodingAgentOperation::Prompt(prompt_options(
                    second_api,
                    "second question",
                )))
                .await
                .unwrap(),
        );

        assert!(matches!(
            outcome,
            PromptTurnOutcome::Success { final_text, .. } if final_text == "second answer"
        ));
        let contexts = contexts.lock().unwrap();
        assert_eq!(contexts.len(), 1);
        assert_eq!(contexts[0].messages.len(), 3);
        assert!(matches!(
            &contexts[0].messages[0],
            Message::User { content }
                if content == &vec![ContentBlock::Text {
                    text: "first question".into(),
                    text_signature: None,
                }]
        ));
        assert!(matches!(
            &contexts[0].messages[1],
            Message::Assistant { content }
                if content == &vec![ContentBlock::Text {
                    text: "first answer".into(),
                    text_signature: None,
                }]
        ));
        assert!(matches!(
            &contexts[0].messages[2],
            Message::User { content }
                if content == &vec![ContentBlock::Text {
                    text: "second question".into(),
                    text_signature: None,
                }]
        ));
    }

    #[tokio::test]
    async fn prompt_hydrates_replayed_tool_calls_when_opening_session() {
        let first_api = "coding-session-hydrate-tool-first";
        let second_api = "coding-session-hydrate-tool-second";
        let contexts = Arc::new(Mutex::new(Vec::new()));
        let _provider_guard = crate::test_support::ProviderGuard::register_many(vec![
            (
                first_api.to_string(),
                Arc::new(FauxProvider::with_call_queue(vec![
                    FauxProvider::single_call(
                        vec![FauxResponse {
                            text_deltas: vec!["I will use echo.".into()],
                            thinking_deltas: Vec::new(),
                            tool_calls: vec![FauxToolCall {
                                id: "toolu_1".into(),
                                name: "echo".into(),
                                deltas: Vec::new(),
                                final_arguments: serde_json::json!({"text": "hi"}),
                            }],
                        }],
                        StopReason::ToolUse,
                    ),
                    FauxProvider::text_call("tool final", StopReason::Stop),
                ])),
            ),
            (
                second_api.to_string(),
                Arc::new(RecordingProvider::new(
                    Arc::clone(&contexts),
                    "second answer",
                )),
            ),
        ]);
        let temp = tempfile::tempdir().unwrap();
        let options = CodingAgentSessionOptions::new()
            .with_ai_client(_provider_guard.ai_client())
            .with_session_id("sess_tool_hydrate")
            .with_session_log_root(temp.path());
        let mut created = CodingAgentSession::create_internal(options.clone())
            .await
            .unwrap();
        prompt_outcome(
            created
                .run_internal(CodingAgentOperation::Prompt(prompt_options_with_tools(
                    first_api,
                    "use the tool",
                    vec![echo_tool()],
                )))
                .await
                .unwrap(),
        );
        let mut opened = CodingAgentSession::open_internal(options).await.unwrap();

        prompt_outcome(
            opened
                .run_internal(CodingAgentOperation::Prompt(prompt_options(
                    second_api, "continue",
                )))
                .await
                .unwrap(),
        );

        let contexts = contexts.lock().unwrap();
        assert_eq!(contexts.len(), 1);
        assert_eq!(contexts[0].messages.len(), 5);
        assert!(matches!(
            &contexts[0].messages[0],
            Message::User { content }
                if content == &vec![ContentBlock::Text {
                    text: "use the tool".into(),
                    text_signature: None,
                }]
        ));
        let tool_call_id = match &contexts[0].messages[1] {
            Message::Assistant { content } => match content.as_slice() {
                [
                    ContentBlock::Text { text, .. },
                    ContentBlock::ToolCall {
                        id,
                        name,
                        arguments,
                        ..
                    },
                ] => {
                    assert_eq!(text, "I will use echo.");
                    assert_eq!(name, "echo");
                    assert_eq!(arguments, &serde_json::json!({"text": "hi"}));
                    id.clone()
                }
                other => panic!("unexpected assistant content: {other:?}"),
            },
            other => panic!("unexpected hydrated assistant message: {other:?}"),
        };
        assert!(matches!(
            &contexts[0].messages[2],
            Message::ToolResult {
                tool_call_id: result_tool_call_id,
                tool_name: Some(tool_name),
                is_error: Some(false),
                content,
            } if result_tool_call_id == &tool_call_id
                && tool_name == "echo"
                && content == &vec![ContentBlock::Text {
                    text: "echo: hi".into(),
                    text_signature: None,
                }]
        ));
        assert!(matches!(
            &contexts[0].messages[3],
            Message::Assistant { content }
                if content == &vec![ContentBlock::Text {
                    text: "tool final".into(),
                    text_signature: None,
                }]
        ));
        assert!(matches!(
            &contexts[0].messages[4],
            Message::User { content }
                if content == &vec![ContentBlock::Text {
                    text: "continue".into(),
                    text_signature: None,
                }]
        ));
    }

    #[tokio::test]
    async fn export_current_html_writes_rust_native_session_transcript() {
        let api = "coding-session-export-html";
        let _provider_guard = crate::test_support::ProviderGuard::register(
            api,
            Arc::new(FauxProvider::with_call_queue(vec![
                FauxProvider::single_call(
                    vec![FauxResponse {
                        text_deltas: vec!["I will use echo.".into()],
                        thinking_deltas: Vec::new(),
                        tool_calls: vec![FauxToolCall {
                            id: "toolu_export".into(),
                            name: "echo".into(),
                            deltas: Vec::new(),
                            final_arguments: serde_json::json!({"text": "<hi>"}),
                        }],
                    }],
                    StopReason::ToolUse,
                ),
                FauxProvider::text_call("tool final <done>", StopReason::Stop),
            ])),
        );
        let temp = tempfile::tempdir().unwrap();
        let options = CodingAgentSessionOptions::new()
            .with_ai_client(_provider_guard.ai_client())
            .with_session_id("sess_export_html")
            .with_session_log_root(temp.path());
        let mut session = CodingAgentSession::create_internal(options).await.unwrap();
        prompt_outcome(
            session
                .run_internal(CodingAgentOperation::Prompt(prompt_options_with_tools(
                    api,
                    "use <tool>",
                    vec![echo_tool()],
                )))
                .await
                .unwrap(),
        );
        let output = temp.path().join("exports/session.html");

        let exported = session
            .run_internal(CodingAgentOperation::ExportCurrentHtml(output.clone()))
            .await
            .unwrap()
            .into_export_html()
            .expect("expected html export outcome");

        assert_eq!(exported, output);
        let html = std::fs::read_to_string(&exported).unwrap();
        assert!(html.contains("<!doctype html>"), "{html}");
        assert!(html.contains("sess_export_html"), "{html}");
        assert!(html.contains("use &lt;tool&gt;"), "{html}");
        assert!(html.contains("I will use echo."), "{html}");
        assert!(html.contains("Tool: echo"), "{html}");
        assert!(html.contains("&lt;hi&gt;"), "{html}");
        assert!(html.contains("echo: &lt;hi&gt;"), "{html}");
        assert!(html.contains("tool final &lt;done&gt;"), "{html}");
    }

    #[tokio::test]
    async fn export_current_html_rejects_jsonl_target() {
        let temp = tempfile::tempdir().unwrap();
        let mut session = CodingAgentSession::create_internal(
            CodingAgentSessionOptions::new()
                .with_session_id("sess_export_jsonl")
                .with_session_log_root(temp.path()),
        )
        .await
        .unwrap();
        let output = temp.path().join("session.jsonl");

        let error = session
            .run_internal(CodingAgentOperation::ExportCurrentHtml(output.clone()))
            .await
            .unwrap_err();

        assert_eq!(error.code(), "input");
        assert_eq!(
            error.to_string(),
            "invalid input: JSONL session export is no longer supported"
        );
        assert!(!output.exists());
    }

    #[tokio::test]
    async fn export_current_html_uses_read_only_operation_admission_while_root_busy() {
        let temp = tempfile::tempdir().unwrap();
        let mut session = CodingAgentSession::create_internal(
            CodingAgentSessionOptions::new()
                .with_session_id("sess_export_busy")
                .with_session_log_root(temp.path()),
        )
        .await
        .unwrap();
        let _operation = session
            .runtime_host
            .operation_supervisor
            .control
            .begin(OperationKind::Prompt, "op_test".into())
            .unwrap();
        let output = temp.path().join("session.html");

        let exported = session
            .run_internal(CodingAgentOperation::ExportCurrentHtml(output.clone()))
            .await
            .unwrap()
            .into_export_html()
            .expect("expected html export outcome");

        assert_eq!(exported, output);
        assert!(output.exists());
        assert_eq!(
            session.runtime_host.operation_supervisor.control.active(),
            Some(OperationKind::Prompt)
        );
    }

    fn outcome_turn_id(outcome: &PromptTurnOutcome) -> &str {
        match outcome {
            PromptTurnOutcome::Success { turn_id, .. } => turn_id,
            _ => panic!("expected success outcome"),
        }
    }

    fn assert_event_order(events: &[ProductEvent], expected: &[&str]) {
        let observed = events
            .iter()
            .map(|event| typed_event_kind(event.event()))
            .collect::<Vec<_>>();
        let mut next_index = 0;
        for kind in observed {
            if next_index < expected.len() && kind == expected[next_index] {
                next_index += 1;
            }
        }
        assert_eq!(
            next_index,
            expected.len(),
            "did not observe event order {expected:?}"
        );
    }

    fn typed_event_kind(event: &CodingAgentProductEventKind) -> &'static str {
        match event {
            CodingAgentProductEventKind::Session(CodingAgentSessionProductEvent::Opened {
                ..
            }) => "session_opened",
            CodingAgentProductEventKind::Session(
                CodingAgentSessionProductEvent::WritePending { .. },
            ) => "session_write_pending",
            CodingAgentProductEventKind::Session(
                CodingAgentSessionProductEvent::WriteCommitted { .. },
            ) => "session_write_committed",
            CodingAgentProductEventKind::Session(
                CodingAgentSessionProductEvent::WriteSkipped { .. },
            ) => "session_write_skipped",
            CodingAgentProductEventKind::Session(CodingAgentSessionProductEvent::WriteFailed {
                ..
            }) => "session_write_failed",
            CodingAgentProductEventKind::Session(
                CodingAgentSessionProductEvent::CompactionCompleted { .. },
            ) => "session_compaction_completed",
            CodingAgentProductEventKind::Profile(
                CodingAgentProfileProductEvent::DefaultChanged { .. },
            ) => "default_agent_profile_changed",
            CodingAgentProductEventKind::Agent(event) => match event {
                CodingAgentAgentProductEvent::InvocationStarted { .. } => {
                    "agent_invocation_started"
                }
                CodingAgentAgentProductEvent::InvocationCompleted { .. } => {
                    "agent_invocation_completed"
                }
                CodingAgentAgentProductEvent::InvocationFailed { .. } => "agent_invocation_failed",
                CodingAgentAgentProductEvent::InvocationAborted { .. } => {
                    "agent_invocation_aborted"
                }
                CodingAgentAgentProductEvent::TurnStarted { .. } => "agent_turn_started",
                CodingAgentAgentProductEvent::ProviderRequestStarted { .. } => {
                    "provider_request_started"
                }
            },
            CodingAgentProductEventKind::Team(event) => match event {
                CodingAgentTeamProductEvent::Started { .. } => "agent_team_started",
                CodingAgentTeamProductEvent::MemberStarted { .. } => "agent_team_member_started",
                CodingAgentTeamProductEvent::MemberCompleted { .. } => {
                    "agent_team_member_completed"
                }
                CodingAgentTeamProductEvent::Completed { .. } => "agent_team_completed",
                CodingAgentTeamProductEvent::Failed { .. } => "agent_team_failed",
                CodingAgentTeamProductEvent::Aborted { .. } => "agent_team_aborted",
            },
            CodingAgentProductEventKind::Message(event) => match event {
                CodingAgentMessageProductEvent::Started { .. } => "assistant_message_started",
                CodingAgentMessageProductEvent::Delta { .. } => "assistant_message_delta",
                CodingAgentMessageProductEvent::ThinkingDelta { .. } => "assistant_thinking_delta",
                CodingAgentMessageProductEvent::Completed { .. } => "assistant_message_completed",
            },
            CodingAgentProductEventKind::Tool(event) => match event {
                CodingAgentToolProductEvent::AuthorizationRequired { .. } => {
                    "tool_authorization_required"
                }
                CodingAgentToolProductEvent::AuthorizationApproved { .. } => {
                    "tool_authorization_approved"
                }
                CodingAgentToolProductEvent::AuthorizationDenied { .. } => {
                    "tool_authorization_denied"
                }
                CodingAgentToolProductEvent::AuthorizationCancelled { .. } => {
                    "tool_authorization_cancelled"
                }
                CodingAgentToolProductEvent::Started { .. } => "tool_call_started",
                CodingAgentToolProductEvent::Updated { .. } => "tool_call_updated",
                CodingAgentToolProductEvent::Completed { .. } => "tool_call_completed",
                CodingAgentToolProductEvent::Failed { .. } => "tool_call_failed",
            },
            CodingAgentProductEventKind::Runtime(
                CodingAgentRuntimeProductEvent::CompactionCompleted { .. },
            ) => "runtime_compaction_completed",
            CodingAgentProductEventKind::Runtime(CodingAgentRuntimeProductEvent::ShutDown) => {
                "runtime_shut_down"
            }
            CodingAgentProductEventKind::Delegation(event) => match event {
                CodingAgentDelegationProductEvent::Requested { .. } => "delegation_requested",
                CodingAgentDelegationProductEvent::Rejected { .. } => "delegation_rejected",
                CodingAgentDelegationProductEvent::Approved { .. } => "delegation_approved",
                CodingAgentDelegationProductEvent::ConfirmationRequired { .. } => {
                    "delegation_confirmation_required"
                }
                CodingAgentDelegationProductEvent::Started { .. } => "delegation_started",
                CodingAgentDelegationProductEvent::Completed { .. } => "delegation_completed",
                CodingAgentDelegationProductEvent::Failed { .. } => "delegation_failed",
            },
            CodingAgentProductEventKind::Workflow(event) => match event {
                CodingAgentWorkflowProductEvent::SelfHealingEditStarted { .. } => {
                    "self_healing_edit_started"
                }
                CodingAgentWorkflowProductEvent::SelfHealingEditRepairAttempted { .. } => {
                    "self_healing_edit_repair_attempted"
                }
                CodingAgentWorkflowProductEvent::SelfHealingEditCompleted { .. } => {
                    "self_healing_edit_completed"
                }
                CodingAgentWorkflowProductEvent::SelfHealingEditFailed { .. } => {
                    "self_healing_edit_failed"
                }
                CodingAgentWorkflowProductEvent::SelfHealingEditAborted { .. } => {
                    "self_healing_edit_aborted"
                }
                CodingAgentWorkflowProductEvent::PromptStarted { .. } => "prompt_started",
                CodingAgentWorkflowProductEvent::PromptCompleted { .. } => "prompt_completed",
                CodingAgentWorkflowProductEvent::PromptFailed { .. } => "prompt_failed",
                CodingAgentWorkflowProductEvent::PromptAborted { .. } => "prompt_aborted",
                CodingAgentWorkflowProductEvent::OperationRecoveryPending { .. } => {
                    "operation_recovery_pending"
                }
                CodingAgentWorkflowProductEvent::OperationRecoveryResolved { .. } => {
                    "operation_recovery_resolved"
                }
                CodingAgentWorkflowProductEvent::OperationRecovered { .. } => "operation_recovered",
            },
            CodingAgentProductEventKind::Diagnostic(
                CodingAgentDiagnosticProductEvent::Diagnostic { .. },
            ) => "diagnostic",
            CodingAgentProductEventKind::Capability(
                CodingAgentCapabilityProductEvent::Changed { .. },
            ) => "capability_changed",
        }
    }
}
