use std::collections::{BTreeMap, VecDeque};
use std::fmt;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use thiserror::Error;
use tracing::field::{Field, Visit};
use tracing::{Event, Subscriber};
use tracing_subscriber::layer::{Context, SubscriberExt};
use tracing_subscriber::{Layer, Registry};

use crate::crash::{self, CrashReportConfig};
use crate::scrub::OutboundPolicy;

pub const TELEMETRY_SCHEMA_VERSION: u16 = 1;
pub const CONSENT_SCHEMA_VERSION: u16 = 1;
const DEFAULT_RECENT_EVENT_CAPACITY: usize = 64;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TelemetryConsent {
    pub schema_version: u16,
    pub consent_version: String,
    pub granted_at_unix_ms: u64,
}

impl TelemetryConsent {
    pub fn new(consent_version: impl Into<String>, granted_at_unix_ms: u64) -> Self {
        Self {
            schema_version: CONSENT_SCHEMA_VERSION,
            consent_version: consent_version.into(),
            granted_at_unix_ms,
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct TelemetryConfig {
    enabled: bool,
    consent: Option<TelemetryConsent>,
}

impl TelemetryConfig {
    pub const fn disabled() -> Self {
        Self {
            enabled: false,
            consent: None,
        }
    }

    pub fn enabled(consent: TelemetryConsent) -> Self {
        Self {
            enabled: true,
            consent: Some(consent),
        }
    }

    pub const fn is_enabled(&self) -> bool {
        self.enabled
    }

    pub fn consent(&self) -> Option<&TelemetryConsent> {
        self.consent.as_ref()
    }
}

pub trait TelemetrySink: fmt::Debug + Send + Sync + 'static {
    /// Receives an already-scrubbed, bounded JSON envelope. Implementations
    /// must not be given raw tracing fields.
    fn emit(&self, payload: &[u8]);
}

#[derive(Debug, Clone, Copy, Default)]
pub struct NoopTelemetrySink;

impl TelemetrySink for NoopTelemetrySink {
    fn emit(&self, _payload: &[u8]) {}
}

#[derive(Debug, Clone)]
pub struct ObservabilityConfig {
    pub telemetry: TelemetryConfig,
    pub outbound: OutboundPolicy,
    pub recent_event_capacity: usize,
    pub crash_report: Option<CrashReportConfig>,
}

impl Default for ObservabilityConfig {
    fn default() -> Self {
        Self {
            telemetry: TelemetryConfig::disabled(),
            outbound: OutboundPolicy::default(),
            recent_event_capacity: DEFAULT_RECENT_EVENT_CAPACITY,
            crash_report: None,
        }
    }
}

impl ObservabilityConfig {
    pub fn with_crash_report(mut self, crash_report: CrashReportConfig) -> Self {
        self.crash_report = Some(crash_report);
        self
    }
}

#[derive(Debug, Error)]
pub enum ObservabilityError {
    #[error("telemetry cannot be enabled without an explicit consent record")]
    MissingConsent,
    #[error("telemetry consent version must not be empty")]
    EmptyConsentVersion,
    #[error("the global tracing subscriber is already installed")]
    SubscriberAlreadyInstalled,
    #[error(transparent)]
    CrashReport(#[from] crate::crash::CrashReportError),
}

/// Process-lifetime observability installation. Dropping the value does not
/// uninstall the global subscriber or panic hook.
#[derive(Debug)]
pub struct ObservabilityRuntime {
    _state: Arc<ObservabilityState>,
}

impl ObservabilityRuntime {
    pub fn install_global(
        config: ObservabilityConfig,
        sink: Arc<dyn TelemetrySink>,
    ) -> Result<Self, ObservabilityError> {
        validate_telemetry(&config.telemetry)?;
        if let Some(crash_report) = config.crash_report.as_ref() {
            crash::prepare_panic_hook(crash_report)?;
        }
        let state = Arc::new(ObservabilityState::new(&config));
        let subscriber = Registry::default().with(SafeTelemetryLayer {
            config: config.telemetry,
            outbound: config.outbound,
            sink,
            state: state.clone(),
        });
        tracing::subscriber::set_global_default(subscriber)
            .map_err(|_| ObservabilityError::SubscriberAlreadyInstalled)?;
        if let Some(crash_report) = config.crash_report {
            crash::install_panic_hook(crash_report, state.clone())?;
        }
        Ok(Self { _state: state })
    }
}

fn validate_telemetry(config: &TelemetryConfig) -> Result<(), ObservabilityError> {
    if !config.enabled {
        return Ok(());
    }
    let consent = config
        .consent
        .as_ref()
        .ok_or(ObservabilityError::MissingConsent)?;
    if consent.consent_version.trim().is_empty() {
        return Err(ObservabilityError::EmptyConsentVersion);
    }
    Ok(())
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SafeEvent {
    pub(crate) timestamp_unix_ms: u64,
    pub(crate) level: String,
    pub(crate) target: String,
    pub(crate) fields: BTreeMap<String, String>,
}

#[derive(Debug)]
pub(crate) struct ObservabilityState {
    recent: Mutex<VecDeque<SafeEvent>>,
    capacity: usize,
}

impl ObservabilityState {
    fn new(config: &ObservabilityConfig) -> Self {
        let capacity = config.recent_event_capacity.max(1);
        Self {
            recent: Mutex::new(VecDeque::with_capacity(capacity)),
            capacity,
        }
    }

    fn record(&self, event: SafeEvent) {
        let mut recent = self
            .recent
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        recent.push_back(event);
        while recent.len() > self.capacity {
            recent.pop_front();
        }
    }

    pub(crate) fn snapshot(&self) -> Vec<SafeEvent> {
        self.recent
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .iter()
            .cloned()
            .collect()
    }
}

#[derive(Debug)]
struct SafeTelemetryLayer {
    config: TelemetryConfig,
    outbound: OutboundPolicy,
    sink: Arc<dyn TelemetrySink>,
    state: Arc<ObservabilityState>,
}

impl<S> Layer<S> for SafeTelemetryLayer
where
    S: Subscriber,
{
    fn on_event(&self, event: &Event<'_>, _context: Context<'_, S>) {
        let metadata = event.metadata();
        let mut visitor = FieldVisitor::default();
        event.record(&mut visitor);
        let safe = bound_event(
            SafeEvent {
                timestamp_unix_ms: unix_millis(),
                level: metadata.level().as_str().to_ascii_lowercase(),
                target: self.outbound.sanitize_field(metadata.target()),
                fields: visitor
                    .fields
                    .into_iter()
                    .filter(|(key, _)| is_safe_field(key))
                    .map(|(key, value)| {
                        (
                            self.outbound.sanitize_field(&key),
                            self.outbound.sanitize_field(&value),
                        )
                    })
                    .collect(),
            },
            &self.outbound,
        );
        self.state.record(safe.clone());
        if let Some(consent) = self.config.consent.as_ref().filter(|_| self.config.enabled)
            && let Some(payload) = telemetry_payload(&safe, consent, &self.outbound)
        {
            self.sink.emit(&payload);
        }
    }
}

pub(crate) fn is_safe_field(name: &str) -> bool {
    matches!(
        name,
        "domain"
            | "phase"
            | "operation_id"
            | "session_id"
            | "mode"
            | "kind"
            | "dispatch"
            | "duration_ms"
            | "tool_call_id"
            | "tool_name"
            | "task_id"
            | "owner_kind"
            | "owner_id"
            | "state"
            | "has_timeout"
            | "worktree_id"
            | "owner_operation"
            | "creation_mode"
            | "event_kind"
            | "outcome"
            | "hook_count"
            | "cache_status"
            | "query_kind"
            | "shutdown_reason"
            | "handled_queries"
            | "panicked"
            | "cursor"
            | "wait_micros"
            | "update_kind"
            | "batch_size"
            | "state_key"
            | "bytes"
            | "parse_to_layout_us"
            | "input_bytes"
            | "latency_micros"
            | "layout_width"
            | "visible_rows"
            | "event"
            | "block_id"
    )
}

#[derive(Default)]
struct FieldVisitor {
    fields: BTreeMap<String, String>,
}

impl Visit for FieldVisitor {
    fn record_debug(&mut self, field: &Field, value: &dyn fmt::Debug) {
        self.fields
            .insert(field.name().to_owned(), format!("{value:?}"));
    }

    fn record_str(&mut self, field: &Field, value: &str) {
        self.fields
            .insert(field.name().to_owned(), value.to_owned());
    }

    fn record_bool(&mut self, field: &Field, value: bool) {
        self.fields
            .insert(field.name().to_owned(), value.to_string());
    }

    fn record_i64(&mut self, field: &Field, value: i64) {
        self.fields
            .insert(field.name().to_owned(), value.to_string());
    }

    fn record_u64(&mut self, field: &Field, value: u64) {
        self.fields
            .insert(field.name().to_owned(), value.to_string());
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct TelemetryEnvelope<'a> {
    schema_version: u16,
    consent: SafeConsent,
    event: &'a SafeEvent,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SafeConsent {
    schema_version: u16,
    consent_version: String,
    granted_at_unix_ms: u64,
}

fn telemetry_payload(
    event: &SafeEvent,
    consent: &TelemetryConsent,
    outbound: &OutboundPolicy,
) -> Option<Vec<u8>> {
    let consent = SafeConsent {
        schema_version: consent.schema_version,
        consent_version: outbound.sanitize_with_budget(&consent.consent_version, 128),
        granted_at_unix_ms: consent.granted_at_unix_ms,
    };
    let envelope = TelemetryEnvelope {
        schema_version: TELEMETRY_SCHEMA_VERSION,
        consent,
        event,
    };
    let payload = serde_json::to_vec(&envelope).ok()?;
    (payload.len() <= outbound.max_event_bytes()).then_some(payload)
}

fn bound_event(mut event: SafeEvent, outbound: &OutboundPolicy) -> SafeEvent {
    while serde_json::to_vec(&event).is_ok_and(|payload| payload.len() > outbound.max_event_bytes())
    {
        if event.fields.pop_last().is_none() {
            break;
        }
    }
    event
}

fn unix_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::SecretsScrubber;

    #[derive(Debug, Default)]
    struct CollectingSink(Mutex<Vec<Vec<u8>>>);

    impl TelemetrySink for CollectingSink {
        fn emit(&self, payload: &[u8]) {
            self.0
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .push(payload.to_vec());
        }
    }

    fn layer(
        telemetry: TelemetryConfig,
        sink: Arc<dyn TelemetrySink>,
    ) -> (SafeTelemetryLayer, Arc<ObservabilityState>) {
        let config = ObservabilityConfig {
            telemetry: telemetry.clone(),
            outbound: OutboundPolicy::new(
                SecretsScrubber::with_secrets(["registered-secret"]),
                256,
                2048,
            ),
            recent_event_capacity: 4,
            crash_report: None,
        };
        let state = Arc::new(ObservabilityState::new(&config));
        (
            SafeTelemetryLayer {
                config: telemetry,
                outbound: config.outbound,
                sink,
                state: state.clone(),
            },
            state,
        )
    }

    #[test]
    fn telemetry_is_disabled_by_default_but_recent_safe_events_remain() {
        let sink = Arc::new(CollectingSink::default());
        let (layer, state) = layer(TelemetryConfig::default(), sink.clone());
        let subscriber = Registry::default().with(layer);
        tracing::subscriber::with_default(subscriber, || {
            tracing::info!(target: "evo::lifecycle", domain = "operation", phase = "started");
        });
        assert!(sink.0.lock().unwrap().is_empty());
        assert_eq!(state.snapshot().len(), 1);
    }

    #[test]
    fn enabled_payload_has_schema_consent_scrubbing_and_budget() {
        let sink = Arc::new(CollectingSink::default());
        let consent = TelemetryConsent::new("privacy-v1", 42);
        let (layer, _) = layer(TelemetryConfig::enabled(consent), sink.clone());
        let subscriber = Registry::default().with(layer);
        tracing::subscriber::with_default(subscriber, || {
            tracing::info!(
                target: "evo::lifecycle",
                domain = "tool",
                phase = "failed",
                owner_id = "registered-secret Bearer eyJhbGciOiJub25lIn0 /home/user/secret.rs",
                prompt = "must never leave the process",
            );
        });
        let payloads = sink.0.lock().unwrap();
        assert_eq!(payloads.len(), 1);
        assert!(payloads[0].len() <= 2048);
        let text = String::from_utf8(payloads[0].clone()).unwrap();
        assert!(text.contains("\"schemaVersion\":1"));
        assert!(text.contains("privacy-v1"));
        assert!(!text.contains("registered-secret"));
        assert!(!text.contains("eyJhbGci"));
        assert!(!text.contains("/home/user"));
        assert!(!text.contains("must never leave"));
    }

    #[test]
    fn enabled_config_requires_nonempty_consent() {
        let missing = TelemetryConfig {
            enabled: true,
            consent: None,
        };
        assert!(matches!(
            validate_telemetry(&missing),
            Err(ObservabilityError::MissingConsent)
        ));
        assert!(matches!(
            validate_telemetry(&TelemetryConfig::enabled(TelemetryConsent::new("", 0))),
            Err(ObservabilityError::EmptyConsentVersion)
        ));
    }
}
