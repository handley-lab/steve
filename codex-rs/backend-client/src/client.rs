use anyhow::Result;
use codex_api::SharedAuthProvider;
use codex_http_client::ClientRouteClass;
use codex_http_client::HttpClientFactory;
use codex_http_client::RequestBuilder;
use codex_http_client::RouteAwareClientPool;
use codex_login::CodexAuth;
use codex_login::default_client::get_codex_user_agent;
use http::Method;
use http::StatusCode;
use http::header::CONTENT_TYPE;
use http::header::HeaderMap;
use http::header::HeaderName;
use http::header::HeaderValue;
use http::header::USER_AGENT;
use serde::de::DeserializeOwned;
use std::fmt;

pub(crate) mod analytics;
pub(crate) mod plan_history;
pub(crate) mod profile;
pub(crate) mod task_usage;
mod thread_usage;
pub(crate) mod turn_usage;

pub use thread_usage::ThreadUsage;
pub use thread_usage::ThreadUsageBreakdownGroup;

#[derive(Debug)]
pub enum RequestError {
    Policy(codex_http_client::NetworkPolicyDenied),
    UnexpectedStatus {
        method: String,
        url: String,
        status: StatusCode,
        content_type: String,
        body: String,
    },
    Other(anyhow::Error),
}

impl RequestError {
    pub fn status(&self) -> Option<StatusCode> {
        match self {
            Self::UnexpectedStatus { status, .. } => Some(*status),
            Self::Policy(_) | Self::Other(_) => None,
        }
    }

    pub fn is_unauthorized(&self) -> bool {
        self.status() == Some(StatusCode::UNAUTHORIZED)
    }
}

impl fmt::Display for RequestError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Policy(denied) => denied.fmt(f),
            Self::UnexpectedStatus {
                method,
                url,
                status,
                content_type,
                body,
            } => write!(
                f,
                "{method} {url} failed: {status}; content-type={content_type}; body={body}"
            ),
            Self::Other(err) => write!(f, "{err}"),
        }
    }
}

impl std::error::Error for RequestError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Policy(denied) => Some(denied),
            Self::UnexpectedStatus { .. } => None,
            Self::Other(err) => Some(err.as_ref()),
        }
    }
}

impl From<anyhow::Error> for RequestError {
    fn from(err: anyhow::Error) -> Self {
        Self::Other(err)
    }
}

impl From<codex_http_client::HttpError> for RequestError {
    fn from(error: codex_http_client::HttpError) -> Self {
        match error {
            codex_http_client::HttpError::Policy(denied) => Self::Policy(denied),
            error @ (codex_http_client::HttpError::Request(_)
            | codex_http_client::HttpError::Route(_)
            | codex_http_client::HttpError::Build(_)
            | codex_http_client::HttpError::UnsupportedRedirectScheme(_)
            | codex_http_client::HttpError::TooManyRedirects
            | codex_http_client::HttpError::Timeout) => Self::Other(error.into()),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PathStyle {
    /// /api/codex/…
    CodexApi,
    /// /wham/…
    ChatGptApi,
}

impl PathStyle {
    pub fn from_base_url(base_url: &str) -> Self {
        if base_url.contains("/backend-api") {
            PathStyle::ChatGptApi
        } else {
            PathStyle::CodexApi
        }
    }
}

#[derive(Clone)]
pub struct Client {
    base_url: String,
    http: RouteAwareClientPool,
    auth_provider: SharedAuthProvider,
    user_agent: Option<HeaderValue>,
    chatgpt_account_id: Option<String>,
    chatgpt_account_is_fedramp: bool,
    path_style: PathStyle,
}

impl fmt::Debug for Client {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Client")
            .field("base_url", &self.base_url)
            .field("auth_provider", &"<provider>")
            .field("user_agent", &self.user_agent)
            .field("chatgpt_account_id", &self.chatgpt_account_id)
            .field(
                "chatgpt_account_is_fedramp",
                &self.chatgpt_account_is_fedramp,
            )
            .field("path_style", &self.path_style)
            .finish_non_exhaustive()
    }
}

impl Client {
    pub fn new(base_url: impl Into<String>, http_client_factory: HttpClientFactory) -> Self {
        let http = RouteAwareClientPool::with_chatgpt_cloudflare_cookies_without_request_logging(
            http_client_factory,
            ClientRouteClass::Api,
        );
        Self::with_http(base_url.into(), http)
    }

    /// Creates a client that never forwards its credentials to a redirect destination.
    pub fn new_without_redirects(
        base_url: impl Into<String>,
        http_client_factory: HttpClientFactory,
    ) -> Self {
        let http =
            RouteAwareClientPool::with_chatgpt_cloudflare_cookies_without_redirects_or_request_logging(
                http_client_factory,
                ClientRouteClass::Api,
            );
        Self::with_http(base_url.into(), http)
    }

    fn with_http(mut base_url: String, http: RouteAwareClientPool) -> Self {
        // Normalize common ChatGPT hostnames to include /backend-api so we hit the WHAM paths.
        // Also trim trailing slashes for consistent URL building.
        while base_url.ends_with('/') {
            base_url.pop();
        }
        if (base_url.starts_with("https://chatgpt.com")
            || base_url.starts_with("https://chat.openai.com"))
            && !base_url.contains("/backend-api")
        {
            base_url = format!("{base_url}/backend-api");
        }
        let path_style = PathStyle::from_base_url(&base_url);
        Self {
            base_url,
            http,
            auth_provider: codex_model_provider::unauthenticated_auth_provider(),
            user_agent: None,
            chatgpt_account_id: None,
            chatgpt_account_is_fedramp: false,
            path_style,
        }
    }

    pub fn from_auth(
        base_url: impl Into<String>,
        auth: &CodexAuth,
        http_client_factory: HttpClientFactory,
    ) -> Self {
        Self::new(base_url, http_client_factory)
            .with_user_agent(get_codex_user_agent())
            .with_auth_provider(codex_model_provider::auth_provider_from_auth(auth))
    }

    pub fn with_auth_provider(mut self, auth: SharedAuthProvider) -> Self {
        self.auth_provider = auth;
        self
    }

    pub fn with_user_agent(mut self, ua: impl Into<String>) -> Self {
        if let Ok(hv) = HeaderValue::from_str(&ua.into()) {
            self.user_agent = Some(hv);
        }
        self
    }

    pub fn with_chatgpt_account_id(mut self, account_id: impl Into<String>) -> Self {
        self.chatgpt_account_id = Some(account_id.into());
        self
    }

    pub fn with_fedramp_routing_header(mut self) -> Self {
        self.chatgpt_account_is_fedramp = true;
        self
    }

    pub fn with_path_style(mut self, style: PathStyle) -> Self {
        self.path_style = style;
        self
    }

    fn headers(&self) -> HeaderMap {
        let mut h = HeaderMap::new();
        if let Some(ua) = &self.user_agent {
            h.insert(USER_AGENT, ua.clone());
        } else {
            h.insert(USER_AGENT, HeaderValue::from_static("codex-cli"));
        }
        self.auth_provider.add_auth_headers(&mut h);
        if let Some(acc) = &self.chatgpt_account_id
            && let Ok(name) = HeaderName::from_bytes(b"ChatGPT-Account-Id")
            && let Ok(hv) = HeaderValue::from_str(acc)
        {
            h.insert(name, hv);
        }
        if self.chatgpt_account_is_fedramp
            && let Ok(name) = HeaderName::from_bytes(b"X-OpenAI-Fedramp")
        {
            h.insert(name, HeaderValue::from_static("true"));
        }
        h
    }

    fn request(&self, method: Method, url: &str) -> RequestBuilder {
        self.http.request(method, url)
    }

    async fn exec_request_detailed(
        &self,
        req: RequestBuilder,
        method: &str,
        url: &str,
    ) -> std::result::Result<(String, String), RequestError> {
        let res = req.send().await?;
        let status = res.status();
        let content_type = res
            .headers()
            .get(CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();
        let body = res.text().await?;
        if !status.is_success() {
            return Err(RequestError::UnexpectedStatus {
                method: method.to_string(),
                url: url.to_string(),
                status,
                content_type,
                body,
            });
        }
        Ok((body, content_type))
    }

    fn decode_json<T: DeserializeOwned>(&self, url: &str, ct: &str, body: &str) -> Result<T> {
        match serde_json::from_str::<T>(body) {
            Ok(v) => Ok(v),
            Err(e) => {
                anyhow::bail!("Decode error for {url}: {e}; content-type={ct}; body={body}");
            }
        }
    }



}

#[cfg(test)]
#[path = "client_request_tests.rs"]
mod request_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn authenticated_user_settings_client_uses_active_workspace_headers() {
        let auth = CodexAuth::from_external_chatgpt_tokens(
            "e30.e30.c2ln",
            "workspace-123",
            Some("enterprise"),
        )
        .unwrap();
        let client = Client::from_auth(
            "https://chatgpt.com/backend-api",
            &auth,
            HttpClientFactory::new(codex_http_client::OutboundProxyPolicy::ReqwestDefault),
        );
        let headers = client.headers();

        assert_eq!(
            [
                headers
                    .get("authorization")
                    .and_then(|value| value.to_str().ok()),
                headers
                    .get("chatgpt-account-id")
                    .and_then(|value| value.to_str().ok()),
            ],
            [Some("Bearer e30.e30.c2ln"), Some("workspace-123")]
        );
    }
}
