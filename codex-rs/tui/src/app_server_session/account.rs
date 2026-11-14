//! Account details shared by startup and fresh threads, resolved by the connected server.

use super::*;
use crate::config_update::EffectiveConfig;
use codex_app_server_protocol::ConfigReadParams;

#[derive(Default)]
pub(crate) struct SessionAccount {
    pub(crate) display: Option<StatusAccountDisplay>,
    pub(crate) requires_openai_auth: bool,
}

impl From<GetAccountResponse> for SessionAccount {
    fn from(response: GetAccountResponse) -> Self {
        Self {
            requires_openai_auth: response.requires_openai_auth,
            display: match response.account {
                Some(Account::ApiKey {}) => Some(StatusAccountDisplay::ApiKey),
                Some(Account::AmazonBedrock { .. } | Account::Gcp {}) | None => None,
            },
        }
    }
}

impl SessionAccount {
    pub(super) fn auth_mode(&self) -> Option<TelemetryAuthMode> {
        match self.display {
            Some(StatusAccountDisplay::ApiKey) => Some(TelemetryAuthMode::ApiKey),
            None => None,
        }
    }
}

impl AppServerSession {
    pub(crate) async fn read_thread_account(
        &mut self,
        session: &ThreadSessionState,
    ) -> Result<SessionAccount> {
        let response = self.read_account().await?;
        // account/read resolves the server's non-project configuration. A thread can override it.
        let request_id = self.next_request_id();
        let defaults: EffectiveConfig = self
            .request_handle
            .request_typed(ClientRequest::ConfigRead {
                request_id,
                params: ConfigReadParams {
                    include_layers: false,
                    cwd: None,
                },
            })
            .await?;
        let config = defaults.config;
        let provider = config.model_provider.as_deref().ok_or_else(|| {
            color_eyre::eyre::eyre!("the server has no configured model provider")
        })?;
        // Compare against the configured provider, including custom endpoints.
        let auth_mismatch = config
            .additional
            .get("model_providers")
            .and_then(|providers| providers.get(provider))
            .and_then(|info| info.get("requires_openai_auth"))
            .and_then(serde_json::Value::as_bool)
            .is_some_and(|requires_auth| requires_auth != response.requires_openai_auth);
        if provider != session.model_provider_id || auth_mismatch {
            color_eyre::eyre::bail!(
                "the server's account details do not match this chat's provider"
            );
        }
        Ok(response.into())
    }
}
