use http::StatusCode;
use http::header::HeaderValue;
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
    user_agent: Option<HeaderValue>,
    chatgpt_account_id: Option<String>,
    chatgpt_account_is_fedramp: bool,
    path_style: PathStyle,
}

impl fmt::Debug for Client {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Client")
            .field("base_url", &self.base_url)
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
    pub fn new(base_url: impl Into<String>) -> Self {
        Self::with_base_url(base_url.into())
    }

    pub fn new_without_redirects(base_url: impl Into<String>) -> Self {
        Self::with_base_url(base_url.into())
    }

    fn with_base_url(mut base_url: String) -> Self {
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
            user_agent: None,
            chatgpt_account_id: None,
            chatgpt_account_is_fedramp: false,
            path_style,
        }
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
}
