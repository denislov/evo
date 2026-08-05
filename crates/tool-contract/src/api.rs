pub mod definition {
    pub use crate::contract::{
        AuthorizationRisk, ToolBehaviorVersion, ToolCapabilities, ToolDefinition,
        ToolDefinitionError, ToolExecutionMode, ToolId, ToolKind, ToolRequirement,
    };
}

pub mod schema {
    pub use crate::contract::{ToolArgs, schema_for};
}

pub mod output {
    pub use crate::contract::{
        ChangeReceipt, ToolContent, ToolError, ToolErrorKind, ToolOutput, ToolProgress,
    };
}
