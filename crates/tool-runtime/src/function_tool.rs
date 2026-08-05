use std::sync::Arc;

use tool_contract::api::definition::ToolDefinition;

use crate::runtime::{DynamicTool, ToolCallContext, ToolFuture};

type FunctionExecutor = Arc<dyn Fn(ToolCallContext, serde_json::Value) -> ToolFuture + Send + Sync>;

/// Product-owned function tool adapter for definitions that need no typed decoder.
pub struct FunctionTool {
    definition: ToolDefinition,
    executor: FunctionExecutor,
}

impl FunctionTool {
    pub fn new(
        definition: ToolDefinition,
        executor: impl Fn(ToolCallContext, serde_json::Value) -> ToolFuture + Send + Sync + 'static,
    ) -> Arc<Self> {
        Arc::new(Self {
            definition,
            executor: Arc::new(executor),
        })
    }
}

impl DynamicTool for FunctionTool {
    fn definition(&self) -> &ToolDefinition {
        &self.definition
    }

    fn execute(&self, context: ToolCallContext, arguments: serde_json::Value) -> ToolFuture {
        (self.executor)(context, arguments)
    }
}
