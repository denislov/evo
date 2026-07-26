/// Retry and end-to-end timeout policy derived from
/// [`crate::protocol::StreamOptions`].
/// Retries never replay a response after provider-neutral output is visible.
pub const MAX_RETRIES: u32 = 8;
pub const MAX_TIMEOUT_MS: u64 = 60 * 60 * 1_000;
pub const MAX_RETRY_DELAY_MS: u64 = 60 * 1_000;
const BASE_RETRY_DELAY_MS: u64 = 250;

#[derive(Debug, Clone, Copy)]
pub struct RetryConfig {
    pub max_retries: u32,
    pub timeout_ms: Option<u64>,
    pub max_retry_delay_ms: u64,
}

impl RetryConfig {
    pub fn from_options(opts: Option<&crate::protocol::StreamOptions>) -> Self {
        let default_max_retries = 0;
        let default_max_retry_delay_ms = 10_000;
        match opts {
            Some(o) => Self {
                max_retries: o.max_retries.unwrap_or(default_max_retries),
                timeout_ms: o.timeout_ms,
                max_retry_delay_ms: o.max_retry_delay_ms.unwrap_or(default_max_retry_delay_ms),
            },
            None => Self {
                max_retries: default_max_retries,
                timeout_ms: None,
                max_retry_delay_ms: default_max_retry_delay_ms,
            },
        }
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.max_retries > MAX_RETRIES {
            return Err(format!(
                "max_retries must be at most {MAX_RETRIES}, got {}",
                self.max_retries
            ));
        }
        if self
            .timeout_ms
            .is_some_and(|timeout_ms| timeout_ms > MAX_TIMEOUT_MS)
        {
            return Err(format!(
                "timeout_ms must be at most {MAX_TIMEOUT_MS}, got {}",
                self.timeout_ms.unwrap_or_default()
            ));
        }
        if self.max_retry_delay_ms > MAX_RETRY_DELAY_MS {
            return Err(format!(
                "max_retry_delay_ms must be at most {MAX_RETRY_DELAY_MS}, got {}",
                self.max_retry_delay_ms
            ));
        }
        Ok(())
    }

    pub const fn backoff_delay_ms(&self, attempt: u32) -> u64 {
        let exponent = if attempt > 16 { 16 } else { attempt };
        let delay = BASE_RETRY_DELAY_MS.saturating_mul(1_u64 << exponent);
        if delay > self.max_retry_delay_ms {
            self.max_retry_delay_ms
        } else {
            delay
        }
    }
}

pub fn is_retryable_status(status: u16) -> bool {
    matches!(status, 408 | 409 | 429 | 500..=599)
}

pub fn parse_retry_after_ms(header: Option<&str>, cfg: &RetryConfig) -> Result<u64, String> {
    let ms = match header {
        Some(header) => {
            let value = header.trim();
            parse_seconds_as_milliseconds(value)?
        }
        None => return Ok(0),
    };
    if ms > cfg.max_retry_delay_ms {
        return Err(format!(
            "Retry-After {}ms exceeds max_retry_delay_ms {}ms",
            ms, cfg.max_retry_delay_ms
        ));
    }
    Ok(ms)
}

fn parse_seconds_as_milliseconds(value: &str) -> Result<u64, String> {
    let (whole, fraction) = match value.split_once('.') {
        Some((whole, fraction))
            if !whole.is_empty()
                && !fraction.is_empty()
                && fraction.len() <= 3
                && !fraction.contains('.') =>
        {
            (whole, Some(fraction))
        }
        Some(_) => return Err("Retry-After header has invalid decimal precision".into()),
        None => (value, None),
    };
    if whole.is_empty()
        || !whole.bytes().all(|byte| byte.is_ascii_digit())
        || fraction.is_some_and(|value| !value.bytes().all(|byte| byte.is_ascii_digit()))
    {
        return Err("Retry-After header is not a valid non-negative number".into());
    }

    let whole_ms = whole
        .parse::<u64>()
        .map_err(|_| "Retry-After header is outside the supported range".to_string())?
        .checked_mul(1_000)
        .ok_or_else(|| "Retry-After header is outside the supported range".to_string())?;
    let fractional_ms = match fraction {
        Some(fraction) => {
            let parsed = fraction
                .parse::<u64>()
                .map_err(|_| "Retry-After header has invalid decimal precision".to_string())?;
            parsed * 10_u64.pow(3 - fraction.len() as u32)
        }
        None => 0,
    };
    whole_ms
        .checked_add(fractional_ms)
        .ok_or_else(|| "Retry-After header is outside the supported range".to_string())
}
