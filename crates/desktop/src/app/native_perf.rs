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
use gpui::{App, Bounds, Window, WindowBounds, WindowOptions, point, prelude::*, px, size};
use gpui_component::Root;

use super::native_shell::{NativeShell, NativeShellInit};
use crate::preferences::DesktopPreferences;
use crate::projection::DesktopProjection;
use crate::runtime::{DesktopRuntimeBridge, DesktopRuntimeHydratedSnapshot};

const PERFORMANCE_REPLAY_ENV: &str = "EVO_DESKTOP_NATIVE_PERF_REPLAY";
const VISUAL_REPLAY_ENV: &str = "EVO_DESKTOP_NATIVE_VISUAL_REPLAY";
const WARMUP_FRAMES: usize = 20;
const SAMPLE_FRAMES: usize = 200;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum NativeReplayRequest {
    Performance,
    Visual(VisualReplayLayout),
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
}

pub(super) fn request() -> Result<Option<NativeReplayRequest>, String> {
    let performance = std::env::var(PERFORMANCE_REPLAY_ENV)
        .is_ok_and(|value| value == "1" || value.eq_ignore_ascii_case("true"));
    let visual = std::env::var(VISUAL_REPLAY_ENV).ok();
    request_from_values(performance, visual.as_deref())
}

fn request_from_values(
    performance: bool,
    visual: Option<&str>,
) -> Result<Option<NativeReplayRequest>, String> {
    if performance && visual.is_some() {
        return Err(format!(
            "{PERFORMANCE_REPLAY_ENV} and {VISUAL_REPLAY_ENV} are mutually exclusive"
        ));
    }
    if performance {
        return Ok(Some(NativeReplayRequest::Performance));
    }
    visual
        .map(VisualReplayLayout::parse)
        .transpose()
        .map(|layout| layout.map(NativeReplayRequest::Visual))
}

pub(super) fn open(cx: &mut App, request: NativeReplayRequest) -> Result<(), String> {
    let (projection, viewport, title, replay) = match request {
        NativeReplayRequest::Performance => (
            performance_projection()?,
            (1_300., 900.),
            "evo · native performance replay".to_owned(),
            Some(Rc::new(RefCell::new(NativeFrameReplay {
                callbacks: 0,
                last_callback_at: None,
                cadence_samples: Vec::with_capacity(SAMPLE_FRAMES),
            }))),
        ),
        NativeReplayRequest::Visual(layout) => (
            visual_projection()?,
            layout.viewport(),
            format!("evo-desktop-visual-{}", layout.key()),
            None,
        ),
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

fn schedule_frame(window: &mut Window, replay: Rc<RefCell<NativeFrameReplay>>) {
    window.on_next_frame(move |window, cx| {
        let now = Instant::now();
        let mut replay_ref = replay.borrow_mut();
        if let Some(previous) = replay_ref.last_callback_at
            && replay_ref.callbacks >= WARMUP_FRAMES
        {
            replay_ref
                .cadence_samples
                .push(now.saturating_duration_since(previous).as_micros());
        }
        replay_ref.last_callback_at = Some(now);
        replay_ref.callbacks += 1;
        let complete = replay_ref.cadence_samples.len() >= SAMPLE_FRAMES;
        drop(replay_ref);

        if complete {
            let mut replay_ref = replay.borrow_mut();
            let cadence_p95_micros = percentile_95(&mut replay_ref.cadence_samples);
            println!(
                "desktop_perf\tnative_presented_frames={}\t\
                 native_frame_cadence_p95_us={cadence_p95_micros}",
                replay_ref.cadence_samples.len()
            );
            cx.quit();
        } else {
            schedule_frame(window, Rc::clone(&replay));
            window.refresh();
        }
    });
    window.refresh();
}

fn percentile_95(samples: &mut [u128]) -> u128 {
    samples.sort_unstable();
    let index = (samples.len() * 95).div_ceil(100).saturating_sub(1);
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
        assert_eq!(percentile_95(&mut samples), 190);
    }

    #[test]
    fn native_replay_request_parser_rejects_conflicts_and_unknown_layouts() {
        assert_eq!(
            request_from_values(false, Some("medium")),
            Ok(Some(NativeReplayRequest::Visual(
                VisualReplayLayout::Medium
            )))
        );
        assert!(request_from_values(true, Some("wide")).is_err());
        assert!(request_from_values(false, Some("compact")).is_err());
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
