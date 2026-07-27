use std::cell::RefCell;
use std::rc::Rc;
use std::time::Instant;

use coding_agent::api::client::{
    CodingAgentContextSnapshot, CodingAgentSnapshot, CodingAgentSnapshotCursor,
    UI_SNAPSHOT_PROTOCOL_VERSION,
};
use coding_agent::api::embedding::{
    CodingAgentEmbeddingSnapshot, CodingAgentResourceSummary, CodingAgentSettingsSummary,
};
use coding_agent::api::view::{
    CodingAgentCapabilities, CodingAgentSessionTranscriptItem, CodingAgentSessionView,
    CodingAgentTranscriptSnapshot, ProfileId,
};
use gpui::{
    App, Bounds, Context, FocusHandle, KeyDownEvent, Keystroke, Render, Window, WindowBounds,
    WindowOptions, div, point, prelude::*, px, rgb, size,
};
use gpui_component::Root;

use super::native_shell::{NativeShell, NativeShellInit};
use crate::preferences::DesktopPreferences;
use crate::projection::DesktopProjection;
use crate::runtime::{DesktopRuntimeBridge, DesktopRuntimeHydratedSnapshot};

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
    Visual(VisualReplayLayout),
    ClickToPhoton,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum VisualReplayLayout {
    Wide,
    Medium,
    Narrow,
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
    bright: bool,
    samples: u64,
}

impl ClickToPhotonReplay {
    fn new(cx: &mut Context<Self>) -> Self {
        Self {
            focus_handle: cx.focus_handle(),
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
        let received_at = Instant::now();
        println!("desktop_trace\tclick_to_photon_input_received\tsample={sample}\tbright={bright}");
        window.on_next_frame(move |_, _| {
            println!(
                "desktop_trace\tclick_to_photon_post_render\tsample={sample}\tbright={bright}\t\
                 input_received_to_post_render_us={}",
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
        .map(VisualReplayLayout::parse)
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
        NativeReplayRequest::Visual(layout) => (
            visual_projection()?,
            layout.viewport(),
            format!("evo-desktop-visual-{}", layout.key()),
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
    cx.open_window(options, move |window, cx| {
        window.set_window_title(&title);
        let shell = cx.new(|cx| {
            NativeShell::new(
                NativeShellInit {
                    runtime: DesktopRuntimeBridge::disconnected_for_replay(),
                    projection,
                    preferences: DesktopPreferences::default(),
                    preference_writer: None,
                    preference_notice: None,
                    initial_session_id: None,
                },
                window,
                cx,
            )
        });
        if let Some(replay) = replay {
            schedule_frame(window, replay);
        }
        cx.new(|cx| Root::new(shell, window, cx))
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

fn visual_projection() -> Result<DesktopProjection, String> {
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
        CodingAgentSessionTranscriptItem::Diagnostic {
            message: "One stale render sample was discarded and recovered without losing product events."
                .into(),
        },
        CodingAgentSessionTranscriptItem::Assistant {
            id: "visual-assistant-final".into(),
            text: "## Desktop update\n\nThe conversation now keeps a stable geometry while content streams.\n\n- Focus uses color without changing bounds\n- Streaming text updates continuously\n- Native frame budgets are enforced\n\n```rust\nwindow.refresh();\n```"
                .into(),
            thinking: "Checked layout stability, render isolation, and the native presentation gate."
                .into(),
            images: Vec::new(),
            done: true,
            reasoning_duration_millis: Some(2_430),
        },
    ];
    projection_with_transcript("desktop-native-visual", items)
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
    let snapshot = DesktopRuntimeHydratedSnapshot {
        project: CodingAgentEmbeddingSnapshot {
            cwd: std::path::PathBuf::from("/desktop-native-replay"),
            global_config_dir: std::path::PathBuf::from("/desktop-native-replay/config"),
            selected_model_id: "performance-fixture".into(),
            default_agent_profile_id: ProfileId::from("default"),
            models: Vec::new(),
            profiles: Vec::new(),
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
    };
    DesktopProjection::new(snapshot).map_err(|issue| issue.message)
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
            Ok(Some(NativeReplayRequest::Visual(
                VisualReplayLayout::Medium
            )))
        );
        assert_eq!(
            request_from_values(false, true, None),
            Ok(Some(NativeReplayRequest::ClickToPhoton))
        );
        assert!(request_from_values(true, false, Some("wide")).is_err());
        assert!(request_from_values(true, true, None).is_err());
        assert!(request_from_values(false, true, Some("wide")).is_err());
        assert!(request_from_values(false, false, Some("compact")).is_err());
    }

    #[test]
    fn visual_replay_layouts_have_stable_viewports() {
        assert_eq!(VisualReplayLayout::Wide.viewport(), (1_300., 900.));
        assert_eq!(VisualReplayLayout::Medium.viewport(), (900., 800.));
        assert_eq!(VisualReplayLayout::Narrow.viewport(), (700., 800.));
        assert_eq!(
            visual_projection()
                .expect("visual fixture remains valid")
                .conversation()
                .blocks()
                .len(),
            4
        );
    }
}
