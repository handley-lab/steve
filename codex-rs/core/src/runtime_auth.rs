//! Construct and share the harness-owned runtime credential context.

use crate::config::Config;
use codex_login::AuthManager;
use codex_protocol::error::Result as CodexResult;
use codex_runtime_agent_instance::RuntimeAuthManager;
use std::sync::Arc;
use std::sync::OnceLock;

// Hosts may explicitly install a runtime shared by a thread and its descendants.
pub(crate) type RuntimeAuthSlot = Arc<OnceLock<Arc<RuntimeAuthManager>>>;

/// Share this owner between thread sessions and first-party API clients.
/// Construction does not register a runtime or generate its key.
pub fn build_runtime_auth_manager(
    config: &Config,
    credentials: Arc<AuthManager>,
    installation_id: &str,
) -> Option<Arc<RuntimeAuthManager>> {
    runtime_for_config(
        &RuntimeAuthSlot::default(),
        config,
        credentials,
        installation_id,
    )
}

pub(crate) fn runtime_for_config(
    slot: &RuntimeAuthSlot,
    config: &Config,
    credentials: Arc<AuthManager>,
    installation_id: &str,
) -> Option<Arc<RuntimeAuthManager>> {
    let _ = (slot, config, credentials, installation_id);
    None
}

pub(crate) fn is_openai_realtime_url(base_url: &str) -> CodexResult<bool> {
    let Ok(mut url) = url::Url::parse(base_url) else {
        return Ok(false);
    };
    if url.scheme() == "wss" && url.set_scheme("https").is_err() {
        return Ok(false);
    }
    codex_model_provider::validate_runtime_destination(Some(url.as_str()))?;
    Ok(codex_runtime_agent_instance::is_openai_api_url(
        url.as_str(),
    ))
}

#[cfg(test)]
#[path = "runtime_auth_tests.rs"]
mod tests;
