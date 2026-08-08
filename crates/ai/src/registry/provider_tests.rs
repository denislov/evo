use std::sync::{Arc, Mutex};

use futures::StreamExt;

use super::provider::ApiProvider;
use crate::model::Model;
use crate::protocol::stream::EventStream;
use crate::protocol::{
    AssistantMessage, AssistantMessageEvent, Context, ProviderAuthDiagnostic, StopReason,
    StreamOptions,
};
use crate::registry::{ProviderAuth, ProviderAuthResolver, ProviderRegistry};
use crate::transport::circuit_breaker::{BreakerKey, CircuitBreaker, CircuitBreakerConfig};
use crate::transport::http::SendResilience;
use observability::SecretsScrubber;

type CapturedInvocation = (Option<StreamOptions>, SendResilience);

struct CapturingProvider {
    captured: Arc<Mutex<Option<CapturedInvocation>>>,
}

impl ApiProvider for CapturingProvider {
    fn stream(&self, model: &Model, _ctx: Context, _opts: Option<StreamOptions>) -> EventStream {
        let model_id = model.id.clone();
        Box::pin(async_stream::stream! {
            let mut msg = AssistantMessage::empty("test-capturing", &model_id);
            msg.stop_reason = StopReason::Stop;
            yield AssistantMessageEvent::Done {
                reason: StopReason::Stop,
                message: msg,
            };
        })
    }

    fn stream_with_resilience(
        &self,
        model: &Model,
        ctx: Context,
        opts: Option<StreamOptions>,
        resilience: SendResilience,
    ) -> EventStream {
        *self.captured.lock().unwrap() = Some((opts.clone(), resilience));
        self.stream(model, ctx, opts)
    }
}

#[derive(Clone, Copy)]
struct StaticResolver;

impl ProviderAuthResolver for StaticResolver {
    fn resolve_model_auth(&self, _model: &Model) -> ProviderAuth {
        ProviderAuth {
            api_key: Some("auto-key".into()),
            diagnostics: vec![ProviderAuthDiagnostic {
                field: "api_key".into(),
                source: "env var TEST_API_KEY".into(),
            }],
            ..ProviderAuth::default()
        }
    }
}

fn test_model() -> Model {
    let mut model = crate::model::get_model("deepseek", "deepseek-v4-flash")
        .expect("DeepSeek V4 Flash is in the catalog");
    model.api = "test-capturing".into();
    model
}

fn test_context() -> Context {
    Context {
        system_prompt: None,
        messages: Vec::new(),
        tools: None,
    }
}

async fn capture_once(opts: Option<StreamOptions>) -> (Option<StreamOptions>, SendResilience) {
    let registry = ProviderRegistry::new();
    let captured = Arc::new(Mutex::new(None));
    registry.register(
        "test-capturing",
        Arc::new(CapturingProvider {
            captured: captured.clone(),
        }),
    );
    let model = test_model();
    let stream = registry.stream_model_with_resilience(
        &model,
        test_context(),
        opts,
        Arc::new(StaticResolver),
        SendResilience {
            breaker: Some(Arc::new(CircuitBreaker::new(
                BreakerKey::new("deepseek", "deepseek-responses"),
                CircuitBreakerConfig::default(),
            ))),
            scrubber: Some(Arc::new(SecretsScrubber::new())),
            ..SendResilience::default()
        },
    );
    let mut stream = stream;
    while let Some(_event) = stream.next().await {}
    captured
        .lock()
        .unwrap()
        .take()
        .expect("capturing provider was invoked")
}

#[tokio::test]
async fn automatic_credentials_receive_a_refresh_closure() {
    let (opts, resilience) = capture_once(None).await;
    assert_eq!(
        opts.as_ref().and_then(|o| o.api_key.as_deref()),
        Some("auto-key")
    );
    assert!(resilience.breaker.is_some(), "breaker threads through");
    assert!(resilience.scrubber.is_some(), "scrubber threads through");
    let mut refresh = resilience
        .refresh_auth
        .expect("automatic credentials get a refresh closure");
    let fresh = refresh().expect("resolver yields a refreshed snapshot");
    assert_eq!(fresh.api_key.as_deref(), Some("auto-key"));
}

#[tokio::test]
async fn explicit_api_key_never_gets_a_refresh_closure() {
    let explicit = StreamOptions {
        api_key: Some("explicit-key".into()),
        ..StreamOptions::default()
    };
    let (opts, resilience) = capture_once(Some(explicit)).await;
    assert_eq!(
        opts.as_ref().and_then(|o| o.api_key.as_deref()),
        Some("explicit-key"),
        "resolver must not overwrite an explicit api_key"
    );
    assert!(
        resilience.refresh_auth.is_none(),
        "explicit credentials must not be refreshable"
    );
}
