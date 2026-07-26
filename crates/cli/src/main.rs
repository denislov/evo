mod cli;
mod error;
mod interactive;
mod output;
mod protocol;
mod rpc;

use std::io::IsTerminal;

use cli::{CliMode, help_text, parse_args};
use coding_agent::api::embedding::{CodingAgentApplicationStartup, CodingAgentInteractiveStartup};
use coding_agent::api::operation::CodingAgentPromptExecution;
use output::CliOutput;

#[tokio::main]
async fn main() {
    let output = run(std::env::args().skip(1)).await;

    if !output.stdout.is_empty() {
        print!("{}", output.stdout);
    }
    if !output.stderr.is_empty() {
        eprint!("{}", output.stderr);
    }

    std::process::exit(output.exit_code);
}

async fn run(args: impl IntoIterator<Item = String>) -> CliOutput {
    let parsed = match parse_args(args) {
        Ok(parsed) => parsed,
        Err(error) => return failure(error),
    };

    let cwd = match std::env::current_dir() {
        Ok(cwd) => cwd,
        Err(error) => return failure(format!("failed to resolve current directory: {error}")),
    };

    if parsed.mode == CliMode::Rpc {
        let application = match CodingAgentApplicationStartup::resolve(cwd) {
            Ok(application) => application,
            Err(error) => return failure(error),
        };
        return match rpc::run_rpc_mode_stdio(application).await {
            Ok(()) => success(String::new()),
            Err(error) => failure(error),
        };
    }

    let stdin = if std::io::stdin().is_terminal() {
        None
    } else {
        match cli::io::read_text_from(std::io::stdin().lock(), cli::io::MAX_STDIN_BYTES) {
            Ok(input) => Some(input),
            Err(error) => return failure(format!("failed to read stdin: {error}")),
        }
    };

    if parsed.help {
        return success(help_text());
    }
    if parsed.version {
        return success(format!("{}\n", env!("CARGO_PKG_VERSION")));
    }
    if let Some(search) = parsed.list_models.as_ref() {
        return match cli::list_models::list_models_output(
            search.as_deref(),
            parsed.provider.as_deref(),
            parsed.json,
        ) {
            Ok(stdout) => success(stdout),
            Err(error) => failure(error),
        };
    }

    let invocation = parsed.invocation_options();
    if !parsed.print && !parsed.mode_explicit {
        let startup = match CodingAgentInteractiveStartup::resolve(cwd, invocation) {
            Ok(startup) => startup,
            Err(error) => return failure(error),
        };
        return interactive::run_interactive_mode(startup).await;
    }

    let mode = invocation.prompt_mode;
    let presentation_cwd = cwd.clone();
    let preparation = match CodingAgentPromptExecution::prepare(cwd, invocation, stdin) {
        Ok(preparation) => preparation,
        Err(error) => return failure(error),
    };
    cli::headless::run(
        mode,
        preparation.execution,
        preparation.diagnostics,
        &presentation_cwd,
    )
    .await
}

fn success(stdout: String) -> CliOutput {
    CliOutput::success(stdout)
}

fn failure(error: impl std::fmt::Display) -> CliOutput {
    CliOutput::failure(error)
}
