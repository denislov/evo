use crate::model::Model;
use crate::protocol::{ProviderAuthDiagnostic, StreamOptions};

#[derive(Clone, Default, PartialEq)]
pub struct ProviderAuth {
    pub api_key: Option<String>,
    pub headers: Option<serde_json::Value>,
    pub diagnostics: Vec<ProviderAuthDiagnostic>,
}

impl std::fmt::Debug for ProviderAuth {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ProviderAuth")
            .field("api_key", &self.api_key.as_ref().map(|_| "[REDACTED]"))
            .field("headers", &self.headers.as_ref().map(|_| "[REDACTED]"))
            .field("diagnostics", &self.diagnostics)
            .finish()
    }
}

pub trait ProviderAuthResolver: Send + Sync {
    fn resolve_api_key(&self, _provider: &str) -> Option<String> {
        None
    }

    fn resolve_auth(&self, provider: &str) -> ProviderAuth {
        ProviderAuth {
            api_key: self.resolve_api_key(provider),
            ..ProviderAuth::default()
        }
    }

    fn resolve_model_auth(&self, model: &Model) -> ProviderAuth {
        self.resolve_auth(&model.provider)
    }

    /// Whether credentials produced by this resolver must stay on the
    /// generated catalog's approved HTTPS origin.
    fn requires_approved_https_origin(&self) -> bool {
        false
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct EnvProviderAuthResolver;

impl ProviderAuthResolver for EnvProviderAuthResolver {
    fn resolve_api_key(&self, provider: &str) -> Option<String> {
        crate::registry::env::env_api_key(provider)
    }

    fn resolve_auth(&self, provider: &str) -> ProviderAuth {
        match crate::registry::env::env_api_key_with_source(provider) {
            Some((api_key, source)) => ProviderAuth {
                api_key: Some(api_key),
                diagnostics: vec![auth_diagnostic("api_key", source)],
                ..ProviderAuth::default()
            },
            None => ProviderAuth::default(),
        }
    }

    fn resolve_model_auth(&self, model: &Model) -> ProviderAuth {
        self.resolve_auth(&model.provider)
    }

    fn requires_approved_https_origin(&self) -> bool {
        true
    }
}

fn auth_diagnostic(field: impl Into<String>, source: impl Into<String>) -> ProviderAuthDiagnostic {
    ProviderAuthDiagnostic {
        field: field.into(),
        source: source.into(),
    }
}

pub(super) fn apply_auth_material(
    mut opts: Option<StreamOptions>,
    auth: ProviderAuth,
) -> Option<StreamOptions> {
    if auth == ProviderAuth::default() {
        return opts;
    }

    let ProviderAuth {
        api_key,
        headers,
        diagnostics,
    } = auth;

    let options = opts.get_or_insert_with(StreamOptions::default);
    let mut applied_fields = Vec::new();
    if fill_if_none(&mut options.api_key, api_key) {
        applied_fields.push("api_key");
    }
    options.headers = merge_auth_headers(headers, options.headers.take());
    append_applied_auth_diagnostics(&mut options.auth_diagnostics, diagnostics, &applied_fields);
    opts
}

pub(super) fn resolver_auth_will_apply(opts: Option<&StreamOptions>, auth: &ProviderAuth) -> bool {
    let explicit_api_key = opts.and_then(|options| options.api_key.as_ref()).is_some();
    (!explicit_api_key && auth.api_key.is_some()) || auth.headers.is_some()
}

pub(super) fn options_contain_automatic_credentials(opts: Option<&StreamOptions>) -> bool {
    opts.is_some_and(|options| {
        options.auth_diagnostics.iter().any(|diagnostic| {
            diagnostic.field == "api_key" && !diagnostic.source.starts_with("cli:")
        })
    })
}

pub(super) fn validate_automatic_credential_origin(model: &Model) -> Result<(), String> {
    let trusted = crate::model::get_model(&model.provider, &model.id)
        .filter(|trusted| trusted.api == model.api)
        .ok_or_else(credential_origin_error)?;

    let requested = approved_https_url(&model.base_url)?;
    let approved = approved_https_url(&trusted.base_url)?;
    if requested.host_str() != approved.host_str()
        || requested.port_or_known_default() != approved.port_or_known_default()
    {
        return Err(credential_origin_error());
    }
    Ok(())
}

fn approved_https_url(value: &str) -> Result<reqwest::Url, String> {
    let url = reqwest::Url::parse(value).map_err(|_| credential_origin_error())?;
    if url.scheme() != "https"
        || !url.username().is_empty()
        || url.password().is_some()
        || url.host_str().is_none()
    {
        return Err(credential_origin_error());
    }
    Ok(url)
}

fn credential_origin_error() -> String {
    "automatically resolved credentials require an approved HTTPS provider origin".into()
}

fn fill_if_none(target: &mut Option<String>, value: Option<String>) -> bool {
    if target.is_none() && value.is_some() {
        *target = value;
        true
    } else {
        false
    }
}

fn append_applied_auth_diagnostics(
    target: &mut Vec<ProviderAuthDiagnostic>,
    diagnostics: Vec<ProviderAuthDiagnostic>,
    applied_fields: &[&str],
) {
    target.extend(
        diagnostics
            .into_iter()
            .filter(|diagnostic| applied_fields.contains(&diagnostic.field.as_str())),
    );
}

fn merge_auth_headers(
    auth_headers: Option<serde_json::Value>,
    option_headers: Option<serde_json::Value>,
) -> Option<serde_json::Value> {
    match (auth_headers, option_headers) {
        (None, explicit) => explicit,
        (auth, None) => auth,
        (Some(serde_json::Value::Object(mut auth)), Some(serde_json::Value::Object(explicit))) => {
            for (key, value) in explicit {
                auth.insert(key, value);
            }
            Some(serde_json::Value::Object(auth))
        }
        (_, explicit @ Some(_)) => explicit,
    }
}
