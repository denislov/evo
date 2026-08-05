use std::any::{Any, TypeId};
use std::collections::{BTreeMap, HashMap};
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};
use std::time::Instant;

use tokio::sync::{Mutex, MutexGuard};
use tokio_util::sync::CancellationToken;
use tool_contract::api::definition::{ToolDefinition, ToolExecutionMode, ToolId};
use tool_contract::api::output::{ToolError, ToolErrorKind, ToolOutput, ToolProgress};
use tool_contract::api::schema::{ToolArgs, schema_for};

pub type ToolFuture = Pin<Box<dyn Future<Output = Result<ToolOutput, ToolError>> + Send>>;

const TOOL_TEARDOWN_GRACE: std::time::Duration = std::time::Duration::from_secs(5);

#[derive(Clone)]
pub struct ProgressSink {
    callback: Arc<dyn Fn(ToolProgress) + Send + Sync>,
    closed: Arc<AtomicBool>,
}

impl ProgressSink {
    pub fn new(callback: impl Fn(ToolProgress) + Send + Sync + 'static) -> Self {
        Self {
            callback: Arc::new(callback),
            closed: Arc::new(AtomicBool::new(false)),
        }
    }

    pub fn emit(&self, progress: ToolProgress) -> Result<(), ToolError> {
        if self.closed.load(Ordering::Acquire) {
            return Err(ToolError::new(
                ToolErrorKind::Protocol,
                "tool progress emitted after terminal result",
            ));
        }
        (self.callback)(progress);
        Ok(())
    }

    fn close(&self) {
        self.closed.store(true, Ordering::Release);
    }
}

struct ProgressTerminalGuard(Option<ProgressSink>);

impl Drop for ProgressTerminalGuard {
    fn drop(&mut self) {
        if let Some(progress) = &self.0 {
            progress.close();
        }
    }
}

#[derive(Clone, Default)]
struct ContextExtensions {
    values: Arc<RwLock<HashMap<TypeId, Arc<dyn Any + Send + Sync>>>>,
}

impl ContextExtensions {
    fn insert<T: Send + Sync + 'static>(&self, value: T) {
        self.values
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(TypeId::of::<T>(), Arc::new(value));
    }

    fn get<T: Send + Sync + 'static>(&self) -> Option<Arc<T>> {
        self.values
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&TypeId::of::<T>())
            .cloned()
            .and_then(|value| value.downcast().ok())
    }
}

#[derive(Clone)]
pub struct ToolCallContext {
    pub operation_id: Option<String>,
    pub turn: u32,
    pub call_id: String,
    pub tool_id: ToolId,
    pub cwd: Option<PathBuf>,
    pub deadline: Option<Instant>,
    pub trace_id: Option<String>,
    pub cancel: CancellationToken,
    pub progress: Option<ProgressSink>,
    extensions: ContextExtensions,
}

impl ToolCallContext {
    pub fn new(tool_id: ToolId, call_id: impl Into<String>, cancel: CancellationToken) -> Self {
        Self {
            operation_id: None,
            turn: 0,
            call_id: call_id.into(),
            tool_id,
            cwd: None,
            deadline: None,
            trace_id: None,
            cancel,
            progress: None,
            extensions: ContextExtensions::default(),
        }
    }

    pub fn with_operation_id(mut self, operation_id: Option<String>) -> Self {
        self.operation_id = operation_id;
        self
    }

    pub fn with_turn(mut self, turn: u32) -> Self {
        self.turn = turn;
        self
    }

    pub fn with_cwd(mut self, cwd: Option<PathBuf>) -> Self {
        self.cwd = cwd;
        self
    }

    pub fn with_deadline(mut self, deadline: Option<Instant>) -> Self {
        self.deadline = deadline;
        self
    }

    pub fn with_trace_id(mut self, trace_id: Option<String>) -> Self {
        self.trace_id = trace_id;
        self
    }

    pub fn with_progress(mut self, progress: Option<ProgressSink>) -> Self {
        self.progress = progress;
        self
    }

    pub fn insert_extension<T: Send + Sync + 'static>(&self, value: T) {
        self.extensions.insert(value);
    }

    pub fn extension<T: Send + Sync + 'static>(&self) -> Option<Arc<T>> {
        self.extensions.get()
    }
}

pub trait DynamicTool: Send + Sync {
    fn definition(&self) -> &ToolDefinition;
    fn validate_arguments(&self, _arguments: &serde_json::Value) -> Result<(), ToolError> {
        Ok(())
    }
    fn execute(&self, context: ToolCallContext, arguments: serde_json::Value) -> ToolFuture;
}

type TypedExecutor<T> = Arc<dyn Fn(ToolCallContext, T) -> ToolFuture + Send + Sync>;

pub struct TypedTool<T> {
    definition: ToolDefinition,
    executor: TypedExecutor<T>,
}

impl<T: ToolArgs> TypedTool<T> {
    pub fn new(
        definition: ToolDefinition,
        executor: impl Fn(ToolCallContext, T) -> ToolFuture + Send + Sync + 'static,
    ) -> Result<Self, ToolRegistryError> {
        definition
            .validate()
            .map_err(ToolRegistryError::Definition)?;
        let expected = schema_for::<T>().map_err(ToolRegistryError::Definition)?;
        if definition.parameters != expected {
            return Err(ToolRegistryError::SchemaMismatch(definition.id));
        }
        Ok(Self {
            definition,
            executor: Arc::new(executor),
        })
    }
}

impl<T: ToolArgs> DynamicTool for TypedTool<T> {
    fn definition(&self) -> &ToolDefinition {
        &self.definition
    }

    fn validate_arguments(&self, arguments: &serde_json::Value) -> Result<(), ToolError> {
        serde_json::from_value::<T>(arguments.clone())
            .map(|_| ())
            .map_err(|error| {
                ToolError::new(
                    ToolErrorKind::InvalidArguments,
                    format!("invalid tool arguments: {error}"),
                )
            })
    }

    fn execute(&self, context: ToolCallContext, arguments: serde_json::Value) -> ToolFuture {
        let arguments = match serde_json::from_value(arguments) {
            Ok(arguments) => arguments,
            Err(error) => {
                return Box::pin(async move {
                    Err(ToolError::new(
                        ToolErrorKind::InvalidArguments,
                        format!("invalid tool arguments: {error}"),
                    ))
                });
            }
        };
        (self.executor)(context, arguments)
    }
}

struct RegistryEntry {
    tool: Arc<dyn DynamicTool>,
    sequential: Option<Arc<Mutex<()>>>,
}

#[derive(Default)]
pub struct ToolRegistry {
    entries: BTreeMap<ToolId, RegistryEntry>,
}

impl ToolRegistry {
    pub fn register(&mut self, tool: Arc<dyn DynamicTool>) -> Result<(), ToolRegistryError> {
        tool.definition()
            .validate()
            .map_err(ToolRegistryError::Definition)?;
        let id = tool.definition().id.clone();
        if self.entries.contains_key(&id) {
            return Err(ToolRegistryError::Duplicate(id));
        }
        let sequential = (tool.definition().capabilities.execution
            == ToolExecutionMode::Sequential)
            .then(|| Arc::new(Mutex::new(())));
        self.entries.insert(id, RegistryEntry { tool, sequential });
        Ok(())
    }

    pub fn validate_requirements(&self) -> Result<(), ToolRegistryError> {
        for entry in self.entries.values() {
            for requirement in &entry.tool.definition().requirements {
                let Some(required) = self.entries.get(&requirement.tool) else {
                    return Err(ToolRegistryError::MissingRequirement {
                        tool: entry.tool.definition().id.clone(),
                        required: requirement.tool.clone(),
                    });
                };
                if required.tool.definition().behavior.get() < requirement.minimum_behavior.get() {
                    return Err(ToolRegistryError::BehaviorTooOld {
                        tool: entry.tool.definition().id.clone(),
                        required: requirement.tool.clone(),
                    });
                }
            }
        }
        Ok(())
    }

    pub fn definitions(&self) -> Vec<ToolDefinition> {
        self.entries
            .values()
            .map(|entry| entry.tool.definition().clone())
            .collect()
    }

    fn entry(&self, id: &ToolId) -> Option<&RegistryEntry> {
        self.entries.get(id)
    }
}

#[derive(Clone)]
pub struct ToolRuntime {
    registry: Arc<ToolRegistry>,
}

impl ToolRuntime {
    pub fn new(registry: ToolRegistry) -> Result<Self, ToolRegistryError> {
        registry.validate_requirements()?;
        Ok(Self {
            registry: Arc::new(registry),
        })
    }

    pub async fn execute(
        &self,
        context: ToolCallContext,
        arguments: serde_json::Value,
    ) -> Result<ToolOutput, ToolError> {
        let _progress_terminal = ProgressTerminalGuard(context.progress.clone());
        let entry = self.registry.entry(&context.tool_id).ok_or_else(|| {
            ToolError::new(
                ToolErrorKind::Unavailable,
                format!("unknown tool: {}", context.tool_id),
            )
        })?;
        let mut execution_context = context.clone();
        execution_context.cancel = context.cancel.child_token();
        let _sequential_guard =
            acquire_gate(entry.sequential.as_deref(), &execution_context).await?;
        let future = entry.tool.execute(execution_context.clone(), arguments);
        await_tool_with_controls(&execution_context, future).await
    }

    pub fn validate_arguments(
        &self,
        id: &ToolId,
        arguments: &serde_json::Value,
    ) -> Result<(), ToolError> {
        let entry = self.registry.entry(id).ok_or_else(|| {
            ToolError::new(ToolErrorKind::Unavailable, format!("unknown tool: {id}"))
        })?;
        entry.tool.validate_arguments(arguments)
    }

    pub fn definitions(&self) -> Vec<ToolDefinition> {
        self.registry.definitions()
    }

    pub fn definition(&self, id: &ToolId) -> Option<ToolDefinition> {
        self.registry
            .entry(id)
            .map(|entry| entry.tool.definition().clone())
    }
}

async fn await_tool_with_controls(
    context: &ToolCallContext,
    future: ToolFuture,
) -> Result<ToolOutput, ToolError> {
    tokio::pin!(future);
    let terminal = match context.deadline {
        Some(deadline) => {
            let deadline = tokio::time::Instant::from_std(deadline);
            tokio::select! {
                biased;
                _ = context.cancel.cancelled() => Some(cancelled_error()),
                _ = tokio::time::sleep_until(deadline) => Some(timeout_error()),
                result = &mut future => return result,
            }
        }
        None => {
            tokio::select! {
                biased;
                _ = context.cancel.cancelled() => Some(cancelled_error()),
                result = &mut future => return result,
            }
        }
    };

    context.cancel.cancel();
    let _ = tokio::time::timeout(TOOL_TEARDOWN_GRACE, &mut future).await;
    Err(terminal.expect("a control branch always supplies a terminal error"))
}

async fn acquire_gate<'a>(
    gate: Option<&'a Mutex<()>>,
    context: &ToolCallContext,
) -> Result<Option<MutexGuard<'a, ()>>, ToolError> {
    let Some(gate) = gate else {
        return Ok(None);
    };
    await_with_controls(context, gate.lock()).await.map(Some)
}

async fn await_with_controls<F, T>(context: &ToolCallContext, future: F) -> Result<T, ToolError>
where
    F: Future<Output = T>,
{
    tokio::pin!(future);
    match context.deadline {
        Some(deadline) => {
            let deadline = tokio::time::Instant::from_std(deadline);
            tokio::select! {
                biased;
                _ = context.cancel.cancelled() => Err(cancelled_error()),
                _ = tokio::time::sleep_until(deadline) => Err(timeout_error()),
                result = &mut future => Ok(result),
            }
        }
        None => {
            tokio::select! {
                biased;
                _ = context.cancel.cancelled() => Err(cancelled_error()),
                result = &mut future => Ok(result),
            }
        }
    }
}

fn cancelled_error() -> ToolError {
    ToolError::new(ToolErrorKind::Cancelled, "tool call cancelled")
}

fn timeout_error() -> ToolError {
    ToolError::new(ToolErrorKind::Timeout, "tool call timed out")
}

#[derive(Debug, thiserror::Error)]
pub enum ToolRegistryError {
    #[error(transparent)]
    Definition(#[from] tool_contract::api::definition::ToolDefinitionError),
    #[error("duplicate tool: {0}")]
    Duplicate(ToolId),
    #[error("typed argument schema does not match tool definition: {0}")]
    SchemaMismatch(ToolId),
    #[error("tool {tool} requires missing tool {required}")]
    MissingRequirement { tool: ToolId, required: ToolId },
    #[error("tool {tool} requires a newer behavior of {required}")]
    BehaviorTooOld { tool: ToolId, required: ToolId },
}

#[cfg(test)]
mod tests {
    use super::*;
    use schemars::JsonSchema;
    use serde::Deserialize;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;
    use tokio::sync::Notify;
    use tool_contract::api::definition::{
        AuthorizationRisk, ToolBehaviorVersion, ToolCapabilities, ToolKind, ToolRequirement,
    };
    use tool_contract::api::output::ToolContent;
    use tool_contract::api::schema::schema_for;

    #[derive(Deserialize, JsonSchema)]
    struct EchoArgs {
        text: String,
    }

    fn definition(id: &str) -> ToolDefinition {
        ToolDefinition {
            id: ToolId::new(id).unwrap(),
            kind: ToolKind::Function,
            description: "Echo text".into(),
            parameters: schema_for::<EchoArgs>().unwrap(),
            capabilities: ToolCapabilities::default(),
            behavior: ToolBehaviorVersion::V1,
            authorization_risk: AuthorizationRisk::None,
            requirements: Vec::new(),
        }
    }

    fn context(id: &str, progress: Option<ProgressSink>) -> ToolCallContext {
        ToolCallContext {
            operation_id: Some("operation-1".into()),
            turn: 1,
            call_id: "call-1".into(),
            tool_id: ToolId::new(id).unwrap(),
            cwd: None,
            deadline: None,
            trace_id: None,
            cancel: CancellationToken::new(),
            progress,
            extensions: ContextExtensions::default(),
        }
    }

    fn echo_tool(
        definition: ToolDefinition,
        executor: impl Fn(ToolCallContext, EchoArgs) -> ToolFuture + Send + Sync + 'static,
    ) -> Arc<dyn DynamicTool> {
        Arc::new(TypedTool::<EchoArgs>::new(definition, executor).unwrap())
    }

    #[tokio::test]
    async fn typed_tool_executes_and_closes_progress_after_terminal() {
        let progress = ProgressSink::new(|_| {});
        let observer = progress.clone();
        let tool = TypedTool::<EchoArgs>::new(definition("echo"), |context, args| {
            Box::pin(async move {
                context.progress.as_ref().unwrap().emit(ToolProgress {
                    content: vec![ToolContent::Text {
                        text: "working".into(),
                    }],
                    details: None,
                })?;
                Ok(ToolOutput {
                    content: vec![ToolContent::Text { text: args.text }],
                    ..Default::default()
                })
            })
        })
        .unwrap();
        let mut registry = ToolRegistry::default();
        registry.register(Arc::new(tool)).unwrap();
        let runtime = ToolRuntime::new(registry).unwrap();
        runtime
            .execute(
                context("echo", Some(progress)),
                serde_json::json!({"text": "ok"}),
            )
            .await
            .unwrap();
        assert!(
            observer
                .emit(ToolProgress {
                    content: Vec::new(),
                    details: None,
                })
                .is_err()
        );
    }

    #[test]
    fn duplicate_registration_fails_closed() {
        let tool = || {
            echo_tool(definition("echo"), |_context, _args| {
                Box::pin(async { Ok(ToolOutput::default()) })
            })
        };
        let mut registry = ToolRegistry::default();
        registry.register(tool()).unwrap();
        assert!(matches!(
            registry.register(tool()),
            Err(ToolRegistryError::Duplicate(_))
        ));
    }

    #[test]
    fn typed_tool_rejects_schema_drift() {
        let mut drifted = definition("echo");
        drifted.parameters = serde_json::json!({
            "type": "object",
            "properties": {"different": {"type": "string"}}
        });
        assert!(matches!(
            TypedTool::<EchoArgs>::new(drifted, |_context, _args| {
                Box::pin(async { Ok(ToolOutput::default()) })
            }),
            Err(ToolRegistryError::SchemaMismatch(_))
        ));
    }

    #[test]
    fn requirements_fail_closed_for_missing_or_old_dependencies() {
        let required = ToolRequirement {
            tool: ToolId::new("provider").unwrap(),
            minimum_behavior: ToolBehaviorVersion::new(2).unwrap(),
        };
        let mut consumer = definition("consumer");
        consumer.requirements.push(required.clone());

        let mut missing = ToolRegistry::default();
        missing
            .register(echo_tool(consumer.clone(), |_context, _args| {
                Box::pin(async { Ok(ToolOutput::default()) })
            }))
            .unwrap();
        assert!(matches!(
            ToolRuntime::new(missing),
            Err(ToolRegistryError::MissingRequirement { .. })
        ));

        let mut old = ToolRegistry::default();
        old.register(echo_tool(consumer, |_context, _args| {
            Box::pin(async { Ok(ToolOutput::default()) })
        }))
        .unwrap();
        old.register(echo_tool(definition("provider"), |_context, _args| {
            Box::pin(async { Ok(ToolOutput::default()) })
        }))
        .unwrap();
        assert!(matches!(
            ToolRuntime::new(old),
            Err(ToolRegistryError::BehaviorTooOld { .. })
        ));
    }

    #[test]
    fn definitions_are_listed_in_stable_tool_id_order() {
        let mut registry = ToolRegistry::default();
        for id in ["zeta", "alpha"] {
            registry
                .register(echo_tool(definition(id), |_context, _args| {
                    Box::pin(async { Ok(ToolOutput::default()) })
                }))
                .unwrap();
        }
        let runtime = ToolRuntime::new(registry).unwrap();
        assert_eq!(
            runtime
                .definitions()
                .iter()
                .map(|definition| definition.id.as_str())
                .collect::<Vec<_>>(),
            ["alpha", "zeta"]
        );
    }

    #[tokio::test]
    async fn sequential_tools_never_overlap() {
        let active = Arc::new(AtomicUsize::new(0));
        let maximum = Arc::new(AtomicUsize::new(0));
        let mut sequential = definition("echo");
        sequential.capabilities.execution = ToolExecutionMode::Sequential;
        let tool = echo_tool(sequential, {
            let active = active.clone();
            let maximum = maximum.clone();
            move |_context, _args| {
                let active = active.clone();
                let maximum = maximum.clone();
                Box::pin(async move {
                    let current = active.fetch_add(1, Ordering::SeqCst) + 1;
                    maximum.fetch_max(current, Ordering::SeqCst);
                    tokio::time::sleep(Duration::from_millis(20)).await;
                    active.fetch_sub(1, Ordering::SeqCst);
                    Ok(ToolOutput::default())
                })
            }
        });
        let mut registry = ToolRegistry::default();
        registry.register(tool).unwrap();
        let runtime = ToolRuntime::new(registry).unwrap();
        let first = runtime.execute(context("echo", None), serde_json::json!({"text": "a"}));
        let second = runtime.execute(context("echo", None), serde_json::json!({"text": "b"}));
        let (first, second) = tokio::join!(first, second);
        first.unwrap();
        second.unwrap();
        assert_eq!(maximum.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn cancellation_and_deadline_apply_while_waiting_for_sequential_gate() {
        let started = Arc::new(Notify::new());
        let release = Arc::new(Notify::new());
        let mut sequential = definition("echo");
        sequential.capabilities.execution = ToolExecutionMode::Sequential;
        let tool = echo_tool(sequential, {
            let started = started.clone();
            let release = release.clone();
            move |_context, _args| {
                let started = started.clone();
                let release = release.clone();
                Box::pin(async move {
                    started.notify_one();
                    release.notified().await;
                    Ok(ToolOutput::default())
                })
            }
        });
        let mut registry = ToolRegistry::default();
        registry.register(tool).unwrap();
        let runtime = ToolRuntime::new(registry).unwrap();
        let first_runtime = runtime.clone();
        let first = tokio::spawn(async move {
            first_runtime
                .execute(context("echo", None), serde_json::json!({"text": "first"}))
                .await
        });
        started.notified().await;

        let cancelled_progress = ProgressSink::new(|_| {});
        let cancelled_observer = cancelled_progress.clone();
        let cancelled_context = context("echo", Some(cancelled_progress));
        cancelled_context.cancel.cancel();
        let cancelled = tokio::time::timeout(
            Duration::from_millis(100),
            runtime.execute(cancelled_context, serde_json::json!({"text": "second"})),
        )
        .await
        .expect("cancel must not wait for the sequential holder")
        .unwrap_err();
        assert_eq!(cancelled.kind, ToolErrorKind::Cancelled);
        assert!(
            cancelled_observer
                .emit(ToolProgress {
                    content: Vec::new(),
                    details: None,
                })
                .is_err()
        );

        let mut deadline_context = context("echo", None);
        deadline_context.deadline = Some(Instant::now() + Duration::from_millis(20));
        let timed_out = tokio::time::timeout(
            Duration::from_millis(100),
            runtime.execute(deadline_context, serde_json::json!({"text": "third"})),
        )
        .await
        .expect("deadline must not wait for the sequential holder")
        .unwrap_err();
        assert_eq!(timed_out.kind, ToolErrorKind::Timeout);

        release.notify_waiters();
        first.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn cancellation_and_deadline_wait_for_cooperative_tool_teardown() {
        for deadline in [false, true] {
            let teardown = Arc::new(AtomicUsize::new(0));
            let tool = echo_tool(definition("echo"), {
                let teardown = teardown.clone();
                move |context, _args| {
                    let teardown = teardown.clone();
                    Box::pin(async move {
                        context.cancel.cancelled().await;
                        tokio::time::sleep(Duration::from_millis(10)).await;
                        teardown.fetch_add(1, Ordering::SeqCst);
                        Err(ToolError::new(ToolErrorKind::Cancelled, "cleaned up"))
                    })
                }
            });
            let mut registry = ToolRegistry::default();
            registry.register(tool).unwrap();
            let runtime = ToolRuntime::new(registry).unwrap();
            let mut tool_context = context("echo", None);
            let parent_cancel = tool_context.cancel.clone();
            if deadline {
                tool_context.deadline = Some(Instant::now() + Duration::from_millis(10));
            } else {
                tokio::spawn(async move {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                    parent_cancel.cancel();
                });
            }
            let error = runtime
                .execute(tool_context, serde_json::json!({"text": "wait"}))
                .await
                .unwrap_err();
            assert_eq!(
                error.kind,
                if deadline {
                    ToolErrorKind::Timeout
                } else {
                    ToolErrorKind::Cancelled
                }
            );
            assert_eq!(teardown.load(Ordering::SeqCst), 1);
        }
    }

    #[tokio::test]
    async fn invalid_arguments_and_unknown_tools_close_progress() {
        let mut registry = ToolRegistry::default();
        registry
            .register(echo_tool(definition("echo"), |_context, _args| {
                Box::pin(async { Ok(ToolOutput::default()) })
            }))
            .unwrap();
        let runtime = ToolRuntime::new(registry).unwrap();

        for (id, arguments, expected) in [
            (
                "echo",
                serde_json::json!({}),
                ToolErrorKind::InvalidArguments,
            ),
            (
                "missing",
                serde_json::json!({"text": "ok"}),
                ToolErrorKind::Unavailable,
            ),
        ] {
            let progress = ProgressSink::new(|_| {});
            let observer = progress.clone();
            let error = runtime
                .execute(context(id, Some(progress)), arguments)
                .await
                .unwrap_err();
            assert_eq!(error.kind, expected);
            assert!(
                observer
                    .emit(ToolProgress {
                        content: Vec::new(),
                        details: None,
                    })
                    .is_err()
            );
        }
    }
}
