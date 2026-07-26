//! General CLI argument parsing behavior.

use super::{CliArgs, help_text, parse_args};
use crate::error::CliError;
use coding_agent::api::embedding::{CodingAgentThinkingLevel, CodingAgentToolExecutionMode};
use coding_agent::api::settings::CodingAgentPresentationMode;

fn parse(values: &[&str]) -> Result<CliArgs, CliError> {
    parse_args(values.iter().map(|value| value.to_string()))
}

#[test]
fn parses_short_print_with_prompt() {
    let args = parse(&["-p", "hello"]).unwrap();
    assert!(args.print);
    assert_eq!(args.prompt.as_deref(), Some("hello"));
}

#[test]
fn default_max_turns_is_none_to_match_typescript_pi() {
    // TS `pi/packages/agent` runs `while (true)` with no turn cap. The Rust
    // CLI must keep that behavior unless `--max-turns` is explicitly passed.
    let args = parse(&["-p", "hello"]).unwrap();
    assert_eq!(args.max_turns, None);
}

#[test]
fn parses_long_print_with_prompt() {
    let args = parse(&["--print", "hello"]).unwrap();
    assert!(args.print);
    assert_eq!(args.prompt.as_deref(), Some("hello"));
}

#[test]
fn parses_prompt_after_flags() {
    let args = parse(&[
        "--model",
        "claude-haiku-4-5",
        "--api-key",
        "sk-test",
        "--system-prompt",
        "Be terse.",
        "--max-turns",
        "7",
        "-p",
        "say hi",
    ])
    .unwrap();

    assert_eq!(args.model.as_deref(), Some("claude-haiku-4-5"));
    assert_eq!(args.api_key.as_deref(), Some("sk-test"));
    assert_eq!(args.system_prompt.as_deref(), Some("Be terse."));
    assert_eq!(args.max_turns, Some(7));
    assert_eq!(args.prompt.as_deref(), Some("say hi"));
}

#[test]
fn joins_multiple_positional_words_into_one_prompt() {
    let args = parse(&["-p", "say", "hello", "now"]).unwrap();
    assert_eq!(args.prompt.as_deref(), Some("say hello now"));
}

#[test]
fn print_does_not_consume_following_option_as_prompt() {
    let args = parse(&["-p", "--model", "claude-haiku-4-5", "hello"]).unwrap();
    assert!(args.print);
    assert_eq!(args.model.as_deref(), Some("claude-haiku-4-5"));
    assert_eq!(args.prompt.as_deref(), Some("hello"));
}

#[test]
fn print_consumes_yaml_frontmatter_prompt() {
    let prompt = "---\ntitle: hello\n---\nSay hi.";
    let args = parse(&["-p", prompt]).unwrap();
    assert_eq!(args.prompt.as_deref(), Some(prompt));
}

#[test]
fn parses_help_and_version() {
    let help = parse(&["--help"]).unwrap();
    let version = parse(&["-v"]).unwrap();
    assert!(help.help);
    assert!(version.version);
}

#[test]
fn parses_typed_tui_mode() {
    assert_eq!(
        parse(&["--tui-mode", "fullscreen"]).unwrap().tui_mode,
        Some(CodingAgentPresentationMode::Fullscreen)
    );
    assert_eq!(
        parse(&["--tui-mode", "inline"]).unwrap().tui_mode,
        Some(CodingAgentPresentationMode::Inline)
    );
    assert!(matches!(
        parse(&["--tui-mode", "windowed"]),
        Err(CliError::InvalidInput(_))
    ));
}

#[test]
fn parses_list_models_flag_with_optional_search_and_json() {
    let args = parse(&["--list-models"]).unwrap();
    assert_eq!(args.list_models, Some(None));
    assert!(!args.json);

    let args = parse(&[
        "--list-models",
        "claude",
        "--provider",
        "anthropic",
        "--json",
    ])
    .unwrap();
    assert_eq!(args.list_models, Some(Some("claude".to_string())));
    assert_eq!(args.provider.as_deref(), Some("anthropic"));
    assert!(args.json);
    assert!(args.prompt.is_none());
}

#[test]
fn list_models_does_not_consume_flags_or_file_args_as_search() {
    let args = parse(&["--list-models", "--provider", "openai"]).unwrap();
    assert_eq!(args.list_models, Some(None));
    assert_eq!(args.provider.as_deref(), Some("openai"));

    let args = parse(&["--list-models", "@prompt.md"]).unwrap();
    assert_eq!(args.list_models, Some(None));
    assert_eq!(args.prompt.as_deref(), Some("@prompt.md"));
}

#[test]
fn rejects_missing_flag_values() {
    assert_eq!(
        parse(&["--model"]).unwrap_err(),
        CliError::MissingValue("--model".into())
    );
    assert_eq!(
        parse(&["--api-key"]).unwrap_err(),
        CliError::MissingValue("--api-key".into())
    );
    assert_eq!(
        parse(&["--system-prompt"]).unwrap_err(),
        CliError::MissingValue("--system-prompt".into())
    );
    assert_eq!(
        parse(&["--max-turns"]).unwrap_err(),
        CliError::MissingValue("--max-turns".into())
    );
}

#[test]
fn rejects_invalid_max_turns() {
    assert_eq!(
        parse(&["--max-turns", "0"]).unwrap_err(),
        CliError::InvalidMaxTurns("0".into())
    );
    assert_eq!(
        parse(&["--max-turns", "abc"]).unwrap_err(),
        CliError::InvalidMaxTurns("abc".into())
    );
}

#[test]
fn rejects_unknown_flags() {
    assert_eq!(
        parse(&["--definitely-unknown"]).unwrap_err(),
        CliError::UnknownFlag("--definitely-unknown".into())
    );
    assert_eq!(
        parse(&["-x"]).unwrap_err(),
        CliError::UnknownFlag("-x".into())
    );
}

#[test]
fn theme_flag_is_repeatable_and_collects_paths() {
    let args = parse(&["--theme", "a.json", "--theme", "themes/"]).unwrap();
    assert_eq!(
        args.theme_paths,
        vec!["a.json".to_string(), "themes/".to_string()]
    );
}

#[test]
fn theme_flag_requires_a_value() {
    let err = parse(&["--theme"]).unwrap_err();
    let msg = format!("{err}");
    assert!(msg.contains("--theme"), "{msg}");
}

#[test]
fn parses_agent_runtime_feature_flags_and_defaults() {
    let args = parse(&[
        "-p",
        "hello",
        "--thinking",
        "high",
        "--tool-execution",
        "sequential",
        "--skills",
        "dir1",
        "--skills",
        "dir2",
        "--prompt-templates",
        "templates",
        "--skill",
        "rust",
        "--template-arg",
        "arg1",
        "--template-arg",
        "arg2",
    ])
    .unwrap();

    assert_eq!(args.thinking, Some(CodingAgentThinkingLevel::High));
    assert_eq!(
        args.tool_execution,
        Some(CodingAgentToolExecutionMode::Sequential)
    );
    assert_eq!(args.skills, ["dir1", "dir2"]);
    assert_eq!(args.prompt_templates, ["templates"]);
    assert_eq!(args.skill.as_deref(), Some("rust"));
    assert_eq!(args.template_args, ["arg1", "arg2"]);

    let defaults = parse(&["-p", "hello"]).unwrap();
    assert_eq!(defaults.thinking, None);
    assert_eq!(defaults.tool_execution, None);
    assert!(defaults.skills.is_empty());
    assert!(defaults.prompt_templates.is_empty());
    assert!(defaults.skill.is_none());
    assert!(defaults.prompt_template.is_none());
    assert!(defaults.template_args.is_empty());
}

#[test]
fn parses_product_resource_and_tool_flags() {
    let args = parse(&[
        "-p",
        "hello",
        "--provider",
        "anthropic",
        "--append-system-prompt",
        "A",
        "--append-system-prompt",
        "B",
        "--tools",
        "read,bash",
        "--exclude-tools",
        "bash",
        "--models",
        "claude-*:high",
        "--no-context-files",
        "--no-skills",
        "--no-prompt-templates",
        "--no-themes",
        "--verbose",
        "--offline",
    ])
    .unwrap();

    assert_eq!(args.provider.as_deref(), Some("anthropic"));
    assert_eq!(args.append_system_prompt, ["A", "B"]);
    assert_eq!(args.tools, ["read", "bash"]);
    assert_eq!(args.exclude_tools, ["bash"]);
    assert_eq!(args.models.as_deref(), Some("claude-*:high"));
    assert!(args.no_context_files);
    assert!(args.no_skills);
    assert!(args.no_prompt_templates);
    assert!(args.no_themes);
    assert!(args.verbose);
    assert!(args.offline);
}

#[test]
fn parses_prompt_template_with_arguments() {
    let args = parse(&[
        "-p",
        "hello",
        "--prompt-template",
        "review",
        "--template-arg",
        "arg1",
        "--template-arg",
        "arg2",
    ])
    .unwrap();

    assert_eq!(args.prompt_template.as_deref(), Some("review"));
    assert_eq!(args.template_args, ["arg1", "arg2"]);
}

#[test]
fn rejects_invalid_agent_runtime_feature_values_and_conflicts() {
    for values in [
        &["-p", "hello", "--thinking", "extreme"][..],
        &["-p", "hello", "--tool-execution", "serial"][..],
        &[
            "-p",
            "hello",
            "--skill",
            "rust",
            "--prompt-template",
            "review",
        ][..],
    ] {
        assert!(parse(values).is_err(), "expected rejection for {values:?}");
    }
}

#[test]
fn help_lists_agent_runtime_feature_flags() {
    let text = help_text();
    for flag in [
        "--thinking",
        "--tool-execution",
        "--skills",
        "--prompt-templates",
        "--skill",
        "--prompt-template",
        "--template-arg",
        "--tui-mode",
    ] {
        assert!(text.contains(flag), "help must list {flag}");
    }
}
