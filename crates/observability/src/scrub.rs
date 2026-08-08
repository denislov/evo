use std::sync::{OnceLock, RwLock};

use regex::Regex;

pub const REDACTED: &str = "[REDACTED]";
const REDACTED_PATH: &str = "[REDACTED_PATH]";
const MIN_SECRET_LEN: usize = 8;

const KEY_NAMES: &[&str] = &[
    "api_key",
    "api-key",
    "apikey",
    "secret",
    "token",
    "refresh_token",
    "refresh-token",
    "access_token",
    "access-token",
    "authorization",
    "auth",
    "password",
    "passwd",
];

/// Exact-secret and structural credential scrubber shared by every outbound
/// observability path.
#[derive(Debug, Clone, Default)]
pub struct SecretsScrubber {
    secrets: Vec<String>,
}

impl SecretsScrubber {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_secrets(secrets: impl IntoIterator<Item = impl AsRef<str>>) -> Self {
        let mut scrubber = Self::new();
        for secret in secrets {
            scrubber.add_secret(secret);
        }
        scrubber
    }

    pub fn add_secret(&mut self, secret: impl AsRef<str>) {
        let secret = secret.as_ref();
        if secret.len() >= MIN_SECRET_LEN && !self.secrets.iter().any(|known| known == secret) {
            self.secrets.push(secret.to_owned());
            self.secrets
                .sort_by_key(|known| std::cmp::Reverse(known.len()));
        }
    }

    pub fn secrets(&self) -> &[String] {
        &self.secrets
    }

    pub fn is_empty(&self) -> bool {
        self.secrets.is_empty()
    }

    pub fn scrub(&self, input: &str) -> String {
        let mut output = input.to_owned();
        for secret in &self.secrets {
            output = output.replace(secret, REDACTED);
        }
        output = redact_json_credentials(&output);
        output = redact_assignment_credentials(&output);
        output = redact_bearer_tokens(&output);
        redact_sk_tokens(&output)
    }
}

pub fn scrub_text(input: &str, secrets: &[impl AsRef<str>]) -> String {
    SecretsScrubber::with_secrets(secrets.iter().map(AsRef::as_ref)).scrub(input)
}

pub fn scrub_sensitive_text(input: &str) -> String {
    OutboundPolicy::default().sanitize_with_budget(input, usize::MAX)
}

pub fn scrub_and_bound(input: &str, max_bytes: usize) -> String {
    OutboundPolicy::default().sanitize_with_budget(input, max_bytes)
}

/// Concurrent secret registry used by credential-owning components.
#[derive(Debug, Default)]
pub struct SecretStore {
    secrets: RwLock<Vec<String>>,
}

impl SecretStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn remember(&self, secret: impl Into<String>) {
        let secret = secret.into();
        if secret.len() < MIN_SECRET_LEN {
            return;
        }
        let mut secrets = self
            .secrets
            .write()
            .unwrap_or_else(|error| error.into_inner());
        if !secrets.iter().any(|known| known == &secret) {
            secrets.push(secret);
        }
    }

    pub fn snapshot(&self) -> SecretsScrubber {
        let secrets = self
            .secrets
            .read()
            .unwrap_or_else(|error| error.into_inner());
        SecretsScrubber::with_secrets(secrets.iter())
    }

    pub fn len(&self) -> usize {
        self.secrets
            .read()
            .unwrap_or_else(|error| error.into_inner())
            .len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// Outbound privacy and byte-budget policy. Scrubbing always precedes
/// truncation, and truncation always stops at a UTF-8 boundary.
#[derive(Debug, Clone)]
pub struct OutboundPolicy {
    scrubber: SecretsScrubber,
    max_field_bytes: usize,
    max_event_bytes: usize,
}

impl Default for OutboundPolicy {
    fn default() -> Self {
        Self {
            scrubber: SecretsScrubber::new(),
            max_field_bytes: 512,
            max_event_bytes: 8 * 1024,
        }
    }
}

impl OutboundPolicy {
    pub fn new(scrubber: SecretsScrubber, max_field_bytes: usize, max_event_bytes: usize) -> Self {
        Self {
            scrubber,
            max_field_bytes: max_field_bytes.max(32),
            max_event_bytes: max_event_bytes.max(512),
        }
    }

    pub fn scrubber(&self) -> &SecretsScrubber {
        &self.scrubber
    }

    pub const fn max_field_bytes(&self) -> usize {
        self.max_field_bytes
    }

    pub const fn max_event_bytes(&self) -> usize {
        self.max_event_bytes
    }

    pub fn sanitize_field(&self, input: &str) -> String {
        self.sanitize_with_budget(input, self.max_field_bytes)
    }

    pub fn sanitize_with_budget(&self, input: &str, max_bytes: usize) -> String {
        let scrubbed = redact_absolute_paths(&self.scrubber.scrub(input));
        truncate_utf8(&scrubbed, max_bytes)
    }
}

fn redact_absolute_paths(input: &str) -> String {
    static ABSOLUTE_PATH: OnceLock<Regex> = OnceLock::new();
    let regex = ABSOLUTE_PATH.get_or_init(|| {
        Regex::new(
            r#"(?:\b[A-Za-z]:\\[^\s\"'`;|]+|(?:^|[\s(])(?:/|~/)[^\s\"'`;|]+|(?:^|[\s(])(?:\.{0,2}/)?(?:[A-Za-z0-9_.-]+/)+[A-Za-z0-9_.-]+)"#,
        )
        .expect("path redaction regex is valid")
    });
    regex
        .replace_all(input, |captures: &regex::Captures<'_>| {
            let matched = captures.get(0).expect("whole match exists").as_str();
            if matched.starts_with(char::is_whitespace) || matched.starts_with('(') {
                format!("{}{REDACTED_PATH}", &matched[..1])
            } else {
                REDACTED_PATH.to_owned()
            }
        })
        .into_owned()
}

fn truncate_utf8(input: &str, max_bytes: usize) -> String {
    if input.len() <= max_bytes {
        return input.to_owned();
    }
    let mut end = max_bytes;
    while !input.is_char_boundary(end) {
        end -= 1;
    }
    input[..end].to_owned()
}

fn redact_sk_tokens(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut output = String::with_capacity(input.len());
    let mut cursor = 0;
    while cursor < bytes.len() {
        if bytes[cursor..].starts_with(b"sk-") {
            let mut end = cursor + 3;
            while end < bytes.len() && is_token_byte(bytes[end]) {
                end += 1;
            }
            if end - cursor - 3 >= 20 {
                output.push_str(REDACTED);
                cursor = end;
                continue;
            }
        }
        let next = input[cursor..]
            .chars()
            .next()
            .expect("cursor is on a char boundary");
        output.push(next);
        cursor += next.len_utf8();
    }
    output
}

fn redact_bearer_tokens(input: &str) -> String {
    static BEARER: OnceLock<Regex> = OnceLock::new();
    BEARER
        .get_or_init(|| {
            Regex::new(r#"(?i)\bbearer\s+[^\s;&|\"']+"#).expect("bearer regex is valid")
        })
        .replace_all(input, format!("Bearer {REDACTED}"))
        .into_owned()
}

fn redact_json_credentials(input: &str) -> String {
    static JSON_FIELD: OnceLock<Regex> = OnceLock::new();
    JSON_FIELD
        .get_or_init(|| {
            Regex::new(
                r#"(?i)([\"']?(?:[a-z0-9]+_)*(?:api[_-]?key|access[_-]?key|refresh[_-]?token|auth(?:orization)?|token|password|passwd|secret)[\"']?\s*:\s*)[\"'][^\"']*[\"']"#,
            )
            .expect("JSON credential regex is valid")
        })
        .replace_all(input, format!("$1\"{REDACTED}\""))
        .into_owned()
}

fn redact_assignment_credentials(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut output = String::with_capacity(input.len());
    let mut cursor = 0;
    while cursor < bytes.len() {
        let mut matched = None;
        for key in KEY_NAMES {
            if bytes[cursor..].starts_with(key.as_bytes()) {
                let after = cursor + key.len();
                let boundary = cursor == 0
                    || !(bytes[cursor - 1].is_ascii_alphanumeric() || bytes[cursor - 1] == b'-');
                if boundary {
                    matched = Some(after);
                    break;
                }
            }
        }
        if let Some(after_key) = matched
            && bytes.get(after_key) == Some(&b'=')
        {
            let value_start = after_key + 1;
            let value_end = value_start
                + input[value_start..]
                    .find(|character: char| {
                        character.is_whitespace()
                            || matches!(character, '&' | ';' | '|' | '"' | '\'')
                    })
                    .unwrap_or(input.len() - value_start);
            if value_end > value_start && &input[value_start..value_end] != REDACTED {
                output.push_str(&input[cursor..=after_key]);
                output.push_str(REDACTED);
                cursor = value_end;
                continue;
            }
        }
        let next = input[cursor..]
            .chars()
            .next()
            .expect("cursor is on a char boundary");
        output.push(next);
        cursor += next.len_utf8();
    }
    output
}

fn is_token_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scrubs_exact_structural_and_path_values_before_bounding() {
        let secret = "custom-secret-value";
        let policy = OutboundPolicy::new(SecretsScrubber::with_secrets([secret]), 256, 1024);
        let output = policy.sanitize_field(&format!(
            "{secret} api_key=abc Bearer eyJhbGciOiJub25lIn0 /home/user/project/src/lib.rs"
        ));
        assert!(!output.contains(secret));
        assert!(!output.contains("abc"));
        assert!(!output.contains("eyJhbGci"));
        assert!(!output.contains("/home/user"));
        assert!(output.contains(REDACTED));
        assert!(output.contains(REDACTED_PATH));
    }

    #[test]
    fn field_budget_is_utf8_safe() {
        let policy = OutboundPolicy::new(SecretsScrubber::new(), 33, 1024);
        let output = policy.sanitize_field(&"中文".repeat(30));
        assert!(output.len() <= 33);
        assert!(output.is_char_boundary(output.len()));
    }

    #[test]
    fn secret_store_deduplicates_and_ignores_short_values() {
        let store = SecretStore::new();
        store.remember("short");
        store.remember("long-secret");
        store.remember("long-secret");
        assert_eq!(store.len(), 1);
        assert_eq!(store.snapshot().scrub("long-secret"), REDACTED);
    }

    #[test]
    fn structural_scrubbing_preserves_valid_json() {
        let input = r#"{"prompt":"api_key=secret-value","auth":{"token":"token-value"}}"#;
        let output = SecretsScrubber::new().scrub(input);
        let parsed: serde_json::Value = serde_json::from_str(&output).unwrap();
        assert!(!output.contains("secret-value"));
        assert!(!output.contains("token-value"));
        assert_eq!(parsed["auth"]["token"], REDACTED);
    }
}
