use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use extension_host::api::{
    EnableRequest, ExtensionError, ExtensionEvent, ExtensionEventKind, ExtensionEventPayload,
    HookGate,
};

use crate::application::snapshot::SnapshotCoordinator;
use crate::authorization::{ToolAuthorizationDecision, ToolAuthorizationRequest};
use crate::events::ProductEvent;
use crate::kernel::capability::CapabilityGeneration;
use crate::kernel::error::CodingSessionError;
use crate::mutex::MutexExt;
use crate::operations::prompt::context::DelegationRequest;
use crate::platform::time::Clock;
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
/// ARC-710 接线：产品默认通过 [`NoopExtensionHostPort`] 保持「无 host」
/// 状态（CLI/Desktop 未接线，行为不变）；[`LiveExtensionHostPort`] 是
/// 真实 host 适配器（`CodingAgentSessionOptions::with_extension_host_options`
/// 启用），把 user hooks 事件提交到 host、暴露 Tool/Stop gate 评估入口。
/// 端口不直接依赖 extension-host 的具体 host 类型（`ExtensionHostView` 是
/// 自包含的只读面，由适配器实现）。
pub(crate) trait ExtensionHostPort: std::fmt::Debug + Send + Sync {
    /// 当前会话绑定的 extension host 视图；`None` 表示未接线。
    fn extension_host(&self) -> Option<Arc<dyn ExtensionHostView>>;
}

/// 产品对 extension host 的只读视图（ARC-710 真实接口）。
pub(crate) trait ExtensionHostView: Send + Sync {
    /// 提交一个事件给 host dispatch（Observe 事件派发 + gate 事件记账）。
    fn submit(&self, event: ExtensionEvent) -> Result<(), ExtensionError>;

    /// session 关闭通知：extension host 按确定性顺序 shutdown。
    fn notify_shutdown(&self, reason: &str);

    /// 等待 host dispatch task 结束（shutdown 后回收）。
    fn join_shutdown(&self) -> Pin<Box<dyn Future<Output = ()> + Send>>;

    /// 首次启用请求（等待产品放行；来源 + 能力已在此展示）。
    fn first_enables(&self) -> Vec<EnableRequest>;

    /// Tool/Stop gate 评估入口（有启用扩展时返回 `Some`）。
    fn gate(&self) -> Option<Arc<HookGate>>;
}

/// 事件提交抽象：产品侧发出 user hooks 事件的统一入口（无 host 时
/// no-op，行为不变）。
pub(crate) trait ExtensionEventSink: std::fmt::Debug + Send + Sync {
    fn submit(
        &self,
        kind: ExtensionEventKind,
        session_id: &str,
        workspace_root: &str,
        payload: ExtensionEventPayload,
    );

    /// Tool/Stop gate 评估入口（无 host 时 `None`）。
    fn hook_gate(&self) -> Option<Arc<HookGate>>;
}

/// 空实现：产品当前未接线 extension host（ARC-710 前保持；测试默认）。
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct NoopExtensionHostPort;

impl ExtensionHostPort for NoopExtensionHostPort {
    fn extension_host(&self) -> Option<Arc<dyn ExtensionHostView>> {
        None
    }
}

/// 空事件 sink：无 host 时提交为 no-op（行为不变）。
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct NoopExtensionEventSink;

impl ExtensionEventSink for NoopExtensionEventSink {
    fn submit(
        &self,
        _kind: ExtensionEventKind,
        _session_id: &str,
        _workspace_root: &str,
        _payload: ExtensionEventPayload,
    ) {
    }

    fn hook_gate(&self) -> Option<Arc<HookGate>> {
        None
    }
}

/// extension 事件提交句柄：sink + 会话身份（session id / workspace root）。
///
/// ARC-730 接线（subagent / compaction）把 sink 与事件信封所需的会话身份
/// 一起穿透操作层（纯函数式服务组合多层签名只加这一个参数）；`sink` 为
/// `None`（无 host）时 `submit` 是 no-op，既有行为不变。
#[derive(Debug, Clone)]
pub(crate) struct ExtensionEventDispatch {
    sink: Option<Arc<dyn ExtensionEventSink>>,
    session_id: String,
    workspace_root: String,
}

impl ExtensionEventDispatch {
    pub(crate) fn from_parts(
        sink: Option<Arc<dyn ExtensionEventSink>>,
        session_id: impl Into<String>,
        workspace_root: impl Into<String>,
    ) -> Self {
        Self {
            sink,
            session_id: session_id.into(),
            workspace_root: workspace_root.into(),
        }
    }

    /// 无 host 的占位（`submit` no-op）。
    pub(crate) fn none() -> Self {
        Self {
            sink: None,
            session_id: String::new(),
            workspace_root: String::new(),
        }
    }

    pub(crate) fn submit(&self, kind: ExtensionEventKind, payload: ExtensionEventPayload) {
        if let Some(sink) = &self.sink {
            sink.submit(kind, &self.session_id, &self.workspace_root, payload);
        }
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

    /// 事件提交入口（产品各接线点使用）。无 host 时 no-op。
    pub(crate) fn submit_event(
        &self,
        kind: ExtensionEventKind,
        session_id: &str,
        workspace_root: &str,
        payload: ExtensionEventPayload,
    ) {
        if let Some(host) = self.port.extension_host() {
            let event = ExtensionEvent::new(
                kind,
                session_id,
                workspace_root,
                crate::platform::time::SystemClock.now_rfc3339(),
                payload,
            );
            if let Err(error) = host.submit(event) {
                let _ = error; // host 拒绝（未运行/关闭中）不影响产品行为。
            }
        }
    }

    /// 会话级事件提交器的 trait 对象（穿透到 operation 层）。
    pub(crate) fn sink(&self) -> Arc<dyn ExtensionEventSink> {
        if self.port.extension_host().is_some() {
            Arc::new(LiveExtensionEventSink {
                service: self.clone(),
            })
        } else {
            Arc::new(NoopExtensionEventSink)
        }
    }

    /// Tool/Stop gate 评估入口（无 host 时 `None`）。
    pub(crate) fn gate(&self) -> Option<Arc<HookGate>> {
        self.port.extension_host().and_then(|host| host.gate())
    }

    /// 首次启用请求（无 host 时空）。
    pub(crate) fn first_enables(&self) -> Vec<EnableRequest> {
        self.port
            .extension_host()
            .map(|host| host.first_enables())
            .unwrap_or_default()
    }

    /// 生命周期通知：session 关闭时通知 extension host 按确定性顺序关闭
    /// （ARC-710 接线后由 host 完成 shutdown + join；当前 Noop 下为 no-op）。
    pub(crate) fn notify_shutdown(&self, reason: &str) {
        if let Some(host) = self.port.extension_host() {
            host.notify_shutdown(reason);
        }
    }

    /// 等待 host 退出（shutdown 后回收 dispatch task）。
    pub(crate) async fn join_shutdown(&self) {
        if let Some(host) = self.port.extension_host() {
            host.join_shutdown().await;
        }
    }
}

/// 转发到 host 的事件 sink（host 存活时使用）。
#[derive(Debug, Clone)]
struct LiveExtensionEventSink {
    service: ExtensionHostService,
}

impl ExtensionEventSink for LiveExtensionEventSink {
    fn submit(
        &self,
        kind: ExtensionEventKind,
        session_id: &str,
        workspace_root: &str,
        payload: ExtensionEventPayload,
    ) {
        self.service
            .submit_event(kind, session_id, workspace_root, payload);
    }

    fn hook_gate(&self) -> Option<Arc<HookGate>> {
        self.service.gate()
    }
}

/// 真实 host 适配器：把扩展目录装配成运行中的 extension host。
///
/// host 由 [`ExtensionHost::new`] + [`ExtensionHost::start`] 驱动；首次
/// 启用请求（folder trust 未决定）经 [`ExtensionHostService::first_enables`]
/// 暴露给产品（ARC-710 展示来源与能力；产品放行路径由后续 ARC 完成）。
#[derive(Debug, Clone)]
pub(crate) struct LiveExtensionHostPort {
    host: extension_host::api::ExtensionHost,
    handle: extension_host::api::ExtensionHostHandle,
    task: std::sync::Arc<std::sync::Mutex<Option<extension_host::api::ExtensionHostTask>>>,
}

impl LiveExtensionHostPort {
    pub(crate) fn start(
        options: extension_host::api::ExtensionHostOptions,
    ) -> Result<Self, ExtensionError> {
        let (host, _errors) = extension_host::api::ExtensionHost::new(options);
        let (handle, task) = host.clone().start()?;
        Ok(Self {
            host,
            handle,
            task: std::sync::Arc::new(std::sync::Mutex::new(Some(task))),
        })
    }
}

impl Drop for LiveExtensionHostPort {
    fn drop(&mut self) {
        // 确定性关闭；dispatch task 在 handle drop 后自动退出
        // （SendersDropped），此处显式触发让在途 hook 尽快结束。
        self.handle.shutdown("port dropped");
    }
}

impl ExtensionHostPort for LiveExtensionHostPort {
    fn extension_host(&self) -> Option<Arc<dyn ExtensionHostView>> {
        let task = self.task.clone();
        let handle = self.handle.clone();
        let host = self.host.clone();
        Some(Arc::new(LiveHostView { host, handle, task }))
    }
}

/// `ExtensionHostView` 的实现（`LiveExtensionHostPort` 的视图面）。
#[derive(Debug)]
struct LiveHostView {
    host: extension_host::api::ExtensionHost,
    handle: extension_host::api::ExtensionHostHandle,
    task: std::sync::Arc<std::sync::Mutex<Option<extension_host::api::ExtensionHostTask>>>,
}

impl ExtensionHostView for LiveHostView {
    fn submit(&self, event: ExtensionEvent) -> Result<(), ExtensionError> {
        self.handle.submit_event(event)
    }

    fn notify_shutdown(&self, reason: &str) {
        self.handle.shutdown(reason);
    }

    fn join_shutdown(&self) -> Pin<Box<dyn Future<Output = ()> + Send>> {
        let task = self
            .task
            .lock_or_recover("extension host task")
            .take()
            .expect("extension host task joined twice");
        Box::pin(async move {
            let _ = task.join().await;
        })
    }

    fn first_enables(&self) -> Vec<EnableRequest> {
        self.host.info().first_enables().to_vec()
    }

    fn gate(&self) -> Option<Arc<HookGate>> {
        self.host.gate()
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
