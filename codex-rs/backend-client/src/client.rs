use crate::types::CodeTaskDetailsResponse;
use crate::types::ConfigBundleResponse;
use crate::types::PaginatedListTaskListItem;
use crate::types::TurnAttemptsSiblingTurnsResponse;
use anyhow::Result;
use codex_api::AuthError;
use codex_api::SharedAuthProvider;
use codex_features::RuntimeAuthMode;
use codex_http_client::CHATGPT_ACCOUNT_ID_HEADER;
use codex_http_client::ClientRouteClass;
use codex_http_client::HttpClientFactory;
use codex_http_client::OutboundProxyPolicy;
use codex_http_client::RequestBuilder;
use codex_http_client::RouteAwareClientPool;
use codex_http_client::RouteAwareRequestError;
use codex_login::CodexAuth;
use codex_login::default_client::get_codex_user_agent;
use codex_model_provider::ClientAuth;
use codex_runtime_agent_instance::RuntimeAuthManager;
use http::Method;
use http::StatusCode;
use http::header::CONTENT_TYPE;
use http::header::HeaderMap;
use http::header::HeaderName;
use http::header::HeaderValue;
use http::header::USER_AGENT;
use serde::de::DeserializeOwned;
use std::fmt;
use std::sync::Arc;
use std::time::Duration;

pub(crate) mod turn_usage;

#[derive(Debug)]
pub enum RequestError {
    Policy(codex_http_client::NetworkPolicyDenied),
    UnexpectedStatus {
        method: String,
        url: String,
        status: StatusCode,
        content_type: String,
        body: String,
        identity_error_code: Option<String>,
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
        matches!(self, Self::UnexpectedStatus { status, body, identity_error_code, .. }
        if *status == StatusCode::UNAUTHORIZED
            && !codex_protocol::error::is_runtime_auth_denial(
            *status,
            body,
            identity_error_code.as_deref(),
        ))
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
                ..
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

impl From<AuthError> for RequestError {
    fn from(error: AuthError) -> Self {
        match error {
            AuthError::Policy(denied) => Self::Policy(denied),
            other => Self::Other(other.into()),
        }
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
    pub fn from_account_style(style: codex_config::AccountPathStyle) -> Self {
        match style {
            codex_config::AccountPathStyle::CodexApi => PathStyle::CodexApi,
            codex_config::AccountPathStyle::ChatgptApi => PathStyle::ChatGptApi,
        }
    }
}

#[derive(Clone)]
pub struct Client {
    base_url: String,
    http: RouteAwareClientPool,
    auth: ClientAuth,
    user_agent: Option<HeaderValue>,
    chatgpt_account_id: Option<String>,
    chatgpt_account_is_fedramp: bool,
    path_style: PathStyle,
    config_server_base_url: Option<String>,
}

impl fmt::Debug for Client {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Client")
            .field("base_url", &self.base_url)
            .field("auth", &"<provider>")
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
    /// Uses the configured backend without following redirects.
    pub fn new(base_url: impl Into<String>, http_client_factory: HttpClientFactory) -> Self {
        Self::new_without_redirects(base_url, http_client_factory)
    }

    /// Creates a client that never forwards its credentials to a redirect destination.
    pub fn new_without_redirects(
        base_url: impl Into<String>,
        http_client_factory: HttpClientFactory,
    ) -> Self {
        let http = RouteAwareClientPool::new_without_redirects_or_request_logging(
            http_client_factory,
            ClientRouteClass::Api,
        );
        Self::with_http(base_url.into(), http)
    }

    pub fn for_config_endpoint(
        base_url: impl Into<String>,
        http_client_factory: HttpClientFactory,
    ) -> Self {
        let base_url = base_url.into();
        let mut client = Self::with_http(
            base_url.clone(),
            RouteAwareClientPool::new_without_redirects_or_request_logging(
                http_client_factory,
                ClientRouteClass::Api,
            ),
        );
        client.config_server_base_url = Some(base_url);
        client
    }

    fn with_http(mut base_url: String, http: RouteAwareClientPool) -> Self {
        while base_url.ends_with('/') {
            base_url.pop();
        }
        let path_style = PathStyle::CodexApi;
        Self {
            base_url,
            http,
            auth: ClientAuth::new(codex_model_provider::unauthenticated_auth_provider()),
            user_agent: None,
            chatgpt_account_id: None,
            chatgpt_account_is_fedramp: false,
            path_style,
            config_server_base_url: None,
        }
    }

    pub fn from_auth(
        base_url: impl Into<String>,
        auth: &CodexAuth,
        http_client_factory: HttpClientFactory,
    ) -> Self {
        let mut client =
            Self::new(base_url, http_client_factory).with_user_agent(get_codex_user_agent());
        client.auth = ClientAuth::new(codex_model_provider::auth_provider_from_auth(auth));
        client
    }

    /// Build a client using the captured credential and the request's organization/project headers.
    pub async fn from_auth_with_runtime(
        base_url: impl Into<String>,
        auth: &CodexAuth,
        http_client_factory: HttpClientFactory,
        runtime: Option<&(Arc<RuntimeAuthManager>, RuntimeAuthMode)>,
        headers: &HeaderMap,
    ) -> Result<Self> {
        let base_url = base_url.into();
        let runtime = codex_model_provider::runtime_auth_for_url(runtime, &base_url)?;
        Self::from_auth_with_selected_runtime(
            base_url,
            auth,
            http_client_factory,
            runtime,
            headers,
            Self::new,
        )
        .await
    }

    /// Build an authenticated client without following redirects.
    pub async fn from_auth_with_runtime_without_redirects(
        base_url: impl Into<String>,
        auth: &CodexAuth,
        http_client_factory: HttpClientFactory,
        runtime: Option<&(Arc<RuntimeAuthManager>, RuntimeAuthMode)>,
        headers: &HeaderMap,
    ) -> Result<Self> {
        let base_url = base_url.into();
        let runtime = codex_model_provider::runtime_auth_for_url(runtime, &base_url)?;
        Self::from_auth_with_selected_runtime(
            base_url,
            auth,
            http_client_factory,
            runtime,
            headers,
            Self::new_without_redirects,
        )
        .await
    }

    async fn from_auth_with_selected_runtime(
        base_url: String,
        auth: &CodexAuth,
        http_client_factory: HttpClientFactory,
        runtime: Option<&(Arc<RuntimeAuthManager>, RuntimeAuthMode)>,
        headers: &HeaderMap,
        new_client: fn(String, HttpClientFactory) -> Self,
    ) -> Result<Self> {
        let (auth, http_client_factory) =
            ClientAuth::for_auth(auth, runtime, http_client_factory, headers).await?;
        let mut client = new_client(base_url, http_client_factory);
        client.auth = auth;
        Ok(client.with_user_agent(get_codex_user_agent()))
    }

    /// Configure provider auth, returning an error for a runtime-bound client.
    pub fn with_auth_provider(mut self, auth: SharedAuthProvider) -> Result<Self> {
        self.auth.set_provider(auth)?;
        Ok(self)
    }

    pub fn with_bearer_token(self, token: String) -> Result<Self> {
        self.with_auth_provider(std::sync::Arc::new(
            codex_model_provider::BearerAuthProvider::new(token),
        ))
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

    pub fn with_path_style(mut self, style: PathStyle) -> Self {
        self.path_style = style;
        self
    }

    pub fn with_account_path_style(self, style: codex_config::AccountPathStyle) -> Self {
        self.with_path_style(PathStyle::from_account_style(style))
    }

    pub fn path_style(&self) -> PathStyle {
        self.path_style
    }

    async fn headers(&self) -> Result<HeaderMap, RequestError> {
        self.headers_with(HeaderMap::new()).await
    }

    async fn headers_with(&self, mut h: HeaderMap) -> Result<HeaderMap, RequestError> {
        if let Some(ua) = &self.user_agent {
            h.insert(USER_AGENT, ua.clone());
        } else {
            h.insert(USER_AGENT, HeaderValue::from_static("codex-cli"));
        }

        let selected = self.auth.resolve(&h).await?;
        h = selected.auth.apply_auth_headers(h).await?;
        if let Some(acc) = &self.chatgpt_account_id
            && let Ok(hv) = HeaderValue::from_str(acc)
        {
            h.insert(CHATGPT_ACCOUNT_ID_HEADER, hv);
        }
        if self.chatgpt_account_is_fedramp
            && let Ok(name) = HeaderName::from_bytes(b"X-OpenAI-Fedramp")
        {
            h.insert(name, HeaderValue::from_static("true"));
        }
        selected
            .validate_headers(&h)
            .map_err(|error| RequestError::Other(error.into()))?;
        Ok(h)
    }

    fn request(&self, method: Method, url: &str) -> RequestBuilder {
        self.http.request(method, url)
    }

    async fn exec_request(
        &self,
        req: RequestBuilder,
        method: &str,
        url: &str,
    ) -> Result<(String, String)> {
        self.auth.validate_destination(&self.base_url, url)?;
        let res = req.send().await?;
        let status = res.status();
        let ct = res
            .headers()
            .get(CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();
        let body = res.text().await.map_err(anyhow::Error::from)?;
        if !status.is_success() {
            anyhow::bail!("{method} {url} failed: {status}; content-type={ct}; body={body}");
        }
        Ok((body, ct))
    }

    async fn exec_request_detailed(
        &self,
        req: RequestBuilder,
        method: &str,
        url: &str,
    ) -> std::result::Result<(String, String), RequestError> {
        self.auth
            .validate_destination(&self.base_url, url)
            .map_err(|error| RequestError::Other(error.into()))?;
        let res = req.send().await?;
        let status = res.status();
        let content_type = res
            .headers()
            .get(CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();
        let identity_error_code =
            codex_response_debug_context::extract_response_debug_context_from_headers(
                res.headers(),
            )
            .auth_error_code;
        let body = res.text().await?;
        if !status.is_success() {
            return Err(RequestError::UnexpectedStatus {
                method: method.to_string(),
                url: url.to_string(),
                status,
                content_type,
                body,
                identity_error_code,
            });
        }
        Ok((body, content_type))
    }

    async fn exec_bootstrap_get(
        &self,
        url: &str,
    ) -> std::result::Result<(String, String), RequestError> {
        let request = self
            .request(Method::GET, url)
            .headers(self.headers().await?);
        if !self.http.allows_system_proxy_fallback() {
            return self.exec_request_detailed(request, "GET", url).await;
        }

        // Bound the complete GET, including its body, to leave time for proxy discovery and retry
        // within the cloud loader's startup budget. GETs are safe to retry after a response timeout.
        match tokio::time::timeout(
            Duration::from_secs(5),
            self.exec_request_detailed(request, "GET", url),
        )
        .await
        {
            Ok(Err(RequestError::Other(error)))
                if error
                    .downcast_ref::<RouteAwareRequestError>()
                    .is_some_and(|error| {
                        error.is_connect() || error.is_timeout() || error.is_request()
                    }) => {}
            Err(_) => {}
            Ok(response) => return response,
        }

        let http = self
            .http
            .clone()
            .with_outbound_proxy_policy(OutboundProxyPolicy::RespectSystemProxy);
        let request = http.get(url).headers(self.headers().await?);
        self.exec_request_detailed(request, "GET", url).await
    }

    fn decode_json<T: DeserializeOwned>(&self, url: &str, ct: &str, body: &str) -> Result<T> {
        match serde_json::from_str::<T>(body) {
            Ok(v) => Ok(v),
            Err(e) => {
                anyhow::bail!("Decode error for {url}: {e}; content-type={ct}; body={body}");
            }
        }
    }

    pub async fn list_tasks(
        &self,
        limit: Option<i32>,
        task_filter: Option<&str>,
        environment_id: Option<&str>,
        cursor: Option<&str>,
    ) -> Result<PaginatedListTaskListItem> {
        let url = self.list_tasks_url(limit, task_filter, environment_id, cursor)?;
        let req = self
            .request(Method::GET, &url)
            .headers(self.headers().await?);
        let (body, ct) = self.exec_request(req, "GET", &url).await?;
        self.decode_json::<PaginatedListTaskListItem>(&url, &ct, &body)
    }

    fn list_tasks_url(
        &self,
        limit: Option<i32>,
        task_filter: Option<&str>,
        environment_id: Option<&str>,
        cursor: Option<&str>,
    ) -> Result<String> {
        let url = match self.path_style {
            PathStyle::CodexApi => format!("{}/api/codex/tasks/list", self.base_url),
            PathStyle::ChatGptApi => format!("{}/wham/tasks/list", self.base_url),
        };
        if limit.is_none() && task_filter.is_none() && environment_id.is_none() && cursor.is_none()
        {
            return Ok(url);
        }
        let mut url = url::Url::parse(&url)?;
        {
            let mut query = url.query_pairs_mut();
            if let Some(limit) = limit {
                query.append_pair("limit", &limit.to_string());
            }
            if let Some(task_filter) = task_filter {
                query.append_pair("task_filter", task_filter);
            }
            if let Some(cursor) = cursor {
                query.append_pair("cursor", cursor);
            }
            if let Some(environment_id) = environment_id {
                query.append_pair("environment_id", environment_id);
            }
        }
        Ok(url.to_string())
    }

    pub async fn get_task_details(&self, task_id: &str) -> Result<CodeTaskDetailsResponse> {
        let (parsed, _body, _ct) = self.get_task_details_with_body(task_id).await?;
        Ok(parsed)
    }

    pub async fn get_task_details_with_body(
        &self,
        task_id: &str,
    ) -> Result<(CodeTaskDetailsResponse, String, String)> {
        let url = match self.path_style {
            PathStyle::CodexApi => format!("{}/api/codex/tasks/{}", self.base_url, task_id),
            PathStyle::ChatGptApi => format!("{}/wham/tasks/{}", self.base_url, task_id),
        };
        let req = self
            .request(Method::GET, &url)
            .headers(self.headers().await?);
        let (body, ct) = self.exec_request(req, "GET", &url).await?;
        let parsed: CodeTaskDetailsResponse = self.decode_json(&url, &ct, &body)?;
        Ok((parsed, body, ct))
    }

    pub async fn list_sibling_turns(
        &self,
        task_id: &str,
        turn_id: &str,
    ) -> Result<TurnAttemptsSiblingTurnsResponse> {
        let url = match self.path_style {
            PathStyle::CodexApi => format!(
                "{}/api/codex/tasks/{}/turns/{}/sibling_turns",
                self.base_url, task_id, turn_id
            ),
            PathStyle::ChatGptApi => format!(
                "{}/wham/tasks/{}/turns/{}/sibling_turns",
                self.base_url, task_id, turn_id
            ),
        };
        let req = self
            .request(Method::GET, &url)
            .headers(self.headers().await?);
        let (body, ct) = self.exec_request(req, "GET", &url).await?;
        self.decode_json::<TurnAttemptsSiblingTurnsResponse>(&url, &ct, &body)
    }

    /// `GET /api/codex/config/bundle` (Codex API style) or
    /// `GET /wham/config/bundle` (ChatGPT backend-api style), or
    /// `GET /codex/config/bundle` beneath a custom config server's base URL.
    pub async fn get_config_bundle(
        &self,
    ) -> std::result::Result<ConfigBundleResponse, RequestError> {
        let url = self.config_bundle_url();
        let (body, ct) = self.exec_bootstrap_get(&url).await?;
        self.decode_json::<ConfigBundleResponse>(&url, &ct, &body)
            .map_err(RequestError::from)
    }

    /// Returns the normalized discovery endpoint used by this client.
    pub fn config_bundle_url(&self) -> String {
        if let Some(base_url) = &self.config_server_base_url {
            let base_url = base_url.strip_suffix('/').unwrap_or(base_url);
            return format!("{base_url}/codex/config/bundle");
        }
        match self.path_style {
            PathStyle::CodexApi => format!("{}/api/codex/config/bundle", self.base_url),
            PathStyle::ChatGptApi => format!("{}/wham/config/bundle", self.base_url),
        }
    }

    /// Create a new task (user turn) by POSTing to the appropriate backend path
    /// based on `path_style`. Returns the created task id.
    pub async fn create_task(&self, request_body: serde_json::Value) -> Result<String> {
        let url = match self.path_style {
            PathStyle::CodexApi => format!("{}/api/codex/tasks", self.base_url),
            PathStyle::ChatGptApi => format!("{}/wham/tasks", self.base_url),
        };
        let req = self
            .request(Method::POST, &url)
            .headers(self.headers().await?)
            .header(CONTENT_TYPE, HeaderValue::from_static("application/json"))
            .json(&request_body);
        let (body, ct) = self.exec_request(req, "POST", &url).await?;
        // Extract id from JSON: prefer `task.id`; fallback to top-level `id` when present.
        match serde_json::from_str::<serde_json::Value>(&body) {
            Ok(v) => {
                if let Some(id) = v
                    .get("task")
                    .and_then(|t| t.get("id"))
                    .and_then(|s| s.as_str())
                {
                    Ok(id.to_string())
                } else if let Some(id) = v.get("id").and_then(|s| s.as_str()) {
                    Ok(id.to_string())
                } else {
                    anyhow::bail!(
                        "POST {url} succeeded but no task id found; content-type={ct}; body={body}"
                    );
                }
            }
            Err(e) => anyhow::bail!("Decode error for {url}: {e}; content-type={ct}; body={body}"),
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
    fn config_bundle_url_follows_the_account_path_style() {
        let factory =
            HttpClientFactory::new(codex_http_client::OutboundProxyPolicy::ReqwestDefault);
        let backend_api = "https://example.test/backend-api";
        let already_prefixed = "https://example.test/api/codex";
        assert_eq!(
            Client::new(backend_api, factory.clone()).config_bundle_url(),
            "https://example.test/backend-api/api/codex/config/bundle"
        );
        assert_eq!(
            Client::new(already_prefixed, factory.clone()).config_bundle_url(),
            "https://example.test/api/codex/api/codex/config/bundle"
        );
        assert_eq!(
            Client::new(backend_api, factory)
                .with_path_style(PathStyle::ChatGptApi)
                .config_bundle_url(),
            "https://example.test/backend-api/wham/config/bundle"
        );
    }
}

#[cfg(test)]
#[path = "client/runtime_auth_tests.rs"]
mod runtime_auth_tests;
