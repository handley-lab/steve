use chrono::DateTime;
use chrono::Local;
use chrono::Utc;
use http::header::HeaderMap;

use codex_core::config::Config;
use codex_http_client::HttpClientFactory;
use codex_http_client::OutboundProxyPolicy;
use codex_login::AuthManager;
use std::sync::Arc;

pub fn set_user_agent_suffix(suffix: &str) {
    if let Ok(mut guard) = codex_login::default_client::USER_AGENT_SUFFIX.lock() {
        guard.replace(suffix.to_string());
    }
}

pub fn append_error_log(message: impl AsRef<str>) {
    let ts = Utc::now().to_rfc3339();
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open("error.log")
    {
        use std::io::Write as _;
        let _ = writeln!(f, "[{ts}] {}", message.as_ref());
    }
}

/// Normalize the configured base URL to a canonical form used by the backend client.
/// - trims trailing '/'
/// - appends '/backend-api' for ChatGPT hosts when missing
pub fn normalize_base_url(input: &str) -> String {
    let mut base_url = input.trim_end_matches('/').to_string();
    if (base_url.starts_with("https://chatgpt.com")
        || base_url.starts_with("https://chat.openai.com"))
        && !base_url.contains("/backend-api")
    {
        base_url = format!("{base_url}/backend-api");
    }
    base_url
}

/// Validate the configured destination before loading credentials, including in mock mode:
/// environment discovery still makes authenticated HTTP requests when the task backend is mocked.
pub(crate) fn validate_backend_base_url(input: &str) -> anyhow::Result<String> {
    let invalid_url = || {
        anyhow::anyhow!(
            "STEVE_CLOUD_TASKS_BASE_URL must be an HTTP(S) base URL without user information, a query, or a fragment"
        )
    };
    let uri = input.parse::<http::Uri>().map_err(|_| invalid_url())?;
    let authority = uri
        .authority()
        .ok_or_else(invalid_url)?
        .as_str()
        .to_ascii_lowercase();
    let scheme = uri.scheme_str().ok_or_else(invalid_url)?;
    if !matches!(scheme, "http" | "https")
        || authority.contains('@')
        || uri.query().is_some()
        || input.contains('#')
    {
        return Err(invalid_url());
    }
    Ok(normalize_base_url(&format!(
        "{scheme}://{authority}{}",
        uri.path()
    )))
}

/// Select the cloud destination before credentials are loaded. An environment override
/// may change the path, but must retain the operator-configured recipient.
pub(crate) fn select_backend_base_url(
    configured: &str,
    override_url: Option<&str>,
) -> anyhow::Result<String> {
    let configured = validate_backend_base_url(configured)?;
    let selected = validate_backend_base_url(override_url.unwrap_or(&configured))?;
    if url::Url::parse(&configured)?.origin() != url::Url::parse(&selected)?.origin() {
        anyhow::bail!("STEVE_CLOUD_TASKS_BASE_URL must retain the configured backend origin");
    }
    Ok(selected)
}

pub async fn load_auth_manager(base_url: &str) -> (Option<Arc<AuthManager>>, HttpClientFactory) {
    // TODO: pass in cli overrides once cloud tasks properly support them.
    let config = match Config::load_with_cli_overrides(Vec::new()).await {
        Ok(config) => config,
        Err(error) => {
            append_error_log(format!(
                "failed to load auth config; using transport-default proxy handling: {error}"
            ));
            let http_client_factory = HttpClientFactory::new(OutboundProxyPolicy::ReqwestDefault);
            return (None, http_client_factory);
        }
    };
    let http_client_factory = config.http_client_factory();
    let mut auth_config = config.auth_config();
    auth_config.chatgpt_base_url = Some(base_url.to_string());
    let auth_manager = match AuthManager::shared_from_auth_config(
        auth_config,
        /*enable_codex_api_key_env*/ false,
    )
    .await
    {
        Ok(auth_manager) => auth_manager,
        Err(error) => {
            append_error_log(format!("failed to load auth: {error}"));
            return (None, http_client_factory);
        }
    };
    (Some(auth_manager), http_client_factory)
}

/// Build headers for the configured backend: `User-Agent`, optional `Authorization`,
/// and optional `ChatGPT-Account-Id`.
pub async fn build_backend_headers(base_url: &str) -> HeaderMap {
    use http::header::HeaderValue;
    use http::header::USER_AGENT;

    set_user_agent_suffix("codex_cloud_tasks_tui");
    let ua = codex_login::default_client::get_codex_user_agent();
    let mut headers = HeaderMap::new();
    headers.insert(
        USER_AGENT,
        HeaderValue::from_str(&ua).unwrap_or(HeaderValue::from_static("codex-cli")),
    );
    if let Some(am) = load_auth_manager(base_url).await.0
        && let Some(auth) = am.auth().await
        && (auth.uses_codex_backend() || matches!(&auth, codex_login::CodexAuth::ApiKey(_)))
    {
        headers.extend(codex_model_provider::auth_provider_from_auth(&auth).to_auth_headers());
    }
    headers
}

/// Construct a browser-friendly task URL for the given backend base URL.
pub fn task_url(base_url: &str, task_id: &str) -> String {
    let normalized = normalize_base_url(base_url);
    if let Some(root) = normalized.strip_suffix("/backend-api") {
        return format!("{root}/codex/tasks/{task_id}");
    }
    if let Some(root) = normalized.strip_suffix("/api/codex") {
        return format!("{root}/codex/tasks/{task_id}");
    }
    if normalized.ends_with("/codex") {
        return format!("{normalized}/tasks/{task_id}");
    }
    format!("{normalized}/codex/tasks/{task_id}")
}

pub fn format_relative_time(reference: DateTime<Utc>, ts: DateTime<Utc>) -> String {
    let mut secs = (reference - ts).num_seconds();
    if secs < 0 {
        secs = 0;
    }
    if secs < 60 {
        return format!("{secs}s ago");
    }
    let mins = secs / 60;
    if mins < 60 {
        return format!("{mins}m ago");
    }
    let hours = mins / 60;
    if hours < 24 {
        return format!("{hours}h ago");
    }
    let local = ts.with_timezone(&Local);
    local.format("%b %e %H:%M").to_string()
}

pub fn format_relative_time_now(ts: DateTime<Utc>) -> String {
    format_relative_time(Utc::now(), ts)
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;

    use super::normalize_base_url;
    use super::validate_backend_base_url;

    #[test]
    fn configured_backend_urls_preserve_destination_and_reject_ambiguous_bases() {
        for (input, expected) in [
            (
                "http://127.0.0.1:8080/service/",
                "http://127.0.0.1:8080/service",
            ),
            ("https://backend.example/", "https://backend.example"),
            ("https://chatgpt.com/", "https://chatgpt.com/backend-api"),
        ] {
            assert_eq!(validate_backend_base_url(input).unwrap(), expected);
        }
        for input in [
            "",
            "/relative",
            "ftp://backend.example/",
            "https://user:password@backend.example/",
            "https://backend.example/?query=value",
            "https://backend.example/#fragment",
        ] {
            assert!(validate_backend_base_url(input).is_err(), "{input}");
        }
    }

    #[test]
    fn cloud_override_retains_configured_credential_recipient() {
        use super::select_backend_base_url;
        assert_eq!(
            select_backend_base_url("http://localhost:8080/base", None).unwrap(),
            "http://localhost:8080/base"
        );
        assert_eq!(
            select_backend_base_url(
                "https://backend.example/base",
                Some("https://backend.example:443/tasks")
            )
            .unwrap(),
            "https://backend.example:443/tasks"
        );
        for (configured, selected) in [
            ("", "https://backend.example"),
            ("https://backend.example", "https://other.example"),
            ("https://backend.example", "http://backend.example"),
            ("http://localhost:8080", "http://localhost:8081"),
            ("https://backend.example", ""),
        ] {
            assert!(select_backend_base_url(configured, Some(selected)).is_err());
        }
    }

    #[test]
    fn normalize_base_url_normalizes_urls() {
        for (input, expected) in [
            ("https://example.com/path", "https://example.com/path"),
            ("https://example.com/path/", "https://example.com/path"),
            ("https://example.com/path///", "https://example.com/path"),
            ("", ""),
            ("///", ""),
            ("https://chatgpt.com///", "https://chatgpt.com/backend-api"),
            (
                "https://chatgpt.com/backend-api///",
                "https://chatgpt.com/backend-api",
            ),
            (
                "https://chat.openai.com///",
                "https://chat.openai.com/backend-api",
            ),
            (
                "https://chat.openai.com/backend-api///",
                "https://chat.openai.com/backend-api",
            ),
        ] {
            assert_eq!(normalize_base_url(input), expected, "input: {input:?}");
        }
    }
}
