use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use coding_agent::api::authorization::{
    ToolAuthorizationPreview, ToolAuthorizationRequest, ToolAuthorizationRisk,
    ToolAuthorizationScope,
};
use coding_agent::api::client::{
    CodingAgentContextSnapshot, CodingAgentFileChangeSnapshot, CodingAgentSnapshot,
    CodingAgentSnapshotCursor, UI_SNAPSHOT_PROTOCOL_VERSION,
};
use coding_agent::api::embedding::{
    CodingAgentEmbeddingSnapshot, CodingAgentModelChoice, CodingAgentProfileChoice,
    CodingAgentResourceCommand, CodingAgentResourceCommandKind, CodingAgentResourceSummary,
    CodingAgentSettingsSummary,
};
use coding_agent::api::event::CodingAgentProductEvent;
use coding_agent::api::view::{
    CodingAgentCapabilities, CodingAgentSessionTranscriptItem, CodingAgentSessionView,
    CodingAgentTranscriptSnapshot, ProfileId, ProfileKind, ProfileSource,
};
use gpui::{
    App, Bounds, Context, FocusHandle, KeyDownEvent, Keystroke, Render, Window, WindowBounds,
    WindowOptions, div, point, prelude::*, px, rgb, size,
};
use gpui_component::Root;

use super::native_shell::{NativeShell, NativeShellInit};
use crate::preferences::DesktopPreferences;
use crate::projection::DesktopProjection;
use crate::runtime::{DesktopRuntimeBridge, DesktopRuntimeHydratedSnapshot, DesktopRuntimeUpdate};

const PERFORMANCE_REPLAY_ENV: &str = "EVO_DESKTOP_NATIVE_PERF_REPLAY";
const VISUAL_REPLAY_ENV: &str = "EVO_DESKTOP_NATIVE_VISUAL_REPLAY";
const CLICK_TO_PHOTON_REPLAY_ENV: &str = "EVO_DESKTOP_CLICK_TO_PHOTON_REPLAY";
const WARMUP_FRAMES: usize = 20;
const SAMPLE_FRAMES: usize = 200;
const INPUT_SAMPLE_FRAMES: usize = 50;
const INPUT_SAMPLE_STRIDE: usize = SAMPLE_FRAMES / INPUT_SAMPLE_FRAMES;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum NativeReplayRequest {
    Performance,
    Visual(VisualReplaySpec),
    ClickToPhoton,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum VisualReplayLayout {
    Wide,
    Medium,
    Narrow,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum VisualReplayState {
    Standard,
    Idle,
    Authorization,
    ReducedMotion,
    KeyboardFocus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct VisualReplaySpec {
    layout: VisualReplayLayout,
    state: VisualReplayState,
}

impl VisualReplaySpec {
    fn parse(value: &str) -> Result<Self, String> {
        let (layout, state) = if let Some(layout) = value.strip_suffix("-idle") {
            (layout, VisualReplayState::Idle)
        } else if let Some(layout) = value.strip_suffix("-authorization") {
            (layout, VisualReplayState::Authorization)
        } else if let Some(layout) = value.strip_suffix("-reduced-motion") {
            (layout, VisualReplayState::ReducedMotion)
        } else if let Some(layout) = value.strip_suffix("-keyboard-focus") {
            (layout, VisualReplayState::KeyboardFocus)
        } else {
            (value, VisualReplayState::Standard)
        };
        Ok(Self {
            layout: VisualReplayLayout::parse(layout)?,
            state,
        })
    }

    fn key(self) -> String {
        let state = match self.state {
            VisualReplayState::Standard => return self.layout.key().into(),
            VisualReplayState::Idle => "idle",
            VisualReplayState::Authorization => "authorization",
            VisualReplayState::ReducedMotion => "reduced-motion",
            VisualReplayState::KeyboardFocus => "keyboard-focus",
        };
        format!("{}-{state}", self.layout.key())
    }
}

impl VisualReplayLayout {
    fn parse(value: &str) -> Result<Self, String> {
        match value {
            "wide" => Ok(Self::Wide),
            "medium" => Ok(Self::Medium),
            "narrow" => Ok(Self::Narrow),
            other => Err(format!(
                "{VISUAL_REPLAY_ENV} must be wide, medium, or narrow; got {other}"
            )),
        }
    }

    fn key(self) -> &'static str {
        match self {
            Self::Wide => "wide",
            Self::Medium => "medium",
            Self::Narrow => "narrow",
        }
    }

    fn viewport(self) -> (f32, f32) {
        match self {
            Self::Wide => (1_300., 900.),
            Self::Medium => (900., 800.),
            Self::Narrow => (700., 800.),
        }
    }
}

struct NativeFrameReplay {
    callbacks: usize,
    last_callback_at: Option<Instant>,
    cadence_samples: Vec<u128>,
    pending_input_at: Option<Instant>,
    input_dispatches: usize,
    input_post_render_samples: Vec<u128>,
    resident_before: Option<u64>,
    resident_after_warmup: Option<u64>,
}

struct ClickToPhotonReplay {
    focus_handle: FocusHandle,
    run_id: String,
    bright: bool,
    samples: u64,
}

impl ClickToPhotonReplay {
    fn new(cx: &mut Context<Self>) -> Self {
        let run_id = format!(
            "{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        );
        println!("desktop_trace\tclick_to_photon_run\trun={run_id}");
        Self {
            focus_handle: cx.focus_handle(),
            run_id,
            bright: false,
            samples: 0,
        }
    }

    fn on_key_down(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        if event.keystroke.key == "escape" {
            cx.quit();
            return;
        }
        if event.is_held || event.keystroke.key != "space" {
            return;
        }

        self.bright = !self.bright;
        self.samples = self.samples.saturating_add(1);
        let sample = self.samples;
        let bright = self.bright;
        let run_id = self.run_id.clone();
        let received_at = Instant::now();
        println!(
            "desktop_trace\tclick_to_photon_input_received\trun={run_id}\tsample={sample}\tbright={bright}"
        );
        window.on_next_frame(move |_, _| {
            println!(
                "desktop_trace\tclick_to_photon_post_render\trun={run_id}\tsample={sample}\t\
                 bright={bright}\tinput_received_to_post_render_us={}",
                received_at.elapsed().as_micros()
            );
        });
        cx.notify();
    }
}

impl Render for ClickToPhotonReplay {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl gpui::IntoElement {
        div()
            .id("desktop-click-to-photon-surface")
            .track_focus(&self.focus_handle)
            .key_context("DesktopClickToPhotonReplay")
            .capture_key_down(cx.listener(Self::on_key_down))
            .size_full()
            .bg(rgb(if self.bright { 0xffffff } else { 0x000000 }))
    }
}

impl NativeFrameReplay {
    fn new() -> Self {
        Self {
            callbacks: 0,
            last_callback_at: None,
            cadence_samples: Vec::with_capacity(SAMPLE_FRAMES),
            pending_input_at: None,
            input_dispatches: 0,
            input_post_render_samples: Vec::with_capacity(INPUT_SAMPLE_FRAMES),
            resident_before: crate::resident_memory::resident_bytes(),
            resident_after_warmup: None,
        }
    }

    fn observe_frame_callback(&mut self, now: Instant) {
        // The input is dispatched from the previous frame callback, before
        // that frame draws. GPUI runs this callback after that frame renders,
        // so the interval is a conservative app/render/present-submit upper
        // bound. It deliberately does not claim display click-to-photon time.
        if let Some(dispatched_at) = self.pending_input_at.take() {
            self.input_post_render_samples
                .push(now.saturating_duration_since(dispatched_at).as_micros());
        }
        if let Some(previous) = self.last_callback_at
            && self.callbacks >= WARMUP_FRAMES
            && self.cadence_samples.len() < SAMPLE_FRAMES
        {
            self.cadence_samples
                .push(now.saturating_duration_since(previous).as_micros());
        }
        self.last_callback_at = Some(now);
        self.callbacks += 1;
        if self.callbacks == WARMUP_FRAMES {
            self.resident_after_warmup = crate::resident_memory::resident_bytes();
        }
    }

    fn should_dispatch_input(&self) -> bool {
        self.pending_input_at.is_none()
            && self.input_dispatches < INPUT_SAMPLE_FRAMES
            && !self.cadence_samples.is_empty()
            && self
                .cadence_samples
                .len()
                .is_multiple_of(INPUT_SAMPLE_STRIDE)
    }

    fn mark_input_dispatched(&mut self, now: Instant) {
        self.pending_input_at = Some(now);
        self.input_dispatches += 1;
    }

    fn complete(&self) -> bool {
        self.cadence_samples.len() >= SAMPLE_FRAMES
            && self.input_post_render_samples.len() >= INPUT_SAMPLE_FRAMES
            && self.pending_input_at.is_none()
    }
}

pub(super) fn request() -> Result<Option<NativeReplayRequest>, String> {
    let performance = std::env::var(PERFORMANCE_REPLAY_ENV)
        .is_ok_and(|value| value == "1" || value.eq_ignore_ascii_case("true"));
    let click_to_photon = std::env::var(CLICK_TO_PHOTON_REPLAY_ENV)
        .is_ok_and(|value| value == "1" || value.eq_ignore_ascii_case("true"));
    let visual = std::env::var(VISUAL_REPLAY_ENV).ok();
    request_from_values(performance, click_to_photon, visual.as_deref())
}

fn request_from_values(
    performance: bool,
    click_to_photon: bool,
    visual: Option<&str>,
) -> Result<Option<NativeReplayRequest>, String> {
    if usize::from(performance) + usize::from(click_to_photon) + usize::from(visual.is_some()) > 1 {
        return Err(format!(
            "{PERFORMANCE_REPLAY_ENV}, {CLICK_TO_PHOTON_REPLAY_ENV}, and {VISUAL_REPLAY_ENV} are mutually exclusive"
        ));
    }
    if performance {
        return Ok(Some(NativeReplayRequest::Performance));
    }
    if click_to_photon {
        return Ok(Some(NativeReplayRequest::ClickToPhoton));
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
            state: VisualReplayState::ReducedMotion,
            ..
        })
    );
    let idle_replay = matches!(
        request,
        NativeReplayRequest::Visual(VisualReplaySpec {
            state: VisualReplayState::Idle,
            ..
        })
    );
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
    let project = projection.project().clone();
    let projection = (!idle_replay).then_some(projection);
    let global_skills: Arc<[CodingAgentResourceCommand]> = Arc::from(if idle_replay {
        vec![CodingAgentResourceCommand {
            name: "review-plan".into(),
            command: "/review-plan".into(),
            description: "Review an implementation plan before coding.".into(),
            kind: CodingAgentResourceCommandKind::Skill,
            model_invocable: true,
        }]
    } else {
        Vec::new()
    });
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
                    project,
                    projection,
                    global_skills,
                    preferences,
                    preference_writer: None,
                    preference_notice: toast_replay.then(|| {
                        "Desktop notification paths now appear as transient toasts.".into()
                    }),
                    initial_session_id: None,
                },
                window,
                cx,
            );
            if matches!(request, NativeReplayRequest::Visual(_)) && !idle_replay {
                shell.install_native_visual_session_fixture();
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
        root
    })
    .map(|_| ())
    .map_err(|error| error.to_string())
}

fn open_click_to_photon(cx: &mut App) -> Result<(), String> {
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(Bounds {
            origin: point(px(40.), px(40.)),
            size: size(px(1_000.), px(700.)),
        })),
        window_min_size: Some(size(px(640.), px(480.))),
        app_id: Some("evo.desktop.click-to-photon".into()),
        ..WindowOptions::default()
    };
    cx.open_window(options, |window, cx| {
        window.set_window_title("evo · click-to-photon measurement · Space toggles · Esc exits");
        let replay = cx.new(ClickToPhotonReplay::new);
        let focus_handle = replay.read(cx).focus_handle.clone();
        focus_handle.focus(window, cx);
        cx.new(|cx| Root::new(replay, window, cx))
    })
    .map(|_| ())
    .map_err(|error| error.to_string())
}

fn schedule_frame(window: &mut Window, replay: Rc<RefCell<NativeFrameReplay>>) {
    window.on_next_frame(move |window, cx| {
        let now = Instant::now();
        let mut replay_ref = replay.borrow_mut();
        replay_ref.observe_frame_callback(now);
        let dispatch_input = replay_ref.should_dispatch_input();
        let complete = replay_ref.complete();
        drop(replay_ref);

        if complete {
            let mut replay_ref = replay.borrow_mut();
            let cadence_p95_micros = percentile(&mut replay_ref.cadence_samples, 95);
            let input_post_render_p95_micros =
                percentile(&mut replay_ref.input_post_render_samples, 95);
            let input_post_render_p99_micros =
                percentile(&mut replay_ref.input_post_render_samples, 99);
            let resident_after = crate::resident_memory::resident_bytes();
            let resident_startup_growth =
                match (replay_ref.resident_before, replay_ref.resident_after_warmup) {
                    (Some(before), Some(after)) => Some(after.saturating_sub(before)),
                    _ => None,
                };
            let resident_steady_growth = match (replay_ref.resident_after_warmup, resident_after) {
                (Some(before), Some(after)) => Some(after.saturating_sub(before)),
                _ => None,
            };
            println!(
                "desktop_perf\tplatform={}\tnative_presented_frames={}\t\
                 native_frame_cadence_p95_us={cadence_p95_micros}\t\
                 native_input_samples={}\t\
                 native_input_dispatch_to_post_render_p95_us={input_post_render_p95_micros}\t\
                 native_input_dispatch_to_post_render_p99_us={input_post_render_p99_micros}\t\
                 native_rss_supported={}\tnative_rss_before_bytes={}\t\
                 native_rss_after_warmup_bytes={}\tnative_rss_after_bytes={}\t\
                 native_rss_startup_growth_bytes={}\tnative_rss_steady_growth_bytes={}",
                std::env::consts::OS,
                replay_ref.cadence_samples.len(),
                replay_ref.input_post_render_samples.len(),
                replay_ref.resident_before.is_some()
                    && replay_ref.resident_after_warmup.is_some()
                    && resident_after.is_some(),
                replay_ref.resident_before.unwrap_or_default(),
                replay_ref.resident_after_warmup.unwrap_or_default(),
                resident_after.unwrap_or_default(),
                resident_startup_growth.unwrap_or_default(),
                resident_steady_growth.unwrap_or_default()
            );
            cx.quit();
        } else {
            if dispatch_input {
                let dispatched_at = Instant::now();
                let keystroke =
                    Keystroke::parse("a").expect("the native replay input keystroke remains valid");
                if !window.dispatch_keystroke(keystroke, cx) {
                    eprintln!(
                        "desktop native performance replay could not dispatch composer input"
                    );
                    cx.quit();
                    return;
                }
                replay.borrow_mut().mark_input_dispatched(dispatched_at);
            }
            schedule_frame(window, Rc::clone(&replay));
            window.refresh();
        }
    });
    window.refresh();
}

fn percentile(samples: &mut [u128], percentile: usize) -> u128 {
    assert!(
        !samples.is_empty(),
        "percentile requires at least one sample"
    );
    assert!(
        (1..=100).contains(&percentile),
        "percentile must be between 1 and 100"
    );
    samples.sort_unstable();
    let index = (samples.len() * percentile).div_ceil(100).saturating_sub(1);
    samples[index]
}

fn performance_projection() -> Result<DesktopProjection, String> {
    let payload = "native frame replay 中文 🙂 ".repeat(8);
    let items = (0..crate::conversation::MAX_TRANSCRIPT_BLOCKS)
        .map(|index| CodingAgentSessionTranscriptItem::User {
            text: format!("message {index}: {payload}"),
        })
        .collect();
    projection_with_transcript("desktop-native-performance", items)
}

fn visual_projection(state: VisualReplayState) -> Result<DesktopProjection, String> {
    let items = vec![
        CodingAgentSessionTranscriptItem::User {
            text: "请优化 desktop 的消息流体验，并保持键盘导航和中文输入稳定。".into(),
        },
        CodingAgentSessionTranscriptItem::Tool {
            call_id: "visual-read-shell".into(),
            name: "read".into(),
            args: serde_json::json!({"path": "crates/desktop/src/app/native_shell.rs"}),
            result: Some("Loaded the native shell layout and render boundaries.".into()),
            is_error: false,
            duration_millis: Some(842),
        },
        CodingAgentSessionTranscriptItem::Tool {
            call_id: "visual-failed-shell".into(),
            name: "shell".into(),
            args: serde_json::json!({"command": "cargo test -p desktop"}),
            result: Some("test failed: responsive context tabs exceeded their panel bounds".into()),
            is_error: true,
            duration_millis: Some(1_184),
        },
        CodingAgentSessionTranscriptItem::Diagnostic {
            message: "One stale render sample was discarded and recovered without losing product events."
                .into(),
        },
        CodingAgentSessionTranscriptItem::Assistant {
            id: "visual-assistant-final".into(),
            text: "## Desktop update\n\nThe conversation now keeps a stable geometry while content streams. This longer fixture deliberately exercises wrapped prose, headings, lists, quotes, inline code, and a fenced block without relying on a synthetic fixed row height.\n\n> Every message remains reachable, even when the viewport changes while content is streaming.\n\n- Focus uses a visible outline and a text marker without changing bounds\n- Streaming text updates continuously while finalized Markdown is cached\n- Native frame budgets and stale-measurement rejection remain enforced\n- 中文、emoji 🙂 and composed text stay intact across line wrapping\n\n```rust\nwindow.on_next_frame(|window, _| {\n    window.refresh();\n});\n```\n\nThe final paragraph is intentionally long enough to exercise multiple body lines and stable bottom anchoring in wide, medium, and narrow layouts."
                .into(),
            thinking: "Checked layout stability, render isolation, and the native presentation gate."
                .into(),
            images: Vec::new(),
            done: true,
            reasoning_duration_millis: Some(2_430),
        },
    ];
    let session_id = "desktop-native-visual".to_owned();
    let transcript = CodingAgentTranscriptSnapshot {
        session_id: session_id.clone(),
        active_leaf_id: None,
        items,
    };
    let mut snapshot = hydrated_snapshot(session_id, transcript);
    snapshot.session.context.changes = vec![
        CodingAgentFileChangeSnapshot {
            path: "crates/desktop/src/app/native_shell/inspector_pane.rs".into(),
            mutation_kind: "edit".into(),
            operation_id: "visual-operation".into(),
            tool_call_id: Some("visual-running-edit".into()),
            updated_sequence: 2,
            first_changed_line: Some(343),
            added_lines: Some(1),
            removed_lines: Some(0),
            diff: Some("@@ -348,0 +349 @@\n+                    .flex_wrap()".into()),
        },
        CodingAgentFileChangeSnapshot {
            path: "scripts/desktop-visual-golden.sh".into(),
            mutation_kind: "edit".into(),
            operation_id: "visual-operation".into(),
            tool_call_id: Some("visual-running-edit".into()),
            updated_sequence: 2,
            first_changed_line: Some(1),
            added_lines: Some(24),
            removed_lines: Some(3),
            diff: None,
        },
    ];
    if state == VisualReplayState::Authorization {
        snapshot.session.pending_authorizations = vec![visual_authorization_request()];
    }
    let mut projection = DesktopProjection::new(snapshot).map_err(|issue| issue.message)?;
    apply_visual_running_tool(&mut projection)?;
    Ok(projection)
}

fn visual_authorization_request() -> ToolAuthorizationRequest {
    ToolAuthorizationRequest {
        authorization_id: "visual-authorization".into(),
        operation_id: "visual-operation".into(),
        turn_id: "visual-turn".into(),
        tool_call_id: "visual-authorized-shell".into(),
        tool_name: "shell".into(),
        risk: ToolAuthorizationRisk::ShellExecution,
        scope: ToolAuthorizationScope::Shell {
            cwd: "/desktop-native-replay".into(),
            command_fingerprint: "visual-golden-cargo-test".into(),
        },
        preview: ToolAuthorizationPreview {
            summary: "Run the desktop verification suite before updating reviewed visual goldens."
                .into(),
            path: None,
            command: Some("cargo test -p desktop --all-targets".into()),
            cwd: Some("/desktop-native-replay".into()),
            content_preview: None,
        },
        capability_generation: 0,
        requested_at: "2026-07-27T00:00:00Z".into(),
    }
}

fn apply_visual_running_tool(projection: &mut DesktopProjection) -> Result<(), String> {
    let mut events = serde_json::from_str::<Vec<CodingAgentProductEvent>>(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../coding-agent/tests/fixtures/client_projection/cross-adapter-events.json"
    )))
    .map_err(|error| format!("visual product-event fixture is invalid: {error}"))?;
    let base = events
        .drain(..)
        .next()
        .ok_or_else(|| "visual product-event fixture is empty".to_owned())?;
    let stream_id = projection.cursor().stream_id.clone();
    let session_id = projection.snapshot().session.session_id.clone();
    for (sequence, event) in [
        serde_json::json!({
            "family": "workflow",
            "payload": {
                "kind": "prompt_started",
                "operation_id": "visual-operation",
                "turn_id": "visual-turn"
            }
        }),
        serde_json::json!({
            "family": "tool",
            "payload": {
                "kind": "started",
                "operation_id": "visual-operation",
                "turn_id": "visual-turn",
                "tool_call_id": "visual-running-edit",
                "name": "edit",
                "arguments_json": "{\"path\":\"crates/desktop/src/app/native_shell/inspector_pane.rs\"}"
            }
        }),
    ]
    .into_iter()
    .enumerate()
    {
        let mut value = serde_json::to_value(&base)
            .map_err(|error| format!("could not encode visual product event: {error}"))?;
        value["stream_id"] = serde_json::json!(stream_id);
        value["sequence"] = serde_json::json!(sequence + 1);
        value["session_id"] = serde_json::json!(session_id);
        value["operation_id"] = serde_json::json!("visual-operation");
        value["parent_operation_id"] = serde_json::Value::Null;
        value["root_operation_id"] = serde_json::Value::Null;
        value["event"] = event;
        value["terminal_status"] = serde_json::Value::Null;
        value["terminal_operation"] = serde_json::Value::Null;
        let event = serde_json::from_value(value)
            .map_err(|error| format!("could not decode visual product event: {error}"))?;
        if !matches!(
            projection.apply(DesktopRuntimeUpdate::product_event(event)),
            crate::projection::DesktopProjectionApply::Applied(_)
        ) {
            return Err("visual running-tool event did not apply to the projection".into());
        }
    }
    Ok(())
}

fn projection_with_transcript(
    session_id: &str,
    items: Vec<CodingAgentSessionTranscriptItem>,
) -> Result<DesktopProjection, String> {
    let session_id = session_id.to_owned();
    let transcript = CodingAgentTranscriptSnapshot {
        session_id: session_id.clone(),
        active_leaf_id: None,
        items,
    };
    projection_from_transcript(session_id, transcript)
}

fn projection_from_transcript(
    session_id: String,
    transcript: CodingAgentTranscriptSnapshot,
) -> Result<DesktopProjection, String> {
    DesktopProjection::new(hydrated_snapshot(session_id, transcript)).map_err(|issue| issue.message)
}

fn hydrated_snapshot(
    session_id: String,
    transcript: CodingAgentTranscriptSnapshot,
) -> DesktopRuntimeHydratedSnapshot {
    DesktopRuntimeHydratedSnapshot {
        project: CodingAgentEmbeddingSnapshot {
            cwd: std::path::PathBuf::from("/desktop-native-replay"),
            global_config_dir: std::path::PathBuf::from("/desktop-native-replay/config"),
            selected_model_id: "performance-fixture".into(),
            default_agent_profile_id: ProfileId::from("default"),
            models: vec![
                CodingAgentModelChoice {
                    id: "performance-fixture".into(),
                    name: "Performance Fixture".into(),
                    provider: "fixture".into(),
                    reasoning: true,
                    supports_text: true,
                    supports_images: true,
                    context_window: 200_000,
                    max_output_tokens: 32_000,
                    configured: true,
                    selected: true,
                },
                CodingAgentModelChoice {
                    id: "review-fixture".into(),
                    name: "Review Fixture".into(),
                    provider: "fixture".into(),
                    reasoning: false,
                    supports_text: true,
                    supports_images: false,
                    context_window: 100_000,
                    max_output_tokens: 16_000,
                    configured: true,
                    selected: false,
                },
                CodingAgentModelChoice {
                    id: "image-fixture".into(),
                    name: "Image Fixture".into(),
                    provider: "fixture".into(),
                    reasoning: false,
                    supports_text: false,
                    supports_images: true,
                    context_window: 32_000,
                    max_output_tokens: 4_000,
                    configured: true,
                    selected: false,
                },
            ],
            profiles: vec![
                CodingAgentProfileChoice {
                    id: ProfileId::from("default"),
                    display_name: "Default".into(),
                    description: Some("General coding work".into()),
                    kind: ProfileKind::Agent,
                    source: ProfileSource::BuiltIn,
                    model_id: None,
                },
                CodingAgentProfileChoice {
                    id: ProfileId::from("reviewer"),
                    display_name: "Reviewer".into(),
                    description: Some("Review changes before completion".into()),
                    kind: ProfileKind::Agent,
                    source: ProfileSource::Project,
                    model_id: Some("review-fixture".into()),
                },
            ],
            resources: CodingAgentResourceSummary {
                skill_names: Vec::new(),
                prompt_template_names: Vec::new(),
                commands: Vec::new(),
                context_files: Vec::new(),
            },
            settings: CodingAgentSettingsSummary {
                default_provider: None,
                default_model: None,
                default_thinking_level: None,
                session_dir: None,
                no_context_files: true,
            },
            diagnostics: Vec::new(),
        },
        session: CodingAgentSnapshot {
            cursor: CodingAgentSnapshotCursor {
                stream_id: "desktop-native-replay-stream".into(),
                snapshot_protocol_major: UI_SNAPSHOT_PROTOCOL_VERSION.major,
                last_event_sequence: 0,
                last_session_sequence: 0,
                capability_generation: 0,
            },
            version: UI_SNAPSHOT_PROTOCOL_VERSION,
            session: CodingAgentSessionView {
                session_id,
                default_agent_profile_id: ProfileId::from("default"),
            },
            capabilities: CodingAgentCapabilities::idle(false),
            active_operation: None,
            drafts: Vec::new(),
            submitted_operation: None,
            pending_authorizations: Vec::new(),
            context: CodingAgentContextSnapshot::default(),
        },
        transcript,
        pending_recoveries: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_replay_fixture_is_deterministic_and_full_scale() {
        let projection = performance_projection().expect("native fixture remains valid");
        assert_eq!(
            projection.conversation().blocks().len(),
            crate::conversation::MAX_TRANSCRIPT_BLOCKS
        );
        assert_eq!(
            projection.conversation().session_id,
            "desktop-native-performance"
        );
    }

    #[test]
    fn native_replay_percentile_uses_nearest_rank() {
        let mut samples = (1..=200).rev().collect::<Vec<u128>>();
        assert_eq!(percentile(&mut samples, 95), 190);
        assert_eq!(percentile(&mut samples, 99), 198);
    }

    #[test]
    fn native_replay_pairs_input_with_the_next_post_render_callback() {
        let mut replay = NativeFrameReplay::new();
        let started_at = Instant::now();

        for callback in 0..300 {
            let now = started_at + std::time::Duration::from_millis(callback * 8);
            replay.observe_frame_callback(now);
            if replay.should_dispatch_input() {
                replay.mark_input_dispatched(now);
            }
            if replay.complete() {
                break;
            }
        }

        assert!(replay.complete());
        assert_eq!(replay.cadence_samples.len(), SAMPLE_FRAMES);
        assert_eq!(replay.input_dispatches, INPUT_SAMPLE_FRAMES);
        assert_eq!(
            replay.input_post_render_samples,
            vec![8_000; INPUT_SAMPLE_FRAMES]
        );
    }

    #[test]
    fn native_replay_request_parser_rejects_conflicts_and_unknown_layouts() {
        assert_eq!(
            request_from_values(false, false, Some("medium")),
            Ok(Some(NativeReplayRequest::Visual(VisualReplaySpec {
                layout: VisualReplayLayout::Medium,
                state: VisualReplayState::Standard,
            })))
        );
        assert_eq!(
            request_from_values(false, true, None),
            Ok(Some(NativeReplayRequest::ClickToPhoton))
        );
        assert!(request_from_values(true, false, Some("wide")).is_err());
        assert!(request_from_values(true, true, None).is_err());
        assert!(request_from_values(false, true, Some("wide")).is_err());
        assert!(request_from_values(false, false, Some("compact")).is_err());
        assert_eq!(
            request_from_values(false, false, Some("wide-authorization")),
            Ok(Some(NativeReplayRequest::Visual(VisualReplaySpec {
                layout: VisualReplayLayout::Wide,
                state: VisualReplayState::Authorization,
            })))
        );
        assert_eq!(
            request_from_values(false, false, Some("narrow-idle")),
            Ok(Some(NativeReplayRequest::Visual(VisualReplaySpec {
                layout: VisualReplayLayout::Narrow,
                state: VisualReplayState::Idle,
            })))
        );
    }

    #[test]
    fn visual_replay_layouts_have_stable_viewports() {
        assert_eq!(VisualReplayLayout::Wide.viewport(), (1_300., 900.));
        assert_eq!(VisualReplayLayout::Medium.viewport(), (900., 800.));
        assert_eq!(VisualReplayLayout::Narrow.viewport(), (700., 800.));
        assert_eq!(
            visual_projection(VisualReplayState::Standard)
                .expect("visual fixture remains valid")
                .conversation()
                .blocks()
                .len(),
            5
        );
        let standard = visual_projection(VisualReplayState::Standard)
            .expect("standard visual fixture remains valid");
        let blocks = standard.conversation().blocks();
        assert!(
            blocks
                .iter()
                .any(|block| { block.kind == crate::conversation::ConversationBlockKind::User })
        );
        assert!(blocks.iter().any(|block| {
            block.kind == crate::conversation::ConversationBlockKind::Assistant
                && !block.detail.is_empty()
        }));
        assert!(blocks.iter().any(|block| {
            block.kind == crate::conversation::ConversationBlockKind::Tool
                && block.done
                && !block.is_error
        }));
        assert!(blocks.iter().any(|block| {
            block.kind == crate::conversation::ConversationBlockKind::Tool && block.is_error
        }));
        assert!(
            blocks.iter().any(|block| {
                block.kind == crate::conversation::ConversationBlockKind::Diagnostic
            })
        );
        assert_eq!(standard.tools().len(), 1);
        let projection = visual_projection(VisualReplayState::Authorization)
            .expect("authorization visual fixture remains valid");
        assert_eq!(projection.tools().len(), 1);
        assert_eq!(projection.snapshot().context.changes.len(), 2);
        assert_eq!(projection.snapshot().pending_authorizations.len(), 1);
    }

    #[test]
    fn visual_golden_updates_require_reviewed_before_after_artifacts() {
        let script = include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../scripts/desktop-visual-golden.sh"
        ));
        assert!(script.contains("--review | --update --review-note FILE"));
        assert!(script.contains("manifest.sha256"));
        assert!(script.contains("park_pointer_outside_replay"));
        assert!(script.contains("-before.png"));
        assert!(script.contains("-after.png"));
        assert!(script.contains("-diff.png"));
        for fixture in [
            "wide-idle",
            "medium-idle",
            "narrow-idle",
            "wide-authorization",
            "wide-reduced-motion",
            "wide-keyboard-focus",
            "wide-no-color",
        ] {
            assert!(script.contains(fixture), "missing visual fixture {fixture}");
        }
    }
}
