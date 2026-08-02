use crate::application::capability::OperationCapabilitySnapshot;
use crate::application::snapshot::SnapshotCoordinator;
use crate::authorization::{
    ToolAuthorizationDecision, ToolAuthorizationMode, ToolAuthorizationPreview,
    ToolAuthorizationRequest, ToolAuthorizationRisk, ToolAuthorizationScope,
};
use crate::kernel::error::CodingSessionError;
use crate::mutex::{MutexExt, report_infallible_resource_error};
use crate::operations::delegation::{DelegationToolResult, DelegationToolResultStatus};
use crate::operations::prompt::context::DelegationRequest;
use crate::platform::fs::capability::{FilesystemBindingDescriptor, FilesystemCapability};
use crate::profiles::{ProfileId, ProfileKind};
use crate::services::event::EventService;
use crate::services::ports::{CapabilityQuery, EventSink, SessionWriterPort};
use crate::session::event::{PersistedToolAuthorizationResolution, SessionEventData};
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
    pub(crate) event_writer: Option<SessionWriterPort>,
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
    event_writer: Option<SessionWriterPort>,
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
    capabilities: Arc<dyn CapabilityQuery>,
    events: Arc<dyn EventSink>,
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
        self.service.persist_resolution_or_diagnose_blocking(
            &entry,
            PersistedToolAuthorizationResolution::Cancelled {
                reason: reason.into(),
            },
        );
        report_infallible_resource_error(
            "authorization waiter Drop event",
            self.service
                .events
                .tool_authorization_cancelled(entry.request, reason.into()),
        );
    }
}

impl std::fmt::Debug for AuthorizationService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AuthorizationService")
            .field("mode", &self.mode)
            .field(
                "pending",
                &self
                    .state
                    .lock_or_recover("authorization state")
                    .pending
                    .len(),
            )
            .finish()
    }
}

impl AuthorizationService {
    pub(crate) fn new(
        mode: ToolAuthorizationMode,
        coordinator: Arc<SnapshotCoordinator>,
        event_service: EventService,
    ) -> Self {
        Self::with_ports(mode, coordinator, Arc::new(event_service))
    }

    fn with_ports(
        mode: ToolAuthorizationMode,
        capabilities: Arc<dyn CapabilityQuery>,
        events: Arc<dyn EventSink>,
    ) -> Self {
        Self {
            mode,
            capabilities,
            events,
            state: Arc::new(Mutex::new(AuthorizationState::default())),
        }
    }

    pub(crate) fn pending(&self) -> Result<Vec<ToolAuthorizationRequest>, CodingSessionError> {
        let state = self.state.lock_resource("authorization state")?;
        Ok(pending_requests(&state))
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
        event_writer: Option<SessionWriterPort>,
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
        let filesystem_binding = match bind_filesystem_target(&context, &snapshot).await {
            Ok(binding) => binding,
            Err(error) => {
                self.events
                    .diagnostic(
                        Some(operation_id.clone()),
                        format!("filesystem capability binding failed: {error}"),
                    )
                    .map_err(|emit_error| emit_error.to_string())?;
                return Err(error);
            }
        };
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
        if self
            .state
            .lock_resource("authorization state")
            .map_err(|error| error.to_string())?
            .grants
            .contains(&grant)
        {
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
                )
                .await?;
                self.events
                    .tool_authorization_required(request.clone())
                    .map_err(|error| error.to_string())?;
                self.events
                    .tool_authorization_denied(request, reason.into())
                    .map_err(|error| error.to_string())?;
                return Ok(Some(blocked(reason)));
            }
            ToolAuthorizationMode::Interactive => {}
        }
        if let Err(error) =
            persist_authorization_events(event_writer.as_ref(), &request, true, None).await
        {
            discard_filesystem_binding(&context, &snapshot);
            return Err(error);
        }
        let (sender, mut receiver) = oneshot::channel();
        let (revision, pending) = {
            let mut state = self
                .state
                .lock_resource("authorization state")
                .map_err(|error| error.to_string())?;
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
        self.sync_pending_snapshot(revision, pending)
            .map_err(|error| error.to_string())?;
        let identity = request.identity();
        self.events
            .tool_authorization_required(request)
            .map_err(|error| error.to_string())?;
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
                    self.persist_resolution_or_diagnose_async(
                        &entry,
                        PersistedToolAuthorizationResolution::Cancelled {
                            reason: "tool authorization was cancelled".into(),
                        },
                    )
                    .await;
                    self.events
                        .tool_authorization_cancelled(
                            entry.request,
                            "tool authorization was cancelled".into(),
                        )
                        .map_err(|error| error.to_string())?;
                }
                return Ok(Some(blocked("tool authorization was cancelled")));
            }
            _ = tokio::time::sleep(TOOL_AUTHORIZATION_RESPONSE_TIMEOUT) => {
                match self
                    .decide(
                        &identity,
                        ToolAuthorizationDecision::Deny {
                            reason: Some(TOOL_AUTHORIZATION_TIMEOUT_REASON.into()),
                        },
                    )
                    .await
                {
                    Ok(()) | Err(CodingSessionError::Input { .. }) => receiver.await,
                    Err(error) => return Err(error.to_string()),
                }
            }
        };
        match resolution {
            Ok(PendingResolution::Allow) => Ok(None),
            Ok(PendingResolution::Deny(reason)) => {
                if let Some(request) = delegation_request(&context, &turn_id, &snapshot) {
                    self.events
                        .delegation_rejected(&request, &reason)
                        .map_err(|error| error.to_string())?;
                    Ok(Some(blocked(delegation_rejected_result(&request, &reason))))
                } else {
                    Ok(Some(blocked(reason)))
                }
            }
            Err(_) => Ok(Some(blocked("tool authorization was interrupted"))),
        }
    }

    pub(crate) async fn decide(
        &self,
        identity: &crate::authorization::ToolAuthorizationIdentity,
        decision: ToolAuthorizationDecision,
    ) -> Result<(), CodingSessionError> {
        let capability_transition = self.capabilities.acquire_transition()?;
        let current_generation = self.capabilities.current_generation()?.get();
        let (entry, revision, pending) = {
            let mut state = self.state.lock_resource("authorization state")?;
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
        self.sync_pending_snapshot(revision, pending)?;
        drop(capability_transition);

        if entry.request.capability_generation != current_generation {
            let reason = "tool authorization capability generation is stale";
            if let Some(binding) = entry.filesystem_binding.as_ref() {
                binding.discard();
            }
            self.persist_resolution_or_diagnose_async(
                &entry,
                PersistedToolAuthorizationResolution::Cancelled {
                    reason: reason.into(),
                },
            )
            .await;
            self.events
                .tool_authorization_cancelled(entry.request.clone(), reason.into())?;
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
        )
        .await
        {
            self.restore_pending(identity.authorization_id.clone(), entry)?;
            return Err(CodingSessionError::Session { message });
        }

        let capability_transition = self.capabilities.acquire_transition()?;
        if entry.request.capability_generation != self.capabilities.current_generation()?.get() {
            drop(capability_transition);
            let reason = "tool authorization capability generation changed while persisting";
            if let Some(binding) = entry.filesystem_binding.as_ref() {
                binding.discard();
            }
            self.persist_resolution_or_diagnose_async(
                &entry,
                PersistedToolAuthorizationResolution::Cancelled {
                    reason: reason.into(),
                },
            )
            .await;
            self.events
                .tool_authorization_cancelled(entry.request.clone(), reason.into())?;
            let _ = entry.sender.send(PendingResolution::Deny(reason.into()));
            return Err(CodingSessionError::Input {
                message: reason.into(),
            });
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
            self.state
                .lock_resource("authorization state")?
                .grants
                .insert(grant);
        }
        match &resolution {
            PendingResolution::Allow => {
                self.events
                    .tool_authorization_approved(entry.request.clone(), decision)?;
            }
            PendingResolution::Deny(reason) => {
                self.events
                    .tool_authorization_denied(entry.request.clone(), reason.clone())?;
            }
        }
        if entry.sender.send(resolution).is_err() {
            if let Some(binding) = entry.filesystem_binding.as_ref() {
                binding.discard();
            }
            if let Some(grant) = operation_grant {
                self.state
                    .lock_resource("authorization state")?
                    .grants
                    .remove(&grant);
            }
            self.events.tool_authorization_cancelled(
                entry.request,
                "authorization waiter is no longer active".into(),
            )?;
            return Err(CodingSessionError::Input {
                message: format!(
                    "authorization waiter is no longer active: {}",
                    identity.authorization_id
                ),
            });
        }
        Ok(())
    }

    pub(crate) async fn cancel_operation(
        &self,
        operation_id: &str,
        reason: &str,
    ) -> Result<(), CodingSessionError> {
        let (entries, revision, pending) = {
            let mut state = self.state.lock_resource("authorization state")?;
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
        self.sync_pending_snapshot(revision, pending)?;
        for entry in entries {
            if let Some(binding) = entry.filesystem_binding.as_ref() {
                binding.discard();
            }
            self.persist_resolution_or_diagnose_async(
                &entry,
                PersistedToolAuthorizationResolution::Cancelled {
                    reason: reason.to_owned(),
                },
            )
            .await;
            self.events
                .tool_authorization_cancelled(entry.request.clone(), reason.into())?;
            let _ = entry
                .sender
                .send(PendingResolution::Deny(reason.to_owned()));
        }
        Ok(())
    }

    pub(crate) fn cancel_operation_blocking(
        &self,
        operation_id: &str,
        reason: &str,
    ) -> Result<(), CodingSessionError> {
        let (entries, revision, pending) = {
            let mut state = self.state.lock_resource("authorization state")?;
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
        self.sync_pending_snapshot(revision, pending)?;
        for entry in entries {
            if let Some(binding) = entry.filesystem_binding.as_ref() {
                binding.discard();
            }
            self.persist_resolution_or_diagnose_blocking(
                &entry,
                PersistedToolAuthorizationResolution::Cancelled {
                    reason: reason.to_owned(),
                },
            );
            self.events
                .tool_authorization_cancelled(entry.request.clone(), reason.into())?;
            let _ = entry
                .sender
                .send(PendingResolution::Deny(reason.to_owned()));
        }
        Ok(())
    }

    pub(crate) fn cancel_all(&self, reason: &str) -> Result<(), CodingSessionError> {
        let (entries, revision) = {
            let mut state = self.state.lock_resource("authorization state")?;
            let entries = std::mem::take(&mut state.pending)
                .into_values()
                .collect::<Vec<_>>();
            if !entries.is_empty() {
                state.revision = state.revision.wrapping_add(1);
            }
            state.grants.clear();
            (entries, state.revision)
        };
        self.sync_pending_snapshot(revision, Vec::new())?;
        for entry in entries {
            if let Some(binding) = entry.filesystem_binding.as_ref() {
                binding.discard();
            }
            self.persist_resolution_or_diagnose_blocking(
                &entry,
                PersistedToolAuthorizationResolution::Cancelled {
                    reason: reason.to_owned(),
                },
            );
            self.events
                .tool_authorization_cancelled(entry.request.clone(), reason.into())?;
            let _ = entry
                .sender
                .send(PendingResolution::Deny(reason.to_owned()));
        }
        Ok(())
    }

    fn remove_pending(&self, authorization_id: &str) -> Option<PendingAuthorization> {
        let (entry, revision, pending) = {
            // Called by AuthorizationWaiterGuard::drop. Recover only to remove
            // the abandoned waiter and report poison once.
            let mut state = self.state.lock_or_recover("authorization state");
            let entry = state.pending.remove(authorization_id);
            if entry.is_some() {
                state.revision = state.revision.wrapping_add(1);
            }
            let pending = pending_requests(&state);
            (entry, state.revision, pending)
        };
        if let Err(error) = self.sync_pending_snapshot(revision, pending) {
            report_infallible_resource_error(
                "authorization Drop cleanup diagnostic",
                self.events.diagnostic(
                    None::<String>,
                    format!("authorization cleanup could not refresh pending state: {error}"),
                ),
            );
        }
        entry
    }

    fn restore_pending(
        &self,
        authorization_id: String,
        entry: PendingAuthorization,
    ) -> Result<(), CodingSessionError> {
        let (revision, pending) = {
            let mut state = self.state.lock_resource("authorization state")?;
            state.pending.insert(authorization_id, entry);
            state.revision = state.revision.wrapping_add(1);
            (state.revision, pending_requests(&state))
        };
        self.sync_pending_snapshot(revision, pending)
    }

    fn persist_resolution_or_diagnose_blocking(
        &self,
        entry: &PendingAuthorization,
        resolution: PersistedToolAuthorizationResolution,
    ) {
        if let Err(message) = persist_authorization_events_blocking(
            entry.event_writer.as_ref(),
            &entry.request,
            false,
            Some(resolution),
        ) {
            report_infallible_resource_error(
                "authorization blocking audit diagnostic",
                self.events.diagnostic(
                    Some(entry.request.operation_id.clone()),
                    format!("tool authorization audit write failed: {message}"),
                ),
            );
        }
    }

    async fn persist_resolution_or_diagnose_async(
        &self,
        entry: &PendingAuthorization,
        resolution: PersistedToolAuthorizationResolution,
    ) {
        if let Err(message) = persist_authorization_events(
            entry.event_writer.as_ref(),
            &entry.request,
            false,
            Some(resolution),
        )
        .await
        {
            report_infallible_resource_error(
                "authorization audit diagnostic",
                self.events.diagnostic(
                    Some(entry.request.operation_id.clone()),
                    format!("tool authorization audit write failed: {message}"),
                ),
            );
        }
    }

    fn sync_pending_snapshot(
        &self,
        mut revision: u64,
        mut pending: Vec<ToolAuthorizationRequest>,
    ) -> Result<(), CodingSessionError> {
        loop {
            self.capabilities.set_pending_authorizations(pending)?;
            let state = self.state.lock_resource("authorization state")?;
            if state.revision == revision {
                return Ok(());
            }
            revision = state.revision;
            pending = pending_requests(&state);
        }
    }
}

async fn persist_authorization_events(
    event_writer: Option<&SessionWriterPort>,
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
        .await
        .map_err(|error| format!("failed to persist tool authorization fact: {error}"))
}

fn persist_authorization_events_blocking(
    event_writer: Option<&SessionWriterPort>,
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
        .append_blocking(&request.operation_id, &request.turn_id, events)
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

mod evaluation;

#[cfg(test)]
mod tests;

use evaluation::*;
