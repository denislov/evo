/// Secret redaction for text that leaves the `ai` crate: error events,
/// diagnostics, and anything a consumer feeds into logs, telemetry, or crash
/// reports. Secrets shorter than [`MIN_SECRET_LEN`] are ignored to avoid
/// mangling ordinary text.
pub const REDACTED: &str = "[REDACTED]";
const MIN_SECRET_LEN: usize = 8;

/// Known credential-ish key names matched (case-insensitively) inside JSON
/// objects and `key=value` assignments.
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
];

/// Redacts credentials from text: exact secret values (longest first so a
/// longer secret inside a shorter one still fully disappears) plus built-in
/// patterns for `sk-` tokens, `Bearer` headers, and named credential fields.
#[derive(Debug, Clone, Default)]
pub struct SecretsScrubber {
    secrets: Vec<String>,
}

impl SecretsScrubber {
    pub fn new() -> Self {
        Self::default()
    }

    /// Build from an iterator of secret values. Empty and short values are
    /// dropped; duplicates collapse.
    pub fn with_secrets(secrets: impl IntoIterator<Item = impl AsRef<str>>) -> Self {
        let mut scrubber = Self::new();
        for secret in secrets {
            scrubber.add_secret(secret);
        }
        scrubber
    }

    pub fn add_secret(&mut self, secret: impl AsRef<str>) {
        let secret = secret.as_ref();
        if secret.len() >= MIN_SECRET_LEN && !self.secrets.iter().any(|s| s == secret) {
            self.secrets.push(secret.to_string());
        }
        self.secrets.sort_by_key(|s| std::cmp::Reverse(s.len()));
    }

    pub fn secrets(&self) -> &[String] {
        &self.secrets
    }

    pub fn is_empty(&self) -> bool {
        self.secrets.is_empty()
    }

    /// Scrub `input`, returning a copy with every recognized credential
    /// replaced by [`REDACTED`]. Unknown secrets pass through untouched.
    /// Structural patterns (named JSON fields, `key=value` assignments) run
    /// before token patterns so a redacted value is never re-scanned.
    pub fn scrub(&self, input: &str) -> String {
        let mut output = input.to_string();
        for secret in &self.secrets {
            output = output.replace(secret, REDACTED);
        }
        output = redact_json_credentials(&output);
        output = redact_assignment_credentials(&output);
        output = redact_bearer_tokens(&output);
        output = redact_sk_tokens(&output);
        output
    }
}

/// Convenience one-shot redaction without building a scrubber.
pub fn scrub_text(input: &str, secrets: &[impl AsRef<str>]) -> String {
    SecretsScrubber::with_secrets(secrets.iter().map(|s| s.as_ref())).scrub(input)
}

/// Concurrently collected secret values shared across invocations.
/// [`SecretStore::snapshot`] builds a [`SecretsScrubber`] from everything
/// remembered so far; used by `AiClient` to redact automatic credentials.
#[derive(Debug, Default)]
pub struct SecretStore {
    secrets: std::sync::RwLock<Vec<String>>,
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
        let mut secrets = self.secrets.write().unwrap();
        if !secrets.iter().any(|known| known == &secret) {
            secrets.push(secret);
        }
    }

    pub fn snapshot(&self) -> SecretsScrubber {
        SecretsScrubber::with_secrets(self.secrets.read().unwrap().iter())
    }

    pub fn len(&self) -> usize {
        self.secrets.read().unwrap().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

fn redact_sk_tokens(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut output = String::with_capacity(input.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i..].starts_with(b"sk-") {
            let mut j = i + 3;
            while j < bytes.len() && is_token_byte(bytes[j]) {
                j += 1;
            }
            let token_len = j - i - 3;
            if token_len >= 20 {
                output.push_str(REDACTED);
                i = j;
                continue;
            }
        }
        let next = input[i..].chars().next().expect("i is on a char boundary");
        output.push(next);
        i += next.len_utf8();
    }
    output
}

fn redact_bearer_tokens(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut output = String::with_capacity(input.len());
    let mut i = 0;
    while i < bytes.len() {
        let rest = &input[i..];
        let lower_rest = rest.to_lowercase();
        let Some(pos_rel) = lower_rest.find("bearer ") else {
            output.push_str(rest);
            break;
        };
        let pos = i + pos_rel;
        output.push_str(&input[i..pos]);
        let token_start = pos + "bearer ".len();
        let token_end = token_start
            + input[token_start..]
                .find(|c: char| c.is_whitespace() || c == '"' || c == '\'')
                .unwrap_or(input.len() - token_start);
        let token = &input[token_start..token_end];
        if token.len() >= 16 {
            output.push_str("Bearer ");
            output.push_str(REDACTED);
            i = token_end;
        } else {
            output.push_str(&input[pos..token_end]);
            i = token_end;
        }
    }
    output
}

fn redact_json_credentials(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut output = String::with_capacity(input.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'"' {
            let rest = &input[i..];
            let Some(key_end) = rest[1..].find('"') else {
                output.push_str(rest);
                break;
            };
            let key = &rest[1..1 + key_end];
            if is_credential_key(key) {
                let after_key = &rest[1 + key_end + 1..];
                let mut k = 0;
                while after_key.as_bytes().get(k) == Some(&b' ') {
                    k += 1;
                }
                if after_key.as_bytes().get(k) == Some(&b':') {
                    k += 1;
                    while after_key.as_bytes().get(k) == Some(&b' ') {
                        k += 1;
                    }
                    if after_key.as_bytes().get(k) == Some(&b'"') {
                        let content_start = k + 1;
                        if let Some(content_len) = after_key[content_start..].find('"') {
                            let close_quote = content_start + content_len;
                            output.push_str(&rest[..1 + key_end + 1]);
                            output.push_str(&after_key[..k + 1]);
                            output.push_str(REDACTED);
                            output.push('"');
                            i += 1 + key_end + 1 + close_quote + 1;
                            continue;
                        }
                    }
                }
            }
        }
        let next = input[i..].chars().next().expect("i is on a char boundary");
        output.push(next);
        i += next.len_utf8();
    }
    output
}

fn redact_assignment_credentials(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut output = String::with_capacity(input.len());
    let mut i = 0;
    while i < bytes.len() {
        let mut matched = None;
        for key in KEY_NAMES {
            if bytes[i..].starts_with(key.as_bytes()) {
                let after = i + key.len();
                let boundary_ok = i == 0
                    || !(bytes[i - 1].is_ascii_alphanumeric()
                        || bytes[i - 1] == b'_'
                        || bytes[i - 1] == b'-');
                if boundary_ok {
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
                    .find(|c: char| c.is_whitespace() || c == '&' || c == '"' || c == '\'')
                    .unwrap_or(input.len() - value_start);
            let value = &input[value_start..value_end];
            if value_end > value_start && value != REDACTED {
                output.push_str(&input[i..after_key + 1]);
                output.push_str(REDACTED);
                i = value_end;
                continue;
            }
        }
        let next = input[i..].chars().next().expect("i is on a char boundary");
        output.push(next);
        i += next.len_utf8();
    }
    output
}

fn is_token_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_'
}

fn is_credential_key(key: &str) -> bool {
    KEY_NAMES.iter().any(|name| key.eq_ignore_ascii_case(name))
}

#[cfg(test)]
mod tests {
    use super::*;

    const SK_TOKEN: &str = "sk-abcdefghijklmnopqrstuvwxyz123456";
    const BEARER_TOKEN: &str = "eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9";

    #[test]
    fn empty_input_and_empty_secrets() {
        assert_eq!(SecretsScrubber::new().scrub(""), "");
        assert_eq!(
            SecretsScrubber::new().scrub("nothing to see"),
            "nothing to see"
        );
        assert_eq!(scrub_text("hello", &[] as &[String]), "hello");
    }

    #[test]
    fn exact_secret_replaced_everywhere() {
        let scrubber = SecretsScrubber::with_secrets([SK_TOKEN]);
        let out = scrubber.scrub(&format!("key={SK_TOKEN} and again {SK_TOKEN}"));
        assert!(!out.contains(SK_TOKEN));
        assert_eq!(out, format!("key={REDACTED} and again {REDACTED}"));
    }

    #[test]
    fn longest_secret_wins_inside_another() {
        let scrubber = SecretsScrubber::with_secrets(["abcdefghijklmnop", "abcdefghij"]);
        assert_eq!(scrubber.scrub("abcdefghijklmnop"), REDACTED);
    }

    #[test]
    fn chinese_context_is_scrubbed() {
        let scrubber = SecretsScrubber::with_secrets(["sk-中文上下文密钥值很长很长很长"]);
        assert_eq!(
            scrubber.scrub("使用密钥 sk-中文上下文密钥值很长很长很长 调用接口"),
            format!("使用密钥 {REDACTED} 调用接口")
        );
    }

    #[test]
    fn short_secrets_are_ignored() {
        let scrubber = SecretsScrubber::with_secrets(["short", "1234567"]);
        assert_eq!(
            scrubber.scrub("a short word and 1234567"),
            "a short word and 1234567"
        );
        assert!(scrubber.is_empty());
    }

    #[test]
    fn sk_token_pattern_without_secrets() {
        let scrubber = SecretsScrubber::new();
        assert_eq!(
            scrubber.scrub(&format!("key {SK_TOKEN} ok")),
            format!("key {REDACTED} ok")
        );
    }

    #[test]
    fn short_sk_like_text_is_not_redacted() {
        let scrubber = SecretsScrubber::new();
        assert_eq!(
            scrubber.scrub("skill-set and skip and skirt"),
            "skill-set and skip and skirt"
        );
    }

    #[test]
    fn bearer_header_pattern() {
        let scrubber = SecretsScrubber::new();
        assert_eq!(
            scrubber.scrub(&format!("Authorization: Bearer {BEARER_TOKEN}")),
            format!("Authorization: Bearer {REDACTED}")
        );
        assert_eq!(
            scrubber.scrub("authorization: bearer token"),
            "authorization: bearer token"
        );
    }

    #[test]
    fn json_credential_keys() {
        let scrubber = SecretsScrubber::new();
        assert_eq!(
            scrubber.scrub(r#"{"apiKey": "sk-somevalue12345678901234567890", "role": "admin"}"#),
            format!(r#"{{"apiKey": "{REDACTED}", "role": "admin"}}"#)
        );
        assert_eq!(
            scrubber.scrub(r#"{"model": "deepseek", "Authorization": "Bearer xyz"}"#),
            format!(r#"{{"model": "deepseek", "Authorization": "{REDACTED}"}}"#)
        );
    }

    #[test]
    fn assignment_credential_keys() {
        let scrubber = SecretsScrubber::new();
        assert_eq!(
            scrubber.scrub("api_key=sk-somevalue12345678901234567890&model=x"),
            format!("api_key={REDACTED}&model=x")
        );
        assert_eq!(
            scrubber.scrub("token=abcdefghijklmnopqrstuvwxyz now"),
            format!("token={REDACTED} now")
        );
    }

    #[test]
    fn token_value_in_assignment_is_not_scrubbed_twice() {
        let scrubber = SecretsScrubber::new();
        assert_eq!(
            scrubber.scrub("api_key=sk-abcdefghijklmnopqrstuvwxyz123456"),
            format!("api_key={REDACTED}")
        );
    }

    #[test]
    fn repeated_scrubbing_is_idempotent() {
        let scrubber = SecretsScrubber::with_secrets([SK_TOKEN]);
        let once = scrubber.scrub(&format!("x {SK_TOKEN} y"));
        assert_eq!(scrubber.scrub(&once), once);
    }

    #[test]
    fn unscrubbable_plain_text_passes_through() {
        let scrubber = SecretsScrubber::with_secrets(["not-here-anywhere"]);
        assert_eq!(
            scrubber.scrub("just ordinary prose with nothing sensitive"),
            "just ordinary prose with nothing sensitive"
        );
    }
}
