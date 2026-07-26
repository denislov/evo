//! Real-process contracts owned by the terminal application.

use std::path::Path;
use std::process::{Command, Output, Stdio};

fn isolated_command(project_dir: &Path, runtime_dir: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_coding-agent"));
    command
        .current_dir(project_dir)
        .stdin(Stdio::null())
        .env("EVO_DIR", runtime_dir)
        .env_remove("agent_DIR")
        .env_remove("EVO_SESSION_DIR");
    command
}

fn run(args: &[&str]) -> Output {
    let root = tempfile::tempdir().expect("create isolated CLI process root");
    let project_dir = root.path().join("project");
    let runtime_dir = root.path().join("runtime");
    std::fs::create_dir(&project_dir).expect("create isolated project directory");

    isolated_command(&project_dir, &runtime_dir)
        .args(args)
        .output()
        .expect("coding-agent binary should run")
}

fn stdout(output: &Output) -> &str {
    std::str::from_utf8(&output.stdout).expect("CLI stdout must be UTF-8")
}

fn stderr(output: &Output) -> &str {
    std::str::from_utf8(&output.stderr).expect("CLI stderr must be UTF-8")
}

fn assert_success(output: &Output) {
    assert!(
        output.status.success(),
        "expected CLI success\nstatus: {}\nstdout:\n{}\nstderr:\n{}",
        output.status,
        stdout(output),
        stderr(output)
    );
    assert!(stderr(output).is_empty(), "stderr:\n{}", stderr(output));
}

#[test]
fn help_is_rendered_by_the_real_binary() {
    let output = run(&["--help"]);

    assert_success(&output);
    assert!(stdout(&output).starts_with(concat!(
        "coding-agent ",
        env!("CARGO_PKG_VERSION"),
        "\n\nUsage:"
    )));
    assert!(stdout(&output).contains("coding-agent -p <prompt>"));
    assert!(stdout(&output).contains("--mode <mode>"));
    assert!(stdout(&output).contains("--tui-mode <mode>"));
}

#[test]
fn version_is_rendered_by_the_real_binary() {
    let output = run(&["--version"]);

    assert_success(&output);
    assert_eq!(stdout(&output), concat!(env!("CARGO_PKG_VERSION"), "\n"));
}

#[test]
fn model_list_text_is_rendered_without_starting_a_prompt() {
    let output = run(&["--list-models", "claude"]);

    assert_success(&output);
    assert!(stdout(&output).starts_with("provider"));
    assert!(stdout(&output).contains("anthropic"));
    assert!(stdout(&output).contains("claude"));
}

#[test]
fn model_list_json_honors_the_provider_filter() {
    let output = run(&["--list-models", "--provider", "anthropic", "--json"]);

    assert_success(&output);
    let rows: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("model list must be valid JSON");
    let rows = rows.as_array().expect("model list JSON must be an array");
    assert!(!rows.is_empty());
    assert!(rows.iter().all(|row| row["provider"] == "anthropic"));
    assert!(rows.iter().all(|row| row.get("model").is_some()));
}

#[test]
fn model_list_is_read_only_for_session_selection() {
    let root = tempfile::tempdir().expect("create isolated CLI process root");
    let project_dir = root.path().join("project");
    let runtime_dir = root.path().join("runtime");
    let session_dir = root.path().join("sessions");
    std::fs::create_dir(&project_dir).expect("create isolated project directory");

    let output = isolated_command(&project_dir, &runtime_dir)
        .args([
            "--session-id",
            "read-only-models",
            "--session-dir",
            session_dir.to_str().expect("temporary path must be UTF-8"),
            "--list-models",
            "--provider",
            "anthropic",
        ])
        .output()
        .expect("coding-agent binary should run");

    assert_success(&output);
    assert!(
        !runtime_dir.exists(),
        "--list-models must not initialize the product runtime directory"
    );
    assert!(
        !session_dir.exists(),
        "--list-models must not create or reserve a session"
    );
}

#[test]
fn print_mode_rejects_a_missing_prompt_on_stderr() {
    let output = run(&["-p"]);

    assert_eq!(output.status.code(), Some(1));
    assert!(stdout(&output).is_empty());
    assert_eq!(stderr(&output), "A prompt is required.\n");
}

#[test]
fn unknown_model_uses_the_safe_public_error() {
    let output = run(&["--model", "missing-model", "-p", "hello"]);

    assert_eq!(output.status.code(), Some(1));
    assert!(stdout(&output).is_empty());
    assert_eq!(stderr(&output), "The requested model is not available.\n");
    assert!(!stderr(&output).contains("missing-model"));
}

#[test]
fn default_invocation_routes_to_the_interactive_adapter() {
    let output = run(&[]);

    assert_eq!(output.status.code(), Some(1));
    assert!(stdout(&output).is_empty());
    assert_eq!(stderr(&output), "interactive mode requires a TTY\n");
}
