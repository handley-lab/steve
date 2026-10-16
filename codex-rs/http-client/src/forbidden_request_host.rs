use std::error::Error;
use std::fmt;

const FORBIDDEN_HOSTS: &[&str] = &[
    "openai.com",
    "chatgpt.com",
    "chatgpt-staging.com",
    "openai.org",
];

/// A configured URL names a host this build must not request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForbiddenOpenAiRequestHost {
    host: String,
}

impl ForbiddenOpenAiRequestHost {
    pub fn host(&self) -> &str {
        &self.host
    }
}

impl fmt::Display for ForbiddenOpenAiRequestHost {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "requests to {} are not available", self.host)
    }
}

impl Error for ForbiddenOpenAiRequestHost {}

/// Exact host or a subdomain of `openai.com`, `chatgpt.com`,
/// `chatgpt-staging.com`, or `openai.org`.
///
/// `evilopenai.com` and `openai.com.evil.example` do not match.
pub fn is_forbidden_openai_request_host(host: &str) -> bool {
    let host = host.trim_end_matches('.').to_ascii_lowercase();
    FORBIDDEN_HOSTS.iter().any(|forbidden| {
        host == *forbidden || host.ends_with(&format!(".{forbidden}"))
    })
}

/// Rejects a URL before a request to that host is built.
///
/// A value that does not parse, or that has no host, is not one of these hosts.
pub fn reject_forbidden_openai_request_url(url: &str) -> Result<(), ForbiddenOpenAiRequestHost> {
    let Some(host) = request_host(url) else {
        return Ok(());
    };
    if is_forbidden_openai_request_host(&host) {
        Err(ForbiddenOpenAiRequestHost { host })
    } else {
        Ok(())
    }
}

fn request_host(url: &str) -> Option<String> {
    let trimmed = url.trim();
    if trimmed.is_empty() {
        return None;
    }
    let with_scheme = if trimmed.contains("://") {
        trimmed.to_string()
    } else {
        format!("https://{trimmed}")
    };
    reqwest::Url::parse(&with_scheme)
        .ok()
        .and_then(|parsed| parsed.host_str().map(str::to_string))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_forbidden_hosts_and_subdomains_only() {
        for url in [
            "https://openai.com/v1",
            "https://api.openai.com/v1",
            "https://API.OPENAI.COM/v1",
            "https://chatgpt.com/backend-api",
            "https://chat.openai.com/backend-api",
            "https://auth.openai.com/api/accounts",
            "https://developers.openai.com/mcp",
            "https://chatgpt-staging.com/backend-api",
            "https://api.chatgpt-staging.com/v1",
            "https://openai.org/api",
            "https://auth.api.openai.org/api/accounts",
            "api.openai.com/v1",
        ] {
            assert!(
                reject_forbidden_openai_request_url(url).is_err(),
                "{url}"
            );
        }

        for url in [
            "https://llm.hpc.cam.ac.uk/v1",
            "https://evilopenai.com/v1",
            "https://openai.com.evil.example/v1",
            "https://notchatgpt-staging.com/v1",
            "https://example.com/v1",
            "http://127.0.0.1:9/v1",
        ] {
            assert!(
                reject_forbidden_openai_request_url(url).is_ok(),
                "{url}"
            );
        }
    }
}
