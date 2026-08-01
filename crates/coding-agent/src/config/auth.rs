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

#[derive(Clone)]
pub(crate) struct AuthStoreProviderAuthResolver {
    current_provider: String,
    current_auth: ai::api::auth::ProviderAuth,
    store: AuthStore,
}

impl AuthStoreProviderAuthResolver {
    pub(crate) fn new(
        current_provider: impl Into<String>,
        api_key: Option<String>,
        diagnostics: Vec<ai::api::auth::ProviderAuthDiagnostic>,
        store: AuthStore,
    ) -> Self {
        Self {
            current_provider: current_provider.into(),
            current_auth: ai::api::auth::ProviderAuth {
                api_key,
                diagnostics,
                ..Default::default()
            },
            store,
        }
    }

    fn stored_auth(&self, provider: &str) -> ai::api::auth::ProviderAuth {
        let mut diagnostics = Vec::new();
        let resolved = resolve_api_key(provider, None, &self.store, &mut diagnostics);
        ai::api::auth::ProviderAuth {
            api_key: resolved.as_ref().map(|resolved| resolved.value.clone()),
            diagnostics: resolved
                .as_ref()
                .map(ResolvedKey::provider_auth_diagnostic)
                .into_iter()
                .collect(),
            ..Default::default()
        }
    }
}

impl ai::api::auth::ProviderAuthResolver for AuthStoreProviderAuthResolver {
    fn resolve_model_auth(&self, model: &ai::api::model::Model) -> ai::api::auth::ProviderAuth {
        // Preserve environment-resolved auth material, then replace only the
        // provider credential with the product-resolved material. The current
        // provider may carry an invocation-scoped key; every other provider is
        // resolved separately from the global auth store and its supported
        // environment variables.
        let mut auth = ai::api::auth::EnvProviderAuthResolver.resolve_model_auth(model);
        let resolved = if model.provider == self.current_provider {
            self.current_auth.clone()
        } else {
            self.stored_auth(&model.provider)
        };
        if resolved.api_key.is_some() {
            auth.api_key = resolved.api_key;
            auth.diagnostics
                .retain(|diagnostic| diagnostic.field != "api_key");
            auth.diagnostics.extend(resolved.diagnostics);
        }
        auth
    }

    fn requires_approved_https_origin(&self) -> bool {
        true
    }
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
