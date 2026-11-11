//! Resolves credentials for a gateway, using its configured exporter or OAuth session.

use std::sync::Arc;

use crate::ProviderUnauthorizedRecovery;
use crate::gateway_export::GatewayExport;
use codex_login::AuthManager;
use codex_login::GatewayAuthError;
use codex_login::OAuthSessionManager;
use codex_model_provider_info::GatewayOAuthConfig;
use codex_model_provider_info::ModelProviderInfo;
use codex_protocol::error::CodexErr;
use codex_protocol::error::CodexErrorDetails;
use codex_protocol::error::Result;

/// Resolves gateway request credentials and exposes the optional interactive OAuth session.
#[derive(Debug)]
pub struct GatewayAuthManager {
    source: GatewayCredentialSource,
    allow_interactive_login: bool,
}

#[derive(Debug)]
enum GatewayCredentialSource {
    Export(Arc<GatewayExport>),
    OAuth(Arc<OAuthSessionManager>),
}

impl GatewayAuthManager {
    #[cfg(test)]
    pub(crate) fn from_oauth_session(session: Arc<OAuthSessionManager>) -> Self {
        Self {
            source: GatewayCredentialSource::OAuth(session),
            allow_interactive_login: true,
        }
    }

    pub(crate) fn new(
        provider: &ModelProviderInfo,
        config: &GatewayOAuthConfig,
        owner: &Arc<AuthManager>,
    ) -> std::result::Result<Self, String> {
        let state = crate::shared_state::process_shared_state();
        let source = match config {
            GatewayOAuthConfig::Export {
                credential_export, ..
            } => GatewayCredentialSource::Export(state.gateway_export(
                provider,
                credential_export,
                owner,
            )),
            GatewayOAuthConfig::OAuth(settings) => GatewayCredentialSource::OAuth(
                state
                    .gateway_oauth_session(settings, &owner.runtime_config())
                    .map_err(|_| "failed to create provider OAuth HTTP client".to_string())?,
            ),
        };
        Ok(Self {
            source,
            allow_interactive_login: true,
        })
    }

    pub(crate) async fn prepare_retry(&self) -> ProviderUnauthorizedRecovery {
        match &self.source {
            GatewayCredentialSource::Export(export) => {
                export.invalidate().await;
                ProviderUnauthorizedRecovery::Recovered
            }
            GatewayCredentialSource::OAuth(_) => ProviderUnauthorizedRecovery::NotConfigured,
        }
    }

    /// The session used for explicit sign-in and OAuth status, when configured.
    pub fn oauth_session(&self) -> Option<Arc<OAuthSessionManager>> {
        match &self.source {
            GatewayCredentialSource::OAuth(session) => Some(Arc::clone(session)),
            GatewayCredentialSource::Export(_) => None,
        }
    }

    /// Resolve existing credentials without starting an interactive authorization flow.
    pub fn without_interactive_login(self: &Arc<Self>) -> Arc<Self> {
        let GatewayCredentialSource::OAuth(session) = &self.source else {
            return Arc::clone(self);
        };
        // Keep the canonical session owner alive for the shared-state registry.
        Arc::new(Self {
            source: GatewayCredentialSource::OAuth(Arc::clone(session)),
            allow_interactive_login: false,
        })
    }

    pub(crate) fn cached_access_token(&self) -> Option<Arc<str>> {
        match &self.source {
            GatewayCredentialSource::Export(export) => export.cached_token(),
            GatewayCredentialSource::OAuth(session) => session.cached_access_token().map(Arc::from),
        }
    }

    pub async fn resolve_access_token(&self) -> Result<Arc<str>> {
        match &self.source {
            GatewayCredentialSource::Export(export) => export.token().await,
            GatewayCredentialSource::OAuth(session) => {
                let session = if self.allow_interactive_login {
                    session.as_ref().clone()
                } else {
                    session.as_ref().clone().without_interactive_login()
                };
                session
                    .resolve_access_token()
                    .await
                    .map(Arc::from)
                    .map_err(gateway_auth_error)
            }
        }
    }
}

fn gateway_auth_error(error: std::io::Error) -> CodexErr {
    if let Some(login_error) = error
        .get_ref()
        .and_then(|source| source.downcast_ref::<GatewayAuthError>())
    {
        return match login_error {
            GatewayAuthError::LoginRequired => {
                CodexErrorDetails::GatewayLoginRequired(login_error.to_string()).into()
            }
            GatewayAuthError::LoginInProgress => CodexErr::InvalidRequest(login_error.to_string()),
        };
    }
    // Issuer errors may echo arbitrary credentials from configured URLs. Keep the
    // diagnostic safe and bounded, preserving retries for transient failures.
    CodexErr::Io(std::io::Error::new(
        error.kind(),
        "Gateway OAuth authentication failed; check the gateway configuration and credential store.",
    ))
}
