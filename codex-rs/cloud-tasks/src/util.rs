use chrono::DateTime;
use chrono::Local;
use chrono::Utc;
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

/// Reads the configured cloud-tasks base URL.
///
/// A missing value is an error. This does not substitute a ChatGPT host.
pub(crate) fn configured_cloud_tasks_base_url() -> anyhow::Result<String> {
    let base_url = std::env::var("STEVE_CLOUD_TASKS_BASE_URL").map_err(|_| {
        anyhow::anyhow!("STEVE_CLOUD_TASKS_BASE_URL is required")
    })?;
    validate_chatgpt_base_url(&base_url)
}

/// Validate the destination before loading saved ChatGPT credentials, including in mock mode:
/// environment discovery still makes authenticated HTTP requests when the task backend is mocked.
pub(crate) fn validate_chatgpt_base_url(input: &str) -> anyhow::Result<String> {
    let invalid_url = || {
        anyhow::anyhow!(
            "STEVE_CLOUD_TASKS_BASE_URL must use a trusted HTTPS origin on port 443, without user information, a query, or a fragment; custom backends cannot use saved ChatGPT credentials"
        )
    };
    codex_http_client::reject_forbidden_openai_request_url(input)
        .map_err(|error| anyhow::anyhow!(error))?;
    let uri = input.parse::<http::Uri>().map_err(|_| invalid_url())?;
    let authority = uri.authority().ok_or_else(invalid_url)?;
    if uri.scheme_str() != Some("https")
        || authority.as_str().contains('@')
        || uri.query().is_some()
        || input.contains('#')
    {
        return Err(invalid_url());
    }
    // Saved ChatGPT credentials are not attached to a cloud-tasks host.
    Err(invalid_url())
}

pub async fn load_auth_manager(
    chatgpt_base_url: Option<String>,
) -> (Option<Arc<AuthManager>>, HttpClientFactory) {
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
    auth_config.chatgpt_base_url = chatgpt_base_url.or(Some(config.chatgpt_base_url.clone()));
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
