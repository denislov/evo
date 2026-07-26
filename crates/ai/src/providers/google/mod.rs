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
use crate::transport::http::send_json_stream;
use convert::build_request;

pub struct GoogleGenerativeAiProvider {
    client: reqwest::Client,
    api_key: Option<String>,
}

impl GoogleGenerativeAiProvider {
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

impl ApiProvider for GoogleGenerativeAiProvider {
    fn stream(&self, model: &Model, ctx: Context, opts: Option<StreamOptions>) -> EventStream {
        let key = opts
            .as_ref()
            .and_then(|o| o.api_key.clone())
            .or_else(|| self.resolve_key());
        let Some(api_key) = key else {
            let model_id = model.id.clone();
            let provider = model.provider.clone();
            return Box::pin(stream! {
                let mut msg = AssistantMessage::empty("google-generative-ai", &model_id);
                msg.provider = Some(provider);
                msg.error_message = Some("No Google API key found. Set GEMINI_API_KEY or GOOGLE_API_KEY or pass apiKey in options.".to_string());
                msg.stop_reason = StopReason::Error;
                yield AssistantMessageEvent::Error {
                    reason: StopReason::Error,
                    message: msg,
                };
            });
        };

        let req_body = build_request(model, &ctx, &opts);
        let payload = match serde_json::to_value(&req_body) {
            Ok(payload) => payload,
            Err(error) => return serialization_error(model, error),
        };
        let base_url = model.base_url.trim_end_matches('/');
        let url = format!(
            "{}/models/{}:streamGenerateContent?alt=sse&key={}",
            base_url, model.id, api_key
        );

        let mut request = self
            .client
            .post(&url)
            .header("content-type", "application/json")
            .header("accept", "text/event-stream");

        if let Some(opts) = &opts
            && let Some(ref headers) = opts.headers
            && let Some(obj) = headers.as_object()
        {
            for (k, v) in obj {
                if let Some(val) = v.as_str() {
                    request = request.header(k.as_str(), val);
                }
            }
        }

        send_json_stream(
            &self.client,
            model,
            opts.as_ref(),
            "google-generative-ai",
            request,
            payload,
            |body, model, cancel| stream::process(body, model, cancel),
        )
    }
}

fn serialization_error(model: &Model, error: serde_json::Error) -> EventStream {
    let model_id = model.id.clone();
    let provider = model.provider.clone();
    Box::pin(stream! {
        let mut message = AssistantMessage::empty("google-generative-ai", &model_id);
        message.provider = Some(provider);
        message.error_message = Some(format!("Google request serialization failed: {error}"));
        message.stop_reason = StopReason::Error;
        yield AssistantMessageEvent::Error {
            reason: StopReason::Error,
            message,
        };
    })
}
