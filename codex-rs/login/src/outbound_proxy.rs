use codex_http_client::HttpClientFactory;
use codex_http_client::NetworkPolicy;

/// Auth-layer adapter around client-owned proxy policy.
///
/// `AuthConfig` carries this value while endpoint resolution and platform details remain in the
/// client layer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthRouteConfig {
    http_client_factory: HttpClientFactory,
    application_network_policy: NetworkPolicy,
}

impl AuthRouteConfig {
    /// Adapts an application-resolved HTTP client factory for auth requests.
    pub fn from_http_client_factory(http_client_factory: HttpClientFactory) -> Self {
        Self {
            application_network_policy: http_client_factory.network_policy().clone(),
            http_client_factory,
        }
    }

    /// Selects the account-owned policy revoked when the authenticated identity changes.
    /// Bootstrap discovery can use separate local rules until this policy is loaded.
    pub fn with_application_network_policy(mut self, policy: NetworkPolicy) -> Self {
        self.application_network_policy = policy;
        self
    }

    /// Returns the HTTP client factory represented by this routing configuration.
    pub fn http_client_factory(&self) -> &HttpClientFactory {
        &self.http_client_factory
    }

    pub(crate) fn application_network_policy(&self) -> &NetworkPolicy {
        &self.application_network_policy
    }
}
