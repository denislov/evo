pub mod convert;
pub mod stream;
pub mod wire;

use async_stream::stream;

use crate::protocol::{
    AssistantMessage, AssistantMessageEvent, Context, StopReason, StreamOptions,
};

use crate::model::Model;
use crate::protocol::stream::EventStream;
use crate::registry::ApiProvider;
use crate::transport::headers::merge_headers;
use crate::transport::http::send_json_stream;
use convert::build_request;

pub struct OpenAIResponsesProvider {
    client: reqwest::Client,
    api_key: Option<String>,
}

impl OpenAIResponsesProvider {
    pub fn new(api_key: Option<String>) -> Self {
        Self {
            client: crate::transport::client::authenticated_client(),
            api_key,
        }
    }

    fn resolve_key(&self) -> Option<String> {
        self.api_key.clone()
    }
}

impl ApiProvider for OpenAIResponsesProvider {
    fn stream(&self, model: &Model, ctx: Context, opts: Option<StreamOptions>) -> EventStream {
        let key = opts
            .as_ref()
            .and_then(|o| o.api_key.clone())
            .or_else(|| self.resolve_key());

        let Some(api_key) = key else {
            let model_id = model.id.clone();
            let provider = model.provider.clone();
            let error_message = format!(
                "No API key found for provider {provider}. Set the appropriate env var or pass apiKey in options."
            );
            return Box::pin(stream! {
                let mut msg = AssistantMessage::empty("openai-responses", &model_id);
                msg.provider = Some(provider);
                msg.error_message = Some(error_message);
                msg.stop_reason = StopReason::Error;
                yield AssistantMessageEvent::Error {
                    reason: StopReason::Error,
                    message: msg,
                };
            });
        };

        let req_body = build_request(model, &ctx, &opts);
        let base_url = model.base_url.trim_end_matches('/');
        let url = if base_url.ends_with("/v1") {
            format!("{}/responses", base_url)
        } else {
            format!("{}/v1/responses", base_url)
        };

        let mut request = self.client.post(&url).bearer_auth(api_key);

        for (key, value) in merge_headers(
            model.headers.as_ref(),
            opts.as_ref().and_then(|opts| opts.headers.as_ref()),
            [
                ("content-type".into(), "application/json".into()),
                ("accept".into(), "text/event-stream".into()),
            ],
        ) {
            request = request.header(key, value);
        }

        send_json_stream(
            &self.client,
            model,
            opts.as_ref(),
            "openai-responses",
            request,
            serde_json::to_value(&req_body).unwrap_or_default(),
            |body_stream, model, cancel| stream::process(body_stream, model, cancel),
        )
    }
}
