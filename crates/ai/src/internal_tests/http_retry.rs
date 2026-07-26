use ai::transport::retry::{RetryConfig, is_retryable_status, parse_retry_after_ms};

fn default_cfg() -> RetryConfig {
    RetryConfig {
        max_retries: 2,
        timeout_ms: None,
        max_retry_delay_ms: 10_000,
    }
}

#[test]
fn retryable_408() {
    assert!(is_retryable_status(408));
}

#[test]
fn retryable_409() {
    assert!(is_retryable_status(409));
}

#[test]
fn retryable_429() {
    assert!(is_retryable_status(429));
}

#[test]
fn retryable_500() {
    assert!(is_retryable_status(500));
}

#[test]
fn retryable_503() {
    assert!(is_retryable_status(503));
}

#[test]
fn non_retryable_200() {
    assert!(!is_retryable_status(200));
}

#[test]
fn non_retryable_400() {
    assert!(!is_retryable_status(400));
}

#[test]
fn non_retryable_404() {
    assert!(!is_retryable_status(404));
}

#[test]
fn parse_retry_after_seconds() {
    let ms = parse_retry_after_ms(Some("5"), &default_cfg()).unwrap();
    assert_eq!(ms, 5000);
}

#[test]
fn parse_retry_after_none_returns_zero() {
    let ms = parse_retry_after_ms(None, &default_cfg()).unwrap();
    assert_eq!(ms, 0);
}

#[test]
fn parse_retry_after_exceeds_max_delay() {
    let cfg = RetryConfig {
        max_retry_delay_ms: 1000,
        ..default_cfg()
    };
    let result = parse_retry_after_ms(Some("5"), &cfg);
    assert!(result.is_err());
}

#[test]
fn parse_retry_after_within_max_delay() {
    let cfg = RetryConfig {
        max_retry_delay_ms: 10000,
        ..default_cfg()
    };
    let result = parse_retry_after_ms(Some("5"), &cfg);
    assert_eq!(result.unwrap(), 5000);
}

#[test]
fn parse_retry_after_invalid_header() {
    let result = parse_retry_after_ms(Some("not-a-number"), &default_cfg());
    assert!(result.is_err());
}

#[test]
fn parse_retry_after_rejects_negative_fractional_and_overflow_values() {
    for value in ["-1", "0.0001", "NaN", "18446744073709551615"] {
        assert!(parse_retry_after_ms(Some(value), &default_cfg()).is_err());
    }
    assert_eq!(
        parse_retry_after_ms(Some("0.5"), &default_cfg()).unwrap(),
        500
    );
}

#[test]
fn retry_config_rejects_unbounded_values() {
    let mut cfg = default_cfg();
    cfg.max_retries = 9;
    assert!(cfg.validate().unwrap_err().contains("max_retries"));

    let mut cfg = default_cfg();
    cfg.timeout_ms = Some(3_600_001);
    assert!(cfg.validate().unwrap_err().contains("timeout_ms"));

    let mut cfg = default_cfg();
    cfg.max_retry_delay_ms = 60_001;
    assert!(cfg.validate().unwrap_err().contains("max_retry_delay_ms"));
}

#[test]
fn exponential_backoff_is_capped_by_policy() {
    let cfg = RetryConfig {
        max_retry_delay_ms: 600,
        ..default_cfg()
    };
    assert_eq!(cfg.backoff_delay_ms(0), 250);
    assert_eq!(cfg.backoff_delay_ms(1), 500);
    assert_eq!(cfg.backoff_delay_ms(2), 600);
    assert_eq!(cfg.backoff_delay_ms(u32::MAX), 600);
}
// Internal HTTP retry tests.
