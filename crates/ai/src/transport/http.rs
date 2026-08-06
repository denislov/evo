use async_stream::stream;
use futures::StreamExt;
use std::future::Future;
use std::sync::{Arc, RwLock};
use std::time::Duration;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use super::circuit_breaker::{BreakerVerdict, CircuitBreaker};
use super::error::ProviderError;
use crate::model::Model;
use crate::protocol::stream::EventStream;
use crate::protocol::{AssistantMessage, AssistantMessageEvent, StopReason, StreamOptions};
use crate::registry::options_contain_automatic_credentials;
use crate::scrub::SecretsScrubber;
use crate::transport::retry::RetryConfig;

/// Refresh callback: re-resolve credentials and return the updated options,
/// or `None` when no fresher credential is available.
pub type AuthRefresh = Box<dyn FnMut() -> Option<StreamOptions> + Send>;

/// Optional resilience policy threaded from the caller into one invocation.
#[derive(Default)]
pub struct SendResilience {
    /// Per-(provider, api) breaker consulted before every attempt.
    pub breaker: Option<Arc<CircuitBreaker>>,
    /// Single-shot 401 credential refresh; `None` disables refresh/retry.
    pub refresh_auth: Option<AuthRefresh>,
    /// Redacts error messages that leave the crate.
    pub scrubber: Option<Arc<SecretsScrubber>>,
}

pub fn send_json_stream<F>(
    _client: &reqwest::Client,
    model: &Model,
    opts: Option<&StreamOptions>,
    api_name: &str,
    request: reqwest::RequestBuilder,
    payload: serde_json::Value,
    process_body: F,
) -> EventStream
where
    F: FnOnce(
            Box<dyn futures::Stream<Item = Result<bytes::Bytes, String>> + Send + Unpin>,
            Model,
            Option<CancellationToken>,
        ) -> EventStream
        + Send
        + 'static,
{
    send_json_stream_with_request_factory(
        model,
        opts,
        api_name,
        payload,
        move |payload| {
            request
                .try_clone()
                .map(|request| request.json(payload))
                .ok_or_else(|| "request could not be cloned for retryable send".to_string())
        },
        process_body,
    )
}

pub fn send_json_stream_with_request_factory<FRequest, FBody>(
    model: &Model,
    opts: Option<&StreamOptions>,
    api_name: &str,
    payload: serde_json::Value,
    build_request: FRequest,
    process_body: FBody,
) -> EventStream
where
    FRequest: FnMut(&serde_json::Value) -> Result<reqwest::RequestBuilder, String> + Send + 'static,
    FBody: FnOnce(
            Box<dyn futures::Stream<Item = Result<bytes::Bytes, String>> + Send + Unpin>,
            Model,
            Option<CancellationToken>,
        ) -> EventStream
        + Send
        + 'static,
{
    send_json_stream_with_resilience(
        model,
        opts,
        api_name,
        payload,
        build_request,
        process_body,
        SendResilience::default(),
    )
}

/// Send path with breaker, single-shot 401 refresh, and error scrubbing.
/// Breaker decisions happen before the request is built or sent; recorded
/// failures are network errors, timeouts, and retryable statuses only, so
/// configuration errors (401/403/404) never open the breaker.
pub(crate) fn send_json_stream_with_resilience<FRequest, FBody>(
    model: &Model,
    opts: Option<&StreamOptions>,
    api_name: &str,
    payload: serde_json::Value,
    mut build_request: FRequest,
    process_body: FBody,
    resilience: SendResilience,
) -> EventStream
where
    FRequest: FnMut(&serde_json::Value) -> Result<reqwest::RequestBuilder, String> + Send + 'static,
    FBody: FnOnce(
            Box<dyn futures::Stream<Item = Result<bytes::Bytes, String>> + Send + Unpin>,
            Model,
            Option<CancellationToken>,
        ) -> EventStream
        + Send
        + 'static,
{
    let SendResilience {
        breaker,
        mut refresh_auth,
        scrubber,
    } = resilience;
    let model = model.clone();
    let model_id = model.id.clone();
    let provider = model.provider.clone();
    let api_name = api_name.to_string();
    let cancel = opts.and_then(|o| o.cancel.clone());
    let retry_cfg = RetryConfig::from_options(opts);
    let hooks = opts.and_then(|o| o.hooks.clone());
    let mut option_error = validate_options(&api_name, opts).err();
    let automatic_credentials = options_contain_automatic_credentials(opts);
    let deadline = retry_cfg.timeout_ms.and_then(|timeout_ms| {
        Instant::now()
            .checked_add(Duration::from_millis(timeout_ms))
            .map(|at| InvocationDeadline { at, timeout_ms })
            .or_else(|| {
                option_error.get_or_insert_with(|| {
                    "timeout_ms cannot be represented by the runtime clock".to_string()
                });
                None
            })
    });

    Box::pin(stream! {
        if let Some(error) = option_error {
            let error = ProviderError::unsupported_option(
                &api_name,
                &model_id,
                &provider,
                error,
            );
            let mut msg = error_event(&api_name, &model_id, &provider, &error);
            scrub_error_message(scrubber.as_deref(), &mut msg);
            yield AssistantMessageEvent::Error {
                reason: StopReason::Error,
                message: msg,
            };
            return;
        }
        if retry_cfg.timeout_ms == Some(0) {
            yield wait_error_event(
                &api_name,
                &model_id,
                &provider,
                WaitError::Timeout { timeout_ms: 0 },
                scrubber.as_deref(),
            );
            return;
        }
        let final_payload = match hooks.as_ref() {
            Some(hooks) => match wait_for(
                hooks.apply_payload(&model, payload),
                cancel.as_ref(),
                deadline,
            ).await {
                Ok(Ok(payload)) => payload,
                Ok(Err(_error)) => {
                    let mut msg = AssistantMessage::empty(&api_name, &model_id);
                    msg.provider = Some(provider.clone());
                    msg.error_message = Some("Payload hook failed".to_string());
                    msg.stop_reason = StopReason::Error;
                    yield AssistantMessageEvent::Error {
                        reason: StopReason::Error,
                        message: msg,
                    };
                    return;
                }
                Err(wait_error) => {
                    yield wait_error_event(
                        &api_name,
                        &model_id,
                        &provider,
                        wait_error,
                        scrubber.as_deref(),
                    );
                    return;
                }
            },
            None => payload,
        };

        let mut last_error: Option<ProviderError> = None;
        let mut auth_refreshed = false;

        let mut attempt: u32 = 0;
        loop {
            if attempt > retry_cfg.max_retries {
                break;
            }
            if let Some(breaker) = breaker.as_ref() {
                match breaker.before_request() {
                    BreakerVerdict::Allow => {}
                    BreakerVerdict::Reject { retry_after_ms } => {
                        last_error = Some(ProviderError::circuit_open(
                            &api_name,
                            &model_id,
                            &provider,
                            retry_after_ms,
                        ));
                        break;
                    }
                }
            }
            let request = match build_request(&final_payload) {
                Ok(request) => request,
                Err(_error) => {
                    last_error = Some(ProviderError::network(
                        &api_name,
                        &model_id,
                        &provider,
                    ));
                    break;
                }
            };
            let response = match wait_for(request.send(), cancel.as_ref(), deadline).await {
                Ok(Ok(response)) => response,
                Ok(Err(_error)) => {
                    record_failure(breaker.as_ref());
                    last_error = Some(ProviderError::network(
                        &api_name,
                        &model_id,
                        &provider,
                    ));
                    if !should_retry(&last_error, &retry_cfg, attempt) {
                        break;
                    }
                    match wait_for(
                        tokio::time::sleep(Duration::from_millis(
                            retry_cfg.backoff_delay_ms(attempt),
                        )),
                        cancel.as_ref(),
                        deadline,
                    )
                    .await
                    {
                        Ok(()) => {
                            attempt += 1;
                            continue;
                        }
                        Err(wait_error) => {
                            yield wait_error_event(
                                &api_name,
                                &model_id,
                                &provider,
                                wait_error,
                                scrubber.as_deref(),
                            );
                            return;
                        }
                    }
                }
                Err(WaitError::Timeout { timeout_ms }) => {
                    record_failure(breaker.as_ref());
                    last_error = Some(ProviderError::timeout(
                        &api_name,
                        &model_id,
                        &provider,
                        timeout_ms,
                    ));
                    break;
                }
                Err(WaitError::Cancelled) => {
                    yield wait_error_event(
                        &api_name,
                        &model_id,
                        &provider,
                        WaitError::Cancelled,
                        scrubber.as_deref(),
                    );
                    return;
                }
            };

            let status = response.status().as_u16();

            if let Some(hooks) = hooks.as_ref() {
                let response_info = crate::protocol::ProviderResponseInfo {
                    status: Some(status),
                };
                match wait_for(
                    hooks.emit_response(response_info),
                    cancel.as_ref(),
                    deadline,
                ).await {
                    Ok(Ok(())) => {}
                    Ok(Err(_error)) => {
                        let mut msg = AssistantMessage::empty(&api_name, &model_id);
                        msg.provider = Some(provider.clone());
                        msg.error_message = Some("Response hook failed".to_string());
                        msg.stop_reason = StopReason::Error;
                        yield AssistantMessageEvent::Error {
                            reason: StopReason::Error,
                            message: msg,
                        };
                        return;
                    }
                    Err(wait_error) => {
                        yield wait_error_event(
                            &api_name,
                            &model_id,
                            &provider,
                            wait_error,
                            scrubber.as_deref(),
                        );
                        return;
                    }
                }
            }

            if !response.status().is_success() {
                if status == 401
                    && !auth_refreshed
                    && refresh_auth.is_some()
                    && automatic_credentials
                    && refresh_auth.as_mut().and_then(|refresh| refresh()).is_some()
                {
                    auth_refreshed = true;
                    drop(response);
                    continue;
                }

                if crate::transport::retry::is_retryable_status(status) && attempt < retry_cfg.max_retries {
                    record_failure(breaker.as_ref());
                    let retry_delay = match retry_delay_ms(
                        response.headers(),
                        &retry_cfg,
                        attempt,
                    ) {
                        Ok(ms) => ms,
                        Err(e) => {
                            last_error = Some(ProviderError::retry_after_too_long(
                                &api_name,
                                &model_id,
                                &provider,
                                e,
                            ));
                            break;
                        }
                    };
                    drop(response);
                    match wait_for(
                        tokio::time::sleep(Duration::from_millis(retry_delay)),
                        cancel.as_ref(),
                        deadline,
                    ).await {
                        Ok(()) => {
                            attempt += 1;
                            continue;
                        }
                        Err(wait_error) => {
                            yield wait_error_event(
                                &api_name,
                                &model_id,
                                &provider,
                                wait_error,
                                scrubber.as_deref(),
                            );
                            return;
                        }
                    }
                }

                record_failure(breaker.as_ref());
                last_error = Some(ProviderError::http_status(
                    &api_name, &model_id, &provider, status,
                ));
                break;
            }

            if let Some(breaker) = breaker.as_ref() {
                breaker.record_success();
            }

            let body_stream: Box<dyn futures::Stream<Item = Result<bytes::Bytes, String>> + Send + Unpin> =
                Box::new(response.bytes_stream().map(|result| {
                    result.map_err(|_error| "response body stream failed".to_string())
                }));

            let mut event_stream = process_body(body_stream, model.clone(), cancel.clone());
            loop {
                match wait_for(event_stream.next(), cancel.as_ref(), deadline).await {
                    Ok(Some(event)) => yield event,
                    Ok(None) => break,
                    Err(wait_error) => {
                        yield wait_error_event(
                            &api_name,
                            &model_id,
                            &provider,
                            wait_error,
                            scrubber.as_deref(),
                        );
                        return;
                    }
                }
            }
            return;
        }

        let err = last_error.unwrap_or_else(|| {
            ProviderError::network(&api_name, &model_id, &provider)
        });
        let mut msg = error_event(&api_name, &model_id, &provider, &err);
        if matches!(
            err.kind(),
            super::error::ProviderErrorKind::Cancelled
        ) {
            msg.stop_reason = StopReason::Aborted;
        }
        scrub_error_message(scrubber.as_deref(), &mut msg);
        yield AssistantMessageEvent::Error {
            reason: msg.stop_reason.clone(),
            message: msg,
        };
    })
}

/// Credential slot wired between a 401 refresh callback and the request
/// builder: the refresh writes the rotated options, the builder reads them on
/// every attempt. Returns the refreshed builder and the refresh closure the
/// send loop should call (which updates the slot before answering).
pub(crate) fn credential_refresh_slot<F>(
    mut build: F,
    refresh: Option<AuthRefresh>,
    initial: Option<StreamOptions>,
) -> (
    impl FnMut(&serde_json::Value) -> Result<reqwest::RequestBuilder, String> + Send,
    Option<AuthRefresh>,
)
where
    F: FnMut(Option<&StreamOptions>, &serde_json::Value) -> Result<reqwest::RequestBuilder, String>
        + Send
        + 'static,
{
    let slot: Arc<RwLock<Option<StreamOptions>>> = Arc::new(RwLock::new(initial));
    let refresh_for_http: Option<AuthRefresh> = refresh.map(|mut refresh| {
        let slot = slot.clone();
        Box::new(move || {
            let fresh = refresh()?;
            fresh.api_key.as_ref()?;
            *slot.write().unwrap() = Some(fresh.clone());
            Some(fresh)
        }) as AuthRefresh
    });
    let build_request = move |payload: &serde_json::Value| {
        let current = slot.read().unwrap().clone();
        build(current.as_ref(), payload)
    };
    (build_request, refresh_for_http)
}

fn record_failure(breaker: Option<&Arc<CircuitBreaker>>) {
    if let Some(breaker) = breaker {
        breaker.record_failure();
    }
}

pub(crate) fn scrub_error_message(scrubber: Option<&SecretsScrubber>, msg: &mut AssistantMessage) {
    let Some(scrubber) = scrubber else {
        return;
    };
    if let Some(message) = msg.error_message.take() {
        msg.error_message = Some(scrubber.scrub(&message));
    }
}

pub(crate) fn validate_options(api: &str, opts: Option<&StreamOptions>) -> Result<(), String> {
    let Some(opts) = opts else {
        return Ok(());
    };

    if let Some(headers) = &opts.headers {
        let object = headers
            .as_object()
            .ok_or_else(|| "headers must be a JSON object".to_string())?;
        if let Some((name, _)) = object.iter().find(|(_, value)| !value.is_string()) {
            return Err(format!("header `{name}` must have a string value"));
        }
    }

    if let Some(transport) = opts.transport.as_deref() {
        let supports_sse = matches!(
            api,
            "anthropic-messages"
                | "deepseek-responses"
                | "openai-completions"
                | "openai-responses"
                | "google-generative-ai"
                | "mistral-conversations"
                | "openai-codex-responses"
        );
        if transport != "sse" || !supports_sse {
            return Err(format!(
                "transport `{transport}` is unsupported by API `{api}`"
            ));
        }
    }

    if opts.session_id.is_some()
        && !matches!(api, "mistral-conversations" | "openai-codex-responses")
    {
        return Err(format!("session_id is unsupported by API `{api}`"));
    }

    if let Some(responses) = &opts.responses {
        if !matches!(api, "deepseek-responses" | "openai-responses") {
            return Err(format!("Responses options are unsupported by API `{api}`"));
        }
        if responses
            .top_p
            .is_some_and(|value| !value.is_finite() || !(0.0..=1.0).contains(&value))
        {
            return Err("Responses top_p must be between 0.0 and 1.0".into());
        }
        if responses.top_logprobs.is_some_and(|value| value > 20) {
            return Err("Responses top_logprobs must be between 0 and 20".into());
        }
        if responses
            .user
            .as_ref()
            .is_some_and(|value| value.trim().is_empty())
        {
            return Err("Responses user must not be empty".into());
        }
        if api == "deepseek-responses" && responses.prompt_cache_key.is_some() {
            return Err(
                "DeepSeek Responses does not support prompt_cache_key; caching is automatic".into(),
            );
        }
    }

    if let Some(tool_choice) = &opts.tool_choice
        && api == "openai-codex-responses"
        && !tool_choice.is_string()
    {
        return Err("Codex tool_choice must be a string".into());
    }

    RetryConfig::from_options(Some(opts)).validate()?;

    Ok(())
}

#[derive(Debug, Clone, Copy)]
struct InvocationDeadline {
    at: Instant,
    timeout_ms: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WaitError {
    Cancelled,
    Timeout { timeout_ms: u64 },
}

async fn wait_for<F: Future>(
    future: F,
    cancel: Option<&CancellationToken>,
    deadline: Option<InvocationDeadline>,
) -> Result<F::Output, WaitError> {
    match (cancel, deadline) {
        (Some(cancel), Some(deadline)) => tokio::select! {
            biased;
            _ = cancel.cancelled() => Err(WaitError::Cancelled),
            _ = tokio::time::sleep_until(deadline.at) => {
                Err(WaitError::Timeout { timeout_ms: deadline.timeout_ms })
            }
            output = future => Ok(output),
        },
        (Some(cancel), None) => tokio::select! {
            biased;
            _ = cancel.cancelled() => Err(WaitError::Cancelled),
            output = future => Ok(output),
        },
        (None, Some(deadline)) => tokio::select! {
            biased;
            _ = tokio::time::sleep_until(deadline.at) => {
                Err(WaitError::Timeout { timeout_ms: deadline.timeout_ms })
            }
            output = future => Ok(output),
        },
        (None, None) => Ok(future.await),
    }
}

fn wait_error_event(
    api_name: &str,
    model_id: &str,
    provider: &str,
    error: WaitError,
    scrubber: Option<&SecretsScrubber>,
) -> AssistantMessageEvent {
    let provider_error = match error {
        WaitError::Cancelled => ProviderError::cancelled(api_name, model_id, provider),
        WaitError::Timeout { timeout_ms } => {
            ProviderError::timeout(api_name, model_id, provider, timeout_ms)
        }
    };
    let mut message = error_event(api_name, model_id, provider, &provider_error);
    let reason = match error {
        WaitError::Cancelled => StopReason::Aborted,
        WaitError::Timeout { .. } => StopReason::Error,
    };
    message.stop_reason = reason.clone();
    scrub_error_message(scrubber, &mut message);
    AssistantMessageEvent::Error { reason, message }
}

fn should_retry(error: &Option<ProviderError>, cfg: &RetryConfig, attempt: u32) -> bool {
    if attempt >= cfg.max_retries {
        return false;
    }
    match error {
        Some(e) => match e.kind() {
            super::error::ProviderErrorKind::Network => true,
            // `timeout_ms` is one invocation-wide deadline, so a timeout means
            // there is no remaining budget for another attempt.
            super::error::ProviderErrorKind::Timeout => false,
            super::error::ProviderErrorKind::HttpStatus => e
                .status()
                .is_some_and(crate::transport::retry::is_retryable_status),
            _ => false,
        },
        None => false,
    }
}

fn retry_delay_ms(
    headers: &reqwest::header::HeaderMap,
    cfg: &RetryConfig,
    attempt: u32,
) -> Result<u64, String> {
    if let Some(value) = headers.get("retry-after-ms") {
        let value = value
            .to_str()
            .map_err(|_| "Retry-After-MS header is not valid ASCII".to_string())?;
        let ms = value
            .trim()
            .parse::<u64>()
            .map_err(|_| "Retry-After-MS header is not a valid integer".to_string())?;
        if ms > cfg.max_retry_delay_ms {
            return Err(format!(
                "Retry-After {}ms exceeds max_retry_delay_ms {}ms",
                ms, cfg.max_retry_delay_ms
            ));
        }
        return Ok(ms);
    }

    if let Some(value) = headers.get("retry-after") {
        let value = value
            .to_str()
            .map_err(|_| "Retry-After header is not valid ASCII".to_string())?;
        return crate::transport::retry::parse_retry_after_ms(Some(value), cfg);
    }

    Ok(cfg.backoff_delay_ms(attempt))
}

fn error_event(
    api_name: &str,
    model_id: &str,
    provider: &str,
    error: &ProviderError,
) -> AssistantMessage {
    let mut msg = AssistantMessage::empty(api_name, model_id);
    msg.provider = Some(provider.to_string());
    msg.error_message = Some(error.message().to_string());
    msg.stop_reason = StopReason::Error;
    msg
}
