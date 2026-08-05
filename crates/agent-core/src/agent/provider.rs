use std::sync::Arc;

use ai_protocol::api::conversation::{AssistantMessage, Context, StopReason};
use ai_protocol::api::model::Model;
use ai_protocol::api::stream::{AssistantMessageEvent, EventStream, StreamOptions};
use async_stream::stream;

pub type ProviderStreamer =
    Arc<dyn Fn(&Model, Context, Option<StreamOptions>) -> EventStream + Send + Sync>;

pub(crate) fn stream_model_with_provider_streamer(
    model: &Model,
    context: Context,
    options: Option<StreamOptions>,
    provider_streamer: Option<ProviderStreamer>,
) -> EventStream {
    match provider_streamer {
        Some(provider_streamer) => provider_streamer(model, context, options),
        None => missing_provider_streamer(model),
    }
}

fn missing_provider_streamer(model: &Model) -> EventStream {
    let model_id = model.id.clone();
    let provider = model.provider.clone();
    Box::pin(stream! {
        let mut message = AssistantMessage::empty("unconfigured", &model_id);
        message.provider = Some(provider);
        message.stop_reason = StopReason::Error;
        message.error_message = Some(
            "provider streamer is required; inject a scoped provider runtime into AgentConfig"
                .into(),
        );
        yield AssistantMessageEvent::Error {
            reason: StopReason::Error,
            message,
        };
    })
}
