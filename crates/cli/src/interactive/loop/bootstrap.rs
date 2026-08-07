use crate::interactive::app::PromptContext;
use coding_agent::api::embedding::CodingAgentResourceCommandKind;

use super::RESIZE_POLL_INTERVAL;

pub(super) struct ResizeSource {
    #[cfg(unix)]
    platform: Option<tokio::signal::unix::Signal>,
    fallback: tokio::time::Interval,
}

impl ResizeSource {
    pub(super) fn new() -> Self {
        let mut fallback = tokio::time::interval(RESIZE_POLL_INTERVAL);
        fallback.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        Self {
            #[cfg(unix)]
            platform: tokio::signal::unix::signal(tokio::signal::unix::SignalKind::window_change())
                .ok(),
            fallback,
        }
    }

    pub(super) async fn recv(&mut self) {
        #[cfg(unix)]
        tokio::select! {
            _ = receive_platform_resize(&mut self.platform), if self.platform.is_some() => {}
            _ = self.fallback.tick() => {}
        }
        #[cfg(not(unix))]
        self.fallback.tick().await;
    }
}

#[cfg(unix)]
async fn receive_platform_resize(signal: &mut Option<tokio::signal::unix::Signal>) {
    match signal {
        Some(signal) => {
            let _ = signal.recv().await;
        }
        None => std::future::pending().await,
    }
}

/// Print startup resource summary to stderr before the TUI takes over.
/// Respects the `quiet_startup` setting.
pub(super) fn print_startup_banner(prompt_context: &PromptContext) {
    if prompt_context
        .settings_snapshot()
        .presentation
        .quiet_startup
    {
        return;
    }
    let cwd = prompt_context.cwd.clone();
    let cwd = cwd.canonicalize().unwrap_or(cwd);

    if !prompt_context.context_files.is_empty() {
        let names: Vec<String> = prompt_context
            .context_files
            .iter()
            .map(|path| {
                if let Some(parent) = path.parent()
                    && parent == cwd
                {
                    path.file_name()
                        .map(|n| n.to_string_lossy().to_string())
                        .unwrap_or_else(|| path.display().to_string())
                } else {
                    path.display().to_string()
                }
            })
            .collect();
        eprintln!("[Context] {}", names.join(", "));
    }

    let skill_names: Vec<&str> = prompt_context
        .resource_commands
        .iter()
        .filter(|resource| {
            resource.kind == CodingAgentResourceCommandKind::Skill && resource.model_invocable
        })
        .map(|resource| resource.name.as_str())
        .collect();
    if !skill_names.is_empty() {
        eprintln!("[Skills] {}", skill_names.join(", "));
    }
}

pub(super) fn print_exit_resume_hint(active_session_id: Option<&str>) {
    if let Some(session_id) = active_session_id {
        eprintln!("To resume this session: evo --session {session_id}");
    }
}
