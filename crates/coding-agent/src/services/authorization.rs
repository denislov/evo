use crate::authorization::{
    ToolAuthorizationDecision, ToolAuthorizationMode, ToolAuthorizationPreview,
    ToolAuthorizationRequest, ToolAuthorizationRisk, ToolAuthorizationScope,
};
use crate::operations::delegation::{DelegationToolResult, DelegationToolResultStatus};
use crate::operations::prompt::context::DelegationRequest;
use crate::profiles::{ProfileId, ProfileKind};
use crate::runtime::capability::{
    FilesystemBindingDescriptor, FilesystemCapability, OperationCapabilitySnapshot,
};
use crate::runtime::facade::CodingSessionError;
use crate::runtime::snapshot::SnapshotCoordinator;
use crate::services::event::EventService;
use crate::session::event::{PersistedToolAuthorizationResolution, SessionEventData};
use crate::session::service::SessionEventWriter;
use agent_core::api::agent::{BeforeToolCallContext, BeforeToolCallResult};
use agent_core::api::tool::AgentTool;
use agent_core::api::transcript::create_session_id;
use agent_core::api::transcript::create_timestamp;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::oneshot;

const TOOL_AUTHORIZATION_RESPONSE_TIMEOUT: Duration = Duration::from_secs(120);
const TOOL_AUTHORIZATION_TIMEOUT_REASON: &str = "tool authorization timed out";

#[derive(Debug, Clone, Default)]
pub(crate) struct ToolAuthorizationInventory {
    explicit_tools: BTreeMap<String, Option<DeclaredToolAuthorizationRisk>>,
}

impl ToolAuthorizationInventory {
    pub(crate) fn new(explicit_tools: &[AgentTool]) -> Self {
        Self {
            explicit_tools: explicit_tools
                .iter()
                .map(|tool| (tool.name.clone(), declared_tool_risk(tool)))
                .collect(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DeclaredToolAuthorizationRisk {
    WorkspaceLocalReadOnly,
    SideEffect,
}

fn declared_tool_risk(tool: &AgentTool) -> Option<DeclaredToolAuthorizationRisk> {
    match tool
        .parameters
        .get("x-evo-authorization-risk")
        .and_then(Value::as_str)
    {
        Some("workspace_local_read_only") => {
            Some(DeclaredToolAuthorizationRisk::WorkspaceLocalReadOnly)
        }
        Some("side_effect") => Some(DeclaredToolAuthorizationRisk::SideEffect),
        _ => None,
    }
}

#[derive(Debug, Clone)]
pub(crate) struct AuthorizationHookContext {
    pub(crate) service: AuthorizationService,
    pub(crate) turn_id: String,
    pub(crate) capability_snapshot: OperationCapabilitySnapshot,
    pub(crate) event_writer: Option<SessionEventWriter>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct OperationGrant {
    operation_id: String,
    tool_name: String,
    scope: ToolAuthorizationScope,
}

struct PendingAuthorization {
    request: ToolAuthorizationRequest,
    sender: oneshot::Sender<PendingResolution>,
    event_writer: Option<SessionEventWriter>,
    filesystem_binding: Option<PendingFilesystemBinding>,
}

struct PendingFilesystemBinding {
    filesystem: FilesystemCapability,
    operation_id: String,
    tool_call_id: String,
}

impl PendingFilesystemBinding {
    fn discard(&self) {
        self.filesystem
            .discard_bound_tool_target(&self.operation_id, &self.tool_call_id);
    }
}

#[derive(Debug)]
enum PendingResolution {
    Allow,
    Deny(String),
}

#[derive(Default)]
struct AuthorizationState {
    pending: BTreeMap<String, PendingAuthorization>,
    grants: HashSet<OperationGrant>,
    revision: u64,
}

#[derive(Clone)]
pub(crate) struct AuthorizationService {
    mode: ToolAuthorizationMode,
    coordinator: Arc<SnapshotCoordinator>,
    event_service: EventService,
    state: Arc<Mutex<AuthorizationState>>,
}

struct AuthorizationWaiterGuard {
    service: AuthorizationService,
    authorization_id: String,
}

impl Drop for AuthorizationWaiterGuard {
    fn drop(&mut self) {
        let Some(entry) = self.service.remove_pending(&self.authorization_id) else {
            return;
        };
        if let Some(binding) = entry.filesystem_binding.as_ref() {
            binding.discard();
        }
        let reason = "tool authorization waiter was dropped";
        self.service.persist_resolution_or_diagnose(
            &entry,
            PersistedToolAuthorizationResolution::Cancelled {
                reason: reason.into(),
            },
        );
        self.service
            .event_service
            .emit_tool_authorization_cancelled(entry.request, reason);
    }
}

impl std::fmt::Debug for AuthorizationService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AuthorizationService")
            .field("mode", &self.mode)
            .field("pending", &self.state.lock().unwrap().pending.len())
            .finish()
    }
}

impl AuthorizationService {
    pub(crate) fn new(
        mode: ToolAuthorizationMode,
        coordinator: Arc<SnapshotCoordinator>,
        event_service: EventService,
    ) -> Self {
        Self {
            mode,
            coordinator,
            event_service,
            state: Arc::new(Mutex::new(AuthorizationState::default())),
        }
    }

    pub(crate) fn pending(&self) -> Vec<ToolAuthorizationRequest> {
        pending_requests(&self.state.lock().unwrap())
    }

    pub(crate) fn uses_interactive_waiters(&self) -> bool {
        self.mode == ToolAuthorizationMode::Interactive
    }

    pub(crate) async fn authorize_with_event_writer(
        &self,
        context: BeforeToolCallContext,
        turn_id: String,
        snapshot: OperationCapabilitySnapshot,
        inventory: ToolAuthorizationInventory,
        event_writer: Option<SessionEventWriter>,
    ) -> Result<Option<BeforeToolCallResult>, String> {
        let operation_id = context
            .execution_context
            .scope_id()
            .ok_or_else(|| "tool authorization requires an operation identity".to_owned())?
            .to_owned();
        if operation_id != snapshot.operation_id {
            return Err("tool authorization operation identity mismatch".into());
        }

        let mut evaluation = evaluate(&context, &snapshot, &inventory)?;
        let filesystem_binding = bind_filesystem_target(&context, &snapshot).await?;
        if let Some(descriptor) = filesystem_binding.as_ref() {
            evaluation.bind_filesystem_descriptor(descriptor);
        }
        let Evaluation::Ask {
            risk,
            scope,
            preview,
        } = evaluation
        else {
            return Ok(None);
        };
        let grant = OperationGrant {
            operation_id: operation_id.clone(),
            tool_name: context.tool_name.clone(),
            scope: scope.clone(),
        };
        if self.state.lock().unwrap().grants.contains(&grant) {
            return Ok(None);
        }

        let authorization_id = format!("auth_{}", create_session_id());
        let request = ToolAuthorizationRequest {
            authorization_id: authorization_id.clone(),
            operation_id,
            turn_id: turn_id.clone(),
            tool_call_id: context.tool_call_id.clone(),
            tool_name: context.tool_name.clone(),
            risk,
            scope,
            preview,
            capability_generation: snapshot.generation.get(),
            requested_at: create_timestamp(),
        };
        match self.mode {
            ToolAuthorizationMode::AllowAll => return Ok(None),
            ToolAuthorizationMode::Deny => {
                discard_filesystem_binding(&context, &snapshot);
                let reason = "tool invocation requires authorization";
                persist_authorization_events(
                    event_writer.as_ref(),
                    &request,
                    true,
                    Some(PersistedToolAuthorizationResolution::Denied {
                        reason: reason.into(),
                    }),
                )?;
                self.event_service
                    .emit_tool_authorization_required(request.clone());
                self.event_service
                    .emit_tool_authorization_denied(request, reason);
                return Ok(Some(blocked(reason)));
            }
            ToolAuthorizationMode::Interactive => {}
        }
        if let Err(error) =
            persist_authorization_events(event_writer.as_ref(), &request, true, None)
        {
            discard_filesystem_binding(&context, &snapshot);
            return Err(error);
        }
        let (sender, mut receiver) = oneshot::channel();
        let (revision, pending) = {
            let mut state = self.state.lock().unwrap();
            state.pending.insert(
                authorization_id.clone(),
                PendingAuthorization {
                    request: request.clone(),
                    sender,
                    event_writer,
                    filesystem_binding: filesystem_binding.and_then(|_| {
                        snapshot.filesystem.as_ref().cloned().map(|filesystem| {
                            PendingFilesystemBinding {
                                filesystem,
                                operation_id: snapshot.operation_id.clone(),
                                tool_call_id: context.tool_call_id.clone(),
                            }
                        })
                    }),
                },
            );
            state.revision = state.revision.wrapping_add(1);
            (state.revision, pending_requests(&state))
        };
        self.sync_pending_snapshot(revision, pending);
        let identity = request.identity();
        self.event_service.emit_tool_authorization_required(request);
        let _waiter_guard = AuthorizationWaiterGuard {
            service: self.clone(),
            authorization_id: authorization_id.clone(),
        };

        let resolution = tokio::select! {
            resolution = &mut receiver => resolution,
            _ = context.execution_context.cancel_token().cancelled() => {
                if let Some(entry) = self.remove_pending(&authorization_id) {
                    if let Some(binding) = entry.filesystem_binding.as_ref() {
                        binding.discard();
                    }
                    self.persist_resolution_or_diagnose(
                        &entry,
                        PersistedToolAuthorizationResolution::Cancelled {
                            reason: "tool authorization was cancelled".into(),
                        },
                    );
                    self.event_service.emit_tool_authorization_cancelled(
                        entry.request,
                        "tool authorization was cancelled",
                    );
                }
                return Ok(Some(blocked("tool authorization was cancelled")));
            }
            _ = tokio::time::sleep(TOOL_AUTHORIZATION_RESPONSE_TIMEOUT) => {
                match self.decide(
                    &identity,
                    ToolAuthorizationDecision::Deny {
                        reason: Some(TOOL_AUTHORIZATION_TIMEOUT_REASON.into()),
                    },
                ) {
                    Ok(()) | Err(CodingSessionError::Input { .. }) => receiver.await,
                    Err(error) => return Err(error.to_string()),
                }
            }
        };
        match resolution {
            Ok(PendingResolution::Allow) => Ok(None),
            Ok(PendingResolution::Deny(reason)) => {
                if let Some(request) = delegation_request(&context, &turn_id, &snapshot) {
                    self.event_service
                        .emit_delegation_rejected(&request, &reason);
                    Ok(Some(blocked(delegation_rejected_result(&request, &reason))))
                } else {
                    Ok(Some(blocked(reason)))
                }
            }
            Err(_) => Ok(Some(blocked("tool authorization was interrupted"))),
        }
    }

    pub(crate) fn decide(
        &self,
        identity: &crate::authorization::ToolAuthorizationIdentity,
        decision: ToolAuthorizationDecision,
    ) -> Result<(), CodingSessionError> {
        let _capability_transition = self.coordinator.capability_transition_guard();
        let current_generation = self.coordinator.current_capability_generation().get();
        let (entry, revision, pending) = {
            let mut state = self.state.lock().unwrap();
            let Some(entry) = state.pending.get(&identity.authorization_id) else {
                return Err(CodingSessionError::Input {
                    message: format!(
                        "unknown or already resolved authorization: {}",
                        identity.authorization_id
                    ),
                });
            };
            if entry.request.identity() != *identity {
                return Err(CodingSessionError::Input {
                    message: "tool authorization identity is stale".into(),
                });
            }
            let entry = state
                .pending
                .remove(&identity.authorization_id)
                .expect("authorization identity was verified while holding the state lock");
            state.revision = state.revision.wrapping_add(1);
            let revision = state.revision;
            let pending = pending_requests(&state);
            (entry, revision, pending)
        };
        self.sync_pending_snapshot(revision, pending);

        if entry.request.capability_generation != current_generation {
            let reason = "tool authorization capability generation is stale";
            if let Some(binding) = entry.filesystem_binding.as_ref() {
                binding.discard();
            }
            self.persist_resolution_or_diagnose(
                &entry,
                PersistedToolAuthorizationResolution::Cancelled {
                    reason: reason.into(),
                },
            );
            self.event_service
                .emit_tool_authorization_cancelled(entry.request.clone(), reason);
            let _ = entry.sender.send(PendingResolution::Deny(reason.into()));
            return Err(CodingSessionError::Input {
                message: reason.into(),
            });
        }

        let (resolution, persisted_resolution) = match &decision {
            ToolAuthorizationDecision::AllowOnce | ToolAuthorizationDecision::AllowForOperation => {
                (
                    PendingResolution::Allow,
                    PersistedToolAuthorizationResolution::Approved {
                        decision: decision.clone(),
                    },
                )
            }
            ToolAuthorizationDecision::Deny { reason } => {
                let reason = reason
                    .clone()
                    .unwrap_or_else(|| "tool invocation denied by user".into());
                (
                    PendingResolution::Deny(reason.clone()),
                    PersistedToolAuthorizationResolution::Denied { reason },
                )
            }
        };
        if let Err(message) = persist_authorization_events(
            entry.event_writer.as_ref(),
            &entry.request,
            false,
            Some(persisted_resolution),
        ) {
            self.restore_pending(identity.authorization_id.clone(), entry);
            return Err(CodingSessionError::Session { message });
        }
        if matches!(&resolution, PendingResolution::Deny(_))
            && let Some(binding) = entry.filesystem_binding.as_ref()
        {
            binding.discard();
        }

        let operation_grant = matches!(&decision, ToolAuthorizationDecision::AllowForOperation)
            .then(|| OperationGrant {
                operation_id: entry.request.operation_id.clone(),
                tool_name: entry.request.tool_name.clone(),
                scope: entry.request.scope.clone(),
            });
        if let Some(grant) = operation_grant.clone() {
            self.state.lock().unwrap().grants.insert(grant);
        }
        match &resolution {
            PendingResolution::Allow => {
                self.event_service
                    .emit_tool_authorization_approved(entry.request.clone(), decision);
            }
            PendingResolution::Deny(reason) => {
                self.event_service
                    .emit_tool_authorization_denied(entry.request.clone(), reason.clone());
            }
        }
        if entry.sender.send(resolution).is_err() {
            if let Some(binding) = entry.filesystem_binding.as_ref() {
                binding.discard();
            }
            if let Some(grant) = operation_grant {
                self.state.lock().unwrap().grants.remove(&grant);
            }
            self.event_service.emit_tool_authorization_cancelled(
                entry.request,
                "authorization waiter is no longer active",
            );
            return Err(CodingSessionError::Input {
                message: format!(
                    "authorization waiter is no longer active: {}",
                    identity.authorization_id
                ),
            });
        }
        Ok(())
    }

    pub(crate) fn cancel_operation(&self, operation_id: &str, reason: &str) {
        let (entries, revision, pending) = {
            let mut state = self.state.lock().unwrap();
            let ids = state
                .pending
                .iter()
                .filter(|(_, entry)| entry.request.operation_id == operation_id)
                .map(|(id, _)| id.clone())
                .collect::<Vec<_>>();
            let entries = ids
                .into_iter()
                .filter_map(|id| state.pending.remove(&id))
                .collect::<Vec<_>>();
            if !entries.is_empty() {
                state.revision = state.revision.wrapping_add(1);
            }
            state
                .grants
                .retain(|grant| grant.operation_id != operation_id);
            let pending = pending_requests(&state);
            (entries, state.revision, pending)
        };
        self.sync_pending_snapshot(revision, pending);
        for entry in entries {
            if let Some(binding) = entry.filesystem_binding.as_ref() {
                binding.discard();
            }
            self.persist_resolution_or_diagnose(
                &entry,
                PersistedToolAuthorizationResolution::Cancelled {
                    reason: reason.to_owned(),
                },
            );
            self.event_service
                .emit_tool_authorization_cancelled(entry.request.clone(), reason);
            let _ = entry
                .sender
                .send(PendingResolution::Deny(reason.to_owned()));
        }
    }

    pub(crate) fn cancel_all(&self, reason: &str) {
        let (entries, revision) = {
            let mut state = self.state.lock().unwrap();
            let entries = std::mem::take(&mut state.pending)
                .into_values()
                .collect::<Vec<_>>();
            if !entries.is_empty() {
                state.revision = state.revision.wrapping_add(1);
            }
            state.grants.clear();
            (entries, state.revision)
        };
        self.sync_pending_snapshot(revision, Vec::new());
        for entry in entries {
            if let Some(binding) = entry.filesystem_binding.as_ref() {
                binding.discard();
            }
            self.persist_resolution_or_diagnose(
                &entry,
                PersistedToolAuthorizationResolution::Cancelled {
                    reason: reason.to_owned(),
                },
            );
            self.event_service
                .emit_tool_authorization_cancelled(entry.request.clone(), reason);
            let _ = entry
                .sender
                .send(PendingResolution::Deny(reason.to_owned()));
        }
    }

    fn remove_pending(&self, authorization_id: &str) -> Option<PendingAuthorization> {
        let (entry, revision, pending) = {
            let mut state = self.state.lock().unwrap();
            let entry = state.pending.remove(authorization_id);
            if entry.is_some() {
                state.revision = state.revision.wrapping_add(1);
            }
            let pending = pending_requests(&state);
            (entry, state.revision, pending)
        };
        self.sync_pending_snapshot(revision, pending);
        entry
    }

    fn restore_pending(&self, authorization_id: String, entry: PendingAuthorization) {
        let (revision, pending) = {
            let mut state = self.state.lock().unwrap();
            state.pending.insert(authorization_id, entry);
            state.revision = state.revision.wrapping_add(1);
            (state.revision, pending_requests(&state))
        };
        self.sync_pending_snapshot(revision, pending);
    }

    fn persist_resolution_or_diagnose(
        &self,
        entry: &PendingAuthorization,
        resolution: PersistedToolAuthorizationResolution,
    ) {
        if let Err(message) = persist_authorization_events(
            entry.event_writer.as_ref(),
            &entry.request,
            false,
            Some(resolution),
        ) {
            self.event_service.emit_diagnostic(
                Some(entry.request.operation_id.clone()),
                format!("tool authorization audit write failed: {message}"),
            );
        }
    }

    fn sync_pending_snapshot(&self, mut revision: u64, mut pending: Vec<ToolAuthorizationRequest>) {
        loop {
            self.coordinator.set_pending_authorizations(pending);
            let state = self.state.lock().unwrap();
            if state.revision == revision {
                return;
            }
            revision = state.revision;
            pending = pending_requests(&state);
        }
    }
}

fn persist_authorization_events(
    event_writer: Option<&SessionEventWriter>,
    request: &ToolAuthorizationRequest,
    include_request: bool,
    resolution: Option<PersistedToolAuthorizationResolution>,
) -> Result<(), String> {
    let Some(event_writer) = event_writer else {
        return Ok(());
    };
    let mut events = Vec::with_capacity(usize::from(resolution.is_some()) + 1);
    if include_request {
        events.push(SessionEventData::ToolAuthorizationRequested {
            request: request.clone(),
        });
    }
    if let Some(resolution) = resolution {
        events.push(SessionEventData::ToolAuthorizationResolved {
            authorization_id: request.authorization_id.clone(),
            resolution,
        });
    }
    event_writer
        .append(&request.operation_id, &request.turn_id, events)
        .map_err(|error| format!("failed to persist tool authorization fact: {error}"))
}

fn pending_requests(state: &AuthorizationState) -> Vec<ToolAuthorizationRequest> {
    let mut requests = state
        .pending
        .values()
        .map(|entry| entry.request.clone())
        .collect::<Vec<_>>();
    requests.sort_by(|left, right| {
        left.requested_at
            .cmp(&right.requested_at)
            .then_with(|| left.authorization_id.cmp(&right.authorization_id))
    });
    requests
}

enum Evaluation {
    Allow,
    Ask {
        risk: ToolAuthorizationRisk,
        scope: ToolAuthorizationScope,
        preview: ToolAuthorizationPreview,
    },
}

impl Evaluation {
    fn bind_filesystem_descriptor(&mut self, descriptor: &FilesystemBindingDescriptor) {
        let Self::Ask { scope, preview, .. } = self else {
            return;
        };
        let path = descriptor.display.to_string_lossy().into_owned();
        *scope = ToolAuthorizationScope::FilesystemTarget {
            path: path.clone(),
            target_fingerprint: descriptor.target_fingerprint.clone(),
        };
        preview.path = Some(path);
    }
}

async fn bind_filesystem_target(
    context: &BeforeToolCallContext,
    snapshot: &OperationCapabilitySnapshot,
) -> Result<Option<FilesystemBindingDescriptor>, String> {
    let path = match context.tool_name.as_str() {
        "read" | "grep" | "find" | "ls" => context
            .arguments
            .get("path")
            .and_then(Value::as_str)
            .unwrap_or("."),
        "write" | "edit" => context
            .arguments
            .get("path")
            .and_then(Value::as_str)
            .ok_or_else(|| "filesystem mutation is missing `path`".to_owned())?,
        _ => return Ok(None),
    };
    let filesystem = snapshot
        .filesystem
        .as_ref()
        .ok_or_else(|| "filesystem capability is not granted".to_owned())?;
    filesystem
        .bind_tool_target(
            &snapshot.operation_id,
            &context.tool_call_id,
            &context.tool_name,
            path,
        )
        .await
        .map(Some)
        .map_err(|error| error.to_string())
}

fn discard_filesystem_binding(
    context: &BeforeToolCallContext,
    snapshot: &OperationCapabilitySnapshot,
) {
    if let Some(filesystem) = snapshot.filesystem.as_ref() {
        filesystem.discard_bound_tool_target(&snapshot.operation_id, &context.tool_call_id);
    }
}

fn evaluate(
    context: &BeforeToolCallContext,
    snapshot: &OperationCapabilitySnapshot,
    inventory: &ToolAuthorizationInventory,
) -> Result<Evaluation, String> {
    match context.tool_name.as_str() {
        "read" | "grep" | "find" | "ls" => {
            let Some(filesystem) = snapshot.filesystem.as_ref() else {
                return Err("filesystem capability is not granted".into());
            };
            let path = context
                .arguments
                .get("path")
                .and_then(Value::as_str)
                .unwrap_or(".");
            let preview = filesystem
                .preview_path(path)
                .map_err(|error| error.to_string())?;
            if preview.workspace_local {
                Ok(Evaluation::Allow)
            } else {
                Ok(path_request(
                    ToolAuthorizationRisk::ExternalRead,
                    preview.display,
                    "Read outside the workspace",
                ))
            }
        }
        "write" | "edit" => {
            let Some(filesystem) = snapshot.filesystem.as_ref() else {
                return Err("filesystem capability is not granted".into());
            };
            let path = context
                .arguments
                .get("path")
                .and_then(Value::as_str)
                .ok_or_else(|| "filesystem mutation is missing `path`".to_owned())?;
            let target = filesystem
                .preview_path(path)
                .map_err(|error| error.to_string())?;
            Ok(path_request_with_content(
                ToolAuthorizationRisk::FilesystemMutation,
                target.display,
                "Modify a file",
                mutation_content_preview(context),
            ))
        }
        "bash" => {
            let Some(shell) = snapshot.shell.as_ref() else {
                return Err("shell capability is not granted".into());
            };
            let command = context
                .arguments
                .get("command")
                .and_then(Value::as_str)
                .ok_or_else(|| "shell invocation is missing `command`".to_owned())?;
            let redacted = redact_command(command);
            Ok(Evaluation::Ask {
                risk: ToolAuthorizationRisk::ShellExecution,
                scope: ToolAuthorizationScope::Shell {
                    cwd: shell.cwd.to_string_lossy().into_owned(),
                    command_fingerprint: fingerprint(command.as_bytes()),
                },
                preview: ToolAuthorizationPreview {
                    summary: "Execute a shell command".into(),
                    path: None,
                    command: Some(redacted),
                    cwd: Some(shell.cwd.to_string_lossy().into_owned()),
                    content_preview: None,
                },
            })
        }
        "delegate_agent" | "delegate_team" => {
            match inventory
                .explicit_tools
                .get(context.tool_name.as_str())
                .copied()
                .flatten()
            {
                Some(DeclaredToolAuthorizationRisk::SideEffect) => Ok(argument_request(
                    context,
                    ToolAuthorizationRisk::DeclaredSideEffect,
                    "Delegate work to a child agent",
                )),
                _ => Ok(Evaluation::Allow),
            }
        }
        name if inventory.explicit_tools.contains_key(name) => {
            match inventory.explicit_tools.get(name).copied().flatten() {
                Some(DeclaredToolAuthorizationRisk::WorkspaceLocalReadOnly) => {
                    Ok(Evaluation::Allow)
                }
                Some(DeclaredToolAuthorizationRisk::SideEffect) | None => Ok(argument_request(
                    context,
                    ToolAuthorizationRisk::DeclaredSideEffect,
                    "Run a custom tool",
                )),
            }
        }
        _ => Ok(argument_request(
            context,
            ToolAuthorizationRisk::Unknown,
            "Run a tool without risk metadata",
        )),
    }
}

fn path_request(risk: ToolAuthorizationRisk, path: PathBuf, summary: &str) -> Evaluation {
    path_request_with_content(risk, path, summary, None)
}

fn path_request_with_content(
    risk: ToolAuthorizationRisk,
    path: PathBuf,
    summary: &str,
    content_preview: Option<String>,
) -> Evaluation {
    let path = path.to_string_lossy().into_owned();
    Evaluation::Ask {
        risk,
        scope: ToolAuthorizationScope::Path { path: path.clone() },
        preview: ToolAuthorizationPreview {
            summary: summary.into(),
            path: Some(path),
            command: None,
            cwd: None,
            content_preview,
        },
    }
}

fn argument_request(
    context: &BeforeToolCallContext,
    risk: ToolAuthorizationRisk,
    summary: &str,
) -> Evaluation {
    Evaluation::Ask {
        risk,
        scope: ToolAuthorizationScope::ToolArguments {
            fingerprint: argument_fingerprint(&context.arguments),
        },
        preview: ToolAuthorizationPreview {
            summary: format!("{summary}: {}", context.tool_name),
            path: None,
            command: None,
            cwd: None,
            content_preview: None,
        },
    }
}

fn blocked(reason: impl Into<String>) -> BeforeToolCallResult {
    BeforeToolCallResult {
        block: true,
        reason: Some(reason.into()),
    }
}

fn delegation_request(
    context: &BeforeToolCallContext,
    turn_id: &str,
    snapshot: &OperationCapabilitySnapshot,
) -> Option<DelegationRequest> {
    let (target_kind, target_field) = match context.tool_name.as_str() {
        "delegate_agent" => (ProfileKind::Agent, "agent_id"),
        "delegate_team" => (ProfileKind::Team, "team_id"),
        _ => return None,
    };
    let operation_id = context.execution_context.scope_id()?.to_owned();
    let requesting_profile_id = snapshot.model.as_ref()?.profile_id.clone()?;
    let target_id =
        ProfileId::new(context.arguments.get(target_field)?.as_str()?.to_owned()).ok()?;
    let task = context.arguments.get("task")?.as_str()?.trim().to_owned();
    if task.is_empty() {
        return None;
    }
    Some(DelegationRequest {
        operation_id,
        turn_id: turn_id.to_owned(),
        tool_call_id: context.tool_call_id.clone(),
        requesting_profile_id,
        target_kind,
        target_id,
        task,
    })
}

fn delegation_rejected_result(request: &DelegationRequest, reason: &str) -> String {
    let mut result =
        DelegationToolResult::from_request(request, DelegationToolResultStatus::Rejected);
    result.error = Some(reason.to_owned());
    result.to_json()
}

fn argument_fingerprint(arguments: &Value) -> String {
    fingerprint(canonical_json(arguments).as_bytes())
}

fn fingerprint(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn canonical_json(value: &Value) -> String {
    match value {
        Value::Object(values) => {
            let mut fields = values.iter().collect::<Vec<_>>();
            fields.sort_by_key(|(name, _)| *name);
            let fields = fields
                .into_iter()
                .map(|(key, value)| {
                    format!(
                        "{}:{}",
                        serde_json::to_string(key).unwrap(),
                        canonical_json(value)
                    )
                })
                .collect::<Vec<_>>()
                .join(",");
            format!("{{{fields}}}")
        }
        Value::Array(values) => format!(
            "[{}]",
            values
                .iter()
                .map(canonical_json)
                .collect::<Vec<_>>()
                .join(",")
        ),
        _ => serde_json::to_string(value).unwrap(),
    }
}

fn redact_command(command: &str) -> String {
    crate::redaction::redact_sensitive_text(command)
}

fn mutation_content_preview(context: &BeforeToolCallContext) -> Option<String> {
    let raw = if context.tool_name == "write" {
        context.arguments.get("content")?.as_str()?.to_owned()
    } else {
        context
            .arguments
            .get("edits")?
            .as_array()?
            .iter()
            .take(4)
            .flat_map(|edit| {
                let old = edit.get("oldText").and_then(Value::as_str).unwrap_or("");
                let new = edit.get("newText").and_then(Value::as_str).unwrap_or("");
                old.lines()
                    .take(3)
                    .map(|line| format!("- {line}"))
                    .chain(new.lines().take(3).map(|line| format!("+ {line}")))
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    };
    let bounded = raw.lines().take(12).collect::<Vec<_>>().join("\n");
    let bounded = bounded.chars().take(1_200).collect::<String>();
    (!bounded.is_empty()).then(|| crate::redaction::redact_sensitive_text(&bounded))
}
