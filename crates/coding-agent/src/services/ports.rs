use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

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

/// Extension host 端口：application 层持有可选的 extension-host 实例视图。
///
/// ARC-700（`extension-host` crate）只提供治理机制；ARC-710 接线前产品
/// 通过 [`NoopExtensionHostPort`] 保持「无 host」状态，不改变现有产品行为。
/// 端口不直接依赖 `extension-host` 类型（`ExtensionHostView` 是自包含的
/// 最小只读面），接线时由适配器实现。
pub(crate) trait ExtensionHostPort: std::fmt::Debug + Send + Sync {
    /// 当前会话绑定的 extension host 视图；`None` 表示未接线。
    fn extension_host(&self) -> Option<Arc<dyn ExtensionHostView>>;
}

/// 产品对 extension host 的最小只读视图（骨架阶段）。
///
/// ARC-710 接线时扩展查询能力（事件订阅、trust 放行、诊断读取等）。
pub(crate) trait ExtensionHostView: Send + Sync {
    /// session 关闭通知：extension host 按确定性顺序 shutdown。
    fn notify_shutdown(&self, reason: &str);
}

/// 空实现：产品当前未接线 extension host（ARC-710 前保持）。
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct NoopExtensionHostPort;

impl ExtensionHostPort for NoopExtensionHostPort {
    fn extension_host(&self) -> Option<Arc<dyn ExtensionHostView>> {
        None
    }
}

/// 持有 extension host 端口的轻量服务，挂在 [`crate::runtime::owners::RuntimeHost`]。
#[derive(Debug, Clone)]
pub(crate) struct ExtensionHostService {
    port: Arc<dyn ExtensionHostPort>,
}

impl ExtensionHostService {
    pub(crate) fn new(port: Arc<dyn ExtensionHostPort>) -> Self {
        Self { port }
    }

    /// 生命周期通知：session 关闭时通知 extension host 按确定性顺序关闭
    /// （ARC-710 接线后由 host 完成 shutdown + join；当前 Noop 下为 no-op）。
    pub(crate) fn notify_shutdown(&self, reason: &str) {
        if let Some(host) = self.port.extension_host() {
            host.notify_shutdown(reason);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn noop_port_reports_no_host() {
        assert!(NoopExtensionHostPort.extension_host().is_none());
    }

    #[test]
    fn noop_service_notify_is_a_noop() {
        let service = ExtensionHostService::new(Arc::new(NoopExtensionHostPort));
        service.notify_shutdown("test"); // 无 host：no-op，不 panic。
    }
}
