use crate::protocol::{AssistantMessage, AssistantMessageEvent, StopReason};
use futures::{Stream, StreamExt};
use std::pin::Pin;

/// Sendable stream of incremental assistant events ending in exactly one
/// provider-neutral terminal event.
pub type EventStream = Pin<Box<dyn Stream<Item = AssistantMessageEvent> + Send>>;

/// Collect a stream until its terminal event.
///
/// Returns only successful `Done` messages whose event and message reasons are
/// both `Stop`, `Length`, or `ToolUse`. Error/aborted `Done` shapes from custom
/// providers and EOF without a terminal event are rejected defensively.
pub async fn complete(mut stream: EventStream) -> Result<AssistantMessage, String> {
    while let Some(event) = stream.next().await {
        match event {
            AssistantMessageEvent::Done { reason, message }
                if is_success_reason(&reason) && is_success_reason(&message.stop_reason) =>
            {
                return Ok(message);
            }
            AssistantMessageEvent::Done { reason, message } => {
                return Err(message.error_message.unwrap_or_else(|| {
                    format!(
                        "stream emitted Done with invalid terminal reasons: event={reason:?}, message={:?}",
                        message.stop_reason
                    )
                }));
            }
            AssistantMessageEvent::Error { message, .. } => {
                return Err(message.error_message.unwrap_or_default());
            }
            _ => continue,
        }
    }
    Err("stream ended without Done event".into())
}

fn is_success_reason(reason: &StopReason) -> bool {
    matches!(
        reason,
        StopReason::Stop | StopReason::Length | StopReason::ToolUse
    )
}
