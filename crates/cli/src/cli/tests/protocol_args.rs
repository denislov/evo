//! RPC/print mode argument selection behavior.

use super::{CliMode, parse_args};
use crate::error::CliError;

#[test]
fn print_flag_selects_print_mode() {
    let args = parse_args(vec!["-p".to_string(), "hello".to_string()]).unwrap();
    assert_eq!(args.mode, CliMode::Print);
    assert_eq!(args.prompt.as_deref(), Some("hello"));
}

#[test]
fn explicit_json_mode_accepts_positional_prompt() {
    let args = parse_args(vec![
        "--mode".to_string(),
        "json".to_string(),
        "hello world".to_string(),
    ])
    .unwrap();
    assert_eq!(args.mode, CliMode::Json);
    assert_eq!(args.prompt.as_deref(), Some("hello world"));
    assert!(!args.print);
}

#[test]
fn explicit_rpc_mode_accepts_without_prompt() {
    let args = parse_args(vec!["--mode".to_string(), "rpc".to_string()]).unwrap();
    assert_eq!(args.mode, CliMode::Rpc);
    assert_eq!(args.prompt, None);
}

#[test]
fn print_flag_cannot_select_json_mode() {
    let err = parse_args(vec![
        "--mode".to_string(),
        "json".to_string(),
        "-p".to_string(),
        "hello".to_string(),
    ])
    .unwrap_err();
    assert_eq!(
        err,
        CliError::InvalidInput("--print can only be combined with --mode print".into())
    );
}

#[test]
fn rpc_mode_rejects_positional_prompt_for_m5() {
    let err = parse_args(vec![
        "--mode".to_string(),
        "rpc".to_string(),
        "hello".to_string(),
    ])
    .unwrap_err();
    assert_eq!(
        err,
        CliError::InvalidInput(
            "unsupported mode input: rpc does not accept positional prompt".into()
        )
    );
}

#[test]
fn unknown_mode_is_rejected() {
    let err = parse_args(vec!["--mode".to_string(), "xml".to_string()]).unwrap_err();
    assert_eq!(err, CliError::InvalidInput("unknown mode: xml".into()));
}
