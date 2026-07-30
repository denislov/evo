use std::fmt;
use std::path::{Path, PathBuf};

use crate::app::embedding::{
    CodingAgentModelCatalogEntry, model_catalog_entry, model_from_catalog_entry,
};
use crate::app::operation_factory::CodingAgentOperationFactory;
use crate::app::startup::{
    configured_model_choices, format_application_diagnostics, resolve_provider_api_key,
};
use crate::config::AuthStore;
use crate::config::auth::AuthMaterialKind;
use crate::runtime::facade::{
    CodingAgentErrorCategory, CodingAgentErrorContext, CodingAgentPublicError,
};

const MAX_AUTH_PROVIDERS: usize = 256;
const MAX_PROVIDER_ID_CHARS: usize = 128;
const MAX_API_KEY_BYTES: usize = 16 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CodingAgentProviderAuthKind {
    ApiKey,
    OauthAccessToken,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodingAgentProviderAuthState {
    pub provider: String,
    pub kind: CodingAgentProviderAuthKind,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CodingAgentAuthSnapshot {
    pub providers: Vec<CodingAgentProviderAuthState>,
    pub truncated: bool,
}

impl CodingAgentAuthSnapshot {
    pub fn uses_oauth(&self, provider: &str) -> bool {
        self.providers.iter().any(|state| {
            state.provider == provider
                && state.kind == CodingAgentProviderAuthKind::OauthAccessToken
        })
    }
}

/// Return a safe status projection of user-global provider credentials.
///
/// Credential values and the backing auth store remain private.
pub fn global_auth_snapshot() -> CodingAgentAuthSnapshot {
    let auth = load_global_auth_store();
    CodingAgentAuthController::from_internal(".", None, auth).snapshot()
}

pub(crate) fn load_global_auth_store() -> AuthStore {
    let paths = crate::config::resolve_paths(Path::new("."));
    let mut diagnostics = Vec::new();
    AuthStore::load(&paths.global_auth(), &mut diagnostics)
}

pub enum CodingAgentAuthCommand {
    StoreApiKey { provider: String, api_key: String },
    RemoveProvider { provider: String },
}

impl CodingAgentAuthCommand {
    pub fn store_api_key(provider: impl Into<String>, api_key: impl Into<String>) -> Self {
        Self::StoreApiKey {
            provider: provider.into(),
            api_key: api_key.into(),
        }
    }

    pub fn remove_provider(provider: impl Into<String>) -> Self {
        Self::RemoveProvider {
            provider: provider.into(),
        }
    }
}

impl fmt::Debug for CodingAgentAuthCommand {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::StoreApiKey { provider, .. } => formatter
                .debug_struct("CodingAgentAuthCommand::StoreApiKey")
                .field("provider", provider)
                .field("api_key", &"<redacted>")
                .finish(),
            Self::RemoveProvider { provider } => formatter
                .debug_struct("CodingAgentAuthCommand::RemoveProvider")
                .field("provider", provider)
                .finish(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CodingAgentAuthMutation {
    Stored,
    Removed,
    NotFound,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodingAgentAuthMutationOutcome {
    pub provider: String,
    pub mutation: CodingAgentAuthMutation,
    pub snapshot: CodingAgentAuthSnapshot,
}

#[derive(Clone)]
pub struct CodingAgentAuthController {
    cwd: PathBuf,
    invocation_api_key: Option<String>,
    auth: AuthStore,
}

impl fmt::Debug for CodingAgentAuthController {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CodingAgentAuthController")
            .field(
                "configured_provider_count",
                &self.snapshot().providers.len(),
            )
            .finish_non_exhaustive()
    }
}

impl CodingAgentAuthController {
    pub(crate) fn from_internal(
        cwd: impl Into<PathBuf>,
        invocation_api_key: Option<String>,
        auth: AuthStore,
    ) -> Self {
        Self {
            cwd: cwd.into(),
            invocation_api_key,
            auth,
        }
    }

    pub fn snapshot(&self) -> CodingAgentAuthSnapshot {
        let mut providers = self
            .auth
            .configured_materials()
            .map(|(provider, material)| CodingAgentProviderAuthState {
                provider: bound_provider(provider),
                kind: match material {
                    AuthMaterialKind::ApiKey => CodingAgentProviderAuthKind::ApiKey,
                    AuthMaterialKind::OauthAccessToken => {
                        CodingAgentProviderAuthKind::OauthAccessToken
                    }
                },
            })
            .collect::<Vec<_>>();
        providers.sort_by(|left, right| left.provider.cmp(&right.provider));
        let truncated = providers.len() > MAX_AUTH_PROVIDERS;
        providers.truncate(MAX_AUTH_PROVIDERS);
        CodingAgentAuthSnapshot {
            providers,
            truncated,
        }
    }

    pub fn apply(
        &mut self,
        command: CodingAgentAuthCommand,
        operation_factory: &mut CodingAgentOperationFactory,
    ) -> Result<CodingAgentAuthMutationOutcome, CodingAgentPublicError> {
        let (provider, mutation) = match command {
            CodingAgentAuthCommand::StoreApiKey { provider, api_key } => {
                validate_provider(&provider)?;
                validate_api_key(&api_key)?;
                let mut next = self.auth.clone();
                next.set_api_key(provider.clone(), api_key);
                save_auth(&self.cwd, &next)?;
                self.auth = next;
                (provider, CodingAgentAuthMutation::Stored)
            }
            CodingAgentAuthCommand::RemoveProvider { provider } => {
                validate_provider(&provider)?;
                let mut next = self.auth.clone();
                let removed = next.remove_entry(&provider);
                save_auth(&self.cwd, &next)?;
                self.auth = next;
                (
                    provider,
                    if removed {
                        CodingAgentAuthMutation::Removed
                    } else {
                        CodingAgentAuthMutation::NotFound
                    },
                )
            }
        };

        if operation_factory.selected_provider_id() == provider {
            self.refresh_operation_factory_auth(operation_factory);
        }

        Ok(CodingAgentAuthMutationOutcome {
            provider: bound_provider(&provider),
            mutation,
            snapshot: self.snapshot(),
        })
    }

    pub fn bind_model(
        &self,
        selection: &CodingAgentModelCatalogEntry,
        operation_factory: &mut CodingAgentOperationFactory,
    ) -> Result<String, crate::api::error::CodingAgentPublicError> {
        let model = model_from_catalog_entry(selection)
            .ok_or_else(|| crate::app::error::ApplicationError::UnknownModel(selection.id.clone()))
            .map_err(crate::api::error::CodingAgentPublicError::from)?;
        let (api_key, auth_diagnostics, diagnostics) = resolve_provider_api_key(
            &model.provider,
            self.invocation_api_key.as_deref(),
            &self.auth,
        );
        operation_factory.replace_provider_runtime(model, api_key, auth_diagnostics);
        Ok(format_application_diagnostics(&diagnostics))
    }

    pub fn configured_models(
        &self,
        current: &CodingAgentModelCatalogEntry,
    ) -> Vec<CodingAgentModelCatalogEntry> {
        let Some(model) = model_from_catalog_entry(current) else {
            return vec![current.clone()];
        };
        configured_model_choices(&model, self.invocation_api_key.as_deref(), &self.auth)
            .iter()
            .map(model_catalog_entry)
            .collect()
    }

    fn refresh_operation_factory_auth(&self, operation_factory: &mut CodingAgentOperationFactory) {
        let (api_key, auth_diagnostics, _) = resolve_provider_api_key(
            operation_factory.selected_provider_id(),
            self.invocation_api_key.as_deref(),
            &self.auth,
        );
        operation_factory.replace_auth(api_key, auth_diagnostics);
    }
}

fn save_auth(cwd: &Path, auth: &AuthStore) -> Result<(), CodingAgentPublicError> {
    let path = crate::config::resolve_paths(cwd).global_auth();
    auth.save(&path).map_err(|_| CodingAgentPublicError {
        category: CodingAgentErrorCategory::Persistence,
        code: "auth_persistence".into(),
        retryable: true,
        summary: "failed to update provider authentication".into(),
        context: CodingAgentErrorContext::None,
    })
}

fn validate_provider(provider: &str) -> Result<(), CodingAgentPublicError> {
    if provider.is_empty()
        || provider.chars().count() > MAX_PROVIDER_ID_CHARS
        || provider.chars().any(char::is_whitespace)
    {
        return Err(invalid_auth_command(
            "provider identifier is empty or invalid",
        ));
    }
    Ok(())
}

fn validate_api_key(api_key: &str) -> Result<(), CodingAgentPublicError> {
    if api_key.is_empty() || api_key.len() > MAX_API_KEY_BYTES {
        return Err(invalid_auth_command("API key is empty or too large"));
    }
    Ok(())
}

fn invalid_auth_command(summary: &str) -> CodingAgentPublicError {
    CodingAgentPublicError {
        category: CodingAgentErrorCategory::Input,
        code: "invalid_auth_command".into(),
        retryable: false,
        summary: summary.into(),
        context: CodingAgentErrorContext::None,
    }
}

fn bound_provider(provider: &str) -> String {
    provider.chars().take(MAX_PROVIDER_ID_CHARS).collect()
}
