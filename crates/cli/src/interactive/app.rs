use std::io::IsTerminal;
use std::path::PathBuf;

use tui::api::terminal::ProcessTerminal;
use tui::api::theme::TuiTheme;

use crate::interactive::error::CliError;
use crate::interactive::input::InputPump;
use crate::interactive::r#loop::run_interactive_loop_with_input;
use crate::interactive::session_actions::SessionChoice;
use crate::output::CliOutput;
use coding_agent::api::authorization::ToolAuthorizationMode;
use coding_agent::api::embedding::{
    CodingAgentAuthCommand, CodingAgentAuthController, CodingAgentAuthMutationOutcome,
    CodingAgentInteractiveStartup, CodingAgentModelCatalogEntry, CodingAgentPreparedPrompt,
    CodingAgentProfileCatalog, CodingAgentResourceCommand, CodingAgentSessionQuery,
    CodingAgentThinkingLevel,
};
use coding_agent::api::operation::{
    BranchSummaryReusePolicy, CodingAgentOperation, CodingAgentOperationFactory, PromptInvocation,
    SelfHealingEditModelRepairOptions, SelfHealingEditRequest,
};
use coding_agent::api::runtime::CodingAgentSessionBootstrap;
use coding_agent::api::settings::{
    CodingAgentSettingsCommand, CodingAgentSettingsController, CodingAgentSettingsMutationOutcome,
    CodingAgentSettingsSnapshot, CodingAgentThemeController, CodingAgentThemeSnapshot,
};
use coding_agent::api::view::ProfileId;

pub async fn run_interactive_mode(startup: CodingAgentInteractiveStartup) -> CliOutput {
    if !std::io::stdin().is_terminal() || !std::io::stdout().is_terminal() {
        return CliOutput {
            exit_code: 1,
            stdout: String::new(),
            stderr: "interactive mode requires a TTY\n".to_string(),
        };
    }

    let terminal = ProcessTerminal::new();
    match run_interactive_loop_with_input(startup, terminal, InputPump::from_stdin).await {
        Ok(mut result) => {
            let shutdown_error = match result.coding_session.as_mut() {
                Some(session) => session.shutdown().await.err(),
                None => None,
            };
            match shutdown_error {
                Some(error) => CliOutput {
                    exit_code: 1,
                    stdout: String::new(),
                    stderr: format!("{error}\n"),
                },
                None => CliOutput {
                    exit_code: result.exit_code,
                    stdout: String::new(),
                    stderr: String::new(),
                },
            }
        }
        Err(error) => CliOutput {
            exit_code: 1,
            stdout: String::new(),
            stderr: format!("{error}\n"),
        },
    }
}

#[derive(Clone)]
pub(super) struct PromptContext {
    startup_source: CodingAgentInteractiveStartup,
    pub(super) operation_factory: CodingAgentOperationFactory,
    pub(super) auth_controller: CodingAgentAuthController,
    pub(super) settings_controller: CodingAgentSettingsController,
    pub(super) session_bootstrap: CodingAgentSessionBootstrap,
    pub(super) cwd: PathBuf,
    pub(super) thinking_level: Option<CodingAgentThinkingLevel>,
    pub(super) profile_catalog: CodingAgentProfileCatalog,
    pub(super) default_agent_profile_id: ProfileId,
    pub(super) context_files: Vec<PathBuf>,
    pub(super) theme: TuiTheme,
    pub(super) resolved_theme: CodingAgentThemeSnapshot,
    pub(super) theme_controller: CodingAgentThemeController,
    pub(super) model_summary: CodingAgentModelCatalogEntry,
    pub(super) model_choices: Vec<CodingAgentModelCatalogEntry>,
    pub(super) model_rotation: Vec<CodingAgentModelCatalogEntry>,
    pub(super) resource_commands: Vec<CodingAgentResourceCommand>,
    pub(super) session_query: CodingAgentSessionQuery,
    pub(super) session_choices: Vec<SessionChoice>,
    pub(super) permission_mode: ToolAuthorizationMode,
}

impl PromptContext {
    pub(super) fn from_startup(startup: CodingAgentInteractiveStartup) -> Self {
        let application = startup.application().clone();
        Self {
            startup_source: startup.clone(),
            operation_factory: application.operation_factory,
            auth_controller: application.auth_controller,
            settings_controller: application.settings_controller,
            session_bootstrap: application.session_bootstrap,
            cwd: application.cwd,
            thinking_level: application.thinking_level,
            profile_catalog: application.profile_catalog,
            default_agent_profile_id: application.default_agent_profile_id,
            context_files: startup.context_file_paths,
            theme: crate::interactive::theme::tui_theme_from_snapshot(&startup.theme),
            resolved_theme: startup.theme,
            theme_controller: startup.theme_controller,
            model_summary: application.model_summary,
            model_choices: startup.model_choices,
            model_rotation: startup.model_rotation,
            resource_commands: startup.resource_commands,
            session_query: application.session_query,
            session_choices: startup.session_choices,
            permission_mode: application.permission_mode,
        }
    }

    pub(super) fn reload(&self) -> Result<Self, coding_agent::api::error::CodingAgentPublicError> {
        self.startup_source.reload().map(Self::from_startup)
    }

    pub(super) fn prepare_prompt(
        &self,
        prompt: &str,
    ) -> Result<CodingAgentPreparedPrompt, coding_agent::api::error::CodingAgentPublicError> {
        self.startup_source.prepare_prompt(prompt)
    }

    pub(super) fn prepared_prompt_operation(
        &self,
        prompt: CodingAgentPreparedPrompt,
    ) -> CodingAgentOperation {
        self.startup_source.prompt_operation(prompt)
    }

    pub(super) fn resource_prompt_operation(
        &self,
        invocation: PromptInvocation,
    ) -> CodingAgentOperation {
        self.operation_factory
            .prompt_operation(invocation, self.thinking_level)
    }

    pub(super) fn compact_operation(
        &self,
        custom_instructions: Option<String>,
    ) -> CodingAgentOperation {
        self.operation_factory
            .compact_operation(custom_instructions)
    }

    pub(super) fn agent_invocation_operation(
        &self,
        profile_id: ProfileId,
        task: String,
    ) -> CodingAgentOperation {
        self.operation_factory
            .agent_invocation_operation(profile_id, task, self.thinking_level)
    }

    pub(super) fn team_invocation_operation(
        &self,
        team_id: ProfileId,
        task: String,
    ) -> CodingAgentOperation {
        self.operation_factory
            .team_invocation_operation(team_id, task, self.thinking_level)
    }

    pub(super) fn branch_summary_operation(
        &self,
        source_leaf_id: String,
        target_leaf_id: String,
        custom_instructions: Option<String>,
        reuse: BranchSummaryReusePolicy,
    ) -> CodingAgentOperation {
        self.operation_factory.branch_summary_operation(
            source_leaf_id,
            target_leaf_id,
            custom_instructions,
            reuse,
        )
    }

    pub(super) fn self_healing_edit_operation(
        &self,
        request: SelfHealingEditRequest,
    ) -> CodingAgentOperation {
        self.operation_factory.self_healing_edit_operation(request)
    }

    pub(super) fn fork_session_operation(
        &self,
        target_leaf_id: Option<String>,
    ) -> CodingAgentOperation {
        self.operation_factory
            .fork_session_operation(target_leaf_id)
    }

    pub(super) fn model_repair_options(
        &self,
        max_attempts: usize,
    ) -> SelfHealingEditModelRepairOptions {
        self.operation_factory
            .model_repair_options(self.thinking_level, max_attempts)
    }

    pub(super) fn session_bootstrap(&self) -> CodingAgentSessionBootstrap {
        self.session_bootstrap.clone()
    }

    pub(super) fn select_model(
        &mut self,
        selection: &CodingAgentModelCatalogEntry,
    ) -> Result<String, CliError> {
        let diagnostics = self
            .auth_controller
            .bind_model(selection, &mut self.operation_factory)?;
        self.model_summary = selection.clone();
        self.model_choices = self.auth_controller.configured_models(&self.model_summary);
        Ok(diagnostics)
    }

    pub(super) fn apply_auth_command(
        &mut self,
        command: CodingAgentAuthCommand,
    ) -> Result<CodingAgentAuthMutationOutcome, coding_agent::api::error::CodingAgentPublicError>
    {
        let outcome = self
            .auth_controller
            .apply(command, &mut self.operation_factory)?;
        self.model_choices = self.auth_controller.configured_models(&self.model_summary);
        Ok(outcome)
    }

    pub(super) fn settings_snapshot(&self) -> CodingAgentSettingsSnapshot {
        self.settings_controller.snapshot()
    }

    pub(super) fn show_progress(&self) -> bool {
        self.settings_snapshot().presentation.show_progress
    }

    pub(super) fn apply_settings_command(
        &mut self,
        command: CodingAgentSettingsCommand,
    ) -> Result<CodingAgentSettingsMutationOutcome, coding_agent::api::error::CodingAgentPublicError>
    {
        let selected_theme = match &command {
            CodingAgentSettingsCommand::SetTheme(theme) => {
                Some(self.theme_controller.select(theme)?)
            }
            _ => None,
        };
        let outcome = self
            .settings_controller
            .apply(command, &mut self.operation_factory)?;
        if let Some(snapshot) = selected_theme {
            self.theme = crate::interactive::theme::tui_theme_from_snapshot(&snapshot);
            self.resolved_theme = snapshot;
        }
        Ok(outcome)
    }
}

pub(super) fn session_label(persistent: bool) -> String {
    if persistent {
        "session".to_string()
    } else {
        "no-session".to_string()
    }
}

pub(super) fn welcome_line() -> String {
    format!(
        "{logo}\n{}\nReady · /help for commands",
        env!("CARGO_PKG_VERSION"),
        logo = r#"███████╗ ██╗   ██╗  ██████╗
██╔════╝ ██║   ██║ ██╔═══██╗
█████╗   ██║   ██║ ██║   ██║
██╔══╝   ╚██╗ ██╔╝ ██║   ██║
███████╗  ╚████╔╝  ╚██████╔╝
╚══════╝   ╚═══╝    ╚═════╝"#
    )
}

#[cfg(test)]
mod tests {
    use super::welcome_line;

    #[test]
    fn welcome_line_features_the_evo_ascii_logo() {
        let welcome = welcome_line();
        assert!(welcome.contains("███████╗"), "{welcome}");
        assert!(welcome.contains("╚══════╝"), "{welcome}");
        assert!(welcome.contains(env!("CARGO_PKG_VERSION")), "{welcome}");
        assert!(welcome.contains("/help for commands"), "{welcome}");
    }
}
