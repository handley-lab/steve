//! Local account reads for `account/read`. Workspace routing is not requested.

use super::*;
use codex_app_server_protocol::AccountRoutingOverride;
use codex_config::ConfigLoadError;
use codex_http_client::NetworkPolicyDenied;
use codex_login::WorkspaceRouting;
use codex_login::WorkspaceRoutingRequest;
use codex_login::WorkspaceRoutingResolver;
use codex_model_provider::ProviderAccountError;
use codex_model_provider::ProviderAccountState;
use std::future::Future;
use std::io;
use std::pin::Pin;

pub(super) struct AccountRead {
    account_state: ProviderAccountState,
    pub(super) workspace_routing: Option<WorkspaceRouting>,
}

#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub(super) enum AccountReadError {
    #[error(transparent)]
    InvalidAccount(#[from] ProviderAccountError),
    #[error(transparent)]
    Routing(#[from] WorkspaceRoutingError),
}

/// Routing failures carry no account, URL, or backend data. Config failures retain
/// only a location or structured error category, never the raw config error.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub(super) enum WorkspaceRoutingError {
    #[error("failed to load workspace requirements: {0}")]
    RequirementsLoad(String),
    #[error("account changed during workspace routing discovery")]
    AccountChanged,
    #[error("workspace routing discovery cancelled during shutdown")]
    Shutdown,
    #[error("workspace routing discovery timed out")]
    DiscoveryTimeout,
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

impl WorkspaceRoutingResolver for AccountRequestProcessor {
    fn resolve(
        &self,
        request: WorkspaceRoutingRequest,
    ) -> Pin<Box<dyn Future<Output = io::Result<Option<WorkspaceRouting>>> + Send + '_>> {
        Box::pin(async move {
            self.read_account(Some(&request))
                .await
                .map(|account| account.workspace_routing)
                .map_err(|error| match error {
                    AccountReadError::InvalidAccount(error) => io::Error::other(error),
                    AccountReadError::Routing(error) => io::Error::other(error),
                })
        })
    }
}

impl AccountRequestProcessor {
    pub(super) async fn read_account_for_background_workspace_routing(
        &self,
    ) -> Result<Option<AccountRead>, AccountReadError> {
        if !self
            .auth_manager
            .auth_cached()
            .as_ref()
            .is_some_and(CodexAuth::is_chatgpt_auth)
        {
            return Ok(None);
        }

        self.read_account(/*request*/ None).await.map(Some)
    }

    pub(crate) fn notify_workspace_routing_to_connection(&self, connection_id: ConnectionId) {
        let processor = self.clone();
        let auth_changes = self.auth_manager.auth_change_state_receiver();
        let owner_generation = auth_changes.borrow().owner_generation;
        tokio::spawn(async move {
            if auth_changes.borrow().owner_generation != owner_generation {
                return;
            }
            if let Ok(Some(response)) = processor
                .read_account_for_background_workspace_routing()
                .await
                && response.workspace_routing.is_some()
                && auth_changes.borrow().owner_generation == owner_generation
            {
                let notification = processor.current_account_updated_notification();
                processor
                    .outgoing
                    .send_account_notification(
                        Some(connection_id),
                        &auth_changes,
                        owner_generation,
                        AccountNotification::Updated(notification),
                    )
                    .await;
            }
        });
    }

    pub(crate) async fn get_account(
        &self,
        params: GetAccountParams,
    ) -> Result<Option<ClientResponsePayload>, JSONRPCErrorError> {
        self.refresh_token_if_requested(params.refresh_token).await;
        let read = self
            .read_account(/*request*/ None)
            .await
            .map_err(|error| match error {
                AccountReadError::InvalidAccount(error) => invalid_request(error.to_string()),
                AccountReadError::Routing(error) => internal_error(error.to_string()),
            })?;
        Ok(Some(
            GetAccountResponse {
                account: read.account_state.account.map(Account::from),
                requires_openai_auth: read.account_state.requires_openai_auth,
                workspace_routing: read.workspace_routing.map(|routing| {
                    codex_app_server_protocol::WorkspaceRouting {
                        chatgpt_account_id: routing.chatgpt_account_id,
                        backend_origin: routing.backend_origin,
                        account_routing_override: match routing.account_routing_override.as_str() {
                            "NO_CONSTRAINT" => AccountRoutingOverride::NoConstraint,
                            "us" => AccountRoutingOverride::Us,
                            "us_cr" => AccountRoutingOverride::UsCr,
                            _ => unreachable!("routing discovery validates the override"),
                        },
                    }
                }),
            }
            .into(),
        ))
    }

    pub(super) async fn read_account(
        &self,
        request: Option<&WorkspaceRoutingRequest>,
    ) -> Result<AccountRead, AccountReadError> {
        let mut auth_changes = self.auth_manager.auth_change_state_receiver();
        let auth_state = *auth_changes.borrow_and_update();
        let read = Box::pin(async {
            let load_config = async || {
                match request.and_then(|request| request.session.as_deref()) {
                    Some(session) => {
                        self.config_manager
                            .load_retained_session_config(&session.config_layer_stack, &session.cwd)
                            .await
                    }
                    None => {
                        self.config_manager
                            .load_latest_config(/*fallback_cwd*/ None)
                            .await
                    }
                }
            };
            let config = match load_config().await {
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
                    return Err(WorkspaceRoutingError::RequirementsLoad(
                        workspace_config_load_reason(&error),
                    )
                    .into());
                }
            };
            let provider = create_model_provider(
                config.model_provider.clone(),
                Some(self.auth_manager.clone()),
            );
            let account_state = provider
                .account_state()
                .map_err(AccountReadError::InvalidAccount)?;
            Ok(AccountRead {
                account_state,
                workspace_routing: None,
            })
        });
        let result = tokio::select! {
            biased;
            _ = self.workspace_routing_shutdown.cancelled() => {
                return Err(WorkspaceRoutingError::Shutdown.into());
            }
            _ = auth_changes.wait_for(|state| state.owner_generation != auth_state.owner_generation) => {
                return Err(WorkspaceRoutingError::AccountChanged.into());
            }
            result = tokio::time::timeout(Duration::from_secs(/*secs*/ 15), read) => {
                result.map_err(|_| WorkspaceRoutingError::DiscoveryTimeout)?
            }
        };
        if auth_changes.borrow().owner_generation != auth_state.owner_generation {
            return Err(WorkspaceRoutingError::AccountChanged.into());
        }
        result
    }
}

#[cfg(test)]
#[path = "workspace_routing_tests.rs"]
mod tests;
