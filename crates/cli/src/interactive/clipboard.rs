use std::io::Write;
use std::process::{Command, Stdio};

pub(super) trait ClipboardSink: Send + Sync {
    fn copy_text(&self, text: &str) -> Result<(), String>;
}

#[derive(Debug)]
pub(super) struct SystemClipboard;

impl ClipboardSink for SystemClipboard {
    fn copy_text(&self, text: &str) -> Result<(), String> {
        system_copy_to_clipboard(text)
    }
}

fn system_copy_to_clipboard(text: &str) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        copy_with_command("pbcopy", &[], text)
    }

    #[cfg(target_os = "windows")]
    {
        copy_with_command(
            "powershell",
            &["-NoProfile", "-Command", "Set-Clipboard"],
            text,
        )
    }

    #[cfg(all(not(target_os = "macos"), not(target_os = "windows")))]
    {
        let attempts: &[(&str, &[&str])] = &[
            ("wl-copy", &[]),
            ("xclip", &["-selection", "clipboard"]),
            ("xsel", &["--clipboard", "--input"]),
            ("termux-clipboard-set", &[]),
        ];
        let mut errors = Vec::new();
        for (program, args) in attempts {
            match copy_with_command(program, args, text) {
                Ok(()) => return Ok(()),
                Err(error) => errors.push(error),
            }
        }
        Err(format!(
            "Failed to copy to clipboard: {}",
            errors.join("; ")
        ))
    }
}

fn copy_with_command(program: &str, args: &[&str], text: &str) -> Result<(), String> {
    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|error| format!("{program}: {error}"))?;

    if let Some(stdin) = child.stdin.as_mut() {
        stdin
            .write_all(text.as_bytes())
            .map_err(|error| format!("{program}: {error}"))?;
    }

    let status = child
        .wait()
        .map_err(|error| format!("{program}: {error}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("{program}: exited with status {status}"))
    }
}
