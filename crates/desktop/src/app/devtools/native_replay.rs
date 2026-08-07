use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use coding_agent::api::embedding::{CodingAgentResourceCommand, CodingAgentResourceCommandKind};
use gpui::{
    App, Bounds, Keystroke, Modifiers, MouseButton, MouseDownEvent, MouseUpEvent, PlatformInput,
    WindowBounds, WindowOptions, point, prelude::*, px, size,
};
use gpui_component::Root;

use super::super::native_shell::{
    EvoBrandFixture, EvoBrandMode, NativeShell, NativeShellInit, NativeShellWorkspaceInit,
    NativeVisualCatalogFixture, NativeVisualDrawerFixture,
};
use crate::preferences::DesktopPreferences;

mod fixture;
mod frame;
mod projection;
mod spec;

#[allow(unused_imports)]
use self::fixture::visual_change;
#[allow(unused_imports)]
use self::frame::ClickToPhotonReplay;
#[allow(unused_imports)]
pub(super) use self::{
    frame::{NativeFrameReplay, open_click_to_photon, schedule_frame},
    projection::{
        apply_visual_running_tool, hydrated_snapshot, performance_projection,
        projection_from_transcript, projection_with_transcript, visual_authorization_request,
        visual_projection,
    },
    spec::{NativeReplayRequest, VisualReplayLayout, VisualReplaySpec, VisualReplayState},
};

#[allow(unused_imports)]
use crate::projection::DesktopProjection;
#[allow(unused_imports)]
use crate::runtime::{DesktopRuntimeBridge, DesktopRuntimeHydratedSnapshot};

const PERFORMANCE_REPLAY_ENV: &str = "EVO_DESKTOP_NATIVE_PERF_REPLAY";
pub(in crate::app::devtools) const VISUAL_REPLAY_ENV: &str = "EVO_DESKTOP_NATIVE_VISUAL_REPLAY";
const BRAND_VISUAL_REPLAY_ENV: &str = "EVO_DESKTOP_BRAND_VISUAL_REPLAY";
const CLICK_TO_PHOTON_REPLAY_ENV: &str = "EVO_DESKTOP_CLICK_TO_PHOTON_REPLAY";
pub(in crate::app::devtools) const WARMUP_FRAMES: usize = 20;
pub(in crate::app::devtools) const SAMPLE_FRAMES: usize = 200;
pub(in crate::app::devtools) const INPUT_SAMPLE_FRAMES: usize = 50;
pub(in crate::app::devtools) const INPUT_SAMPLE_STRIDE: usize = SAMPLE_FRAMES / INPUT_SAMPLE_FRAMES;

pub(super) fn request() -> Result<Option<NativeReplayRequest>, String> {
    let performance = std::env::var(PERFORMANCE_REPLAY_ENV)
        .is_ok_and(|value| value == "1" || value.eq_ignore_ascii_case("true"));
    let click_to_photon = std::env::var(CLICK_TO_PHOTON_REPLAY_ENV)
        .is_ok_and(|value| value == "1" || value.eq_ignore_ascii_case("true"));
    let visual = std::env::var(VISUAL_REPLAY_ENV).ok();
    let brand = std::env::var(BRAND_VISUAL_REPLAY_ENV).ok();
    request_from_values(
        performance,
        click_to_photon,
        visual.as_deref(),
        brand.as_deref(),
    )
}

fn request_from_values(
    performance: bool,
    click_to_photon: bool,
    visual: Option<&str>,
    brand: Option<&str>,
) -> Result<Option<NativeReplayRequest>, String> {
    if usize::from(performance)
        + usize::from(click_to_photon)
        + usize::from(visual.is_some())
        + usize::from(brand.is_some())
        > 1
    {
        return Err(format!(
            "{PERFORMANCE_REPLAY_ENV}, {CLICK_TO_PHOTON_REPLAY_ENV}, {VISUAL_REPLAY_ENV}, and {BRAND_VISUAL_REPLAY_ENV} are mutually exclusive"
        ));
    }
    if performance {
        return Ok(Some(NativeReplayRequest::Performance));
    }
    if click_to_photon {
        return Ok(Some(NativeReplayRequest::ClickToPhoton));
    }
    if let Some(brand) = brand {
        return EvoBrandMode::parse(brand).map(|mode| Some(NativeReplayRequest::Brand(mode)));
    }
    visual
        .map(VisualReplaySpec::parse)
        .transpose()
        .map(|layout| layout.map(NativeReplayRequest::Visual))
}

pub(super) fn open(cx: &mut App, request: NativeReplayRequest) -> Result<(), String> {
    if request == NativeReplayRequest::ClickToPhoton {
        return open_click_to_photon(cx);
    }
    if let NativeReplayRequest::Brand(mode) = request {
        return open_brand_fixture(cx, mode);
    }
    let (projection, viewport, title, replay) = match request {
        NativeReplayRequest::Performance => (
            performance_projection()?,
            (1_300., 900.),
            "evo · native performance replay".to_owned(),
            Some(Rc::new(RefCell::new(NativeFrameReplay::new()))),
        ),
        NativeReplayRequest::Visual(spec) => (
            visual_projection(spec.state)?,
            spec.layout.viewport(),
            format!("evo-desktop-visual-{}", spec.key()),
            None,
        ),
        NativeReplayRequest::Brand(_) => unreachable!("handled before projection setup"),
        NativeReplayRequest::ClickToPhoton => unreachable!("handled before projection setup"),
    };
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(Bounds {
            origin: point(px(40.), px(40.)),
            size: size(px(viewport.0), px(viewport.1)),
        })),
        window_min_size: Some(size(px(640.), px(480.))),
        app_id: Some("evo.desktop.native-perf".into()),
        ..WindowOptions::default()
    };
    let keyboard_focus_replay = matches!(
        request,
        NativeReplayRequest::Visual(VisualReplaySpec {
            state: VisualReplayState::KeyboardFocus,
            ..
        })
    );
    let reduced_motion_replay = matches!(
        request,
        NativeReplayRequest::Visual(VisualReplaySpec {
            state: VisualReplayState::ReducedMotion | VisualReplayState::CatalogLoading,
            ..
        })
    );
    let visual_state = match request {
        NativeReplayRequest::Visual(spec) => Some(spec.state),
        NativeReplayRequest::Performance
        | NativeReplayRequest::Brand(_)
        | NativeReplayRequest::ClickToPhoton => None,
    };
    let idle_replay = visual_state.is_some_and(VisualReplayState::uses_home);
    let toast_replay = matches!(
        request,
        NativeReplayRequest::Visual(VisualReplaySpec {
            state: VisualReplayState::Standard,
            ..
        })
    );
    let selection_replay = matches!(
        request,
        NativeReplayRequest::Visual(VisualReplaySpec {
            state: VisualReplayState::Standard
                | VisualReplayState::ReducedMotion
                | VisualReplayState::KeyboardFocus,
            ..
        })
    );
    let workspace = if idle_replay {
        NativeShellWorkspaceInit::Home(Box::new(projection.project().clone()))
    } else {
        NativeShellWorkspaceInit::Session(Box::new(projection))
    };
    let global_skills: Arc<[CodingAgentResourceCommand]> =
        Arc::from([CodingAgentResourceCommand {
            name: "review-plan".into(),
            command: "/review-plan".into(),
            description: "Review an implementation plan before coding.".into(),
            kind: CodingAgentResourceCommandKind::Skill,
            model_invocable: true,
        }]);
    cx.open_window(options, move |window, cx| {
        window.set_window_title(&title);
        let preferences = DesktopPreferences {
            reduced_motion: reduced_motion_replay,
            ..DesktopPreferences::default()
        };
        let shell = cx.new(|cx| {
            let mut shell = NativeShell::new(
                NativeShellInit {
                    runtime: DesktopRuntimeBridge::disconnected_for_replay(),
                    workspace,
                    projectless_workspace_selection:
                        coding_agent::api::embedding::CodingAgentWorkspaceSelection::projectless(
                            "workspace-native-replay",
                        ),
                    global_skills,
                    preferences,
                    preference_writer: None,
                    preference_notice: toast_replay.then(|| {
                        "Desktop notification paths now appear as transient toasts.".into()
                    }),
                },
                window,
                cx,
            );
            if let Some(state) = visual_state {
                match state {
                    VisualReplayState::Standard
                    | VisualReplayState::Authorization
                    | VisualReplayState::ReducedMotion
                    | VisualReplayState::KeyboardFocus
                    | VisualReplayState::InspectorDrawer => {
                        shell.install_native_visual_catalog_fixture(
                            NativeVisualCatalogFixture::Ready,
                            cx,
                        );
                        shell.install_native_visual_drawer_fixture(
                            if state == VisualReplayState::InspectorDrawer {
                                NativeVisualDrawerFixture::Inspector
                            } else {
                                NativeVisualDrawerFixture::Sessions
                            },
                            cx,
                        );
                    }
                    VisualReplayState::Idle
                    | VisualReplayState::ModelMenu
                    | VisualReplayState::ThinkingMenu => {
                        shell.install_native_visual_catalog_fixture(
                            NativeVisualCatalogFixture::NotLoaded,
                            cx,
                        );
                    }
                    VisualReplayState::ThinkingNonReasoning => {
                        shell.install_native_visual_catalog_fixture(
                            NativeVisualCatalogFixture::NotLoaded,
                            cx,
                        );
                        shell.install_native_visual_non_reasoning_fixture(cx);
                    }
                    VisualReplayState::HomeProject => {
                        shell.install_native_visual_catalog_fixture(
                            NativeVisualCatalogFixture::NotLoaded,
                            cx,
                        );
                        shell.install_native_visual_home_project_fixture(
                            std::path::PathBuf::from("/workspace/evo"),
                            cx,
                        );
                    }
                    VisualReplayState::HomeLongProject => {
                        shell.install_native_visual_catalog_fixture(
                            NativeVisualCatalogFixture::NotLoaded,
                            cx,
                        );
                        shell.install_native_visual_home_project_fixture(
                            std::path::PathBuf::from(
                                "/workspace/clients/acme/platform/apps/desktop/a-very-long-multi-project-workspace-name",
                            ),
                            cx,
                        );
                    }
                    VisualReplayState::CatalogLoading => {
                        shell.install_native_visual_catalog_fixture(
                            NativeVisualCatalogFixture::Loading,
                            cx,
                        );
                    }
                    VisualReplayState::CatalogError => {
                        shell.install_native_visual_catalog_fixture(
                            NativeVisualCatalogFixture::Error,
                            cx,
                        );
                    }
                    VisualReplayState::CatalogEmpty => {
                        shell.install_native_visual_catalog_fixture(
                            NativeVisualCatalogFixture::Empty,
                            cx,
                        );
                    }
                }
            }
            shell
        });
        let root = cx.new(|cx| Root::new(shell.clone(), window, cx));
        if selection_replay {
            let selection_shell = shell.clone();
            window.on_next_frame(move |window, cx| {
                // Row render data exists after Root's first frame. Select one
                // stable row now so keyboard-focus and grayscale review
                // fixtures exercise the geometry-neutral selection rail.
                selection_shell
                    .update(cx, |shell, cx| shell.select_adjacent_conversation(true, cx));
                window.refresh();
            });
        }
        if let Some(replay) = replay {
            shell.update(cx, |shell, cx| shell.focus_composer_input(window, cx));
            schedule_frame(window, replay);
        }
        if keyboard_focus_replay {
            window.on_next_frame(|window, cx| {
                let keystroke = Keystroke::parse("tab")
                    .expect("the visual keyboard-focus replay keystroke remains valid");
                if !window.dispatch_keystroke(keystroke, cx) {
                    eprintln!("desktop visual replay could not dispatch keyboard focus input");
                }
                window.refresh();
            });
        }
        if let Some(position) = match visual_state {
            Some(VisualReplayState::ModelMenu) => Some(point(px(610.), px(821.))),
            Some(VisualReplayState::ThinkingMenu) => Some(point(px(700.), px(821.))),
            _ => None,
        } {
            window.on_next_frame(move |window, cx| {
                window.dispatch_event(
                    PlatformInput::MouseDown(MouseDownEvent {
                        button: MouseButton::Left,
                        position,
                        modifiers: Modifiers::default(),
                        click_count: 1,
                        first_mouse: false,
                    }),
                    cx,
                );
                window.dispatch_event(
                    PlatformInput::MouseUp(MouseUpEvent {
                        button: MouseButton::Left,
                        position,
                        modifiers: Modifiers::default(),
                        click_count: 1,
                    }),
                    cx,
                );
                window.refresh();
            });
        }
        root
    })
    .map(|_| ())
    .map_err(|error| error.to_string())
}

fn open_brand_fixture(cx: &mut App, mode: EvoBrandMode) -> Result<(), String> {
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(Bounds {
            origin: point(px(40.), px(40.)),
            size: size(px(900.), px(700.)),
        })),
        window_min_size: Some(size(px(900.), px(700.))),
        app_id: Some("evo.desktop.brand-fixture".into()),
        ..WindowOptions::default()
    };
    let title = format!("evo-brand-visual-{}", mode.key());
    cx.open_window(options, move |window, cx| {
        window.set_window_title(&title);
        let fixture = cx.new(|_| EvoBrandFixture::new(mode));
        cx.new(|cx| Root::new(fixture, window, cx))
    })
    .map(|_| ())
    .map_err(|error| error.to_string())
}
