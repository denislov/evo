use std::path::PathBuf;

use ai_protocol::api::model::Model;

use crate::app::bootstrap::{ApplicationRunOptions, SessionMode, SessionRunOptions};
use crate::app::embedding::embedding_diagnostic_from_application;
use crate::app::error::ApplicationError;
use crate::app::invocation::CodingAgentInvocationOptions;
use crate::app::prompt_execution::{
    CodingAgentPromptExecution, CodingAgentPromptExecutionPreparation,
};
use crate::app::startup;
use crate::config;

pub(crate) fn default_application_options(
    cwd: PathBuf,
    model_override: Option<Model>,
    session_mode: SessionMode,
    session_dir: Option<PathBuf>,
) -> Result<ApplicationRunOptions, crate::runtime::facade::CodingSessionError> {
    let session = SessionRunOptions {
        mode: session_mode,
        cwd: cwd.clone(),
        session_dir,
        workspace: None,
    };
    Ok(ApplicationRunOptions {
        model_override,
        tools: Vec::new(),
        register_builtins: true,
        global_config_only: false,
        ai_client: None,
        session,
        extension_host_options: None,
        code_intelligence: None,
    })
}

pub(crate) fn prepare_prompt_execution(
    cwd: PathBuf,
    invocation: CodingAgentInvocationOptions,
    stdin: Option<String>,
) -> Result<CodingAgentPromptExecutionPreparation, ApplicationError> {
    let options = default_application_options(cwd.clone(), None, SessionMode::Enabled, None)
        .map_err(|error| ApplicationError::SessionFailure(error.to_string()))?;
    let mode = invocation.prompt_mode;
    let config_paths = config::resolve_paths(&cwd);
    let resolved =
        startup::resolve_prompt_request(invocation, options, stdin, cwd, config_paths.global_dir)?;

    match mode {
        crate::runtime::facade::PromptTurnMode::Print
        | crate::runtime::facade::PromptTurnMode::Json => {
            let diagnostics = resolved
                .context
                .diagnostics
                .iter()
                .map(embedding_diagnostic_from_application)
                .collect();
            Ok(CodingAgentPromptExecutionPreparation::from_internal(
                CodingAgentPromptExecution::from_internal(resolved.session_options),
                diagnostics,
            ))
        }
        crate::runtime::facade::PromptTurnMode::Rpc => Err(ApplicationError::UnsupportedMode(
            "rpc requires the streaming application startup".into(),
        )),
    }
}
