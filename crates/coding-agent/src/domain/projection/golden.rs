use std::collections::BTreeSet;

use serde_json::{Value, json};

use crate::events::CodingAgentProductEvent;
use crate::profiles::ProfileId;
use crate::runtime::client::connection::{
    CodingAgentContextSnapshot, CodingAgentSnapshot, CodingAgentSnapshotCursor,
};
use crate::runtime::client::projection::{
    CodingAgentClientMessageStatus, CodingAgentClientProjection, CodingAgentClientProjectionApply,
    CodingAgentClientToolStatus,
};
use crate::runtime::facade::context::CodingAgentCapabilities;
use crate::runtime::version::UI_SNAPSHOT_PROTOCOL_VERSION;
use crate::session::view::CodingAgentSessionView;

const CROSS_ADAPTER_EVENTS: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/client_projection/cross-adapter-events.json"
));
const CROSS_ADAPTER_PROJECTION: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/client_projection/cross-adapter-projection.json"
));
const ALL_PRODUCT_EVENT_FAMILIES: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/client_projection/all-product-event-families.json"
));

#[test]
fn product_event_schema_golden_covers_every_family_and_round_trips() {
    let source: Value = serde_json::from_str(ALL_PRODUCT_EVENT_FAMILIES).expect("schema fixture");
    let events: Vec<CodingAgentProductEvent> =
        serde_json::from_value(source.clone()).expect("deserialize every product event family");
    let families = events
        .iter()
        .map(|event| {
            serde_json::to_value(event.family_typed())
                .expect("serialize product event family")
                .as_str()
                .expect("family serializes as a string")
                .to_owned()
        })
        .collect::<BTreeSet<_>>();
    assert_eq!(
        families,
        BTreeSet::from([
            "session".to_owned(),
            "agent".to_owned(),
            "team".to_owned(),
            "message".to_owned(),
            "tool".to_owned(),
            "runtime".to_owned(),
            "delegation".to_owned(),
            "workflow".to_owned(),
            "diagnostic".to_owned(),
            "capability".to_owned(),
        ])
    );
    assert_eq!(
        serde_json::to_value(events).expect("serialize every product event family"),
        source,
        "a DTO/schema change must update the reviewed family fixture"
    );
}

#[test]
fn shared_cross_adapter_events_match_the_client_projection_golden() {
    let events: Vec<CodingAgentProductEvent> =
        serde_json::from_str(CROSS_ADAPTER_EVENTS).expect("cross-adapter event fixture");
    let mut projection = CodingAgentClientProjection::new(initial_snapshot())
        .expect("valid initial client projection");
    for event in &events {
        assert!(
            matches!(
                projection.apply(event),
                CodingAgentClientProjectionApply::Applied(_)
            ),
            "golden event must be accepted: {event:?}"
        );
    }

    let actual = json!({
        "cursor": projection.snapshot().cursor.last_event_sequence,
        "messages": projection.messages().iter().map(|message| json!({
            "operation_id": message.operation_id,
            "turn_id": message.turn_id,
            "message_id": message.message_id,
            "text": message.text,
            "thinking": message.thinking,
            "reasoning_duration_millis": message.reasoning_duration_millis,
            "status": message_status(message.status),
            "started_sequence": message.started_sequence,
            "updated_sequence": message.updated_sequence,
            "truncated": message.truncated,
        })).collect::<Vec<_>>(),
        "tools": projection.tools().iter().map(|tool| json!({
            "operation_id": tool.operation_id,
            "turn_id": tool.turn_id,
            "tool_call_id": tool.tool_call_id,
            "name": tool.name,
            "arguments": tool.arguments,
            "detail": tool.detail,
            "status": tool_status(tool.status),
            "started_sequence": tool.started_sequence,
            "updated_sequence": tool.updated_sequence,
            "truncated": tool.truncated,
        })).collect::<Vec<_>>(),
        "context": projection.snapshot().context,
    });
    let expected: Value =
        serde_json::from_str(CROSS_ADAPTER_PROJECTION).expect("projection golden fixture");
    assert_eq!(
        actual, expected,
        "review DTO/reducer changes before updating the golden"
    );
}

fn initial_snapshot() -> CodingAgentSnapshot {
    CodingAgentSnapshot {
        cursor: CodingAgentSnapshotCursor {
            stream_id: "test-stream".into(),
            snapshot_protocol_major: UI_SNAPSHOT_PROTOCOL_VERSION.major,
            last_event_sequence: 0,
            last_session_sequence: 0,
            capability_generation: 0,
        },
        version: UI_SNAPSHOT_PROTOCOL_VERSION,
        session: CodingAgentSessionView::new(
            "session-1",
            None,
            ProfileId::new("default").expect("valid default profile id"),
        ),
        capabilities: CodingAgentCapabilities::idle(true),
        active_operation: None,
        drafts: Vec::new(),
        submitted_operation: None,
        pending_authorizations: Vec::new(),
        context: CodingAgentContextSnapshot::default(),
    }
}

const fn message_status(status: CodingAgentClientMessageStatus) -> &'static str {
    match status {
        CodingAgentClientMessageStatus::Streaming => "streaming",
        CodingAgentClientMessageStatus::Completed => "completed",
    }
}

const fn tool_status(status: CodingAgentClientToolStatus) -> &'static str {
    match status {
        CodingAgentClientToolStatus::Running => "running",
        CodingAgentClientToolStatus::Completed => "completed",
        CodingAgentClientToolStatus::Failed => "failed",
    }
}
