//! Validated, shell-free external-editor process adapter.

use std::ffi::OsString;
use std::path::Path;
use std::process::{Command, Stdio};
use std::thread;

use crate::preferences::ExternalEditorPreference;

const MAX_EDITOR_PROGRAM_BYTES: usize = 4 * 1024;
const MAX_EDITOR_ARGUMENTS: usize = 32;
const MAX_EDITOR_ARGUMENT_BYTES: usize = 4 * 1024;
const MAX_EDITOR_ARGUMENT_TOTAL_BYTES: usize = 16 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
struct ExternalEditorConfig {
    program: OsString,
    args: Vec<OsString>,
}

impl TryFrom<&ExternalEditorPreference> for ExternalEditorConfig {
    type Error = ExternalEditorConfigError;

    fn try_from(preference: &ExternalEditorPreference) -> Result<Self, Self::Error> {
        if preference.program.trim().is_empty() {
            return Err(ExternalEditorConfigError::EmptyProgram);
        }
        if preference.program.len() > MAX_EDITOR_PROGRAM_BYTES || preference.program.contains('\0')
        {
            return Err(ExternalEditorConfigError::InvalidProgram);
        }
        let executable = Path::new(&preference.program)
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or(preference.program.as_str())
            .to_ascii_lowercase();
        if matches!(
            executable.as_str(),
            "sh" | "bash" | "zsh" | "fish" | "cmd" | "cmd.exe" | "powershell" | "pwsh"
        ) {
            return Err(ExternalEditorConfigError::ShellProgram);
        }
        if preference.args.len() > MAX_EDITOR_ARGUMENTS {
            return Err(ExternalEditorConfigError::TooManyArguments);
        }
        let mut total = 0usize;
        for argument in &preference.args {
            if argument.len() > MAX_EDITOR_ARGUMENT_BYTES || argument.contains('\0') {
                return Err(ExternalEditorConfigError::InvalidArgument);
            }
            total = total
                .checked_add(argument.len())
                .ok_or(ExternalEditorConfigError::ArgumentsTooLarge)?;
            if total > MAX_EDITOR_ARGUMENT_TOTAL_BYTES {
                return Err(ExternalEditorConfigError::ArgumentsTooLarge);
            }
        }
        Ok(Self {
            program: OsString::from(&preference.program),
            args: preference.args.iter().map(OsString::from).collect(),
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub(crate) enum ExternalEditorConfigError {
    #[error("external editor program must not be empty")]
    EmptyProgram,
    #[error("external editor program is invalid or oversized")]
    InvalidProgram,
    #[error("external editor must be an editor executable, not a command shell")]
    ShellProgram,
    #[error("external editor has too many arguments")]
    TooManyArguments,
    #[error("external editor argument is invalid or oversized")]
    InvalidArgument,
    #[error("external editor arguments exceed the aggregate limit")]
    ArgumentsTooLarge,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ExternalEditorInvocation {
    program: OsString,
    args: Vec<OsString>,
}

fn editor_invocation(
    preference: &ExternalEditorPreference,
    validated_path: &Path,
) -> Result<ExternalEditorInvocation, ExternalEditorConfigError> {
    let config = ExternalEditorConfig::try_from(preference)?;
    let mut args = config.args;
    args.push(validated_path.as_os_str().to_owned());
    Ok(ExternalEditorInvocation {
        program: config.program,
        args,
    })
}

pub(crate) fn validate_external_editor_preference(
    preference: &ExternalEditorPreference,
) -> Result<(), ExternalEditorConfigError> {
    ExternalEditorConfig::try_from(preference).map(|_| ())
}

pub(crate) fn launch_external_editor(
    preference: &ExternalEditorPreference,
    validated_path: &Path,
) -> Result<(), ExternalEditorLaunchError> {
    let invocation = editor_invocation(preference, validated_path)?;
    let mut child = Command::new(invocation.program)
        .args(invocation.args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|_| ExternalEditorLaunchError::Unavailable)?;
    thread::Builder::new()
        .name("evo-desktop-editor-reaper".into())
        .spawn(move || {
            let _ = child.wait();
        })
        .map_err(|_| ExternalEditorLaunchError::ReaperUnavailable)?;
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub(crate) enum ExternalEditorLaunchError {
    #[error(transparent)]
    Configuration(#[from] ExternalEditorConfigError),
    #[error("external editor executable is unavailable")]
    Unavailable,
    #[error("external editor process reaper is unavailable")]
    ReaperUnavailable,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn preference(program: &str, args: &[&str]) -> ExternalEditorPreference {
        ExternalEditorPreference {
            program: program.into(),
            args: args.iter().map(|argument| (*argument).into()).collect(),
        }
    }

    #[test]
    fn invocation_keeps_literal_arguments_and_target_path_separate() {
        let preference = preference("code", &["--reuse-window", "$(touch should-not-run)"]);
        let path = PathBuf::from("project/odd;name $(still-an-argument).rs");
        let invocation = editor_invocation(&preference, &path).unwrap();
        assert_eq!(invocation.program, OsString::from("code"));
        assert_eq!(
            invocation.args,
            vec![
                OsString::from("--reuse-window"),
                OsString::from("$(touch should-not-run)"),
                path.into_os_string(),
            ]
        );
    }

    #[test]
    fn validation_rejects_shells_nuls_and_argument_pressure() {
        assert_eq!(
            validate_external_editor_preference(&preference("/bin/sh", &["-c"])),
            Err(ExternalEditorConfigError::ShellProgram)
        );
        assert_eq!(
            validate_external_editor_preference(&preference("code", &["bad\0argument"])),
            Err(ExternalEditorConfigError::InvalidArgument)
        );
        let pressure = ExternalEditorPreference {
            program: "code".into(),
            args: vec!["argument".into(); MAX_EDITOR_ARGUMENTS + 1],
        };
        assert_eq!(
            validate_external_editor_preference(&pressure),
            Err(ExternalEditorConfigError::TooManyArguments)
        );
    }

    #[test]
    fn launch_reports_missing_executable_as_typed_error() {
        let missing = preference("/definitely/missing/evo-editor", &[]);
        assert_eq!(
            launch_external_editor(
                &missing,
                &PathBuf::from("/definitely/missing/evo-editor-target"),
            ),
            Err(ExternalEditorLaunchError::Unavailable)
        );
    }
}
