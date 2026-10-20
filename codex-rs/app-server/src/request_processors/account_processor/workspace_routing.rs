//! Reads the current account without discovering a ChatGPT workspace route.

use super::*;
use codex_config::ConfigLoadError;
use codex_http_client::NetworkPolicyDenied;
use codex_model_provider::ProviderAccountError;
use codex_model_provider::ProviderAccountState;
use std::io;

pub(super) struct AccountRead {
    account_state: ProviderAccountState,
}

#[derive(Debug, thiserror::Error)]
pub(super) enum AccountReadError {
    #[error(transparent)]
    InvalidAccount(#[from] ProviderAccountError),
    #[error("failed to load account requirements: {0}")]
    RequirementsLoad(String),
}

impl AccountRequestProcessor {
    pub(crate) async fn get_account(
        &self,
        params: GetAccountParams,
    ) -> Result<Option<ClientResponsePayload>, JSONRPCErrorError> {
        self.refresh_token_if_requested(params.refresh_token)
            .await?;
        let read = self.read_account().await.map_err(|error| match error {
            AccountReadError::InvalidAccount(error) => invalid_request(error.to_string()),
            AccountReadError::RequirementsLoad(error) => internal_error(error),
        })?;
        Ok(Some(
            GetAccountResponse {
                account: read.account_state.account.map(Account::from),
                requires_openai_auth: read.account_state.requires_openai_auth,
                workspace_routing: None,
            }
            .into(),
        ))
    }

    pub(super) async fn read_account(&self) -> Result<AccountRead, AccountReadError> {
        let config = match self
            .config_manager
            .load_latest_config(/*fallback_cwd*/ None)
            .await
        {
            Ok(config) => config,
            Err(_)
                if !self
                    .auth_manager
                    .auth_cached()
                    .as_ref()
                    .is_some_and(CodexAuth::is_chatgpt_auth) =>
            {
                self.config.as_ref().clone()
            }
            Err(error) => {
                return Err(AccountReadError::RequirementsLoad(
                    workspace_config_load_reason(&error),
                ));
            }
        };
        let provider = create_model_provider(
            config.model_provider.clone(),
            Some(self.auth_manager.clone()),
        );
        let account_state = provider
            .account_state()
            .map_err(AccountReadError::InvalidAccount)?;
        Ok(AccountRead { account_state })
    }
}

fn workspace_config_load_reason(error: &io::Error) -> String {
    // Parser messages and cloud bundle errors can quote secrets, even without
    // their source snippets. Retain only structured diagnostics before these
    // errors reach JSON-RPC clients, login notifications, or tracing.
    let mut current = error
        .get_ref()
        .map(|source| source as &(dyn std::error::Error + 'static));
    while let Some(source) = current {
        if let Some(config) = source.downcast_ref::<ConfigLoadError>() {
            let config = config.config_error();
            let start = config.range.start;
            // Non-file diagnostics store a display name, which can contain
            // backend-supplied policy names and IDs, in this field.
            if !config.path.is_absolute() {
                return format!(
                    "invalid non-file configuration at line {}, column {}",
                    start.line, start.column
                );
            }
            return format!(
                "invalid configuration at {}:{}:{}",
                config.path.display(),
                start.line,
                start.column
            );
        }
        if let Some(cloud) = source.downcast_ref::<CloudConfigBundleLoadError>() {
            let reason = match cloud.code() {
                CloudConfigBundleLoadErrorCode::Timeout => "request timed out",
                CloudConfigBundleLoadErrorCode::Auth
                | CloudConfigBundleLoadErrorCode::RequestFailed => {
                    // Cloud loading currently erases the typed denial. Match
                    // only known safe policy messages, never forward raw text.
                    let message = cloud.to_string();
                    if let Some(policy) = [
                        NetworkPolicyDenied::Unavailable,
                        NetworkPolicyDenied::Destination,
                        NetworkPolicyDenied::Revoked,
                        NetworkPolicyDenied::UnsupportedTransport,
                    ]
                    .into_iter()
                    .find(|policy| policy.to_string() == message)
                    {
                        return policy.to_string();
                    }
                    if cloud.code() == CloudConfigBundleLoadErrorCode::Auth {
                        "authentication failed"
                    } else {
                        "request failed"
                    }
                }
                CloudConfigBundleLoadErrorCode::InvalidBundle => "invalid bundle",
                CloudConfigBundleLoadErrorCode::Internal => "internal error",
            };
            return match cloud.status_code() {
                Some(status) => format!("cloud config: {reason} (HTTP {status})"),
                None => format!("cloud config: {reason}"),
            };
        }
        if let Some(policy) = source.downcast_ref::<NetworkPolicyDenied>() {
            return policy.to_string();
        }
        current = source.source();
    }
    // Use the OS error or kind, never an untyped error's potentially secret message.
    match error.raw_os_error() {
        Some(code) => io::Error::from_raw_os_error(code).to_string(),
        None => error.kind().to_string(),
    }
}
