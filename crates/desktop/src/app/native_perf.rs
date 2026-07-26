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

const REPLAY_ENV: &str = "EVO_DESKTOP_NATIVE_PERF_REPLAY";
const WARMUP_FRAMES: usize = 20;
const SAMPLE_FRAMES: usize = 200;

struct NativeFrameReplay {
    callbacks: usize,
    last_callback_at: Option<Instant>,
    cadence_samples: Vec<u128>,
}

pub(super) fn enabled() -> bool {
    std::env::var(REPLAY_ENV).is_ok_and(|value| value == "1" || value.eq_ignore_ascii_case("true"))
}

pub(super) fn open(cx: &mut App) -> Result<(), String> {
    let projection = performance_projection()?;
    let replay = Rc::new(RefCell::new(NativeFrameReplay {
        callbacks: 0,
        last_callback_at: None,
        cadence_samples: Vec::with_capacity(SAMPLE_FRAMES),
    }));
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(Bounds {
            origin: point(px(40.), px(40.)),
            size: size(px(1_300.), px(900.)),
        })),
        window_min_size: Some(size(px(640.), px(480.))),
        app_id: Some("evo.desktop.native-perf".into()),
        ..WindowOptions::default()
    };
    cx.open_window(options, move |window, cx| {
        window.set_window_title("evo · native performance replay");
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
        schedule_frame(window, replay);
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
    let session_id = "desktop-native-performance".to_owned();
    let payload = "native frame replay 中文 🙂 ".repeat(8);
    let transcript = CodingAgentTranscriptSnapshot {
        session_id: session_id.clone(),
        active_leaf_id: None,
        items: (0..crate::conversation::MAX_TRANSCRIPT_BLOCKS)
            .map(|index| CodingAgentSessionTranscriptItem::User {
                text: format!("message {index}: {payload}"),
            })
            .collect(),
    };
    let snapshot = DesktopRuntimeHydratedSnapshot {
        project: CodingAgentEmbeddingSnapshot {
            cwd: std::path::PathBuf::from("/desktop-native-performance"),
            global_config_dir: std::path::PathBuf::from("/desktop-native-performance/config"),
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
                stream_id: "desktop-native-performance-stream".into(),
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
}
