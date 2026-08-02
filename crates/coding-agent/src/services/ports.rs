use std::future::Future;
use std::pin::Pin;

use crate::application::snapshot::SnapshotCoordinator;
use crate::authorization::{ToolAuthorizationDecision, ToolAuthorizationRequest};
use crate::events::ProductEvent;
use crate::kernel::capability::CapabilityGeneration;
use crate::kernel::error::CodingSessionError;
use crate::operations::prompt::context::DelegationRequest;
use crate::services::event::EventService;
use crate::session::event::SessionEventData;
use crate::session::service::SessionEventWriter;

pub(crate) type SessionWriterPort = std::sync::Arc<dyn SessionWriter>;

pub(crate) trait SessionWriter: std::fmt::Debug + Send + Sync {
    fn append<'a>(
        &'a self,
        operation_id: &'a str,
        turn_id: &'a str,
        events: Vec<SessionEventData>,
    ) -> Pin<Box<dyn Future<Output = Result<(), CodingSessionError>> + Send + 'a>>;

    fn append_blocking(
        &self,
        operation_id: &str,
        turn_id: &str,
        events: Vec<SessionEventData>,
    ) -> Result<(), CodingSessionError>;
}

impl SessionWriter for SessionEventWriter {
    fn append<'a>(
        &'a self,
        operation_id: &'a str,
        turn_id: &'a str,
        events: Vec<SessionEventData>,
    ) -> Pin<Box<dyn Future<Output = Result<(), CodingSessionError>> + Send + 'a>> {
        Box::pin(SessionEventWriter::append(
            self,
            operation_id,
            turn_id,
            events,
        ))
    }

    fn append_blocking(
        &self,
        operation_id: &str,
        turn_id: &str,
        events: Vec<SessionEventData>,
    ) -> Result<(), CodingSessionError> {
        SessionEventWriter::append_blocking(self, operation_id, turn_id, events)
    }
}

pub(crate) trait EventSink: Send + Sync {
    fn diagnostic(
        &self,
        operation_id: Option<String>,
        message: String,
    ) -> Result<(), CodingSessionError>;

    fn tool_authorization_required(
        &self,
        request: ToolAuthorizationRequest,
    ) -> Result<(), CodingSessionError>;

    fn tool_authorization_approved(
        &self,
        request: ToolAuthorizationRequest,
        decision: ToolAuthorizationDecision,
    ) -> Result<(), CodingSessionError>;

    fn tool_authorization_denied(
        &self,
        request: ToolAuthorizationRequest,
        reason: String,
    ) -> Result<(), CodingSessionError>;

    fn tool_authorization_cancelled(
        &self,
        request: ToolAuthorizationRequest,
        reason: String,
    ) -> Result<(), CodingSessionError>;

    fn delegation_rejected(
        &self,
        request: &DelegationRequest,
        reason: &str,
    ) -> Result<(), CodingSessionError>;
}

impl EventSink for EventService {
    fn diagnostic(
        &self,
        operation_id: Option<String>,
        message: String,
    ) -> Result<(), CodingSessionError> {
        EventService::emit_diagnostic(self, operation_id, message).map(drop_product_event)
    }

    fn tool_authorization_required(
        &self,
        request: ToolAuthorizationRequest,
    ) -> Result<(), CodingSessionError> {
        EventService::emit_tool_authorization_required(self, request).map(drop_product_event)
    }

    fn tool_authorization_approved(
        &self,
        request: ToolAuthorizationRequest,
        decision: ToolAuthorizationDecision,
    ) -> Result<(), CodingSessionError> {
        EventService::emit_tool_authorization_approved(self, request, decision)
            .map(drop_product_event)
    }

    fn tool_authorization_denied(
        &self,
        request: ToolAuthorizationRequest,
        reason: String,
    ) -> Result<(), CodingSessionError> {
        EventService::emit_tool_authorization_denied(self, request, reason).map(drop_product_event)
    }

    fn tool_authorization_cancelled(
        &self,
        request: ToolAuthorizationRequest,
        reason: String,
    ) -> Result<(), CodingSessionError> {
        EventService::emit_tool_authorization_cancelled(self, request, reason)
            .map(drop_product_event)
    }

    fn delegation_rejected(
        &self,
        request: &DelegationRequest,
        reason: &str,
    ) -> Result<(), CodingSessionError> {
        EventService::emit_delegation_rejected(self, request, reason).map(drop_product_event)
    }
}

fn drop_product_event(_: ProductEvent) {}

pub(crate) trait CapabilityTransitionLease {}

impl CapabilityTransitionLease for std::sync::MutexGuard<'_, ()> {}

pub(crate) trait CapabilityQuery: Send + Sync {
    fn acquire_transition(
        &self,
    ) -> Result<Box<dyn CapabilityTransitionLease + '_>, CodingSessionError>;

    fn current_generation(&self) -> Result<CapabilityGeneration, CodingSessionError>;

    fn set_pending_authorizations(
        &self,
        pending: Vec<ToolAuthorizationRequest>,
    ) -> Result<(), CodingSessionError>;
}

impl CapabilityQuery for SnapshotCoordinator {
    fn acquire_transition(
        &self,
    ) -> Result<Box<dyn CapabilityTransitionLease + '_>, CodingSessionError> {
        SnapshotCoordinator::capability_transition_guard(self)
            .map(|guard| Box::new(guard) as Box<dyn CapabilityTransitionLease + '_>)
    }

    fn current_generation(&self) -> Result<CapabilityGeneration, CodingSessionError> {
        SnapshotCoordinator::current_capability_generation(self)
    }

    fn set_pending_authorizations(
        &self,
        pending: Vec<ToolAuthorizationRequest>,
    ) -> Result<(), CodingSessionError> {
        SnapshotCoordinator::set_pending_authorizations(self, pending)
    }
}
