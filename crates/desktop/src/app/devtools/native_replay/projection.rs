//! Replay projection builders: performance and visual fixture state over the
//! desktop product projection.

use coding_agent::api::authorization::{
    ToolAuthorizationPreview, ToolAuthorizationRequest, ToolAuthorizationRisk,
    ToolAuthorizationScope,
};
use coding_agent::api::client::{
    CodingAgentContextSnapshot, CodingAgentSnapshot, CodingAgentSnapshotCursor,
    UI_SNAPSHOT_PROTOCOL_VERSION,
};
use coding_agent::api::embedding::{
    CodingAgentEmbeddingSnapshot, CodingAgentModelChoice, CodingAgentProfileChoice,
    CodingAgentResourceSummary, CodingAgentSettingsSummary, CodingAgentThinkingCapability,
    CodingAgentThinkingLevel,
};
use coding_agent::api::event::CodingAgentProductEvent;
use coding_agent::api::view::{
    CodingAgentCapabilities, CodingAgentSessionTranscriptItem, CodingAgentSessionView,
    CodingAgentTranscriptSnapshot, ProfileId, ProfileKind, ProfileSource,
};

use super::{VisualReplayState, fixture::visual_change};
use crate::projection::DesktopProjection;
use crate::runtime::DesktopRuntimeHydratedSnapshot;

pub(in crate::app::devtools) fn performance_projection() -> Result<DesktopProjection, String> {
    let payload = "native frame replay 中文 🙂 ".repeat(8);
    let items = (0..crate::ui::conversation::model::MAX_TRANSCRIPT_BLOCKS)
        .map(|index| CodingAgentSessionTranscriptItem::User {
            text: format!("message {index}: {payload}"),
            started_at: None,
        })
        .collect();
    projection_with_transcript("desktop-native-performance", items)
}

pub(in crate::app::devtools) fn visual_projection(
    state: VisualReplayState,
) -> Result<DesktopProjection, String> {
    let items = vec![
        CodingAgentSessionTranscriptItem::User {
            text: "请优化 desktop 的消息流体验，并保持键盘导航和中文输入稳定。".into(),
            started_at: None,
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
            model_id: None,
            completed_at: None,
        },
    ];
    let session_id = "desktop-native-visual".to_owned();
    let transcript = CodingAgentTranscriptSnapshot::new(session_id.clone(), None, items);
    let mut snapshot = hydrated_snapshot(session_id, transcript);
    snapshot.session.context.changes = vec![
        visual_change(
            "crates/desktop/src/app/native_shell/inspector_pane.rs",
            343,
            1,
            0,
            Some("@@ -348,0 +349 @@\n+                    .flex_wrap()"),
        ),
        visual_change("scripts/desktop-visual-golden.sh", 1, 24, 3, None),
    ];
    if state == VisualReplayState::Authorization {
        snapshot.session.pending_authorizations = vec![visual_authorization_request()];
    }
    let mut projection = DesktopProjection::new(snapshot).map_err(|issue| issue.message)?;
    apply_visual_running_tool(&mut projection)?;
    Ok(projection)
}

pub(in crate::app::devtools) fn visual_authorization_request() -> ToolAuthorizationRequest {
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

pub(in crate::app::devtools) fn apply_visual_running_tool(
    projection: &mut DesktopProjection,
) -> Result<(), String> {
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
            projection.apply(crate::projection::ProjectionEvent::Product(event)),
            crate::projection::DesktopProjectionApply::Applied(_)
        ) {
            return Err("visual running-tool event did not apply to the projection".into());
        }
    }
    Ok(())
}

pub(in crate::app::devtools) fn projection_with_transcript(
    session_id: &str,
    items: Vec<CodingAgentSessionTranscriptItem>,
) -> Result<DesktopProjection, String> {
    let session_id = session_id.to_owned();
    let transcript = CodingAgentTranscriptSnapshot::new(session_id.clone(), None, items);
    projection_from_transcript(session_id, transcript)
}

pub(in crate::app::devtools) fn projection_from_transcript(
    session_id: String,
    transcript: CodingAgentTranscriptSnapshot,
) -> Result<DesktopProjection, String> {
    DesktopProjection::new(hydrated_snapshot(session_id, transcript)).map_err(|issue| issue.message)
}

pub(in crate::app::devtools) fn hydrated_snapshot(
    session_id: String,
    transcript: CodingAgentTranscriptSnapshot,
) -> DesktopRuntimeHydratedSnapshot {
    DesktopRuntimeHydratedSnapshot {
        project: CodingAgentEmbeddingSnapshot {
            cwd: std::path::PathBuf::from("/desktop-native-replay"),
            workspace: None,
            global_config_dir: std::path::PathBuf::from("/desktop-native-replay/config"),
            selected_model_id: "performance-fixture".into(),
            default_agent_profile_id: ProfileId::from("default"),
            models: vec![
                CodingAgentModelChoice {
                    id: "performance-fixture".into(),
                    name: "Performance Fixture".into(),
                    provider: "fixture".into(),
                    reasoning: true,
                    thinking_capability: CodingAgentThinkingCapability {
                        supported: true,
                        explicit_levels: vec![
                            CodingAgentThinkingLevel::Minimal,
                            CodingAgentThinkingLevel::Low,
                            CodingAgentThinkingLevel::Medium,
                            CodingAgentThinkingLevel::High,
                            CodingAgentThinkingLevel::XHigh,
                        ],
                        can_disable: true,
                    },
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
                    thinking_capability: CodingAgentThinkingCapability::default(),
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
                    thinking_capability: CodingAgentThinkingCapability::default(),
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
            session: CodingAgentSessionView::new(session_id, None, ProfileId::from("default")),
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
