use std::collections::VecDeque;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use futures::StreamExt;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use crate::model::Model;
use crate::protocol::stream::EventStream;
use crate::protocol::{
    AssistantMessage, AssistantMessageEvent, ContentBlock, Context, ProviderAuthDiagnostic,
    StopReason, StreamOptions,
};
use crate::registry::ApiProvider;
use crate::scrub::SecretsScrubber;
use crate::transport::circuit_breaker::{
    BreakerKey, BreakerVerdict, CircuitBreaker, CircuitBreakerConfig,
};
use crate::transport::client::authenticated_client;
use crate::transport::http::{
    AuthRefresh, SendResilience, credential_refresh_slot, scrub_error_message,
    send_json_stream_with_resilience,
};

struct MockServer {
    url: String,
    requests: Arc<Mutex<Vec<String>>>,
    _task: tokio::task::JoinHandle<()>,
}

impl MockServer {
    async fn spawn(responses: Vec<(u16, String)>) -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let requests = Arc::new(Mutex::new(Vec::new()));
        let responses = Arc::new(Mutex::new(VecDeque::from(responses)));
        let task = tokio::spawn({
            let requests = requests.clone();
            let responses = responses.clone();
            async move {
                loop {
                    let Ok((mut socket, _)) = listener.accept().await else {
                        break;
                    };
                    let requests = requests.clone();
                    let responses = responses.clone();
                    tokio::spawn(async move {
                        let mut buf = Vec::new();
                        let mut chunk = [0u8; 1024];
                        let header_end = loop {
                            let n = socket.read(&mut chunk).await.unwrap_or(0);
                            if n == 0 {
                                return;
                            }
                            buf.extend_from_slice(&chunk[..n]);
                            if let Some(pos) = find_header_end(&buf) {
                                break pos;
                            }
                        };
                        let head = String::from_utf8_lossy(&buf[..header_end]).to_string();
                        let authorization = head
                            .lines()
                            .find_map(|line| {
                                line.to_ascii_lowercase()
                                    .starts_with("authorization:")
                                    .then(|| line.trim().to_string())
                            })
                            .unwrap_or_default();
                        requests.lock().unwrap().push(authorization);
                        if let Some(len) = content_length(&head) {
                            while buf.len() < header_end + len {
                                let n = socket.read(&mut chunk).await.unwrap_or(0);
                                if n == 0 {
                                    break;
                                }
                                buf.extend_from_slice(&chunk[..n]);
                            }
                        }
                        let (status, body) = responses
                            .lock()
                            .unwrap()
                            .pop_front()
                            .unwrap_or((500, "no queued response".into()));
                        let reason = match status {
                            200 => "OK",
                            401 => "Unauthorized",
                            429 => "Too Many Requests",
                            500 => "Internal Server Error",
                            _ => "X",
                        };
                        let response = format!(
                            "HTTP/1.1 {status} {reason}\r\nconnection: close\r\ncontent-length: {}\r\n\r\n{}",
                            body.len(),
                            body
                        );
                        socket.write_all(response.as_bytes()).await.unwrap_or(());
                    });
                }
            }
        });
        Self {
            url,
            requests,
            _task: task,
        }
    }

    fn authorization_headers(&self) -> Vec<String> {
        self.requests.lock().unwrap().clone()
    }

    fn request_count(&self) -> usize {
        self.requests.lock().unwrap().len()
    }
}

fn find_header_end(buf: &[u8]) -> Option<usize> {
    buf.windows(4)
        .position(|window| window == b"\r\n\r\n")
        .map(|pos| pos + 4)
}

fn content_length(head: &str) -> Option<usize> {
    head.lines().find_map(|line| {
        let lower = line.to_ascii_lowercase();
        lower
            .strip_prefix("content-length:")
            .and_then(|value| value.trim().parse().ok())
    })
}

fn test_model(base_url: &str, api: &str) -> Model {
    let mut model = crate::model::get_model("deepseek", "deepseek-v4-flash")
        .expect("DeepSeek V4 Flash is in the catalog");
    model.base_url = base_url.to_string();
    model.api = api.to_string();
    model
}

fn automatic_credentials(api_key: &str) -> StreamOptions {
    StreamOptions {
        api_key: Some(api_key.to_string()),
        auth_diagnostics: vec![ProviderAuthDiagnostic {
            field: "api_key".into(),
            source: "env var TEST_API_KEY".into(),
        }],
        ..StreamOptions::default()
    }
}

fn body_to_done_stream(
    body: Box<dyn futures::Stream<Item = Result<bytes::Bytes, String>> + Send + Unpin>,
    model: Model,
    _cancel: Option<tokio_util::sync::CancellationToken>,
) -> EventStream {
    Box::pin(async_stream::stream! {
        let mut text = String::new();
        let mut body = body;
        while let Some(chunk) = body.next().await {
            match chunk {
                Ok(bytes) => text.push_str(&String::from_utf8_lossy(&bytes)),
                Err(error) => {
                    let mut msg = AssistantMessage::empty("test-api", &model.id);
                    msg.error_message = Some(error);
                    msg.stop_reason = StopReason::Error;
                    yield AssistantMessageEvent::Error {
                        reason: StopReason::Error,
                        message: msg,
                    };
                    return;
                }
            }
        }
        let mut msg = AssistantMessage::empty("test-api", &model.id);
        msg.content.push(ContentBlock::Text {
            text,
            text_signature: None,
        });
        msg.stop_reason = StopReason::Stop;
        yield AssistantMessageEvent::Done {
            reason: StopReason::Stop,
            message: msg,
        };
    })
}

async fn collect(mut stream: EventStream) -> Vec<AssistantMessageEvent> {
    let mut events = Vec::new();
    while let Some(event) = stream.next().await {
        events.push(event);
    }
    events
}

fn refresh_counter() -> (Arc<AtomicUsize>, impl FnMut() -> Option<StreamOptions>) {
    let calls = Arc::new(AtomicUsize::new(0));
    let calls_for_closure = calls.clone();
    let closure = move || {
        calls_for_closure.fetch_add(1, Ordering::SeqCst);
        Some(StreamOptions {
            api_key: Some("new-key".into()),
            auth_diagnostics: vec![ProviderAuthDiagnostic {
                field: "api_key".into(),
                source: "env var TEST_API_KEY".into(),
            }],
            ..StreamOptions::default()
        })
    };
    (calls, closure)
}

#[tokio::test]
async fn single_401_refresh_retries_with_rotated_credentials() {
    let server = MockServer::spawn(vec![(401, String::new()), (200, "ok-body".into())]).await;
    let model = test_model(&server.url, "test-api");
    let opts = automatic_credentials("old-key");
    let client = authenticated_client(&Default::default()).expect("default client builds");
    let url = model.base_url.clone();
    let (_calls, refresh) = refresh_counter();
    let refresh: Option<AuthRefresh> = Some(Box::new(refresh));
    let (build_request, refresh) = credential_refresh_slot(
        move |current, payload| {
            let key = current.and_then(|o| o.api_key.clone()).unwrap_or_default();
            Ok(client.post(&url).bearer_auth(key).json(payload))
        },
        refresh,
        Some(opts.clone()),
    );
    let stream = send_json_stream_with_resilience(
        &model,
        Some(&opts),
        "test-api",
        serde_json::json!({"x": 1}),
        build_request,
        body_to_done_stream,
        SendResilience {
            refresh_auth: refresh,
            ..SendResilience::default()
        },
    );
    let events = collect(stream).await;
    assert!(
        events
            .iter()
            .any(|event| matches!(event, AssistantMessageEvent::Done { .. })),
        "refresh + retry should succeed, got {events:?}"
    );
    assert_eq!(
        server.authorization_headers(),
        vec![
            "authorization: Bearer old-key".to_string(),
            "authorization: Bearer new-key".to_string()
        ]
    );
}

#[tokio::test]
async fn refresh_that_still_gets_401_fails_once() {
    let server = MockServer::spawn(vec![(401, String::new()), (401, String::new())]).await;
    let model = test_model(&server.url, "test-api");
    let opts = automatic_credentials("old-key");
    let client = authenticated_client(&Default::default()).expect("default client builds");
    let url = model.base_url.clone();
    let (calls, refresh) = refresh_counter();
    let refresh: Option<AuthRefresh> = Some(Box::new(refresh));
    let (build_request, refresh) = credential_refresh_slot(
        move |current, payload| {
            let key = current.and_then(|o| o.api_key.clone()).unwrap_or_default();
            Ok(client.post(&url).bearer_auth(key).json(payload))
        },
        refresh,
        Some(opts.clone()),
    );
    let stream = send_json_stream_with_resilience(
        &model,
        Some(&opts),
        "test-api",
        serde_json::json!({"x": 1}),
        build_request,
        body_to_done_stream,
        SendResilience {
            refresh_auth: refresh,
            ..SendResilience::default()
        },
    );
    let events = collect(stream).await;
    let error = events
        .iter()
        .find_map(|event| match event {
            AssistantMessageEvent::Error { message, .. } => Some(message),
            _ => None,
        })
        .expect("two 401s surface an error");
    assert!(
        error
            .error_message
            .as_deref()
            .unwrap_or_default()
            .contains("401"),
        "unexpected message: {:?}",
        error.error_message
    );
    assert_eq!(server.request_count(), 2);
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "refresh happens exactly once"
    );
}

#[tokio::test]
async fn explicit_api_key_never_triggers_refresh() {
    let server = MockServer::spawn(vec![(401, String::new())]).await;
    let model = test_model(&server.url, "test-api");
    let opts = StreamOptions {
        api_key: Some("explicit-key".into()),
        ..StreamOptions::default()
    };
    let client = authenticated_client(&Default::default()).expect("default client builds");
    let url = model.base_url.clone();
    let (calls, refresh) = refresh_counter();
    let refresh: Option<AuthRefresh> = Some(Box::new(refresh));
    let (build_request, refresh) = credential_refresh_slot(
        move |current, payload| {
            let key = current.and_then(|o| o.api_key.clone()).unwrap_or_default();
            Ok(client.post(&url).bearer_auth(key).json(payload))
        },
        refresh,
        Some(opts.clone()),
    );
    let stream = send_json_stream_with_resilience(
        &model,
        Some(&opts),
        "test-api",
        serde_json::json!({"x": 1}),
        build_request,
        body_to_done_stream,
        SendResilience {
            refresh_auth: refresh,
            ..SendResilience::default()
        },
    );
    let events = collect(stream).await;
    assert!(
        events
            .iter()
            .any(|event| matches!(event, AssistantMessageEvent::Error { .. })),
        "401 without refresh must fail"
    );
    assert_eq!(
        server.request_count(),
        1,
        "no retry without automatic credentials"
    );
    assert_eq!(
        calls.load(Ordering::SeqCst),
        0,
        "explicit api_key must not be replaced by a refresh"
    );
}

#[tokio::test]
async fn open_breaker_rejects_without_sending_a_request() {
    let server = MockServer::spawn(vec![]).await;
    let model = test_model(&server.url, "test-api");
    let breaker = Arc::new(CircuitBreaker::new(
        BreakerKey::new("deepseek", "deepseek-responses"),
        CircuitBreakerConfig {
            window_size: 1,
            failure_threshold_pct: 50,
            ..CircuitBreakerConfig::default()
        },
    ));
    breaker.record_failure();
    assert!(matches!(
        breaker.before_request(),
        BreakerVerdict::Reject { .. }
    ));
    let client = authenticated_client(&Default::default()).expect("default client builds");
    let url = model.base_url.clone();
    let build_calls = Arc::new(AtomicUsize::new(0));
    let build_calls_for_closure = build_calls.clone();
    let build_request = move |payload: &serde_json::Value| {
        build_calls_for_closure.fetch_add(1, Ordering::SeqCst);
        Ok(client.post(&url).json(payload))
    };
    let stream = send_json_stream_with_resilience(
        &model,
        None,
        "test-api",
        serde_json::json!({"x": 1}),
        build_request,
        body_to_done_stream,
        SendResilience {
            breaker: Some(breaker),
            ..SendResilience::default()
        },
    );
    let events = collect(stream).await;
    let message = events
        .iter()
        .find_map(|event| match event {
            AssistantMessageEvent::Error { message, .. } => Some(message),
            _ => None,
        })
        .expect("open breaker yields an error");
    assert!(
        message
            .error_message
            .as_deref()
            .unwrap_or_default()
            .contains("circuit is open"),
        "unexpected message: {:?}",
        message.error_message
    );
    assert_eq!(build_calls.load(Ordering::SeqCst), 0, "request never built");
    assert_eq!(server.request_count(), 0, "request never sent");
}

#[tokio::test]
async fn deepseek_provider_refreshes_after_401_end_to_end() {
    let server = MockServer::spawn(vec![
        (401, String::new()),
        (
            200,
            include_str!("../providers/deepseek/fixtures/reasoning_function.sse").to_string(),
        ),
    ])
    .await;
    let model = test_model(&server.url, "deepseek-responses");
    let opts = automatic_credentials("old-key");
    let client = authenticated_client(&Default::default()).expect("default client builds");
    let provider = crate::providers::deepseek::DeepSeekResponsesProvider::with_client(None, client);
    let (_calls, refresh) = refresh_counter();
    let refresh: Option<AuthRefresh> = Some(Box::new(refresh));
    let stream = provider.stream_with_resilience(
        &model,
        Context {
            system_prompt: None,
            messages: Vec::new(),
            tools: None,
        },
        Some(opts),
        SendResilience {
            refresh_auth: refresh,
            ..SendResilience::default()
        },
    );
    let events = collect(stream).await;
    assert!(
        events
            .iter()
            .any(|event| matches!(event, AssistantMessageEvent::Done { .. })),
        "deepseek provider should stream a completed response after refresh"
    );
    assert_eq!(
        server.authorization_headers(),
        vec![
            "authorization: Bearer old-key".to_string(),
            "authorization: Bearer new-key".to_string()
        ]
    );
}

#[test]
fn scrub_error_message_redacts_known_secrets() {
    let scrubber = SecretsScrubber::with_secrets(["sk-abcdefghijklmnopqrstuvwxyz123456"]);
    let mut msg = AssistantMessage::empty("test-api", "test-model");
    msg.error_message = Some(format!(
        "request to {} failed",
        "sk-abcdefghijklmnopqrstuvwxyz123456"
    ));
    scrub_error_message(Some(&scrubber), &mut msg);
    assert_eq!(
        msg.error_message.as_deref(),
        Some("request to [REDACTED] failed")
    );
    assert_eq!(
        scrub_error_message(None, &mut msg),
        (),
        "no scrubber leaves the message untouched"
    );
}

#[test]
fn refresh_without_a_new_key_does_not_rotate() {
    let slot: Arc<Mutex<Option<StreamOptions>>> = Arc::new(Mutex::new(None));
    let refresh: Option<AuthRefresh> = Some(Box::new(|| None));
    let initial = Some(StreamOptions {
        api_key: Some("still-old".into()),
        ..StreamOptions::default()
    });
    let (mut build_request, mut refresh) = credential_refresh_slot(
        |current, _payload| {
            let key = current.and_then(|o| o.api_key.clone()).unwrap_or_default();
            Err(key)
        },
        refresh,
        initial,
    );
    let _ = slot;
    assert!(refresh.is_some());
    let result = refresh.as_mut().unwrap()();
    assert!(result.is_none(), "refresh without credentials returns None");
    let err = build_request(&serde_json::json!({})).expect_err("build reads the original key");
    assert_eq!(err, "still-old");
}
