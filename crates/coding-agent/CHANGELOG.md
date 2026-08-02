# Changelog

## Unreleased

- Provider transport now honors `http_proxy` and `websocket_connect_timeout_ms` through a scoped
  `ai::TransportConfig` shared by every built-in provider client.
- Removed the legacy TypeScript-only settings `transport`, `npm_command`, `collapse_changelog`,
  and `warnings.anthropic_extra_usage` from the Rust-native settings schema. Remove these keys from
  existing `settings.toml` files; they no longer produce ignored-setting warnings.
- Removed the permanently unsupported `switch_session` / `switchSession` capability field. Session
  switching remains an adapter workflow that opens a different `CodingAgentSessionOpenTarget`.
- Session summaries now expose an opaque `SessionStorageHandle` instead of a raw session directory.
  Use its explicit event-log and export operations rather than constructing repository paths.
- Removed the `coding-agent/test-support` feature and its public fixture module; they had no CLI or
  Desktop consumers. Coding-agent fixtures remain private to its unit tests.
- The filesystem `read` tool now returns validated JPEG, PNG, GIF, and WebP payloads as base64 image
  content under the existing encoded/decode size limits.
