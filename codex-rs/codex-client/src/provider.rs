//! Endpoint configuration and request construction shared by API clients.

use crate::Request;
use crate::RequestCompression;
use crate::RetryOn;
use crate::RetryPolicy;
use codex_http_client::ForbiddenOpenAiRequestHost;
use codex_http_client::reject_forbidden_openai_request_url;
use http::Method;
use http::header::HeaderMap;
use std::collections::HashMap;
use std::fmt;
use std::time::Duration;
use url::Url;

/// Failure while turning a provider base URL into a request URL.
#[derive(Debug)]
pub enum ProviderUrlError {
    Forbidden(ForbiddenOpenAiRequestHost),
    Parse(url::ParseError),
}

impl fmt::Display for ProviderUrlError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Forbidden(error) => write!(f, "{error}"),
            Self::Parse(error) => write!(f, "{error}"),
        }
    }
}

impl std::error::Error for ProviderUrlError {}

impl From<ForbiddenOpenAiRequestHost> for ProviderUrlError {
    fn from(error: ForbiddenOpenAiRequestHost) -> Self {
        Self::Forbidden(error)
    }
}

impl From<url::ParseError> for ProviderUrlError {
    fn from(error: url::ParseError) -> Self {
        Self::Parse(error)
    }
}

/// High-level retry configuration for a provider.
///
/// This is converted into a `RetryPolicy` used by `codex-client` to drive
/// transport-level retries for both unary and streaming calls.
#[derive(Debug, Clone)]
pub struct RetryConfig {
    pub max_attempts: u64,
    pub base_delay: Duration,
    pub retry_429: bool,
    pub retry_5xx: bool,
    pub retry_transport: bool,
}

impl RetryConfig {
    pub fn to_policy(&self) -> RetryPolicy {
        RetryPolicy {
            max_attempts: self.max_attempts,
            base_delay: self.base_delay,
            retry_on: RetryOn {
                retry_429: self.retry_429,
                retry_5xx: self.retry_5xx,
                retry_transport: self.retry_transport,
            },
        }
    }
}

/// HTTP endpoint configuration used to talk to a concrete API deployment.
///
/// Encapsulates base URL, default headers, query params, retry policy, and
/// stream idle timeout, plus helper methods for building requests.
#[derive(Debug, Clone)]
pub struct Provider {
    pub name: String,
    pub base_url: String,
    pub query_params: Option<HashMap<String, String>>,
    pub headers: HeaderMap,
    pub retry: RetryConfig,
    pub stream_idle_timeout: Duration,
}

impl Provider {
    pub fn url_for_path(&self, path: &str) -> Result<String, ForbiddenOpenAiRequestHost> {
        reject_forbidden_openai_request_url(&self.base_url)?;
        let base = self.base_url.trim_end_matches('/');
        let path = path.trim_start_matches('/');
        let mut url = if path.is_empty() {
            base.to_string()
        } else {
            format!("{base}/{path}")
        };

        if let Some(params) = &self.query_params
            && !params.is_empty()
        {
            let qs = params
                .iter()
                .map(|(k, v)| format!("{k}={v}"))
                .collect::<Vec<_>>()
                .join("&");
            url.push('?');
            url.push_str(&qs);
        }

        Ok(url)
    }

    pub fn build_request(
        &self,
        method: Method,
        path: &str,
    ) -> Result<Request, ForbiddenOpenAiRequestHost> {
        Ok(Request {
            method,
            url: self.url_for_path(path)?,
            headers: self.headers.clone(),
            body: None,
            compression: RequestCompression::None,
            timeout: None,
            response_body_limit_bytes: None,
        })
    }

    pub fn websocket_url_for_path(&self, path: &str) -> Result<Url, ProviderUrlError> {
        let mut url = Url::parse(&self.url_for_path(path)?)?;

        let scheme = match url.scheme() {
            "http" => "ws",
            "https" => "wss",
            "ws" | "wss" => return Ok(url),
            _ => return Ok(url),
        };
        let _ = url.set_scheme(scheme);
        Ok(url)
    }
}
