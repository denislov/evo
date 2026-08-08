//! Safe product observability boundary.
//!
//! Domain crates emit structured `tracing` events containing only opaque
//! identities, categorical state, counters, and durations. This crate owns the
//! one outbound boundary: every field is scrubbed and bounded before telemetry
//! or crash-report persistence can observe it.

mod crash;
mod scrub;
mod telemetry;

pub use crash::{CrashReportConfig, CrashReportError};
pub use scrub::{
    OutboundPolicy, REDACTED, SecretStore, SecretsScrubber, scrub_and_bound, scrub_sensitive_text,
    scrub_text,
};
pub use telemetry::{
    CONSENT_SCHEMA_VERSION, NoopTelemetrySink, ObservabilityConfig, ObservabilityError,
    ObservabilityRuntime, TELEMETRY_SCHEMA_VERSION, TelemetryConfig, TelemetryConsent,
    TelemetrySink,
};
