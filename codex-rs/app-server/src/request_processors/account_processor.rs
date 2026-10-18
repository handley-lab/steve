use super::bedrock_auth::BedrockProviderConfig;
use super::bedrock_auth::clear_user_model_provider_if_bedrock;
use super::bedrock_auth::configure_bedrock_provider;
use super::bedrock_auth::ensure_user_model_provider_can_be_bedrock;
use super::*;
use crate::auth_mode::auth_mode_to_api;
use crate::external_auth::ExternalAuthBridge;
use crate::outgoing_message::AccountNotification;
use chrono::DateTime;
use codex_app_server_protocol::GetAccountRateLimitsParams;
use codex_login::login_with_bedrock_access_keys;
use codex_mcp::ema_auth_scope;
use codex_model_provider::is_supported_amazon_bedrock_region;
use codex_rmcp_client::EnterpriseOAuthCredentialGuard;

mod bedrock_gov_cloud;
mod bedrock_setup;
mod enterprise_login;
mod gateway_oauth;
mod rate_limit_resets;
mod workspace_routing;

pub(super) use enterprise_login::EnterpriseLoginCompletion;
pub(super) use enterprise_login::EnterpriseLoginTarget;

const ACCOUNT_TOKEN_USAGE_FETCH_TIMEOUT: Duration = Duration::from_secs(/*secs*/ 10);
const THREAD_USAGE_FETCH_TIMEOUT: Duration = Duration::from_secs(/*secs*/ 60);
const ACCOUNT_WORKSPACE_MESSAGES_FETCH_TIMEOUT: Duration =
    Duration::from_millis(/*millis*/ 1000);
enum RefreshTokenRequestOutcome {
    NotAttemptedOrSucceeded,
    FailedTransiently,
    FailedPermanently,
}

enum BedrockLoginCredentials {
    ApiKey(String),
    AccessKeys {
        access_key_id: String,
        secret_access_key: String,
        session_token: Option<String>,
    },
}

#[derive(Clone)]
pub(crate) struct AccountRequestProcessor {
    auth_manager: Arc<AuthManager>,
    thread_manager: Arc<ThreadManager>,
    outgoing: Arc<OutgoingMessageSender>,
    config: Arc<Config>,
    config_manager: ConfigManager,
    workspace_routing: Arc<Mutex<Option<workspace_routing::CachedWorkspaceRouting>>>,
    workspace_routing_fetches: Arc<Mutex<workspace_routing::WorkspaceRoutingFetches>>,
    workspace_routing_shutdown: CancellationToken,
    gateway_login: Arc<std::sync::Mutex<Option<gateway_oauth::ActiveGatewayLogin>>>,
    gateway_client: Arc<std::sync::Mutex<Option<Arc<codex_login::GatewayAuthManager>>>>,
    _gateway_notifications: Arc<tokio_util::task::AbortOnDropHandle<()>>,
    pub(super) enterprise_login: Arc<enterprise_login::EnterpriseLoginState>,
}

impl AccountRequestProcessor {
    pub(crate) fn new(
        auth_manager: Arc<AuthManager>,
        thread_manager: Arc<ThreadManager>,
        outgoing: Arc<OutgoingMessageSender>,
        config: Arc<Config>,
        config_manager: ConfigManager,
    ) -> Arc<Self> {
        let gateway_notifications = crate::gateway_oauth_notifications::spawn(
            Arc::clone(&auth_manager),
            config_manager.clone(),
            Arc::clone(&outgoing),
        );
        let enterprise_login = Arc::new(enterprise_login::EnterpriseLoginState::new(
            Arc::clone(&auth_manager),
            Arc::clone(&thread_manager),
            config_manager.clone(),
        ));
        let processor = Arc::new(Self {
            _gateway_notifications: Arc::new(gateway_notifications),
            auth_manager,
            thread_manager,
            outgoing,
            config,
            config_manager,
            gateway_login: Arc::new(std::sync::Mutex::new(/*t*/ None)),
            gateway_client: Arc::new(std::sync::Mutex::new(/*t*/ None)),
            workspace_routing: Arc::new(Mutex::new(None)),
            workspace_routing_fetches: Arc::new(Mutex::new(HashMap::new())),
            workspace_routing_shutdown: CancellationToken::new(),
            enterprise_login,
        });
        let resolver: Arc<dyn codex_login::WorkspaceRoutingResolver> = processor.clone();
        processor
            .auth_manager
            .set_workspace_routing_resolver(Arc::downgrade(&resolver));
        let startup = processor.clone();
        tokio::spawn(async move {
            let _ = startup
                .read_account_for_background_workspace_routing()
                .await;
        });
        processor
    }

    pub(crate) async fn login_account(
        &self,
        request_id: ConnectionRequestId,
        params: LoginAccountParams,
    ) -> Result<Option<ClientResponsePayload>, JSONRPCErrorError> {
        self.login_v2(request_id, params).await.map(|()| None)
    }

    pub(crate) async fn logout_account(
        &self,
        request_id: ConnectionRequestId,
    ) -> Result<Option<ClientResponsePayload>, JSONRPCErrorError> {
        self.logout_v2(request_id).await.map(|()| None)
    }

    pub(crate) async fn cancel_login_account(
        &self,
        params: CancelLoginAccountParams,
    ) -> Result<Option<ClientResponsePayload>, JSONRPCErrorError> {
        self.cancel_login_response(params)
            .await
            .map(|response| Some(response.into()))
    }

    pub(crate) async fn get_auth_status(
        &self,
        params: GetAuthStatusParams,
    ) -> Result<Option<ClientResponsePayload>, JSONRPCErrorError> {
        self.get_auth_status_response(params)
            .await
            .map(|response| Some(response.into()))
    }

    pub(crate) async fn get_account_rate_limits(
        &self,
        params: Option<GetAccountRateLimitsParams>,
    ) -> Result<Option<ClientResponsePayload>, JSONRPCErrorError> {
        self.get_account_rate_limits_response(params.unwrap_or_default())
            .await
            .map(|response| Some(response.into()))
    }

    pub(crate) async fn get_account_token_usage(
        &self,
        params: Option<GetAccountTokenUsageParams>,
    ) -> Result<Option<ClientResponsePayload>, JSONRPCErrorError> {
        self.get_account_token_usage_response(params)
            .await
            .map(|response| Some(response.into()))
    }

    pub(crate) async fn get_workspace_messages(
        &self,
    ) -> Result<Option<ClientResponsePayload>, JSONRPCErrorError> {
        self.get_workspace_messages_response()
            .await
            .map(|response| Some(response.into()))
    }

    pub(crate) async fn send_add_credits_nudge_email(
        &self,
        params: SendAddCreditsNudgeEmailParams,
    ) -> Result<Option<ClientResponsePayload>, JSONRPCErrorError> {
        self.send_add_credits_nudge_email_response(params)
            .await
            .map(|response| Some(response.into()))
    }

    pub(crate) async fn cancel_active_login(&self) {
        self.cancel_gateway_login();
        self.enterprise_login.cancel(/*login_id*/ None).await;
    }

    pub(crate) fn clear_external_auth(&self) {
        self.workspace_routing_shutdown.cancel();
        self.auth_manager.clear_external_auth();
    }

    fn current_account_updated_notification(&self) -> AccountUpdatedNotification {
        let auth = self.auth_manager.auth_cached();
        AccountUpdatedNotification {
            auth_mode: auth
                .as_ref()
                .map(CodexAuth::api_auth_mode)
                .map(auth_mode_to_api),
            plan_type: auth.as_ref().and_then(CodexAuth::account_plan_type),
        }
    }

    async fn load_latest_config(&self) -> Config {
        match self
            .config_manager
            .load_latest_config(/*fallback_cwd*/ None)
            .await
        {
            Ok(config) => config,
            Err(err) => {
                tracing::warn!("failed to reload config, using startup config: {err}");
                self.config.as_ref().clone()
            }
        }
    }

    async fn maybe_refresh_plugin_caches_for_current_config(
        config_manager: &ConfigManager,
        thread_manager: &Arc<ThreadManager>,
        auth: Option<CodexAuth>,
    ) {
        thread_manager
            .plugins_manager()
            .clear_recommended_plugins_cache();

        match config_manager
            .load_latest_config(/*fallback_cwd*/ None)
            .await
        {
            Ok(config) => {
                Self::spawn_effective_plugins_changed_task(
                    Arc::clone(thread_manager),
                    config_manager.clone(),
                );
                let plugins_config = config.plugins_config_input();
                let refresh_thread_manager = Arc::clone(thread_manager);
                let refresh_config_manager = config_manager.clone();
                let on_effective_plugins_changed: Arc<
                    dyn Fn(codex_core_plugins::EffectivePluginsChange) + Send + Sync,
                > = Arc::new(move |_change| {
                    Self::spawn_effective_plugins_changed_task(
                        Arc::clone(&refresh_thread_manager),
                        refresh_config_manager.clone(),
                    );
                });
                thread_manager
                    .plugins_manager()
                    .maybe_start_curated_repo_sync_for_config(
                        &plugins_config,
                        Some(Arc::clone(&on_effective_plugins_changed)),
                    );
                thread_manager
                    .plugins_manager()
                    .maybe_start_remote_plugin_caches_refresh(
                        &plugins_config,
                        auth,
                        Some(on_effective_plugins_changed),
                    );
            }
            Err(err) => {
                warn!(
                    "failed to reload config after account changed, skipping remote installed plugins cache refresh: {err}"
                );
            }
        }
    }

    fn spawn_effective_plugins_changed_task(
        thread_manager: Arc<ThreadManager>,
        config_manager: ConfigManager,
    ) {
        tokio::spawn(async move {
            thread_manager.plugins_manager().clear_cache();
            thread_manager.skills_service().clear_cache();
            crate::mcp_refresh::reload_mcp_config_best_effort(&thread_manager, &config_manager)
                .await;
            thread_manager.invalidate_mcp_runtimes().await;
        });
    }

    async fn login_v2(
        &self,
        request_id: ConnectionRequestId,
        params: LoginAccountParams,
    ) -> Result<(), JSONRPCErrorError> {
        if self.auth_manager.is_workload_identity_selected() {
            return Err(self.configured_auth_owned_by_host_error());
        }
        self.enterprise_login.cancel(/*login_id*/ None).await;
        match params {
            LoginAccountParams::ApiKey { api_key } => {
                self.login_api_key_v2(request_id, LoginApiKeyParams { api_key })
                    .await;
            }
            LoginAccountParams::ChatgptAuthTokens {
                access_token,
                chatgpt_account_id,
                chatgpt_plan_type,
            } => {
                self.login_chatgpt_auth_tokens(
                    request_id,
                    access_token,
                    chatgpt_account_id,
                    chatgpt_plan_type,
                )
                .await;
            }
            LoginAccountParams::AmazonBedrock { api_key, region } => {
                self.login_amazon_bedrock_v2(
                    request_id,
                    BedrockLoginCredentials::ApiKey(api_key),
                    region,
                )
                .await;
            }
            LoginAccountParams::AmazonBedrockAccessKeys {
                access_key_id,
                secret_access_key,
                session_token,
                region,
            } => {
                self.login_amazon_bedrock_v2(
                    request_id,
                    BedrockLoginCredentials::AccessKeys {
                        access_key_id,
                        secret_access_key,
                        session_token,
                    },
                    region,
                )
                .await;
            }
        }
        Ok(())
    }

    fn external_auth_active_error(&self) -> JSONRPCErrorError {
        invalid_request(
            "External auth is active. Use account/login/start (chatgptAuthTokens) to update it or account/logout to clear it.",
        )
    }

    fn configured_auth_owned_by_host_error(&self) -> JSONRPCErrorError {
        invalid_request(
            "Configured external authentication is owned by the app-server host and cannot be changed through account RPCs.",
        )
    }

    fn ensure_bedrock_login_allowed(&self) -> Result<(), JSONRPCErrorError> {
        if self.auth_manager.is_workload_identity_selected() {
            return Err(self.configured_auth_owned_by_host_error());
        }
        if self.auth_manager.is_external_chatgpt_auth_active() {
            return Err(self.external_auth_active_error());
        }
        if !self
            .auth_manager
            .is_login_method_allowed(ForcedLoginMethod::Api)
        {
            return Err(invalid_request(
                "Amazon Bedrock login is disabled by configuration.",
            ));
        }
        Ok(())
    }

    async fn login_api_key_common(
        &self,
        params: &LoginApiKeyParams,
    ) -> std::result::Result<(), JSONRPCErrorError> {
        if self.auth_manager.is_external_chatgpt_auth_active() {
            return Err(self.external_auth_active_error());
        }

        if !self
            .auth_manager
            .is_login_method_allowed(ForcedLoginMethod::Api)
        {
            return Err(invalid_request(
                "API key login is disabled by configuration.",
            ));
        }

        match login_with_api_key(
            &self.config.codex_home,
            &params.api_key,
            self.config.cli_auth_credentials_store_mode,
            self.config.auth_keyring_backend_kind(),
        ) {
            Ok(()) => {
                self.auth_manager.reload().await;
                self.config_manager.clear_cloud_config_bundle_loader();
                Ok(())
            }
            Err(err) => Err(internal_error(format!("failed to save api key: {err}"))),
        }
    }

    async fn login_api_key_v2(&self, request_id: ConnectionRequestId, params: LoginApiKeyParams) {
        let result = self
            .login_api_key_common(&params)
            .await
            .map(|()| LoginAccountResponse::ApiKey {});
        let logged_in = result.is_ok();
        self.outgoing.send_result(request_id, result).await;

        if logged_in {
            self.send_login_success_notifications(/*login_id*/ None)
                .await;
        }
    }

    async fn login_amazon_bedrock_v2(
        &self,
        request_id: ConnectionRequestId,
        credentials: BedrockLoginCredentials,
        region: String,
    ) {
        let result = async {
            self.ensure_bedrock_login_allowed()?;

            match &credentials {
                BedrockLoginCredentials::ApiKey(api_key) => {
                    if api_key.trim().is_empty() {
                        return Err(invalid_request("Amazon Bedrock API key must not be empty."));
                    }
                }
                BedrockLoginCredentials::AccessKeys {
                    access_key_id,
                    secret_access_key,
                    ..
                } => {
                    if access_key_id.trim().is_empty() || secret_access_key.trim().is_empty() {
                        return Err(invalid_request(
                            "AWS access key ID and secret access key must not be empty.",
                        ));
                    }
                }
            }
            let region = region.trim();
            if !is_supported_amazon_bedrock_region(region) {
                return Err(invalid_request(format!(
                    "Amazon Bedrock does not support region `{region}`"
                )));
            }

            self.cancel_active_login().await;
            ensure_user_model_provider_can_be_bedrock(&self.config_manager).await?;
            configure_bedrock_provider(
                &self.config_manager,
                BedrockProviderConfig {
                    region: matches!(&credentials, BedrockLoginCredentials::AccessKeys { .. })
                        .then_some(region),
                    profile: None,
                },
            )
            .await?;

            match credentials {
                BedrockLoginCredentials::ApiKey(api_key) => login_with_bedrock_api_key(
                    &self.config.codex_home,
                    api_key.trim(),
                    region,
                    self.config.cli_auth_credentials_store_mode,
                    self.config.auth_keyring_backend_kind(),
                ),
                BedrockLoginCredentials::AccessKeys {
                    access_key_id,
                    secret_access_key,
                    session_token,
                } => {
                    let session_token = session_token
                        .as_deref()
                        .map(str::trim)
                        .filter(|token| !token.is_empty());
                    login_with_bedrock_access_keys(
                        &self.config.codex_home,
                        access_key_id.trim(),
                        secret_access_key.trim(),
                        session_token,
                        self.config.cli_auth_credentials_store_mode,
                        self.config.auth_keyring_backend_kind(),
                    )
                }
            }
            .map_err(|err| internal_error(format!("failed to save Amazon Bedrock auth: {err}")))?;
            self.auth_manager.reload().await;
            self.config_manager.clear_cloud_config_bundle_loader();
            Ok(LoginAccountResponse::AmazonBedrock {})
        }
        .await;
        let logged_in = result.is_ok();
        self.outgoing.send_result(request_id, result).await;

        if logged_in {
            self.send_login_success_notifications(/*login_id*/ None)
                .await;
        }
    }

    async fn cancel_login_response(
        &self,
        params: CancelLoginAccountParams,
    ) -> Result<CancelLoginAccountResponse, JSONRPCErrorError> {
        let login_id = params.login_id;
        Uuid::parse_str(&login_id)
            .map_err(|_| invalid_request(format!("invalid login id: {login_id}")))?;
        if self.enterprise_login.cancel(Some(&login_id)).await {
            return Ok(CancelLoginAccountResponse {
                status: CancelLoginAccountStatus::Canceled,
            });
        }
        Ok(CancelLoginAccountResponse {
            status: CancelLoginAccountStatus::NotFound,
        })
    }

    async fn login_chatgpt_auth_tokens(
        &self,
        request_id: ConnectionRequestId,
        access_token: String,
        chatgpt_account_id: String,
        chatgpt_plan_type: Option<String>,
    ) {
        let result = self
            .login_chatgpt_auth_tokens_response(access_token, chatgpt_account_id, chatgpt_plan_type)
            .await;
        let logged_in = result.is_ok();
        self.outgoing.send_result(request_id, result).await;

        if logged_in {
            self.send_login_success_notifications(/*login_id*/ None)
                .await;
        }
    }

    async fn login_chatgpt_auth_tokens_response(
        &self,
        access_token: String,
        chatgpt_account_id: String,
        chatgpt_plan_type: Option<String>,
    ) -> Result<LoginAccountResponse, JSONRPCErrorError> {
        if !self
            .auth_manager
            .is_login_method_allowed(ForcedLoginMethod::Chatgpt)
        {
            return Err(invalid_request(
                "External ChatGPT auth is disabled. Use API key login instead.",
            ));
        }

        if let Some(expected_workspaces) = self.auth_manager.effective_chatgpt_workspaces()
            && !expected_workspaces.contains(&chatgpt_account_id)
        {
            return Err(invalid_request(format!(
                "External auth must use one of workspace(s) {expected_workspaces:?}, but received {chatgpt_account_id:?}.",
            )));
        }

        let auth = CodexAuth::from_external_chatgpt_tokens(
            &access_token,
            &chatgpt_account_id,
            chatgpt_plan_type.as_deref(),
        )
        .map_err(|err| internal_error(format!("failed to set external auth: {err}")))?;
        self.auth_manager
            .set_external_auth(Arc::new(ExternalAuthBridge::new(
                Arc::clone(&self.outgoing),
                auth,
            )))
            .await
            .map_err(|err| internal_error(format!("failed to set external auth: {err}")))?;
        self.config_manager.replace_cloud_config_bundle_loader(
            self.auth_manager.clone(),
            self.config.chatgpt_base_url.clone(),
            self.config.http_client_factory(),
        );
        self.config_manager
            .sync_default_client_residency_requirement()
            .await;

        Ok(LoginAccountResponse::ChatgptAuthTokens {})
    }

    async fn send_login_success_notifications(&self, login_id: Option<Uuid>) {
        self.thread_manager.invalidate_mcp_runtimes().await;
        self.send_account_login_notifications(AccountLoginCompletedNotification {
            login_id: login_id.map(|id| id.to_string()),
            success: true,
            error: None,
            onboarding_entrypoint: None,
        })
        .await;
    }

    async fn send_account_login_notifications(
        &self,
        mut payload: AccountLoginCompletedNotification,
    ) {
        let auth_changes = self.auth_manager.auth_change_state_receiver();
        let owner_generation = auth_changes.borrow().owner_generation;
        if payload.success
            && let Err(error) = self.read_account(/*request*/ None).await
        {
            payload.success = false;
            payload.error = Some(error.to_string());
        }
        if payload.success && auth_changes.borrow().owner_generation == owner_generation {
            Self::maybe_refresh_plugin_caches_for_current_config(
                &self.config_manager,
                &self.thread_manager,
                self.auth_manager.auth_cached(),
            )
            .await;
        }

        let success = payload.success;
        self.outgoing
            .send_account_notification(
                /*connection_id*/ None,
                &auth_changes,
                owner_generation,
                AccountNotification::LoginCompleted(payload),
            )
            .await;

        if success {
            let notification = self.current_account_updated_notification();
            self.outgoing
                .send_account_notification(
                    /*connection_id*/ None,
                    &auth_changes,
                    owner_generation,
                    AccountNotification::Updated(notification),
                )
                .await;
        }
    }

    async fn logout_common(&self) -> std::result::Result<Option<AuthMode>, JSONRPCErrorError> {
        if self.auth_manager.is_workload_identity_selected() {
            return Err(self.configured_auth_owned_by_host_error());
        }
        // Another process may have changed the persisted workspace. Reload both
        // account authority and its policy before selecting a grant to remove.
        self.auth_manager.reload().await;
        self.config_manager.replace_cloud_config_bundle_loader(
            Arc::clone(&self.auth_manager),
            self.config.chatgpt_base_url.clone(),
            self.config.http_client_factory(),
        );
        let config = self
            .config_manager
            .load_latest_config(/*fallback_cwd*/ None)
            .await;
        let scope = ema_auth_scope(self.auth_manager.auth_cached().as_ref());
        let enterprise_policy_failed = scope.is_some() && config.is_err();
        // Startup policy may belong to another workspace. Never use that fallback
        // to select a credential; a policy failure must not block primary logout.
        let enterprise_grant = config.as_ref().ok().and_then(|config| {
            scope
                .as_ref()
                .zip(config.mcp_enterprise_managed_auth.as_ref())
                .map(|(scope, profile)| {
                    (
                        profile.idp.credential_name(scope),
                        profile.idp.issuer.clone(),
                        config.auth_keyring_backend_kind(),
                    )
                })
        });
        let config = config.unwrap_or_else(|_| self.config.as_ref().clone());

        self.cancel_active_login().await;

        // Retain the credential lock through primary logout. Otherwise a second
        // process can commit after deletion but before the account is removed.
        let enterprise_guard =
            if let Some((credential_name, issuer, keyring_backend)) = enterprise_grant {
                EnterpriseOAuthCredentialGuard::acquire(&credential_name, &issuer, keyring_backend)
                    .await
                    .map(Some)
            } else {
                Ok(None)
            };
        let cleanup_failed = match &enterprise_guard {
            Ok(Some(guard)) => guard.delete_tokens().is_err(),
            Ok(None) => false,
            Err(_) => true,
        };
        if enterprise_policy_failed || cleanup_failed {
            tracing::warn!("Failed to remove enterprise authorization; continuing account logout");
        }

        match self.auth_manager.logout_with_revoke().await {
            Ok(_) => {}
            Err(err) => {
                return Err(internal_error(format!("logout failed: {err}")));
            }
        }
        drop(enterprise_guard);
        self.thread_manager.invalidate_mcp_runtimes().await;

        self.config_manager.clear_cloud_config_bundle_loader();

        if config.model_provider.is_amazon_bedrock() {
            clear_user_model_provider_if_bedrock(&self.config_manager, &config).await?;
        }

        *self.workspace_routing.lock().await = None;

        Self::maybe_refresh_plugin_caches_for_current_config(
            &self.config_manager,
            &self.thread_manager,
            self.auth_manager.auth_cached(),
        )
        .await;

        // Reflect the current auth method after logout (likely None).
        Ok(self
            .auth_manager
            .auth_cached()
            .as_ref()
            .map(CodexAuth::api_auth_mode)
            .map(auth_mode_to_api))
    }

    async fn logout_v2(&self, request_id: ConnectionRequestId) -> Result<(), JSONRPCErrorError> {
        let result = self.logout_common().await;
        let account_updated =
            result
                .as_ref()
                .ok()
                .cloned()
                .map(|auth_mode| AccountUpdatedNotification {
                    auth_mode,
                    plan_type: None,
                });
        self.outgoing
            .send_result(request_id, result.map(|_| LogoutAccountResponse {}))
            .await;

        if let Some(payload) = account_updated {
            self.outgoing
                .send_server_notification(ServerNotification::AccountUpdated(payload))
                .await;
        }
        Ok(())
    }

    async fn refresh_token_if_requested(&self, do_refresh: bool) -> RefreshTokenRequestOutcome {
        if self.auth_manager.is_external_chatgpt_auth_active() {
            return RefreshTokenRequestOutcome::NotAttemptedOrSucceeded;
        }
        if do_refresh && let Err(err) = self.auth_manager.refresh_token().await {
            let failed_reason = err.failed_reason();
            if failed_reason.is_none() {
                tracing::warn!("failed to refresh token while getting account: {err}");
                return RefreshTokenRequestOutcome::FailedTransiently;
            }
            return RefreshTokenRequestOutcome::FailedPermanently;
        }
        RefreshTokenRequestOutcome::NotAttemptedOrSucceeded
    }

    async fn get_auth_status_response(
        &self,
        params: GetAuthStatusParams,
    ) -> Result<GetAuthStatusResponse, JSONRPCErrorError> {
        let include_token = params.include_token.unwrap_or(false);
        let do_refresh = params.refresh_token.unwrap_or(false);

        self.refresh_token_if_requested(do_refresh).await;

        // Determine whether auth is required based on the active model provider.
        // If a custom provider is configured with `requires_openai_auth == false`,
        // then no auth step is required; otherwise, default to requiring auth.
        let config = self.load_latest_config().await;
        let requires_openai_auth = config.model_provider.requires_openai_auth;

        let response = if !requires_openai_auth {
            GetAuthStatusResponse {
                auth_method: None,
                auth_token: None,
                requires_openai_auth: Some(false),
            }
        } else {
            let auth = if do_refresh {
                self.auth_manager.auth_cached()
            } else {
                self.auth_manager.auth().await
            };
            match auth {
                Some(auth) => {
                    let permanent_refresh_failure =
                        self.auth_manager.refresh_failure_for_auth(&auth).is_some();
                    let auth_mode = auth_mode_to_api(auth.api_auth_mode());
                    let (reported_auth_method, token_opt) =
                        if self.auth_manager.is_workload_identity_selected()
                            || matches!(
                                auth,
                                CodexAuth::Headers(_)
                                    | CodexAuth::AgentIdentity(_)
                                    | CodexAuth::PersonalAccessToken(_)
                            )
                            || include_token && permanent_refresh_failure
                        {
                            // Host-owned and metadata-bearing credentials are never exported.
                            (Some(auth_mode), None)
                        } else {
                            match auth.get_token() {
                                Ok(token) if !token.is_empty() => {
                                    let tok = if include_token { Some(token) } else { None };
                                    (Some(auth_mode), tok)
                                }
                                Ok(_) => (None, None),
                                Err(err) => {
                                    tracing::warn!("failed to get token for auth status: {err}");
                                    (None, None)
                                }
                            }
                        };
                    GetAuthStatusResponse {
                        auth_method: reported_auth_method,
                        auth_token: token_opt,
                        requires_openai_auth: Some(true),
                    }
                }
                None => GetAuthStatusResponse {
                    auth_method: None,
                    auth_token: None,
                    requires_openai_auth: Some(true),
                },
            }
        };

        Ok(response)
    }

    async fn get_account_rate_limits_response(
        &self,
        params: GetAccountRateLimitsParams,
    ) -> Result<GetAccountRateLimitsResponse, JSONRPCErrorError> {
        let Some((auth, http_client_factory)) =
            self.auth_manager.auth_with_http_client_factory().await
        else {
            return Err(invalid_request(
                "codex account authentication required to read rate limits",
            ));
        };

        if !auth.uses_codex_backend() {
            return Err(invalid_request(
                "chatgpt authentication required to read rate limits",
            ));
        }

        let client = BackendClient::from_auth(
            self.config.chatgpt_base_url.clone(),
            &auth,
            http_client_factory,
        );

        let usage_request = async {
            if params.supports_luna_reserve
                && auth.auth_mode() == codex_protocol::auth::AuthMode::Chatgpt
                && !auth.is_fedramp_account()
            {
                client.get_rate_limits_with_luna_reserve().await
            } else {
                client.get_rate_limits_with_reset_credits().await
            }
        };
        let (response, detailed_rate_limit_reset_credits) = tokio::join!(usage_request, async {
            if params.exclude_reset_credit_details {
                None
            } else {
                Self::detailed_rate_limit_reset_credits(&client).await
            }
        },);
        let response = response
            .map_err(|err| internal_error(format!("failed to fetch codex rate limits: {err}")))?;
        if response.rate_limits.is_empty() {
            return Err(internal_error(
                "failed to fetch codex rate limits: no snapshots returned",
            ));
        }

        let rate_limits_by_limit_id: HashMap<_, _> = response
            .rate_limits
            .iter()
            .cloned()
            .map(|snapshot| {
                let limit_id = snapshot
                    .limit_id
                    .clone()
                    .unwrap_or_else(|| "codex".to_string());
                (limit_id, snapshot)
            })
            .collect();
        let rate_limits = response
            .rate_limits
            .iter()
            .find(|snapshot| snapshot.limit_id.as_deref() == Some("codex"))
            .cloned()
            .unwrap_or_else(|| response.rate_limits[0].clone());

        let rate_limit_reset_credits = detailed_rate_limit_reset_credits.or_else(|| {
            response
                .rate_limit_reset_credits
                .map(|summary| RateLimitResetCreditsSummary {
                    available_count: summary.available_count,
                    credits: None,
                })
        });

        // Match desktop's account readiness check before exposing account-bound CTA content.
        // Normal rate limits remain available when older backends omit identity or banner data.
        // Login can change while the backend read is in flight.
        let active_auth = self.auth_manager.auth().await;
        let matches_active_account = active_auth.is_some_and(|auth| {
            !auth.is_fedramp_account()
                && response.account_id.is_some()
                && response.account_id == auth.get_account_id()
                && response.user_id.is_some()
                && response.user_id == auth.get_chatgpt_user_id()
        });
        let rate_limit_upsell = response
            .rate_limit_upsell
            .filter(|_| matches_active_account);

        Ok(GetAccountRateLimitsResponse {
            ordinary_usage_allowed: response
                .ordinary_usage_allowed
                .filter(|_| matches_active_account),
            rate_limits: rate_limits.into(),
            rate_limits_by_limit_id: Some(
                rate_limits_by_limit_id
                    .into_iter()
                    .map(|(limit_id, snapshot)| (limit_id, snapshot.into()))
                    .collect(),
            ),
            rate_limit_reset_credits,
            account_id: response.account_id,
            rate_limit_upsell,
        })
    }

    async fn get_account_token_usage_response(
        &self,
        params: Option<GetAccountTokenUsageParams>,
    ) -> Result<GetAccountTokenUsageResponse, JSONRPCErrorError> {
        let thread_id = params
            .and_then(|params| params.thread_id)
            .map(|thread_id| {
                ThreadId::from_string(&thread_id)
                    .map_err(|err| invalid_request(format!("invalid thread id: {err}")))
            })
            .transpose()?;

        let Some((auth, http_client_factory)) =
            self.auth_manager.auth_with_http_client_factory().await
        else {
            return Err(invalid_request(
                "codex account authentication required to read token usage",
            ));
        };

        if !auth.uses_codex_backend() {
            return Err(invalid_request(
                "chatgpt authentication required to read token usage",
            ));
        }

        let client = BackendClient::from_auth(
            self.config.chatgpt_base_url.clone(),
            &auth,
            http_client_factory,
        );
        if let Some(thread_id) = thread_id {
            let thread_id = thread_id.to_string();
            let usage = tokio::time::timeout(
                THREAD_USAGE_FETCH_TIMEOUT,
                client.get_thread_usage(&thread_id),
            )
            .await
            .map_err(|_| internal_error("thread usage fetch timed out"))?;
            let thread_usage = match usage {
                Ok(usage) => Some(codex_app_server_protocol::ThreadUsage {
                    thread_id: usage.thread_id,
                    estimated_usage_credits_micros: usage.estimated_usage_credits_micros,
                    estimated_usage_usd_micros: usage.estimated_usage_usd_micros,
                    groups: usage
                        .groups
                        .into_iter()
                        .map(
                            |group| codex_app_server_protocol::ThreadUsageBreakdownGroup {
                                model: group.model,
                                reasoning_effort: group.reasoning_effort,
                                speed: group.speed,
                                estimated_usage_credits_micros: group
                                    .estimated_usage_credits_micros,
                                net_new_input_tokens: group.net_new_input_tokens,
                                cached_input_tokens: group.cached_input_tokens,
                                input_tokens: group.input_tokens,
                                output_tokens: group.output_tokens,
                                total_tokens: group.total_tokens,
                            },
                        )
                        .collect(),
                }),
                Err(err)
                    if matches!(err.status().map(|status| status.as_u16()), Some(403 | 404)) =>
                {
                    None
                }
                Err(err) => {
                    return Err(internal_error(format!(
                        "failed to fetch thread usage: {err}"
                    )));
                }
            };
            return Ok(GetAccountTokenUsageResponse {
                summary: AccountTokenUsageSummary {
                    lifetime_tokens: None,
                    peak_daily_tokens: None,
                    longest_running_turn_sec: None,
                    current_streak_days: None,
                    longest_streak_days: None,
                },
                daily_usage_buckets: None,
                thread_usage,
            });
        }
        let profile = tokio::time::timeout(
            ACCOUNT_TOKEN_USAGE_FETCH_TIMEOUT,
            client.get_token_usage_profile(),
        )
        .await
        .map_err(|_| internal_error("token usage profile fetch timed out"))?
        .map_err(|err| internal_error(format!("failed to fetch token usage profile: {err}")))?;
        Ok(Self::account_token_usage_response(profile))
    }

    async fn get_workspace_messages_response(
        &self,
    ) -> Result<GetWorkspaceMessagesResponse, JSONRPCErrorError> {
        let Some((auth, http_client_factory)) =
            self.auth_manager.auth_with_http_client_factory().await
        else {
            return Err(invalid_request(
                "codex account authentication required to read workspace messages",
            ));
        };

        if !auth.uses_codex_backend() {
            return Err(invalid_request(
                "chatgpt authentication required to read workspace messages",
            ));
        }

        let client = BackendClient::from_auth(
            self.config.chatgpt_base_url.clone(),
            &auth,
            http_client_factory,
        );
        let messages = tokio::time::timeout(
            ACCOUNT_WORKSPACE_MESSAGES_FETCH_TIMEOUT,
            client.list_workspace_messages(),
        )
        .await
        .map_err(|_| internal_error("workspace messages fetch timed out"))?;

        match messages {
            Ok(messages) => {
                Self::workspace_messages_response(messages, /*feature_enabled*/ true)
            }
            Err(err) if workspace_messages_feature_disabled(&err) => {
                Self::workspace_messages_response(
                    BackendWorkspaceMessagesResponse {
                        messages: Vec::new(),
                    },
                    /*feature_enabled*/ false,
                )
            }
            Err(err) => Err(internal_error(format!(
                "failed to fetch workspace messages: {err}"
            ))),
        }
    }

    fn account_token_usage_response(profile: TokenUsageProfile) -> GetAccountTokenUsageResponse {
        let stats = profile.stats;
        GetAccountTokenUsageResponse {
            summary: AccountTokenUsageSummary {
                lifetime_tokens: stats.lifetime_tokens,
                peak_daily_tokens: stats.peak_daily_tokens,
                longest_running_turn_sec: stats.longest_running_turn_sec,
                current_streak_days: stats.current_streak_days,
                longest_streak_days: stats.longest_streak_days,
            },
            daily_usage_buckets: stats.daily_usage_buckets.map(|buckets| {
                buckets
                    .into_iter()
                    .map(|bucket| AccountTokenUsageDailyBucket {
                        start_date: bucket.start_date,
                        tokens: bucket.tokens,
                    })
                    .collect()
            }),
            thread_usage: None,
        }
    }

    fn workspace_messages_response(
        messages: BackendWorkspaceMessagesResponse,
        feature_enabled: bool,
    ) -> Result<GetWorkspaceMessagesResponse, JSONRPCErrorError> {
        Ok(GetWorkspaceMessagesResponse {
            feature_enabled,
            messages: messages
                .messages
                .into_iter()
                .map(workspace_message_from_backend)
                .collect::<Result<Vec<_>, _>>()?,
        })
    }

    async fn send_add_credits_nudge_email_response(
        &self,
        params: SendAddCreditsNudgeEmailParams,
    ) -> Result<SendAddCreditsNudgeEmailResponse, JSONRPCErrorError> {
        self.send_add_credits_nudge_email_inner(params)
            .await
            .map(|status| SendAddCreditsNudgeEmailResponse { status })
    }

    async fn send_add_credits_nudge_email_inner(
        &self,
        params: SendAddCreditsNudgeEmailParams,
    ) -> Result<AddCreditsNudgeEmailStatus, JSONRPCErrorError> {
        let Some((auth, http_client_factory)) =
            self.auth_manager.auth_with_http_client_factory().await
        else {
            return Err(invalid_request(
                "codex account authentication required to notify workspace owner",
            ));
        };

        if !auth.uses_codex_backend() {
            return Err(invalid_request(
                "chatgpt authentication required to notify workspace owner",
            ));
        }

        let client = BackendClient::from_auth(
            self.config.chatgpt_base_url.clone(),
            &auth,
            http_client_factory,
        );

        match client
            .send_add_credits_nudge_email(Self::backend_credit_type(params.credit_type))
            .await
        {
            Ok(()) => Ok(AddCreditsNudgeEmailStatus::Sent),
            Err(err) if err.status().is_some_and(|status| status.as_u16() == 429) => {
                Ok(AddCreditsNudgeEmailStatus::CooldownActive)
            }
            Err(err) => Err(internal_error(format!(
                "failed to notify workspace owner: {err}"
            ))),
        }
    }

    fn backend_credit_type(value: AddCreditsNudgeCreditType) -> BackendAddCreditsNudgeCreditType {
        match value {
            AddCreditsNudgeCreditType::Credits => BackendAddCreditsNudgeCreditType::Credits,
            AddCreditsNudgeCreditType::UsageLimit => BackendAddCreditsNudgeCreditType::UsageLimit,
        }
    }
}

fn workspace_message_from_backend(
    message: BackendWorkspaceMessage,
) -> Result<WorkspaceMessage, JSONRPCErrorError> {
    Ok(WorkspaceMessage {
        message_id: message.message_id,
        message_type: workspace_message_type_from_backend(message.message_type),
        message_body: message.message_body,
        created_at: workspace_message_timestamp_from_backend(message.created_at)?,
        archived_at: workspace_message_timestamp_from_backend(message.archived_at)?,
    })
}

fn workspace_message_timestamp_from_backend(
    timestamp: Option<String>,
) -> Result<Option<i64>, JSONRPCErrorError> {
    timestamp
        .map(|timestamp| {
            DateTime::parse_from_rfc3339(&timestamp)
                .map(|timestamp| timestamp.timestamp())
                .map_err(|err| {
                    internal_error(format!(
                        "failed to parse workspace message timestamp `{timestamp}`: {err}"
                    ))
                })
        })
        .transpose()
}

fn workspace_message_type_from_backend(
    message_type: BackendWorkspaceMessageType,
) -> WorkspaceMessageType {
    match message_type {
        BackendWorkspaceMessageType::Headline => WorkspaceMessageType::Headline,
        BackendWorkspaceMessageType::Announcement => WorkspaceMessageType::Announcement,
        BackendWorkspaceMessageType::Unknown => WorkspaceMessageType::Unknown,
    }
}

fn workspace_messages_feature_disabled(err: &BackendRequestError) -> bool {
    err.status().is_some_and(|status| status.as_u16() == 404)
}

#[cfg(test)]
mod tests {
    use super::*;
    use codex_backend_client::TokenUsageProfileDailyBucket;
    use codex_backend_client::TokenUsageProfileStats;
    use http::StatusCode;
    use pretty_assertions::assert_eq;

    #[test]
    fn account_token_usage_response_maps_profile_stats_and_daily_buckets() {
        let response = AccountRequestProcessor::account_token_usage_response(TokenUsageProfile {
            stats: TokenUsageProfileStats {
                lifetime_tokens: Some(123),
                peak_daily_tokens: Some(45),
                longest_running_turn_sec: Some(67),
                current_streak_days: Some(8),
                longest_streak_days: Some(9),
                daily_usage_buckets: Some(vec![TokenUsageProfileDailyBucket {
                    start_date: "2026-05-29".to_string(),
                    tokens: 10,
                }]),
            },
        });

        assert_eq!(
            response,
            GetAccountTokenUsageResponse {
                summary: AccountTokenUsageSummary {
                    lifetime_tokens: Some(123),
                    peak_daily_tokens: Some(45),
                    longest_running_turn_sec: Some(67),
                    current_streak_days: Some(8),
                    longest_streak_days: Some(9),
                },
                daily_usage_buckets: Some(vec![AccountTokenUsageDailyBucket {
                    start_date: "2026-05-29".to_string(),
                    tokens: 10,
                }]),
                thread_usage: None,
            }
        );
    }

    #[test]
    fn workspace_messages_response_maps_backend_messages() {
        let response = AccountRequestProcessor::workspace_messages_response(
            BackendWorkspaceMessagesResponse {
                messages: vec![BackendWorkspaceMessage {
                    message_id: "headline-id".to_string(),
                    message_type: BackendWorkspaceMessageType::Headline,
                    message_body: "Headline body".to_string(),
                    created_at: Some("2026-06-14T00:00:00Z".to_string()),
                    archived_at: Some("2026-06-15T00:00:00Z".to_string()),
                }],
            },
            /*feature_enabled*/ true,
        )
        .expect("workspace message timestamps should parse");

        assert_eq!(
            response,
            GetWorkspaceMessagesResponse {
                feature_enabled: true,
                messages: vec![WorkspaceMessage {
                    message_id: "headline-id".to_string(),
                    message_type: WorkspaceMessageType::Headline,
                    message_body: "Headline body".to_string(),
                    created_at: Some(1_781_395_200),
                    archived_at: Some(1_781_481_600),
                }],
            }
        );
    }

    #[test]
    fn workspace_messages_feature_disabled_only_for_not_found() {
        let cases = [
            (StatusCode::NOT_FOUND, true),
            (StatusCode::UNAUTHORIZED, false),
            (StatusCode::FORBIDDEN, false),
        ];

        for (status, expected) in cases {
            let err = BackendRequestError::UnexpectedStatus {
                method: "GET".to_string(),
                url: "https://example.test/api/codex/workspace-messages".to_string(),
                status,
                content_type: "application/json".to_string(),
                body: "{}".to_string(),
            };
            assert_eq!(workspace_messages_feature_disabled(&err), expected);
        }
    }
}
