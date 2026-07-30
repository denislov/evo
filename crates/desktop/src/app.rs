mod native_perf;
mod native_shell;

use std::sync::Arc;
use std::time::Duration;

use coding_agent::api::embedding::{
    CodingAgentEmbeddingOptions, CodingAgentWorkspaceSelection, global_config_directory,
};
use gpui::{
    App, Bounds, Context, IntoElement, ParentElement as _, Render, Styled as _, Window,
    WindowBounds, WindowOptions, div, point, prelude::*, px, rgb, size,
};
use gpui_component::{Root, Theme, ThemeMode};
use gpui_platform::application;

use self::native_shell::{NativeShell, NativeShellInit};
use crate::preferences::{
    DesktopPreferences, PreferenceLoad, PreferenceRecovery, PreferenceStore, PreferenceWriter,
    resolve_scratch_workspace,
};
use crate::projection::DesktopProjection;
use crate::runtime::{
    DesktopRuntimeBridge, DesktopRuntimeHydratedSnapshot, DesktopRuntimeStartError,
};
use crate::shell::{MONOSPACE_FONT_FAMILY, SemanticTheme, UI_FONT_FAMILY, truncate_label};

const BOOTSTRAP_POLL_INTERVAL: Duration = Duration::from_millis(16);

struct StartupFailure {
    message: String,
}

impl Render for StartupFailure {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let theme = SemanticTheme::GEEK_DARK;
        div()
            .size_full()
            .p_8()
            .flex()
            .flex_col()
            .gap_4()
            .font_family(UI_FONT_FAMILY)
            .bg(rgb(theme.canvas.value()))
            .text_color(rgb(theme.text.value()))
            .child(
                div()
                    .text_xl()
                    .text_color(rgb(theme.danger.value()))
                    .child("× Desktop runtime failed to start"),
            )
            .child(
                div()
                    .font_family(MONOSPACE_FONT_FAMILY)
                    .child(self.message.clone()),
            )
    }
}

pub(crate) fn run(options: crate::DesktopApplicationOptions) {
    let crate::DesktopApplicationOptions {
        cwd,
        projectless,
        session_id,
    } = options;
    let native_replay = native_perf::request();
    // Product-owned Evo vectors and bundled Lucide controls share one explicit
    // application asset boundary. Without it GPUI's SVG elements render
    // nothing.
    application()
        .with_assets(crate::assets::DesktopAssets::new())
        .run(move |cx: &mut App| {
            gpui_component::init(cx);
            crate::actions::bind_keys(cx);
            Theme::change(ThemeMode::Dark, None, cx);

            if let Some(request) = match native_replay {
                Ok(request) => request,
                Err(error) => {
                    eprintln!("desktop: native replay configuration failed: {error}");
                    cx.quit();
                    return;
                }
            } {
                if let Err(error) = native_perf::open(cx, request) {
                    eprintln!("desktop: native replay failed: {error}");
                    cx.quit();
                }
                return;
            }

            cx.spawn(async move |cx| {
                let global_config_dir = global_config_directory();
                let store = PreferenceStore::new(&global_config_dir);
                let (mut loaded, mut notice) = match store.load() {
                    Ok(loaded) => {
                        let notice = preference_notice(&loaded);
                        (loaded, notice)
                    }
                    Err(error) => (
                        PreferenceLoad {
                            preferences: DesktopPreferences::default(),
                            recovery: None,
                        },
                        Some(format!("Preferences could not be loaded: {error}")),
                    ),
                };
                let scratch_id_was_missing = loaded.preferences.scratch_workspace_id.is_none();
                if let Err(error) =
                    resolve_scratch_workspace(&global_config_dir, &mut loaded.preferences)
                {
                    let _ = open_failure(
                        format!("scratch workspace initialization failed: {error}"),
                        cx,
                    );
                    return;
                }
                let projectless_workspace_selection = CodingAgentWorkspaceSelection::projectless(
                    loaded
                        .preferences
                        .scratch_workspace_id
                        .clone()
                        .expect("scratch resolution must persist its workspace id"),
                );
                let workspace_selection = if projectless {
                    projectless_workspace_selection.clone()
                } else {
                    CodingAgentWorkspaceSelection::project(cwd)
                };
                let embedding_options =
                    match CodingAgentEmbeddingOptions::for_workspace(workspace_selection) {
                        Ok(options) => options,
                        Err(error) => {
                            let _ = open_failure(
                                format!("workspace initialization failed: {error}"),
                                cx,
                            );
                            return;
                        }
                    };
                let writer = match PreferenceWriter::spawn(store) {
                    Ok(writer) => Some(writer),
                    Err(error) => {
                        notice = Some(format!("Preference writer unavailable: {error}"));
                        None
                    }
                };
                if scratch_id_was_missing && let Some(writer) = writer.as_ref() {
                    drop(writer.schedule(loaded.preferences.clone()));
                }

                let bootstrap = DesktopRuntimeBridge::spawn(embedding_options);
                let mut bootstrap = match bootstrap {
                    Ok(bootstrap) => bootstrap,
                    Err(error) => {
                        let _ = open_failure(startup_error_message(&error), cx);
                        return;
                    }
                };
                let (mut runtime, snapshot) = loop {
                    match bootstrap.try_ready() {
                        Ok(Some(ready)) => break ready,
                        Ok(None) => {
                            cx.background_executor()
                                .timer(BOOTSTRAP_POLL_INTERVAL)
                                .await;
                        }
                        Err(error) => {
                            let _ = open_failure(startup_error_message(&error), cx);
                            return;
                        }
                    }
                };

                let options = window_options(&loaded.preferences);
                let requested = match session_id.as_deref() {
                    Some(session_id) => {
                        match open_requested_session(&mut runtime, session_id).await {
                            Ok(snapshot) => match DesktopProjection::new(snapshot) {
                                Ok(projection) => Some(projection),
                                Err(issue) => {
                                    let _ = open_failure(
                                        format!(
                                            "projection initialization failed: {}",
                                            issue.message
                                        ),
                                        cx,
                                    );
                                    return;
                                }
                            },
                            Err(message) => {
                                notice = Some(message);
                                None
                            }
                        }
                    }
                    None => None,
                };
                if let Err(error) = cx.open_window(options, |window, cx| {
                    window.set_window_title("evo · native coding agent");
                    let view = cx.new(|cx| {
                        NativeShell::new(
                            NativeShellInit {
                                runtime,
                                project: snapshot.project,
                                projection: requested,
                                projectless_workspace_selection,
                                global_skills: Arc::from(
                                    coding_agent::api::embedding::global_skill_catalog(),
                                ),
                                preferences: loaded.preferences,
                                preference_writer: writer,
                                preference_notice: notice,
                                initial_session_id: None,
                            },
                            window,
                            cx,
                        )
                    });
                    view.update(cx, |shell, cx| shell.request_session_catalog(cx));
                    cx.new(|cx| Root::new(view, window, cx))
                }) {
                    eprintln!("desktop: failed to open native window: {error}");
                }
            })
            .detach();
        });
}

async fn open_requested_session(
    runtime: &mut DesktopRuntimeBridge,
    session_id: &str,
) -> Result<DesktopRuntimeHydratedSnapshot, String> {
    const BOOTSTRAP_OPEN_COMMAND_ID: u64 = u64::MAX;
    runtime
        .open_session_for_bootstrap(BOOTSTRAP_OPEN_COMMAND_ID, session_id)
        .await
}

fn preference_notice(load: &PreferenceLoad) -> Option<String> {
    match &load.recovery {
        None => None,
        Some(PreferenceRecovery::CorruptJson) => {
            Some("Preferences were corrupt; bounded defaults are active.".into())
        }
        Some(PreferenceRecovery::UnsupportedSchema { found }) => Some(format!(
            "Preference schema {found} is unsupported; bounded defaults are active."
        )),
        Some(PreferenceRecovery::Oversized { bytes }) => Some(format!(
            "Preferences were oversized ({bytes} bytes); bounded defaults are active."
        )),
    }
}

fn window_options(preferences: &DesktopPreferences) -> WindowOptions {
    let geometry = &preferences.window;
    let bounds = Bounds {
        origin: point(px(geometry.x as f32), px(geometry.y as f32)),
        size: size(px(geometry.width as f32), px(geometry.height as f32)),
    };
    WindowOptions {
        window_bounds: Some(if geometry.maximized {
            WindowBounds::Maximized(bounds)
        } else {
            WindowBounds::Windowed(bounds)
        }),
        window_min_size: Some(size(px(640.), px(480.))),
        app_id: Some("evo.desktop".into()),
        ..WindowOptions::default()
    }
}

fn open_failure(
    message: String,
    cx: &mut gpui::AsyncApp,
) -> Result<(), Box<dyn std::error::Error>> {
    cx.open_window(WindowOptions::default(), |window, cx| {
        let view = cx.new(|_| StartupFailure { message });
        cx.new(|cx| Root::new(view, window, cx))
    })?;
    Ok(())
}

fn startup_error_message(error: &DesktopRuntimeStartError) -> String {
    match error {
        DesktopRuntimeStartError::Spawn(_) => "failed to start desktop runtime thread".into(),
        DesktopRuntimeStartError::Initialization { code, .. } => format!(
            "desktop runtime initialization failed ({})",
            truncate_label(code, 28)
        ),
        DesktopRuntimeStartError::InitializationChannelClosed => {
            "desktop runtime initialization channel closed".into()
        }
        DesktopRuntimeStartError::InitializationThreadPanicked => {
            "desktop runtime initialization thread panicked".into()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn startup_errors_do_not_expose_secret_bodies() {
        const SECRET: &str = "desktop-secret-canary";
        let startup = startup_error_message(&DesktopRuntimeStartError::Initialization {
            code: "provider_initialization".into(),
            message: format!("provider token={SECRET}"),
        });
        assert!(!startup.contains(SECRET));
        assert!(startup.contains("provider_initialization"));
    }
}
