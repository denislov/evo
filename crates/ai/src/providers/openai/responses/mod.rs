pub mod convert;
pub use crate::providers::responses::{stream, wire};

use async_stream::stream;

use crate::protocol::{
    AssistantMessage, AssistantMessageEvent, Context, StopReason, StreamOptions,
};

use crate::model::Model;
use crate::protocol::stream::EventStream;
use crate::registry::ApiProvider;
use crate::transport::headers::merge_headers;
use crate::transport::http::{
    SendResilience, credential_refresh_slot, send_json_stream_with_resilience,
};
use convert::build_request;

pub struct OpenAIResponsesProvider {
    client: reqwest::Client,
    api_key: Option<String>,
}

impl OpenAIResponsesProvider {
    pub(crate) fn with_client(api_key: Option<String>, client: reqwest::Client) -> Self {
        Self { client, api_key }
    }

    fn resolve_key(&self) -> Option<String> {
        self.api_key.clone()
    }
}

impl ApiProvider for OpenAIResponsesProvider {
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
                let mut request = client.post(&url);
                if let Some(key) = key {
                    request = request.bearer_auth(key);
                }
                for (name, value) in merge_headers(
                    model_headers.as_ref(),
                    current.and_then(|o| o.headers.as_ref()),
                    [
                        ("content-type".into(), "application/json".into()),
                        ("accept".into(), "text/event-stream".into()),
                    ],
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
            "openai-responses",
            serde_json::to_value(&req_body).unwrap_or_default(),
            build_request,
            |body_stream, model, cancel| stream::process(body_stream, model, cancel),
            SendResilience {
                breaker: resilience.breaker,
                refresh_auth: refresh,
                scrubber: resilience.scrubber,
            },
        )
    }
}
