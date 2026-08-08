use std::sync::Arc;

use observability::{
    CrashReportConfig, NoopTelemetrySink, ObservabilityConfig, ObservabilityRuntime,
    OutboundPolicy, SecretStore, SecretsScrubber, TelemetryConfig, TelemetryConsent, TelemetrySink,
    scrub_and_bound, scrub_sensitive_text, scrub_text,
};

#[test]
fn public_observability_contract_is_categorized_and_constructible() {
    let mut scrubber = SecretsScrubber::new();
    scrubber.add_secret("registered-secret");
    assert_eq!(scrubber.secrets(), &["registered-secret"]);
    assert!(!scrubber.is_empty());
    assert!(!scrub_text("registered-secret", &["registered-secret"]).contains("registered-secret"));
    assert!(!scrub_sensitive_text("api_key=secret-value").contains("secret-value"));
    assert!(scrub_and_bound(&"中".repeat(20), 9).len() <= 9);

    let store = SecretStore::new();
    store.remember("registered-secret");
    assert_eq!(store.len(), 1);
    assert!(!store.is_empty());
    assert_eq!(store.snapshot().secrets().len(), 1);

    let outbound = OutboundPolicy::new(scrubber, 128, 1024);
    assert_eq!(outbound.max_field_bytes(), 128);
    assert_eq!(outbound.max_event_bytes(), 1024);
    assert!(
        !outbound
            .sanitize_field("/home/user/private")
            .contains("/home/user")
    );

    let consent = TelemetryConsent::new("privacy-v1", 1);
    let telemetry = TelemetryConfig::enabled(consent);
    assert!(telemetry.is_enabled());
    assert!(telemetry.consent().is_some());

    let crash = CrashReportConfig::new(std::env::temp_dir().join("evo-api-contract"))
        .with_policy(outbound)
        .with_max_report_bytes(2048);
    let config = ObservabilityConfig {
        telemetry,
        crash_report: Some(crash),
        ..ObservabilityConfig::default()
    };
    let sink: Arc<dyn TelemetrySink> = Arc::new(NoopTelemetrySink);
    let install: fn(
        ObservabilityConfig,
        Arc<dyn TelemetrySink>,
    ) -> Result<ObservabilityRuntime, observability::ObservabilityError> =
        ObservabilityRuntime::install_global;
    let _ = (config, sink, install);
}
