mod native_perf;
mod native_shell;

use std::time::Duration;

use coding_agent::api::embedding::CodingAgentEmbeddingOptions;
use gpui::{
    App, Application, Bounds, Context, IntoElement, ParentElement as _, Render, Styled as _, Timer,
    Window, WindowBounds, WindowOptions, div, point, prelude::*, px, rgb, size,
};
use gpui_component::{Root, Theme, ThemeMode};

use self::native_shell::{NativeShell, NativeShellInit};
use crate::preferences::{
    DesktopPreferences, PreferenceLoad, PreferenceRecovery, PreferenceStore, PreferenceWriter,
};
use crate::projection::DesktopProjection;
use crate::runtime::{DesktopRuntimeBridge, DesktopRuntimeStartError};
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
    let crate::DesktopApplicationOptions { cwd, session_id } = options;
    let native_performance_replay = native_perf::enabled();
    Application::new().run(move |cx: &mut App| {
        gpui_component::init(cx);
        crate::actions::bind_keys(cx);
        Theme::change(ThemeMode::Dark, None, cx);

        if native_performance_replay {
            if let Err(error) = native_perf::open(cx) {
                eprintln!("desktop: native performance replay failed: {error}");
                cx.quit();
            }
            return;
        }

        let bootstrap = DesktopRuntimeBridge::spawn(CodingAgentEmbeddingOptions::new(cwd));
        cx.spawn(async move |cx| {
            let mut bootstrap = match bootstrap {
                Ok(bootstrap) => bootstrap,
                Err(error) => {
                    let _ = open_failure(startup_error_message(&error), cx);
                    return;
                }
            };
            let (runtime, snapshot) = loop {
                match bootstrap.try_ready() {
                    Ok(Some(ready)) => break ready,
                    Ok(None) => {
                        Timer::after(BOOTSTRAP_POLL_INTERVAL).await;
                    }
                    Err(error) => {
                        let _ = open_failure(startup_error_message(&error), cx);
                        return;
                    }
                }
            };

            let store = PreferenceStore::new(&snapshot.project.global_config_dir);
            let (loaded, mut notice) = match store.load() {
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
            let writer = match PreferenceWriter::spawn(store) {
                Ok(writer) => Some(writer),
                Err(error) => {
                    notice = Some(format!("Preference writer unavailable: {error}"));
                    None
                }
            };
            let projection = match DesktopProjection::new(snapshot) {
                Ok(projection) => projection,
                Err(issue) => {
                    let _ = open_failure(
                        format!("projection initialization failed: {}", issue.message),
                        cx,
                    );
                    return;
                }
            };
            let options = window_options(&loaded.preferences);
            if let Err(error) = cx.open_window(options, |window, cx| {
                window.set_window_title("evo · native coding agent");
                let view = cx.new(|cx| {
                    NativeShell::new(
                        NativeShellInit {
                            runtime,
                            projection,
                            preferences: loaded.preferences,
                            preference_writer: writer,
                            preference_notice: notice,
                            initial_session_id: session_id,
                        },
                        window,
                        cx,
                    )
                });
                cx.new(|cx| Root::new(view, window, cx))
            }) {
                eprintln!("desktop: failed to open native window: {error}");
            }
        })
        .detach();
    });
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
