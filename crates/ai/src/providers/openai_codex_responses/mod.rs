pub mod convert;
pub mod wire;

use async_stream::stream;
use base64::Engine;
use std::collections::BTreeMap;

use crate::protocol::{
    AssistantMessage, AssistantMessageEvent, Context, StopReason, StreamOptions,
};

use crate::model::Model;
use crate::protocol::stream::EventStream;
use crate::providers::responses;
use crate::registry::ApiProvider;
use crate::transport::http::send_json_stream;
use convert::build_request;

const DEFAULT_CODEX_BASE_URL: &str = "https://chatgpt.com/backend-api";
const JWT_CLAIM_PATH: &str = "https://api.openai.com/auth";

pub struct OpenAICodexResponsesProvider {
    client: reqwest::Client,
    api_key: Option<String>,
}

impl OpenAICodexResponsesProvider {
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

pub fn resolve_codex_url(base_url: &str) -> String {
    let raw = if base_url.trim().is_empty() {
        DEFAULT_CODEX_BASE_URL
    } else {
        base_url.trim()
    };
    let normalized = raw.trim_end_matches('/');
    if normalized.ends_with("/codex/responses") {
        normalized.into()
    } else if normalized.ends_with("/codex") {
        format!("{}/responses", normalized)
    } else {
        format!("{}/codex/responses", normalized)
    }
}

pub fn build_sse_headers(
    init_headers: Option<&serde_json::Value>,
    additional_headers: Option<&serde_json::Value>,
    token: &str,
    session_id: Option<&str>,
) -> Result<BTreeMap<String, String>, String> {
    let account_id = extract_account_id(token)?;
    let mut headers = BTreeMap::new();
    append_json_headers(&mut headers, init_headers);
    append_json_headers(&mut headers, additional_headers);
    headers.insert("authorization".into(), format!("Bearer {}", token));
    headers.insert("chatgpt-account-id".into(), account_id);
    headers.insert("originator".into(), "evo".into());
    headers.insert("user-agent".into(), "evo (rust)".into());
    headers.insert("openai-beta".into(), "responses=experimental".into());
    headers.insert("accept".into(), "text/event-stream".into());
    headers.insert("content-type".into(), "application/json".into());
    if let Some(session_id) = session_id {
        headers.insert("session-id".into(), session_id.into());
        headers.insert("x-client-request-id".into(), session_id.into());
    }
    Ok(headers)
}

fn append_json_headers(headers: &mut BTreeMap<String, String>, value: Option<&serde_json::Value>) {
    let Some(obj) = value.and_then(|v| v.as_object()) else {
        return;
    };
    for (key, value) in obj {
        if let Some(value) = value.as_str() {
            headers.insert(key.to_ascii_lowercase(), value.into());
        }
    }
}

fn extract_account_id(token: &str) -> Result<String, String> {
    let payload = token
        .split('.')
        .nth(1)
        .ok_or_else(|| "Failed to extract accountId from token".to_string())?;
    let decoded = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload)
        .or_else(|_| base64::engine::general_purpose::STANDARD.decode(payload))
        .map_err(|_| "Failed to extract accountId from token".to_string())?;
    let json: serde_json::Value = serde_json::from_slice(&decoded)
        .map_err(|_| "Failed to extract accountId from token".to_string())?;
    json.get(JWT_CLAIM_PATH)
        .and_then(|auth| auth.get("chatgpt_account_id"))
        .and_then(|id| id.as_str())
        .map(str::to_string)
        .ok_or_else(|| "Failed to extract accountId from token".to_string())
}

impl ApiProvider for OpenAICodexResponsesProvider {
    fn stream(&self, model: &Model, ctx: Context, opts: Option<StreamOptions>) -> EventStream {
        let key = opts
            .as_ref()
            .and_then(|o| o.api_key.clone())
            .or_else(|| self.resolve_key());

        let Some(api_key) = key else {
            let model_id = model.id.clone();
            return Box::pin(stream! {
                let mut msg = AssistantMessage::empty("openai-codex-responses", &model_id);
                msg.provider = Some("openai-codex".into());
                msg.error_message = Some("No OpenAI Codex token found. Set OPENAI_CODEX_API_KEY or pass apiKey in options.".into());
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
            Err(error) => {
                let model_id = model.id.clone();
                let provider = model.provider.clone();
                return Box::pin(stream! {
                    let mut msg = AssistantMessage::empty("openai-codex-responses", &model_id);
                    msg.provider = Some(provider);
                    msg.error_message = Some(format!("Codex request serialization failed: {}", error));
                    msg.stop_reason = StopReason::Error;
                    yield AssistantMessageEvent::Error { reason: StopReason::Error, message: msg };
                });
            }
        };
        let headers = match build_sse_headers(
            model.headers.as_ref(),
            opts.as_ref().and_then(|o| o.headers.as_ref()),
            &api_key,
            opts.as_ref().and_then(|o| o.session_id.as_deref()),
        ) {
            Ok(headers) => headers,
            Err(error) => return error_stream(model, error),
        };

        let url = resolve_codex_url(&model.base_url);
        let mut request = self.client.post(url);
        for (key, value) in headers {
            request = request.header(key, value);
        }

        send_json_stream(
            &self.client,
            model,
            opts.as_ref(),
            "openai-codex-responses",
            request,
            payload,
            |body, model, cancel| {
                responses::stream::process_with_api_name(
                    body,
                    model,
                    cancel,
                    "openai-codex-responses",
                )
            },
        )
    }
}

fn error_stream(model: &Model, error: String) -> EventStream {
    let model_id = model.id.clone();
    let provider = model.provider.clone();
    Box::pin(stream! {
        let mut msg = AssistantMessage::empty("openai-codex-responses", &model_id);
        msg.provider = Some(provider);
        msg.error_message = Some(error);
        msg.stop_reason = StopReason::Error;
        yield AssistantMessageEvent::Error {
            reason: StopReason::Error,
            message: msg,
        };
    })
}
