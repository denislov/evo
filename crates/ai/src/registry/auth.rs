use crate::model::Model;
use crate::protocol::{ProviderAuthDiagnostic, StreamOptions};

#[derive(Clone, Default, PartialEq)]
pub struct ProviderAuth {
    pub api_key: Option<String>,
    pub headers: Option<serde_json::Value>,
    pub azure_api_version: Option<String>,
    pub azure_resource_name: Option<String>,
    pub azure_base_url: Option<String>,
    pub azure_deployment_name: Option<String>,
    pub diagnostics: Vec<ProviderAuthDiagnostic>,
}

impl std::fmt::Debug for ProviderAuth {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ProviderAuth")
            .field("api_key", &self.api_key.as_ref().map(|_| "[REDACTED]"))
            .field("headers", &self.headers.as_ref().map(|_| "[REDACTED]"))
            .field("azure_api_version", &self.azure_api_version)
            .field("azure_resource_name", &self.azure_resource_name)
            .field("azure_base_url", &self.azure_base_url)
            .field("azure_deployment_name", &self.azure_deployment_name)
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
        let mut auth = self.resolve_auth(&model.provider);
        if model.provider == "azure-openai-responses" {
            set_auth_from_env(
                &mut auth.azure_api_version,
                &mut auth.diagnostics,
                "azure_api_version",
                "AZURE_OPENAI_API_VERSION",
            );
            set_auth_from_env(
                &mut auth.azure_base_url,
                &mut auth.diagnostics,
                "azure_base_url",
                "AZURE_OPENAI_BASE_URL",
            );
            set_auth_from_env(
                &mut auth.azure_resource_name,
                &mut auth.diagnostics,
                "azure_resource_name",
                "AZURE_OPENAI_RESOURCE_NAME",
            );
            if let Some(deployment_name) = resolve_azure_deployment_name(&model.id) {
                auth.azure_deployment_name = Some(deployment_name);
                auth.diagnostics.push(auth_diagnostic(
                    "azure_deployment_name",
                    "AZURE_OPENAI_DEPLOYMENT_NAME_MAP",
                ));
            }
        }
        auth
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

fn set_auth_from_env(
    target: &mut Option<String>,
    diagnostics: &mut Vec<ProviderAuthDiagnostic>,
    field: &'static str,
    env_name: &'static str,
) {
    if let Some(value) = non_empty_env(env_name) {
        *target = Some(value);
        diagnostics.push(auth_diagnostic(field, env_name));
    }
}

fn non_empty_env(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .filter(|value| !value.trim().is_empty())
}

fn resolve_azure_deployment_name(model_id: &str) -> Option<String> {
    let map = non_empty_env("AZURE_OPENAI_DEPLOYMENT_NAME_MAP")?;
    for entry in map.split(',') {
        let Some((entry_model_id, deployment)) = entry.trim().split_once('=') else {
            continue;
        };
        if entry_model_id.trim() == model_id {
            let deployment = deployment.trim();
            if !deployment.is_empty() {
                return Some(deployment.to_string());
            }
        }
    }
    None
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
        azure_api_version,
        azure_resource_name,
        azure_base_url,
        azure_deployment_name,
        diagnostics,
    } = auth;

    let options = opts.get_or_insert_with(StreamOptions::default);
    let mut applied_fields = Vec::new();
    if fill_if_none(&mut options.api_key, api_key) {
        applied_fields.push("api_key");
    }
    if fill_if_none(&mut options.azure_api_version, azure_api_version) {
        applied_fields.push("azure_api_version");
    }
    if fill_if_none(&mut options.azure_resource_name, azure_resource_name) {
        applied_fields.push("azure_resource_name");
    }
    if fill_if_none(&mut options.azure_base_url, azure_base_url) {
        applied_fields.push("azure_base_url");
    }
    if fill_if_none(&mut options.azure_deployment_name, azure_deployment_name) {
        applied_fields.push("azure_deployment_name");
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

pub(super) fn validate_automatic_credential_origin(
    model: &Model,
    opts: Option<&StreamOptions>,
) -> Result<(), String> {
    let trusted = crate::model::get_model(&model.provider, &model.id)
        .filter(|trusted| trusted.api == model.api)
        .ok_or_else(credential_origin_error)?;

    if model.api == "azure-openai-responses" {
        let target =
            crate::providers::azure_openai_responses::resolve_target(model, &opts.cloned())
                .map_err(|_| credential_origin_error())?;
        let url = approved_https_url(&target.url)?;
        let host = url.host_str().ok_or_else(credential_origin_error)?;
        let azure_host =
            host.ends_with(".openai.azure.com") || host.ends_with(".cognitiveservices.azure.com");
        if !azure_host || url.port_or_known_default() != Some(443) {
            return Err(credential_origin_error());
        }
        return Ok(());
    }

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
