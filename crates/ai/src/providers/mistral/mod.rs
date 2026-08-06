pub mod convert;
pub mod stream;
pub mod wire;

use async_stream::stream;
use std::collections::BTreeMap;

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

pub struct MistralProvider {
    client: reqwest::Client,
    api_key: Option<String>,
}

impl MistralProvider {
    pub(crate) fn with_client(api_key: Option<String>, client: reqwest::Client) -> Self {
        Self { client, api_key }
    }

    fn resolve_key(&self) -> Option<String> {
        self.api_key.clone()
    }
}

/// Header merge for one request without a standalone `StreamOptions` value;
/// the `x-affinity` session header is derived from `current` when present.
fn append_json_headers(headers: &mut BTreeMap<String, String>, value: Option<&serde_json::Value>) {
    let Some(obj) = value.and_then(|v| v.as_object()) else {
        return;
    };
    for (key, value) in obj {
        if let Some(value) = value.as_str() {
            headers.insert(key.clone(), value.to_string());
        }
    }
}

impl ApiProvider for MistralProvider {
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
            return Box::pin(stream! {
                let mut msg = AssistantMessage::empty("mistral-conversations", &model_id);
                msg.provider = Some("mistral".into());
                msg.error_message = Some("No Mistral API key found. Set MISTRAL_API_KEY or pass apiKey in options.".into());
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
                    "mistral-conversations",
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
        let url = if base_url.ends_with("/v1") {
            format!("{}/chat/completions", base_url)
        } else {
            format!("{}/v1/chat/completions", base_url)
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
                let mut request = client
                    .post(&url)
                    .header("content-type", "application/json")
                    .header("accept", "text/event-stream");
                if let Some(key) = key {
                    request = request.bearer_auth(key);
                }
                for (name, value) in build_headers_from(model_headers.as_ref(), current) {
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
            "mistral-conversations",
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

/// Header merge for one request without a standalone `StreamOptions` value;
/// the `x-affinity` session header is derived from `current` when present.
fn build_headers_from(
    model_headers: Option<&serde_json::Value>,
    current: Option<&StreamOptions>,
) -> BTreeMap<String, String> {
    let mut headers = BTreeMap::new();
    append_json_headers(&mut headers, model_headers);
    if let Some(opts) = current {
        append_json_headers(&mut headers, opts.headers.as_ref());
        if let Some(session_id) = &opts.session_id {
            headers
                .entry("x-affinity".into())
                .or_insert_with(|| session_id.clone());
        }
    }
    headers
}

fn serialization_error(model: &Model, error: serde_json::Error) -> EventStream {
    let model_id = model.id.clone();
    let provider = model.provider.clone();
    Box::pin(stream! {
        let mut message = AssistantMessage::empty("mistral-conversations", &model_id);
        message.provider = Some(provider);
        message.error_message = Some(format!("Mistral request serialization failed: {error}"));
        message.stop_reason = StopReason::Error;
        yield AssistantMessageEvent::Error {
            reason: StopReason::Error,
            message,
        };
    })
}
