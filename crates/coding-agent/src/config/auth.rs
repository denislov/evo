use crate::config::ConfigDiagnostic;
use crate::config::storage::{atomic_write_private, read_bounded_text};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;

/// Expand `$VAR` / `${VAR}` from the environment, with `$$` → `$` and `$!` → `!`.
/// Returns `None` (plus a diagnostic) if a referenced variable is unset.
pub fn resolve_config_value(raw: &str, diags: &mut Vec<ConfigDiagnostic>) -> Option<String> {
    let mut out = String::new();
    let mut chars = raw.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '$' {
            out.push(c);
            continue;
        }
        match chars.peek().copied() {
            Some('$') => {
                chars.next();
                out.push('$');
            }
            Some('!') => {
                chars.next();
                out.push('!');
            }
            Some('{') => {
                chars.next(); // consume '{'
                let mut var = String::new();
                let mut closed = false;
                for ch in chars.by_ref() {
                    if ch == '}' {
                        closed = true;
                        break;
                    }
                    var.push(ch);
                }
                if !closed {
                    out.push('$');
                    out.push('{');
                    out.push_str(&var);
                    continue;
                }
                match std::env::var(&var) {
                    Ok(value) => out.push_str(&value),
                    Err(_) => {
                        diags.push(ConfigDiagnostic::warn(
                            format!("env var {var} referenced by auth.toml is unset"),
                            None,
                        ));
                        return None;
                    }
                }
            }
            Some(first) if first.is_ascii_alphabetic() || first == '_' => {
                let mut var = String::new();
                while let Some(&next) = chars.peek() {
                    if next.is_ascii_alphanumeric() || next == '_' {
                        var.push(next);
                        chars.next();
                    } else {
                        break;
                    }
                }
                match std::env::var(&var) {
                    Ok(value) => out.push_str(&value),
                    Err(_) => {
                        diags.push(ConfigDiagnostic::warn(
                            format!("env var {var} referenced by auth.toml is unset"),
                            None,
                        ));
                        return None;
                    }
                }
            }
            _ => out.push('$'),
        }
    }
    Some(out)
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AuthEntry {
    ApiKey {
        key: String,
    },
    Oauth {
        #[serde(default)]
        access: Option<String>,
        #[serde(default)]
        access_token: Option<String>,
        #[serde(default)]
        refresh: Option<String>,
        #[serde(default)]
        refresh_token: Option<String>,
        #[serde(default)]
        expires: Option<i64>,
        #[serde(flatten)]
        extra: BTreeMap<String, toml::Value>,
    },
}

#[derive(Debug, Default, Clone, PartialEq)]
pub struct AuthStore {
    entries: BTreeMap<String, AuthEntry>,
}

impl AuthStore {
    pub fn load(path: &Path, diags: &mut Vec<ConfigDiagnostic>) -> AuthStore {
        let text = match read_bounded_text(path) {
            Ok(Some(text)) => text,
            Ok(None) => return AuthStore::default(),
            Err(err) => {
                diags.push(ConfigDiagnostic::warn(
                    format!("failed to read auth: {err}"),
                    Some(path.to_path_buf()),
                ));
                return AuthStore::default();
            }
        };
        #[cfg(unix)]
        check_permissions(path, diags);
        match toml::from_str::<BTreeMap<String, AuthEntry>>(&text) {
            Ok(entries) => AuthStore { entries },
            Err(err) => {
                diags.push(ConfigDiagnostic::warn(
                    format!("failed to parse auth: {err}"),
                    Some(path.to_path_buf()),
                ));
                AuthStore::default()
            }
        }
    }

    /// Raw `api_key` value for a provider (before `$ENV` substitution).
    pub fn api_key_entry(&self, provider: &str) -> Option<&str> {
        match self.entries.get(provider) {
            Some(AuthEntry::ApiKey { key }) => Some(key.as_str()),
            _ => None,
        }
    }

    /// Raw OAuth bearer token value for a provider (before `$ENV` substitution).
    /// Supports both evo's `access` field and OAuth's wire-style `access_token`.
    pub fn oauth_access_entry(&self, provider: &str) -> Option<&str> {
        match self.entries.get(provider) {
            Some(AuthEntry::Oauth {
                access,
                access_token,
                ..
            }) => access.as_deref().or(access_token.as_deref()),
            _ => None,
        }
    }

    pub fn set_api_key(&mut self, provider: impl Into<String>, key: impl Into<String>) {
        self.entries
            .insert(provider.into(), AuthEntry::ApiKey { key: key.into() });
    }

    pub fn remove_entry(&mut self, provider: &str) -> bool {
        self.entries.remove(provider).is_some()
    }

    pub(crate) fn configured_materials(&self) -> impl Iterator<Item = (&str, AuthMaterialKind)> {
        self.entries.iter().map(|(provider, entry)| {
            let material = match entry {
                AuthEntry::ApiKey { .. } => AuthMaterialKind::ApiKey,
                AuthEntry::Oauth { .. } => AuthMaterialKind::OauthAccessToken,
            };
            (provider.as_str(), material)
        })
    }

    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let text = toml::to_string_pretty(&self.entries)
            .map_err(|err| std::io::Error::other(format!("failed to serialize auth: {err}")))?;
        atomic_write_private(path, text.as_bytes())
    }
}

#[cfg(unix)]
fn check_permissions(path: &Path, diags: &mut Vec<ConfigDiagnostic>) {
    use std::os::unix::fs::PermissionsExt;
    if let Ok(meta) = std::fs::metadata(path) {
        let mode = meta.permissions().mode();
        if mode & 0o077 != 0 {
            diags.push(ConfigDiagnostic::warn(
                format!(
                    "auth.toml has loose permissions {:o}; expected 0600",
                    mode & 0o777
                ),
                Some(path.to_path_buf()),
            ));
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeySource {
    Cli,
    AuthFile,
    Env,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthMaterialKind {
    ApiKey,
    OauthAccessToken,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedKey {
    pub value: String,
    pub source: KeySource,
    pub material: AuthMaterialKind,
}

impl ResolvedKey {
    pub fn provider_auth_diagnostic(&self) -> ai::api::auth::ProviderAuthDiagnostic {
        ai::api::auth::ProviderAuthDiagnostic {
            field: "api_key".into(),
            source: match (&self.source, &self.material) {
                (KeySource::Cli, AuthMaterialKind::ApiKey) => "cli:api_key".into(),
                (KeySource::Env, AuthMaterialKind::ApiKey) => "env:api_key".into(),
                (KeySource::AuthFile, AuthMaterialKind::ApiKey) => "auth.toml:api_key".into(),
                (KeySource::AuthFile, AuthMaterialKind::OauthAccessToken) => {
                    "auth.toml:oauth".into()
                }
                (_, AuthMaterialKind::OauthAccessToken) => "oauth".into(),
            },
        }
    }
}

pub fn resolve_api_key(
    provider: &str,
    explicit_key: Option<&str>,
    store: &AuthStore,
    diags: &mut Vec<ConfigDiagnostic>,
) -> Option<ResolvedKey> {
    if let Some(key) = explicit_key
        && !key.is_empty()
    {
        return Some(ResolvedKey {
            value: key.to_string(),
            source: KeySource::Cli,
            material: AuthMaterialKind::ApiKey,
        });
    }
    if let Some(value) = ai::api::auth::env_api_key(provider) {
        return Some(ResolvedKey {
            value,
            source: KeySource::Env,
            material: AuthMaterialKind::ApiKey,
        });
    }
    if let Some(raw) = store.api_key_entry(provider)
        && let Some(value) = resolve_config_value(raw, diags)
        && !value.is_empty()
    {
        return Some(ResolvedKey {
            value,
            source: KeySource::AuthFile,
            material: AuthMaterialKind::ApiKey,
        });
    }
    if let Some(raw) = store.oauth_access_entry(provider)
        && let Some(value) = resolve_config_value(raw, diags)
        && !value.is_empty()
    {
        return Some(ResolvedKey {
            value,
            source: KeySource::AuthFile,
            material: AuthMaterialKind::OauthAccessToken,
        });
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn literal_passthrough() {
        let mut d = Vec::new();
        assert_eq!(
            resolve_config_value("sk-literal", &mut d),
            Some("sk-literal".into())
        );
        assert!(d.is_empty());
    }

    #[test]
    fn dollar_var_and_braced_var() {
        let env = crate::test_support::EnvGuard::new(&["EVO_TEST_KEY"]);
        env.set("EVO_TEST_KEY", "secret");
        let mut d = Vec::new();
        assert_eq!(
            resolve_config_value("$EVO_TEST_KEY", &mut d),
            Some("secret".into())
        );
        assert_eq!(
            resolve_config_value("pre-${EVO_TEST_KEY}-post", &mut d),
            Some("pre-secret-post".into())
        );
    }

    #[test]
    fn escapes() {
        let mut d = Vec::new();
        assert_eq!(
            resolve_config_value("$$literal", &mut d),
            Some("$literal".into())
        );
        assert_eq!(resolve_config_value("a$!b", &mut d), Some("a!b".into()));
    }

    #[test]
    fn unset_var_returns_none_with_diag() {
        let env = crate::test_support::EnvGuard::new(&["EVO_TEST_MISSING"]);
        env.remove("EVO_TEST_MISSING");
        let mut d = Vec::new();
        assert_eq!(resolve_config_value("$EVO_TEST_MISSING", &mut d), None);
        assert_eq!(d.len(), 1);
    }

    #[test]
    fn loads_api_key_entries_and_skips_oauth() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("auth.toml");
        std::fs::write(
            &path,
            "[anthropic]\ntype = \"api_key\"\nkey = \"sk-x\"\n\n[openai]\ntype = \"oauth\"\naccess_token = \"t\"\n",
        )
        .unwrap();
        let mut d = Vec::new();
        let store = AuthStore::load(&path, &mut d);
        assert_eq!(store.api_key_entry("anthropic"), Some("sk-x"));
        assert_eq!(store.api_key_entry("openai"), None); // oauth skipped in M7
        assert_eq!(store.api_key_entry("missing"), None);
    }

    #[test]
    fn missing_auth_file_is_empty_no_diag() {
        let mut d = Vec::new();
        let store = AuthStore::load(std::path::Path::new("/no/such/auth.toml"), &mut d);
        assert_eq!(store.api_key_entry("anthropic"), None);
        assert!(d.is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn loose_permissions_warn() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("auth.toml");
        std::fs::write(&path, "[anthropic]\ntype = \"api_key\"\nkey = \"sk-x\"\n").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        let mut d = Vec::new();
        let _ = AuthStore::load(&path, &mut d);
        assert!(d.iter().any(|x| x.message.contains("permissions")));
    }

    fn store_with(provider: &str, key: &str) -> AuthStore {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("auth.toml");
        std::fs::write(
            &path,
            format!("[{provider}]\ntype = \"api_key\"\nkey = \"{key}\"\n"),
        )
        .unwrap();
        let mut d = Vec::new();
        AuthStore::load(&path, &mut d)
    }

    #[test]
    fn explicit_key_wins() {
        let env = crate::test_support::EnvGuard::new(&["ANTHROPIC_API_KEY"]);
        let store = store_with("anthropic", "from-file");
        env.set("ANTHROPIC_API_KEY", "from-env");
        let mut d = Vec::new();
        let r = resolve_api_key("anthropic", Some("from-cli"), &store, &mut d).unwrap();
        assert_eq!(r.value, "from-cli");
        assert_eq!(r.source, KeySource::Cli);
    }

    #[test]
    fn env_beats_auth_file() {
        let env = crate::test_support::EnvGuard::new(&["ANTHROPIC_API_KEY"]);
        let store = store_with("anthropic", "from-file");
        env.set("ANTHROPIC_API_KEY", "from-env");
        let mut d = Vec::new();
        let r = resolve_api_key("anthropic", None, &store, &mut d).unwrap();
        assert_eq!(r.value, "from-env");
        assert_eq!(r.source, KeySource::Env);
    }

    #[test]
    fn falls_back_to_env_then_none() {
        let env = crate::test_support::EnvGuard::new(&["ANTHROPIC_API_KEY"]);
        let store = AuthStore::default();
        env.set("ANTHROPIC_API_KEY", "from-env");
        let mut d = Vec::new();
        let r = resolve_api_key("anthropic", None, &store, &mut d).unwrap();
        assert_eq!(r.source, KeySource::Env);
        env.remove("ANTHROPIC_API_KEY");
        assert!(resolve_api_key("anthropic", None, &store, &mut d).is_none());
    }

    #[test]
    fn saves_and_loads_api_key_entries_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("auth.toml");
        let mut store = AuthStore::default();
        store.set_api_key("anthropic", "sk-ant");
        store.set_api_key("openai", "$OPENAI_API_KEY");

        store.save(&path).unwrap();

        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("[anthropic]"));
        assert!(text.contains("type = \"api_key\""));

        let mut d = Vec::new();
        let loaded = AuthStore::load(&path, &mut d);
        assert_eq!(loaded.api_key_entry("anthropic"), Some("sk-ant"));
        assert_eq!(loaded.api_key_entry("openai"), Some("$OPENAI_API_KEY"));
        assert!(d.is_empty());
    }

    #[test]
    fn remove_entry_deletes_provider_and_reports_whether_it_existed() {
        let mut store = AuthStore::default();
        store.set_api_key("anthropic", "sk-ant");

        assert!(store.remove_entry("anthropic"));
        assert_eq!(store.api_key_entry("anthropic"), None);
        assert!(!store.remove_entry("anthropic"));
    }

    #[test]
    fn oauth_access_token_is_used_as_auth_file_bearer_token() {
        let env = crate::test_support::EnvGuard::new(&["OPENAI_CODEX_API_KEY"]);
        let text = r#"
[openai-codex]
type = "oauth"
access = "oauth-access"
refresh = "oauth-refresh"
expires = 4102444800000
"#;
        let entries = toml::from_str::<BTreeMap<String, AuthEntry>>(text).unwrap();
        let store = AuthStore { entries };
        env.remove("OPENAI_CODEX_API_KEY");

        let mut d = Vec::new();
        let key = resolve_api_key("openai-codex", None, &store, &mut d).unwrap();

        assert_eq!(key.value, "oauth-access");
        assert_eq!(key.source, KeySource::AuthFile);
        assert_eq!(key.material, AuthMaterialKind::OauthAccessToken);
        assert!(d.is_empty());
    }

    #[test]
    fn oauth_access_token_field_alias_is_supported() {
        let env =
            crate::test_support::EnvGuard::new(&["COPILOT_TEST_TOKEN", "COPILOT_GITHUB_TOKEN"]);
        let text = r#"
[github-copilot]
type = "oauth"
access_token = "$COPILOT_TEST_TOKEN"
"#;
        let entries = toml::from_str::<BTreeMap<String, AuthEntry>>(text).unwrap();
        let store = AuthStore { entries };
        env.set("COPILOT_TEST_TOKEN", "oauth-from-env-ref");
        env.remove("COPILOT_GITHUB_TOKEN");

        let mut d = Vec::new();
        let key = resolve_api_key("github-copilot", None, &store, &mut d).unwrap();

        assert_eq!(key.value, "oauth-from-env-ref");
        assert_eq!(key.source, KeySource::AuthFile);
        assert_eq!(key.material, AuthMaterialKind::OauthAccessToken);
        assert!(d.is_empty());
    }

    #[test]
    fn saves_and_loads_oauth_access_tokens_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("auth.toml");
        let store = AuthStore {
            entries: BTreeMap::from([(
                "openai-codex".into(),
                AuthEntry::Oauth {
                    access: Some("oauth-access".into()),
                    access_token: None,
                    refresh: None,
                    refresh_token: None,
                    expires: None,
                    extra: BTreeMap::new(),
                },
            )]),
        };

        store.save(&path).unwrap();

        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("[openai-codex]"));
        assert!(text.contains("type = \"oauth\""));
        assert!(text.contains("access = \"oauth-access\""));

        let mut d = Vec::new();
        let loaded = AuthStore::load(&path, &mut d);
        assert_eq!(
            loaded.oauth_access_entry("openai-codex"),
            Some("oauth-access")
        );
        assert!(d.is_empty());
    }
}
