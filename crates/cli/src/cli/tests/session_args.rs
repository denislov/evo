//! Session-specific CLI argument behavior.

use super::{CliArgs, help_text, parse_args};
use crate::error::CliError;

fn parse(values: &[&str]) -> Result<CliArgs, CliError> {
    parse_args(values.iter().map(|value| value.to_string()))
}

#[test]
fn parses_session_flags() {
    let args = parse(&[
        "-p",
        "hi",
        "--continue",
        "--session-dir",
        "/tmp/sessions",
        "--name",
        "work",
    ])
    .unwrap();
    assert!(args.continue_session);
    assert_eq!(args.session_dir.as_deref(), Some("/tmp/sessions"));
    assert_eq!(args.name.as_deref(), Some("work"));
}

#[test]
fn rejects_no_session_with_session_target() {
    let err = parse(&["-p", "hi", "--no-session", "--continue"]).unwrap_err();
    assert_eq!(
        err.to_string(),
        "--no-session cannot be combined with session selection flags"
    );
}

#[test]
fn help_mentions_session_flags() {
    let help = help_text();
    assert!(help.contains("--continue"));
    assert!(help.contains("--session <path|id>"));
    assert!(help.contains("--no-session"));
}

#[test]
fn parses_short_continue_and_resume() {
    let args = parse(&["-c", "-p", "hi"]).unwrap();
    assert!(args.continue_session);

    let args = parse(&["-r", "-p", "hi"]).unwrap();
    assert!(args.resume);
}

#[test]
fn parses_session_id() {
    let args = parse(&["-p", "hi", "--session-id", "my-session-id"]).unwrap();
    assert_eq!(args.session_id.as_deref(), Some("my-session-id"));
}

#[test]
fn parses_fork() {
    let args = parse(&["-p", "hi", "--fork", "/tmp/session.jsonl"]).unwrap();
    assert_eq!(args.fork.as_deref(), Some("/tmp/session.jsonl"));
}

#[test]
fn parses_short_name() {
    let args = parse(&["-p", "hi", "-n", "my-run"]).unwrap();
    assert_eq!(args.name.as_deref(), Some("my-run"));
}

#[test]
fn parses_session_path() {
    let args = parse(&["-p", "hi", "--session", "/tmp/my-session.jsonl"]).unwrap();
    assert_eq!(args.session.as_deref(), Some("/tmp/my-session.jsonl"));
}

#[test]
fn rejects_no_session_with_name() {
    let err = parse(&["-p", "hi", "--no-session", "--name", "test"]).unwrap_err();
    assert_eq!(
        err.to_string(),
        "--no-session cannot be combined with session selection flags"
    );
}

#[test]
fn rejects_no_session_with_fork() {
    let err = parse(&["-p", "hi", "--no-session", "--fork", "test"]).unwrap_err();
    assert_eq!(
        err.to_string(),
        "--no-session cannot be combined with session selection flags"
    );
}
