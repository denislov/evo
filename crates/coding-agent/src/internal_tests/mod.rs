//! Owner-crate behavior tests for private product adapters and built-in tools.

mod support;

pub(crate) mod product_fixture {
    pub(crate) mod command {
        pub(crate) use crate::app::error::*;
        pub(crate) use crate::app::prompt_execution::run_prompt_text_for_tests;
        pub(crate) use crate::app::prompt_runtime::*;
        pub(crate) use crate::app::session::*;
        pub(crate) use crate::app::startup::*;
    }

    pub(crate) mod configuration {
        pub(crate) use crate::app::bootstrap::*;
        pub(crate) use crate::app::model_selection::*;
        pub(crate) use crate::config::auth::*;
        pub(crate) use crate::config::settings::*;
        pub(crate) use crate::config::*;
    }

    pub(crate) mod input {
        pub(crate) use crate::app::prompt_input::*;
    }

    pub(crate) mod resources {
        pub(crate) use crate::resources::*;
        pub(crate) use crate::tools::*;
    }

    pub(crate) mod theme {
        pub(crate) use crate::theme::*;
    }
}

#[path = "../../tests/operation/agent_invocation.rs"]
mod agent_invocation;
#[path = "../../tests/operation/agent_profile_runtime.rs"]
mod agent_profile_runtime;
#[path = "../../tests/operation/agent_team_runner.rs"]
mod agent_team_runner;
#[path = "../../tests/operation/association.rs"]
mod association;
#[path = "../../tests/config_request/config_wiring.rs"]
mod config_wiring;
#[path = "../../tests/operation/delegation_execution.rs"]
mod delegation_execution;
#[path = "../../tests/print_json/print_mode.rs"]
mod print_mode;
#[path = "../../tests/events_snapshot/product_event_contract.rs"]
mod product_event_contract;
#[path = "../../tests/config_request/request_resolution.rs"]
mod request_resolution;
#[path = "../../tests/config_request/runtime_configuration.rs"]
mod runtime_configuration;
#[path = "../../tests/config_request/theme.rs"]
mod theme;
#[path = "../../tests/tools/e2e.rs"]
mod tool_e2e;

mod file_mutation_queue;
mod filesystem_capability;
mod m10_resources_input;
mod runtime_private_seed;
mod tool_bash;
mod tool_edit;
mod tool_find;
mod tool_grep;
mod tool_ls;
mod tool_operations;
mod tool_read;
mod tool_write;
