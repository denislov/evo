use std::future::Future;
use std::pin::Pin;

use super::*;
use crate::kernel::capability::CapabilityGeneration;
use crate::services::ports::{CapabilityTransitionLease, SessionWriter};
use tool_contract::api::definition::{
    ToolBehaviorVersion, ToolCapabilities, ToolDefinition, ToolId, ToolKind,
};

#[test]
fn typed_runtime_definitions_keep_their_declared_authorization_risk() {
    let definition = ToolDefinition {
        id: ToolId::new("read").unwrap(),
        kind: ToolKind::Function,
        description: "Read a file".into(),
        parameters: serde_json::json!({"type": "object"}),
        capabilities: ToolCapabilities {
            read_only: true,
            ..ToolCapabilities::default()
        },
        behavior: ToolBehaviorVersion::V1,
        authorization_risk: AuthorizationRisk::WorkspaceLocalReadOnly,
        requirements: Vec::new(),
    };
    let inventory = ToolAuthorizationInventory::new(&[definition]);
    assert_eq!(
        inventory.explicit_tools.get(&ToolId::new("read").unwrap()),
        Some(&Some(DeclaredToolAuthorizationRisk::WorkspaceLocalReadOnly))
    );
}

#[derive(Debug, Default)]
struct FakeSessionWriter {
    batches: Mutex<Vec<Vec<SessionEventData>>>,
}

impl FakeSessionWriter {
    fn labels(&self) -> Vec<&'static str> {
        self.batches
            .lock_or_recover("fake writer")
            .iter()
            .flatten()
            .map(|event| match event {
                SessionEventData::ToolAuthorizationRequested { .. } => "requested",
                SessionEventData::ToolAuthorizationResolved { resolution, .. } => {
                    match resolution {
                        PersistedToolAuthorizationResolution::Approved { .. } => "approved",
                        PersistedToolAuthorizationResolution::Denied { .. } => "denied",
                        PersistedToolAuthorizationResolution::Cancelled { .. } => "cancelled",
                        PersistedToolAuthorizationResolution::Interrupted { .. } => "interrupted",
                    }
                }
                _ => "other",
            })
            .collect()
    }

    fn record(&self, events: Vec<SessionEventData>) {
        self.batches.lock_or_recover("fake writer").push(events);
    }
}

impl SessionWriter for FakeSessionWriter {
    fn append<'a>(
        &'a self,
        _operation_id: &'a str,
        _turn_id: &'a str,
        events: Vec<SessionEventData>,
    ) -> Pin<Box<dyn Future<Output = Result<(), CodingSessionError>> + Send + 'a>> {
        Box::pin(async move {
            self.record(events);
            Ok(())
        })
    }

    fn append_blocking(
        &self,
        _operation_id: &str,
        _turn_id: &str,
        events: Vec<SessionEventData>,
    ) -> Result<(), CodingSessionError> {
        self.record(events);
        Ok(())
    }
}

#[derive(Default)]
struct FakeEventSink {
    events: Mutex<Vec<&'static str>>,
}

impl FakeEventSink {
    fn push(&self, event: &'static str) {
        self.events.lock_or_recover("fake event sink").push(event);
    }

    fn recorded(&self) -> Vec<&'static str> {
        self.events.lock_or_recover("fake event sink").clone()
    }
}

impl EventSink for FakeEventSink {
    fn diagnostic(
        &self,
        _operation_id: Option<String>,
        _message: String,
    ) -> Result<(), CodingSessionError> {
        self.push("diagnostic");
        Ok(())
    }

    fn tool_authorization_required(
        &self,
        _request: ToolAuthorizationRequest,
    ) -> Result<(), CodingSessionError> {
        self.push("required");
        Ok(())
    }

    fn tool_authorization_approved(
        &self,
        _request: ToolAuthorizationRequest,
        _decision: ToolAuthorizationDecision,
    ) -> Result<(), CodingSessionError> {
        self.push("approved");
        Ok(())
    }

    fn tool_authorization_denied(
        &self,
        _request: ToolAuthorizationRequest,
        _reason: String,
    ) -> Result<(), CodingSessionError> {
        self.push("denied");
        Ok(())
    }

    fn tool_authorization_cancelled(
        &self,
        _request: ToolAuthorizationRequest,
        _reason: String,
    ) -> Result<(), CodingSessionError> {
        self.push("cancelled");
        Ok(())
    }

    fn delegation_rejected(
        &self,
        _request: &DelegationRequest,
        _reason: &str,
    ) -> Result<(), CodingSessionError> {
        self.push("delegation_rejected");
        Ok(())
    }
}

struct NoopTransitionLease;

impl CapabilityTransitionLease for NoopTransitionLease {}

struct FakeCapabilityQuery {
    generation: Mutex<CapabilityGeneration>,
    pending_snapshots: Mutex<Vec<Vec<ToolAuthorizationRequest>>>,
}

impl FakeCapabilityQuery {
    fn new(generation: u64) -> Self {
        Self {
            generation: Mutex::new(CapabilityGeneration::new(generation)),
            pending_snapshots: Mutex::new(Vec::new()),
        }
    }

    fn pending_snapshots(&self) -> Vec<Vec<ToolAuthorizationRequest>> {
        self.pending_snapshots
            .lock_or_recover("fake capability snapshots")
            .clone()
    }
}

impl CapabilityQuery for FakeCapabilityQuery {
    fn acquire_transition(
        &self,
    ) -> Result<Box<dyn CapabilityTransitionLease + '_>, CodingSessionError> {
        Ok(Box::new(NoopTransitionLease))
    }

    fn current_generation(&self) -> Result<CapabilityGeneration, CodingSessionError> {
        Ok(*self.generation.lock_or_recover("fake generation"))
    }

    fn set_pending_authorizations(
        &self,
        pending: Vec<ToolAuthorizationRequest>,
    ) -> Result<(), CodingSessionError> {
        self.pending_snapshots
            .lock_or_recover("fake capability snapshots")
            .push(pending);
        Ok(())
    }
}

fn request(id: &str, generation: u64) -> ToolAuthorizationRequest {
    make_request(id, generation)
}

fn make_request(id: &str, generation: u64) -> ToolAuthorizationRequest {
    ToolAuthorizationRequest {
        authorization_id: id.into(),
        operation_id: "op-1".into(),
        turn_id: "turn-1".into(),
        tool_call_id: format!("call-{id}"),
        tool_name: "shell".into(),
        risk: ToolAuthorizationRisk::ShellExecution,
        scope: ToolAuthorizationScope::ToolArguments {
            fingerprint: "fingerprint".into(),
        },
        preview: ToolAuthorizationPreview {
            summary: "run command".into(),
            path: None,
            command: Some("cargo check".into()),
            cwd: Some("/workspace".into()),
            content_preview: None,
        },
        capability_generation: generation,
        requested_at: "2026-08-02T00:00:00Z".into(),
    }
}

fn service(
    generation: u64,
) -> (
    AuthorizationService,
    Arc<FakeCapabilityQuery>,
    Arc<FakeEventSink>,
) {
    let capabilities = Arc::new(FakeCapabilityQuery::new(generation));
    let events = Arc::new(FakeEventSink::default());
    let service = AuthorizationService::with_ports(
        ToolAuthorizationMode::Ask,
        capabilities.clone(),
        events.clone(),
        Some(Arc::new(crate::services::ports::NoopExtensionEventSink)),
        ("test-session".into(), "/ws".into()),
    );
    (service, capabilities, events)
}

fn insert_pending(
    service: &AuthorizationService,
    request: ToolAuthorizationRequest,
    event_writer: SessionWriterPort,
) -> oneshot::Receiver<PendingResolution> {
    let (sender, receiver) = oneshot::channel();
    let mut state = service.state.lock_or_recover("authorization state");
    state.pending.insert(
        request.authorization_id.clone(),
        PendingAuthorization {
            request,
            sender,
            event_writer: Some(event_writer),
            filesystem_binding: None,
        },
    );
    state.revision = state.revision.wrapping_add(1);
    receiver
}

#[tokio::test]
async fn authorization_persistence_transition_table() {
    let cases = [
        (true, None, vec!["requested"]),
        (
            false,
            Some(PersistedToolAuthorizationResolution::Approved {
                decision: ToolAuthorizationDecision::AllowOnce,
            }),
            vec!["approved"],
        ),
        (
            true,
            Some(PersistedToolAuthorizationResolution::Denied {
                reason: "no".into(),
            }),
            vec!["requested", "denied"],
        ),
    ];

    for (index, (include_request, resolution, expected)) in cases.into_iter().enumerate() {
        let writer = Arc::new(FakeSessionWriter::default());
        let writer_port: SessionWriterPort = writer.clone();
        persist_authorization_events(
            Some(&writer_port),
            &request(&format!("auth-{index}"), 1),
            include_request,
            resolution,
        )
        .await
        .expect("authorization facts should persist through the writer port");
        assert_eq!(writer.labels(), expected);
    }
}

#[tokio::test]
async fn authorization_decision_transition_table() {
    let cases = [
        (ToolAuthorizationDecision::AllowOnce, "approved", "allow", 0),
        (
            ToolAuthorizationDecision::AllowForOperation,
            "approved",
            "allow",
            1,
        ),
        (
            ToolAuthorizationDecision::Deny {
                reason: Some("declined".into()),
            },
            "denied",
            "deny",
            0,
        ),
    ];

    for (index, (decision, expected_event, expected_resolution, expected_grants)) in
        cases.into_iter().enumerate()
    {
        let (service, capabilities, events) = service(7);
        let request = request(&format!("auth-{index}"), 7);
        let identity = request.identity();
        let writer = Arc::new(FakeSessionWriter::default());
        let receiver = insert_pending(&service, request, writer.clone());

        service
            .decide(&identity, decision)
            .await
            .expect("current authorization decision should resolve");

        let actual_resolution = match receiver.await.expect("waiter should resolve") {
            PendingResolution::Allow => "allow",
            PendingResolution::Deny(_) => "deny",
        };
        assert_eq!(actual_resolution, expected_resolution);
        assert_eq!(events.recorded(), vec![expected_event]);
        assert_eq!(
            service
                .state
                .lock_or_recover("authorization state")
                .grants
                .len(),
            expected_grants
        );
        assert_eq!(writer.labels(), vec![expected_event]);
        assert_eq!(capabilities.pending_snapshots(), vec![Vec::new()]);
    }
}

#[tokio::test]
async fn authorization_generation_transition_table() {
    let cases = [(11, 11, true, "approved"), (10, 11, false, "cancelled")];

    for (index, (request_generation, current_generation, succeeds, expected_event)) in
        cases.into_iter().enumerate()
    {
        let (service, capabilities, events) = service(current_generation);
        let request = request(&format!("auth-{index}"), request_generation);
        let identity = request.identity();
        let writer = Arc::new(FakeSessionWriter::default());
        let receiver = insert_pending(&service, request, writer.clone());

        let result = service
            .decide(&identity, ToolAuthorizationDecision::AllowOnce)
            .await;
        assert_eq!(result.is_ok(), succeeds);
        let waiter_allowed = matches!(
            receiver.await.expect("waiter should resolve"),
            PendingResolution::Allow
        );
        assert_eq!(waiter_allowed, succeeds);
        assert_eq!(events.recorded(), vec![expected_event]);
        assert_eq!(writer.labels(), vec![expected_event]);
        assert_eq!(capabilities.pending_snapshots(), vec![Vec::new()]);
    }
}

#[derive(Debug, Default)]
struct RecordingExtensionSink {
    events: std::sync::Mutex<Vec<String>>,
}

impl crate::services::ports::ExtensionEventSink for RecordingExtensionSink {
    fn submit(
        &self,
        kind: extension_host::api::ExtensionEventKind,
        _session_id: &str,
        _workspace_root: &str,
        payload: extension_host::api::ExtensionEventPayload,
    ) {
        self.events
            .lock_or_recover("recording extension sink")
            .push(format!("{kind}:{payload:?}"));
    }

    fn hook_gate(&self) -> Option<Arc<extension_host::api::HookGate>> {
        None
    }
}

#[tokio::test]
async fn denied_decision_emits_permission_denied_to_extension_hooks() {
    let capabilities = Arc::new(FakeCapabilityQuery::new(7));
    let events = Arc::new(FakeEventSink::default());
    let extension_sink = Arc::new(RecordingExtensionSink::default());
    let service = AuthorizationService::with_ports(
        ToolAuthorizationMode::Ask,
        capabilities.clone(),
        events.clone(),
        Some(extension_sink.clone()),
        ("test-session".into(), "/ws".into()),
    );
    let request = make_request("auth-deny", 7);
    let identity = request.identity();
    let writer = Arc::new(FakeSessionWriter::default());
    let _receiver = insert_pending(&service, request.clone(), writer.clone());

    service
        .decide(
            &identity,
            ToolAuthorizationDecision::Deny {
                reason: Some("user declined".into()),
            },
        )
        .await
        .expect("deny resolves");

    let recorded = extension_sink
        .events
        .lock_or_recover("extension sink")
        .clone();
    assert_eq!(recorded.len(), 1, "exactly one permission_denied event");
    assert!(
        recorded[0].starts_with("permission_denied:"),
        "kind must be permission_denied, got {:?}",
        recorded
    );
    assert!(
        recorded[0].contains("user declined"),
        "reason must travel with the event: {:?}",
        recorded
    );

    // Allow 决策不产生 permission_denied。
    let request = make_request("auth-allow", 7);
    let identity = request.identity();
    let writer = Arc::new(FakeSessionWriter::default());
    let _receiver = insert_pending(&service, request, writer);
    service
        .decide(&identity, ToolAuthorizationDecision::AllowOnce)
        .await
        .expect("allow resolves");
    assert_eq!(
        extension_sink
            .events
            .lock_or_recover("extension sink")
            .len(),
        1,
        "allow decisions must not emit permission_denied"
    );
}

#[test]
fn runtime_mode_switch_updates_the_interactive_waiter_policy() {
    let (service, _, _) = service(3);
    assert!(service.uses_interactive_waiters());

    service
        .set_mode(ToolAuthorizationMode::Yolo)
        .expect("yolo switch should succeed");
    assert!(!service.uses_interactive_waiters());

    service
        .set_mode(ToolAuthorizationMode::Plan)
        .expect("plan switch should succeed");
    assert!(!service.uses_interactive_waiters());

    service
        .set_mode(ToolAuthorizationMode::Ask)
        .expect("ask switch should succeed");
    assert!(service.uses_interactive_waiters());
}

#[test]
fn mode_switch_shares_across_service_clones() {
    let (service, _, _) = service(3);
    service
        .clone()
        .set_mode(ToolAuthorizationMode::Plan)
        .expect("mode switch should succeed");
    assert_eq!(
        service.state.lock_or_recover("authorization state").mode,
        ToolAuthorizationMode::Plan
    );
    assert!(!service.uses_interactive_waiters());
}
