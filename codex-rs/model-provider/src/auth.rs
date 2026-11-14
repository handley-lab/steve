use std::collections::HashMap;
use std::sync::Arc;

use codex_api::AuthError;
use codex_api::AuthProvider;
use codex_api::SharedAuthProvider;
use codex_features::RuntimeAuthMode;
use codex_http_client::HttpClientFactory;
use codex_http_client::RouteAwareClientPool;
use codex_login::AuthManager;
use codex_login::CodexAuth;
use codex_model_provider_info::ModelProviderInfo;
use codex_protocol::error::CodexErr;
use codex_runtime_agent_instance::RuntimeAuth;
use codex_runtime_agent_instance::RuntimeAuthError;
use codex_runtime_agent_instance::RuntimeAuthManager;
use http::HeaderMap;

use crate::bearer_auth_provider::BearerAuthProvider;

const BEDROCK_API_KEY_UNSUPPORTED_MESSAGE: &str =
    "Bedrock API key auth is only supported by the Amazon Bedrock model provider";

/// Credential ownership and optional OpenAI runtime auth for one harness/thread context.
/// Publish these together so auxiliary clients cannot select another thread's credentials.
#[derive(Clone, Debug)]
pub struct ProviderAuthContext {
    pub credentials: Arc<AuthManager>,
    pub runtime_auth: Option<(Arc<RuntimeAuthManager>, RuntimeAuthMode)>,
}

impl ProviderAuthContext {
    pub fn model_provider(&self, info: ModelProviderInfo) -> crate::SharedModelProvider {
        crate::provider::create_model_provider_with_runtime(
            info,
            Some(Arc::clone(&self.credentials)),
            self.runtime_auth.clone(),
        )
    }
}

/// Provider auth resolved for a request, plus metadata describing the effective auth.
#[derive(Clone)]
pub struct ResolvedProviderAuth {
    /// Login selected with these headers; routing must use the same credential.
    pub credential: Option<CodexAuth>,
    pub auth: SharedAuthProvider,
    pub runtime: Option<RuntimeAuth>,
}

impl ResolvedProviderAuth {
    pub(crate) fn new(auth: SharedAuthProvider) -> Self {
        Self {
            credential: None,
            auth,
            runtime: None,
        }
    }

    pub(crate) fn for_runtime(selected: RuntimeAuth, mode: RuntimeAuthMode) -> Self {
        Self {
            credential: Some(selected.auth().clone()),
            auth: selected.clone().into_auth_provider(mode),
            runtime: Some(selected),
        }
    }

    /// Preserve the caller's transport configuration and bind it to the selected login.
    pub fn bind_http_client_factory(
        &self,
        factory: HttpClientFactory,
    ) -> Result<HttpClientFactory, AuthError> {
        match &self.runtime {
            Some(runtime) => runtime.bind_http_client_factory(factory),
            None => Ok(factory),
        }
    }

    /// Preserve the caller's connection pool and bind it to the selected login.
    pub fn bind_http_client_pool(
        &self,
        pool: &RouteAwareClientPool,
    ) -> Result<RouteAwareClientPool, AuthError> {
        match &self.runtime {
            Some(runtime) => pool
                .with_account_binding(runtime.http_client_factory().network_policy())
                .ok_or_else(|| {
                    AuthError::Build(
                        "runtime credentials and transport must share the application network policy"
                            .to_string(),
                    )
                }),
            None => Ok(pool.clone()),
        }
    }

    /// Reject account selectors that disagree with the selected runtime.
    pub fn validate_headers(&self, headers: &HeaderMap) -> codex_protocol::error::Result<()> {
        if let Some(runtime) = &self.runtime {
            crate::validate_runtime_selectors(headers, runtime.runtime().actor().info())?;
        }
        Ok(())
    }
}

// Some providers are meant to send no auth headers. Examples include local OSS
// providers and custom test providers with `requires_openai_auth = false`.
#[derive(Clone, Debug)]
struct UnauthenticatedAuthProvider;

impl AuthProvider for UnauthenticatedAuthProvider {
    fn add_auth_headers(&self, _headers: &mut HeaderMap) -> Result<(), AuthError> {
        Ok(())
    }
}

pub fn unauthenticated_auth_provider() -> SharedAuthProvider {
    Arc::new(UnauthenticatedAuthProvider)
}

/// Returns the provider-scoped auth manager when this provider uses command-backed auth.
///
/// Providers without custom auth continue using the caller-supplied base manager, when present.
pub(crate) fn auth_manager_for_provider(
    auth_manager: Option<Arc<AuthManager>>,
    provider: &ModelProviderInfo,
) -> Option<Arc<AuthManager>> {
    match provider.auth.clone() {
        Some(config) => {
            if provider.is_gcp() {
                let mut env = HashMap::new();
                if let Some(credential_file) = &provider.credential_file {
                    env.insert(
                        "GOOGLE_APPLICATION_CREDENTIALS".to_string(),
                        credential_file.as_os_str().to_os_string(),
                    );
                }
                Some(AuthManager::external_bearer_only_with_env(config, env))
            } else {
                Some(match auth_manager {
                    Some(manager) => crate::shared_state::process_shared_state()
                        .command_credentials(&config, &manager.runtime_config()),
                    None => AuthManager::external_bearer_only(config),
                })
            }
        }
        None => auth_manager,
    }
}

pub(crate) fn resolve_provider_auth(
    auth: Option<&CodexAuth>,
    provider: &ModelProviderInfo,
) -> codex_protocol::error::Result<SharedAuthProvider> {
    if let Some(auth) = bearer_auth_for_provider(provider)? {
        return Ok(Arc::new(auth));
    }

    if !provider.requires_openai_auth && provider.auth.is_none() {
        return Ok(unauthenticated_auth_provider());
    }

    if matches!(
        auth,
        Some(CodexAuth::BedrockApiKey(_) | CodexAuth::BedrockAccessKeys(_))
    ) {
        return Err(CodexErr::UnsupportedOperation(
            BEDROCK_API_KEY_UNSUPPORTED_MESSAGE.to_string(),
        ));
    }

    Ok(match auth {
        Some(auth) => auth_provider_from_auth(auth),
        None => unauthenticated_auth_provider(),
    })
}

pub(crate) fn resolve_provider_auth_for_scope(
    auth: Option<&CodexAuth>,
    provider: &ModelProviderInfo,
) -> codex_protocol::error::Result<ResolvedProviderAuth> {
    resolve_provider_auth(auth, provider).map(ResolvedProviderAuth::new)
}

pub(crate) fn bearer_auth_for_provider(
    provider: &ModelProviderInfo,
) -> codex_protocol::error::Result<Option<BearerAuthProvider>> {
    if let Some(api_key) = provider.api_key()? {
        return Ok(Some(BearerAuthProvider::new(api_key)));
    }

    if let Some(token) = provider.experimental_bearer_token.clone() {
        return Ok(Some(BearerAuthProvider::new(token.into_inner())));
    }

    Ok(None)
}

/// Select the shared runtime without changing the identity of captured request data.
pub async fn auth_provider_for_request(
    auth: &CodexAuth,
    runtime: Option<&(Arc<RuntimeAuthManager>, RuntimeAuthMode)>,
    headers: &HeaderMap,
) -> Result<ResolvedProviderAuth, RuntimeAuthError> {
    observe_auth_selection(runtime.map(|(_, mode)| *mode), "request", async {
        match runtime {
            Some((manager, mode)) => {
                let manager = Arc::clone(manager);
                let auth = auth.clone();
                let selection = crate::runtime_auth::resource_selection(headers)?;
                let selected = crate::runtime_auth::finish_runtime_selection(async move {
                    manager.auth_for(&auth, &selection).await
                })
                .await?;
                Ok(ResolvedProviderAuth::for_runtime(selected, *mode))
            }
            None => {
                let mut resolved = ResolvedProviderAuth::new(auth_provider_from_auth(auth));
                resolved.credential = Some(auth.clone());
                Ok(resolved)
            }
        }
    })
    .await
}

/// Record bounded auth-mode and outcome labels without changing the selection result.
pub(crate) async fn observe_auth_selection<T, E>(
    mode: Option<RuntimeAuthMode>,
    operation: &'static str,
    selection: impl Future<Output = Result<T, E>>,
) -> Result<T, E> {
    let started = std::time::Instant::now();
    let result = selection.await;
    let mode = match mode {
        None => "ordinary",
        Some(RuntimeAuthMode::DualHeader) => "dual_header",
        Some(RuntimeAuthMode::AssertionOnly) => "assertion_only",
    };
    let _ = codex_otel::record_global_operation(
        "codex.auth.selection",
        "codex.auth.selection.duration",
        started.elapsed(),
        &[
            ("mode", mode),
            ("operation", operation),
            ("outcome", if result.is_ok() { "success" } else { "error" }),
        ],
    );
    result
}

/// Builds request-header auth for a first-party Codex credential.
pub fn auth_provider_from_auth(auth: &CodexAuth) -> SharedAuthProvider {
    match auth {
        CodexAuth::BedrockApiKey(_) | CodexAuth::BedrockAccessKeys(_) => {
            unreachable!("{BEDROCK_API_KEY_UNSUPPORTED_MESSAGE}")
        }
        CodexAuth::ApiKey(_) => Arc::new(BearerAuthProvider {
            token: auth.get_token().ok(),
            account_id: None,
            is_fedramp_account: auth.is_fedramp_account(),
        }),
    }
}

#[cfg(test)]
#[path = "auth_tests.rs"]
mod tests;
