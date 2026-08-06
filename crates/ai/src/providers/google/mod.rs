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
use crate::transport::http::{
    SendResilience, credential_refresh_slot, send_json_stream_with_resilience,
};
use convert::build_request;

pub struct GoogleGenerativeAiProvider {
    client: reqwest::Client,
    api_key: Option<String>,
}

impl GoogleGenerativeAiProvider {
    pub(crate) fn with_client(api_key: Option<String>, client: reqwest::Client) -> Self {
        Self { client, api_key }
    }

    fn resolve_key(&self) -> Option<String> {
        self.api_key.clone()
    }
}

impl ApiProvider for GoogleGenerativeAiProvider {
    fn stream(&self, model: &Model, ctx: Context, opts: Option<StreamOptions>) -> EventStream {
        self.stream_with_resilience(model, ctx, opts, SendResilience::default())
    }

    fn stream_with_resilience(
        &self,
        model: &Model,
        ctx: Context,
        opts: Option<StreamOptions>,
        resilience: SendResilience,
    ) -> EventStream {
        let key = opts
            .as_ref()
            .and_then(|o| o.api_key.clone())
            .or_else(|| self.resolve_key());
        let Some(_api_key) = key else {
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

        let req_body = match build_request(model, &ctx, &opts) {
            Ok(body) => body,
            Err(error) => {
                return crate::providers::common::request_rejected_stream(
                    "google-generative-ai",
                    model,
                    error,
                );
            }
        };
        let payload = match serde_json::to_value(&req_body) {
            Ok(payload) => payload,
            Err(error) => return serialization_error(model, error),
        };
        let base_url = model.base_url.trim_end_matches('/');
        let url = format!(
            "{}/models/{}:streamGenerateContent?alt=sse",
            base_url, model.id
        );
        let client = self.client.clone();
        let initial_opts = opts.clone();
        let model_headers = model.headers.clone();
        let self_api_key = self.api_key.clone();
        let (build_request, refresh) = credential_refresh_slot(
            move |current: Option<&StreamOptions>, payload| {
                let current = current.or(initial_opts.as_ref());
                let key = current
                    .and_then(|o| o.api_key.clone())
                    .or_else(|| self_api_key.clone());
                let mut generated: Vec<(String, String)> = vec![
                    ("content-type".into(), "application/json".into()),
                    ("accept".into(), "text/event-stream".into()),
                ];
                if let Some(key) = key {
                    generated.push(("x-goog-api-key".into(), key));
                }
                let mut request = client.post(&url);
                for (name, value) in crate::transport::headers::merge_headers(
                    model_headers.as_ref(),
                    current.and_then(|o| o.headers.as_ref()),
                    generated,
                ) {
                    request = request.header(name, value);
                }
                Ok(request.json(payload))
            },
            resilience.refresh_auth,
            opts.clone(),
        );

        send_json_stream_with_resilience(
            model,
            opts.as_ref(),
            "google-generative-ai",
            payload,
            build_request,
            |body, model, cancel| stream::process(body, model, cancel),
            SendResilience {
                breaker: resilience.breaker,
                refresh_auth: refresh,
                scrubber: resilience.scrubber,
            },
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
