use std::collections::HashMap;
use std::env;
use std::ffi::OsString;
use std::fmt::Debug;
use std::future::Future;
use std::path::Path;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::RwLock;
use tokio::sync::Semaphore;
use tokio::sync::watch;
use tracing::instrument;

use codex_protocol::auth::AuthMode;
use codex_protocol::config_types::ForcedLoginMethod;
use codex_protocol::config_types::ModelProviderAuthInfo;

use super::api_key::is_fedramp_api_key;
use super::change_state::AuthChangeState;
use super::change_state::AuthOwner;
use super::external_bearer::BearerTokenRefresher;
pub use crate::auth::bedrock_access_keys::BedrockAccessKeysAuth;
pub use crate::auth::bedrock_api_key::BedrockApiKeyAuth;
pub use crate::auth::storage::AuthDotJson;
pub use crate::auth::storage::AuthKeyringBackendKind;
use crate::auth::storage::AuthStorageBackend;
use crate::auth::storage::auth_storage_commit_guard;
use crate::auth::storage::create_auth_storage;
use crate::outbound_proxy::AuthRouteConfig;
use codex_config::ManagedAuthPolicy;
use codex_config::types::AuthCredentialsStoreMode;
use codex_http_client::HttpClientFactory;
use codex_http_client::OutboundProxyPolicy;
use codex_protocol::auth::RefreshTokenFailedError;
use codex_protocol::auth::RefreshTokenFailedReason;
use thiserror::Error;

/// Authentication mechanism used by the current user.
#[derive(Debug, Clone)]
pub enum CodexAuth {
    ApiKey(ApiKeyAuth),
    BedrockApiKey(BedrockApiKeyAuth),
    BedrockAccessKeys(BedrockAccessKeysAuth),
}

impl PartialEq for CodexAuth {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::BedrockApiKey(a), Self::BedrockApiKey(b)) => a == b,
            (Self::BedrockAccessKeys(a), Self::BedrockAccessKeys(b)) => a == b,
            _ => self.api_auth_mode() == other.api_auth_mode(),
        }
    }
}

#[derive(Clone)]
pub struct ApiKeyAuth {
    pub(super) api_key: String,
}

impl Debug for ApiKeyAuth {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ApiKeyAuth")
            .field("api_key", &"[redacted]")
            .finish_non_exhaustive()
    }
}

const REFRESH_TOKEN_UNKNOWN_MESSAGE: &str =
    "Your access token could not be refreshed. Please log out and sign in again.";
const REFRESH_TOKEN_ACCOUNT_MISMATCH_MESSAGE: &str = "Your access token could not be refreshed because you have since logged out or signed in to another account. Please sign in again.";

#[derive(Debug, Error)]
pub enum RefreshTokenError {
    #[error("{0}")]
    Permanent(#[from] RefreshTokenFailedError),
    #[error(transparent)]
    Transient(#[from] std::io::Error),
    /// Denied by application policy; neither retry nor cache as a credential failure.
    #[error(transparent)]
    Policy(#[from] codex_http_client::NetworkPolicyDenied),
}

/// Error returned when constructing an [`AuthManager`] from resolved configuration.
#[derive(Debug, Error)]
#[error(transparent)]
pub struct AuthManagerInitializationError(AuthManagerInitializationErrorSource);

#[derive(Debug, Error)]
enum AuthManagerInitializationErrorSource {
    #[error(transparent)]
    InitialAuth(RefreshTokenError),
}

impl From<RefreshTokenError> for AuthManagerInitializationError {
    fn from(error: RefreshTokenError) -> Self {
        Self(AuthManagerInitializationErrorSource::InitialAuth(error))
    }
}

impl From<AuthManagerInitializationError> for std::io::Error {
    fn from(error: AuthManagerInitializationError) -> Self {
        Self::other(error)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExternalAuthRefreshReason {
    Unauthorized,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExternalAuthRefreshContext {
    pub reason: ExternalAuthRefreshReason,
}

/// Pluggable auth provider used by `AuthManager` for externally managed auth flows.
///
/// Implementations own the current auth value and any source-specific refresh mechanism.
pub trait ExternalAuth: Send + Sync {
    /// Returns the provider's current auth value.
    fn resolve(&self) -> ExternalAuthFuture<'_, CodexAuth>;

    /// Refreshes auth and makes the returned value current for future `resolve()` calls.
    fn refresh(&self, context: ExternalAuthRefreshContext) -> ExternalAuthFuture<'_, CodexAuth>;

    /// Maps a provider error into the retry policy used by external-auth reload and recovery.
    fn classify_error(&self, error: std::io::Error) -> RefreshTokenError {
        RefreshTokenError::Transient(error)
    }
}

struct FixedApiKeyAuth(CodexAuth);

impl ExternalAuth for FixedApiKeyAuth {
    fn resolve(&self) -> ExternalAuthFuture<'_, CodexAuth> {
        Box::pin(async { Ok(self.0.clone()) })
    }

    fn refresh(&self, _context: ExternalAuthRefreshContext) -> ExternalAuthFuture<'_, CodexAuth> {
        self.resolve()
    }
}

pub type ExternalAuthFuture<'a, T> = Pin<Box<dyn Future<Output = std::io::Result<T>> + Send + 'a>>;

impl RefreshTokenError {
    pub fn failed_reason(&self) -> Option<RefreshTokenFailedReason> {
        match self {
            Self::Permanent(error) => Some(error.reason),
            Self::Transient(_) | Self::Policy(_) => None,
        }
    }
}

impl From<codex_http_client::HttpError> for RefreshTokenError {
    fn from(error: codex_http_client::HttpError) -> Self {
        match error {
            codex_http_client::HttpError::Policy(denied) => Self::Policy(denied),
            error => Self::Transient(std::io::Error::other(error)),
        }
    }
}

impl From<RefreshTokenError> for std::io::Error {
    fn from(err: RefreshTokenError) -> Self {
        match err {
            RefreshTokenError::Permanent(failed) => std::io::Error::other(failed),
            RefreshTokenError::Transient(inner) => inner,
            RefreshTokenError::Policy(error) => {
                std::io::Error::new(std::io::ErrorKind::PermissionDenied, error)
            }
        }
    }
}

impl CodexAuth {
    fn from_auth_dot_json(auth_dot_json: AuthDotJson) -> std::io::Result<Self> {
        let auth_mode = auth_dot_json.resolved_mode();
        if auth_mode == AuthMode::ApiKey {
            let Some(api_key) = auth_dot_json.openai_api_key.as_deref() else {
                return Err(std::io::Error::other("API key auth is missing a key."));
            };
            return Ok(Self::from_api_key(api_key));
        }
        if auth_mode == AuthMode::BedrockApiKey {
            let Some(auth) = auth_dot_json.bedrock_api_key else {
                return Err(std::io::Error::other(
                    "Bedrock API key auth is missing a Bedrock API key.",
                ));
            };
            return Ok(Self::BedrockApiKey(auth));
        }
        if auth_mode == AuthMode::BedrockAccessKeys {
            let Some(auth) = auth_dot_json.bedrock_access_keys else {
                return Err(std::io::Error::other(
                    "Bedrock access keys auth is missing AWS access keys.",
                ));
            };
            return Ok(Self::BedrockAccessKeys(auth));
        }
        Err(std::io::Error::other(
            "externally provided auth cannot be loaded from auth storage.",
        ))
    }

    pub async fn from_auth_storage(
        codex_home: &Path,
        auth_credentials_store_mode: AuthCredentialsStoreMode,
        keyring_backend_kind: AuthKeyringBackendKind,
    ) -> std::io::Result<Option<Self>> {
        load_auth(
            codex_home,
            /*enable_codex_api_key_env*/ false,
            auth_credentials_store_mode,
            /*allowed_login_methods*/ None,
            keyring_backend_kind,
            /*allowed_auth_modes*/ None,
        )
        .await
    }

    /// Returns the effective backend auth mode.
    pub fn auth_mode(&self) -> AuthMode {
        match self {
            Self::ApiKey(_) => AuthMode::ApiKey,
            Self::BedrockApiKey(_) => AuthMode::BedrockApiKey,
            Self::BedrockAccessKeys(_) => AuthMode::BedrockAccessKeys,
        }
    }

    /// Returns the precise kind of credentials backing this authentication.
    pub fn api_auth_mode(&self) -> AuthMode {
        match self {
            Self::ApiKey(_) => AuthMode::ApiKey,
            Self::BedrockApiKey(_) => AuthMode::BedrockApiKey,
            Self::BedrockAccessKeys(_) => AuthMode::BedrockAccessKeys,
        }
    }

    pub fn is_api_key_auth(&self) -> bool {
        self.auth_mode() == AuthMode::ApiKey
    }

    /// Returns `None` if `auth_mode() != AuthMode::ApiKey`.
    pub fn api_key(&self) -> Option<&str> {
        match self {
            Self::ApiKey(auth) => Some(auth.api_key.as_str()),
            Self::BedrockApiKey(_) | Self::BedrockAccessKeys(_) => None,
        }
    }

    /// Returns the token string used for bearer authentication.
    pub fn get_token(&self) -> Result<String, std::io::Error> {
        match self {
            Self::ApiKey(auth) => Ok(auth.api_key.clone()),
            Self::BedrockApiKey(_) | Self::BedrockAccessKeys(_) => Err(std::io::Error::other(
                "Bedrock API key auth does not expose a Codex bearer token",
            )),
        }
    }

    /// Returns whether auth carries a FedRAMP routing hint from an API-key prefix.
    pub fn is_fedramp_account(&self) -> bool {
        match self {
            Self::ApiKey(auth) => is_fedramp_api_key(&auth.api_key),
            _ => false,
        }
    }

    pub fn from_api_key(api_key: &str) -> Self {
        Self::ApiKey(ApiKeyAuth {
            api_key: api_key.to_owned(),
        })
    }
}

pub const OPENAI_API_KEY_ENV_VAR: &str = "OPENAI_API_KEY";
pub const CODEX_API_KEY_ENV_VAR: &str = "STEVE_API_KEY";

pub fn read_openai_api_key_from_env() -> Option<String> {
    env::var(OPENAI_API_KEY_ENV_VAR)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

pub fn read_codex_api_key_from_env() -> Option<String> {
    read_non_empty_env_var(CODEX_API_KEY_ENV_VAR)
}

fn read_non_empty_env_var(key: &str) -> Option<String> {
    env::var(key)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

/// Delete credentials for `codex_home` through the selected storage backend.
/// Returns `Ok(true)` if credentials were removed, `Ok(false)` if none were present.
pub fn logout(
    codex_home: &Path,
    auth_credentials_store_mode: AuthCredentialsStoreMode,
    keyring_backend_kind: AuthKeyringBackendKind,
) -> std::io::Result<bool> {
    if auth_credentials_store_mode == AuthCredentialsStoreMode::File && !codex_home.try_exists()? {
        return Ok(false);
    }
    let storage = create_auth_storage(
        codex_home.to_path_buf(),
        auth_credentials_store_mode,
        keyring_backend_kind,
    );
    let _commit = storage.lock_for_write()?;
    storage.delete()
}

/// Store API-key credentials through the selected storage backend.
pub fn login_with_api_key(
    codex_home: &Path,
    api_key: &str,
    auth_credentials_store_mode: AuthCredentialsStoreMode,
    keyring_backend_kind: AuthKeyringBackendKind,
) -> std::io::Result<()> {
    let auth_dot_json = AuthDotJson {
        auth_mode: Some(AuthMode::ApiKey),
        openai_api_key: Some(api_key.to_string()),
        bedrock_api_key: None,
        bedrock_access_keys: None,
    };
    save_auth(
        codex_home,
        &auth_dot_json,
        auth_credentials_store_mode,
        keyring_backend_kind,
    )
}

/// Persist the provided auth payload using the specified backend.
pub fn save_auth(
    codex_home: &Path,
    auth: &AuthDotJson,
    auth_credentials_store_mode: AuthCredentialsStoreMode,
    keyring_backend_kind: AuthKeyringBackendKind,
) -> std::io::Result<()> {
    let storage = create_auth_storage(
        codex_home.to_path_buf(),
        auth_credentials_store_mode,
        keyring_backend_kind,
    );
    let _commit = storage.lock_for_write()?;
    storage.save(auth)
}

/// Load the raw stored auth payload without applying environment overrides.
///
/// Returns `None` when no credentials are stored. Prefer `AuthManager` for
/// ordinary production reads; this helper is for tests and write-side
/// maintenance that must inspect the exact payload in storage.
pub fn load_auth_dot_json(
    codex_home: &Path,
    auth_credentials_store_mode: AuthCredentialsStoreMode,
    keyring_backend_kind: AuthKeyringBackendKind,
) -> std::io::Result<Option<AuthDotJson>> {
    let _commit = auth_storage_commit_guard()?;
    let storage = create_auth_storage(
        codex_home.to_path_buf(),
        auth_credentials_store_mode,
        keyring_backend_kind,
    );
    storage.load()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthConfig {
    pub codex_home: PathBuf,
    pub auth_credentials_store_mode: AuthCredentialsStoreMode,
    pub keyring_backend_kind: AuthKeyringBackendKind,
    pub forced_login_method: Option<ForcedLoginMethod>,
    pub chatgpt_base_url: Option<String>,
    pub forced_chatgpt_workspace_id: Option<Vec<String>>,
    pub managed_auth_policy: ManagedAuthPolicy,
    pub auth_route_config: AuthRouteConfig,
}

impl AuthConfig {
    pub fn is_login_method_allowed(&self, method: ForcedLoginMethod) -> bool {
        self.managed_auth_policy.allows_login_method(
            method,
            self.forced_login_method,
            self.forced_chatgpt_workspace_id.as_deref(),
        )
    }

    pub fn effective_chatgpt_workspaces(&self) -> Option<Vec<String>> {
        self.managed_auth_policy
            .effective_chatgpt_workspaces(self.forced_chatgpt_workspace_id.as_deref())
    }

    pub fn validate(&self) -> std::io::Result<()> {
        if self.is_login_method_allowed(ForcedLoginMethod::Api)
            || self.is_login_method_allowed(ForcedLoginMethod::Chatgpt)
        {
            Ok(())
        } else {
            Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "authentication requirements do not permit any usable login method",
            ))
        }
    }

    pub fn allows_auth(&self, _auth: &CodexAuth) -> bool {
        validate_auth_restrictions(Some(&self.allowed_login_methods())).is_ok()
    }

    pub async fn load_auth(
        &self,
        enable_codex_api_key_env: bool,
    ) -> std::io::Result<Option<CodexAuth>> {
        self.load_auth_with_modes(enable_codex_api_key_env, /*allowed_auth_modes*/ None)
            .await
    }

    async fn load_auth_with_modes(
        &self,
        enable_codex_api_key_env: bool,
        allowed_auth_modes: Option<&[AuthMode]>,
    ) -> std::io::Result<Option<CodexAuth>> {
        let allowed_login_methods = self.allowed_login_methods();
        let auth = load_auth(
            &self.codex_home,
            enable_codex_api_key_env,
            self.auth_credentials_store_mode,
            Some(&allowed_login_methods),
            self.keyring_backend_kind,
            allowed_auth_modes,
        )
        .await?;
        Ok(auth.filter(|auth| self.allows_auth(auth)))
    }

    fn allowed_login_methods(&self) -> Vec<ForcedLoginMethod> {
        self.managed_auth_policy.allowed_login_methods(
            self.forced_login_method,
            self.forced_chatgpt_workspace_id.as_deref(),
        )
    }
}

fn api_auth_is_allowed(allowed_login_methods: Option<&[ForcedLoginMethod]>) -> bool {
    allowed_login_methods.is_none_or(|allowed| allowed.contains(&ForcedLoginMethod::Api))
}

fn validate_auth_restrictions(
    allowed_login_methods: Option<&[ForcedLoginMethod]>,
) -> Result<(), String> {
    if !api_auth_is_allowed(allowed_login_methods) {
        return Err(match allowed_login_methods {
            Some(methods) if methods.contains(&ForcedLoginMethod::Api) => {
                "API key login is required".to_string()
            }
            Some(_) => "ChatGPT login is required".to_string(),
            None => unreachable!("unrestricted login methods accept every auth mode"),
        });
    }

    Ok(())
}

/// Enforces configured login restrictions using auth-owned HTTP settings.
pub async fn enforce_login_restrictions(config: &AuthConfig) -> std::io::Result<()> {
    // Managed-only restrictions are enforced by AuthManager.
    if config.forced_login_method.is_none() && config.forced_chatgpt_workspace_id.is_none() {
        return Ok(());
    }

    let Some(auth) = load_auth(
        &config.codex_home,
        /*enable_codex_api_key_env*/ true,
        config.auth_credentials_store_mode,
        /*allowed_login_methods*/ None,
        config.keyring_backend_kind,
        /*allowed_auth_modes*/ None,
    )
    .await?
    else {
        return Ok(());
    };

    if let Some(required_method) = config.forced_login_method {
        let method_violation = match (required_method, auth.auth_mode()) {
            (ForcedLoginMethod::Api, AuthMode::ApiKey)
            | (ForcedLoginMethod::Api, AuthMode::BedrockApiKey)
            | (ForcedLoginMethod::Api, AuthMode::BedrockAccessKeys) => None,
            (ForcedLoginMethod::Chatgpt, AuthMode::ApiKey)
            | (ForcedLoginMethod::Chatgpt, AuthMode::BedrockApiKey)
            | (ForcedLoginMethod::Chatgpt, AuthMode::BedrockAccessKeys) => Some(
                "ChatGPT login is required, but an API key is currently being used. Logging out."
                    .to_string(),
            ),
        };

        if let Some(message) = method_violation {
            return logout_with_message(
                &config.codex_home,
                message,
                config.auth_credentials_store_mode,
                config.keyring_backend_kind,
            );
        }
    }

    Ok(())
}

fn logout_with_message(
    codex_home: &Path,
    message: String,
    auth_credentials_store_mode: AuthCredentialsStoreMode,
    keyring_backend_kind: AuthKeyringBackendKind,
) -> std::io::Result<()> {
    // External auth tokens live in the ephemeral store, but persistent auth may still exist
    // from earlier logins. Clear both so a forced logout truly removes all active auth.
    let removal_result = logout_all_stores(
        codex_home,
        auth_credentials_store_mode,
        keyring_backend_kind,
    );
    let error_message = match removal_result {
        Ok(_) => message,
        Err(err) => format!("{message}. Failed to remove stored credentials: {err}"),
    };
    Err(std::io::Error::other(error_message))
}

/// Delete credentials from the selected store and the process-local ephemeral store.
pub fn logout_all_stores(
    codex_home: &Path,
    auth_credentials_store_mode: AuthCredentialsStoreMode,
    keyring_backend_kind: AuthKeyringBackendKind,
) -> std::io::Result<bool> {
    if auth_credentials_store_mode == AuthCredentialsStoreMode::Ephemeral {
        return logout(
            codex_home,
            AuthCredentialsStoreMode::Ephemeral,
            AuthKeyringBackendKind::default(),
        );
    }
    let removed_ephemeral = logout(
        codex_home,
        AuthCredentialsStoreMode::Ephemeral,
        AuthKeyringBackendKind::default(),
    )?;
    let removed_managed = logout(
        codex_home,
        auth_credentials_store_mode,
        keyring_backend_kind,
    )?;
    Ok(removed_ephemeral || removed_managed)
}

async fn load_auth(
    codex_home: &Path,
    enable_codex_api_key_env: bool,
    auth_credentials_store_mode: AuthCredentialsStoreMode,
    allowed_login_methods: Option<&[ForcedLoginMethod]>,
    keyring_backend_kind: AuthKeyringBackendKind,
    allowed_auth_modes: Option<&[AuthMode]>,
) -> std::io::Result<Option<CodexAuth>> {
    let mode_allowed = |mode| allowed_auth_modes.is_none_or(|modes| modes.contains(&mode));
    // API key via env var takes precedence over any other auth method.
    if enable_codex_api_key_env
        && mode_allowed(AuthMode::ApiKey)
        && api_auth_is_allowed(allowed_login_methods)
        && let Some(api_key) = read_codex_api_key_from_env()
    {
        return Ok(Some(CodexAuth::from_api_key(api_key.as_str())));
    }

    let ephemeral_storage = create_auth_storage(
        codex_home.to_path_buf(),
        AuthCredentialsStoreMode::Ephemeral,
        AuthKeyringBackendKind::default(),
    );
    if let Some(auth_dot_json) = load_stored_auth(ephemeral_storage).await?
        && api_auth_is_allowed(allowed_login_methods)
    {
        if !mode_allowed(auth_dot_json.resolved_mode()) {
            return Ok(None);
        }
        return CodexAuth::from_auth_dot_json(auth_dot_json).map(Some);
    }

    // If the caller explicitly requested ephemeral auth, there is no persisted fallback.
    if auth_credentials_store_mode == AuthCredentialsStoreMode::Ephemeral {
        return Ok(None);
    }

    // Fall back to the configured persistent store (file/keyring/auto) for managed auth.
    let storage = create_auth_storage(
        codex_home.to_path_buf(),
        auth_credentials_store_mode,
        keyring_backend_kind,
    );
    let auth_dot_json = match load_stored_auth(storage).await? {
        Some(auth) => auth,
        None => return Ok(None),
    };
    if !mode_allowed(auth_dot_json.resolved_mode()) || !api_auth_is_allowed(allowed_login_methods) {
        return Ok(None);
    }
    CodexAuth::from_auth_dot_json(auth_dot_json).map(Some)
}

// Blocking keyring access must not prevent an async caller's deadline from firing.
async fn load_stored_auth(
    storage: Arc<dyn AuthStorageBackend>,
) -> std::io::Result<Option<AuthDotJson>> {
    let originator = codex_otel::auth_storage::AuthStorageOriginator::current();
    tokio::task::spawn_blocking(move || originator.sync_scope(|| storage.load()))
        .await
        .map_err(|_| std::io::Error::other("credential storage read failed"))?
}

impl AuthDotJson {
    pub(super) fn resolved_mode(&self) -> AuthMode {
        if let Some(mode) = self.auth_mode {
            return mode;
        }
        if self.bedrock_api_key.is_some() {
            return AuthMode::BedrockApiKey;
        }
        if self.bedrock_access_keys.is_some() {
            return AuthMode::BedrockAccessKeys;
        }
        AuthMode::ApiKey
    }
}

struct AuthReload {
    changed: bool,
}

/// Internal cached auth state.
#[derive(Clone)]
struct CachedAuth {
    auth: Option<CodexAuth>,
    /// Permanent refresh failure cached for the current auth snapshot so
    /// later refresh attempts for the same credentials fail fast without network.
    permanent_refresh_failure: Option<AuthScopedRefreshFailure>,
}

#[derive(Clone)]
struct AuthScopedRefreshFailure {
    auth: CodexAuth,
    error: RefreshTokenFailedError,
}

impl Debug for CachedAuth {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CachedAuth")
            .field(
                "auth_mode",
                &self.auth.as_ref().map(CodexAuth::api_auth_mode),
            )
            .field(
                "permanent_refresh_failure",
                &self
                    .permanent_refresh_failure
                    .as_ref()
                    .map(|failure| failure.error.reason),
            )
            .finish()
    }
}

enum UnauthorizedRecoveryStep {
    ExternalRefresh,
    Done,
}

// UnauthorizedRecovery is a state machine that handles an attempt to refresh the authentication when requests
// to API fail with 401 status code.
// The client calls next() every time it encounters a 401 error, one time per retry.
// For API key based authentication, we don't do anything and let the error bubble to the user.
//
// For ChatGPT based authentication, we:
// 1. Attempt to reload the auth data from disk. We only reload if the account id matches the one the current process is running as.
// If the server still responds with 401 after reloading we let the error bubble to the user.
//
// For external auth sources, UnauthorizedRecovery retries once by asking the
// configured provider to refresh and caching the returned auth through the same
// path used by other auth sources.
pub struct UnauthorizedRecovery {
    manager: Arc<AuthManager>,
    step: UnauthorizedRecoveryStep,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UnauthorizedRecoveryStepResult {
    auth_state_changed: Option<bool>,
}

impl UnauthorizedRecoveryStepResult {
    pub fn auth_state_changed(&self) -> Option<bool> {
        self.auth_state_changed
    }
}

impl UnauthorizedRecovery {
    fn new(manager: Arc<AuthManager>) -> Self {
        Self {
            manager,
            step: UnauthorizedRecoveryStep::ExternalRefresh,
        }
    }

    pub fn has_next(&self) -> bool {
        self.manager.has_refreshable_external_auth()
            && !matches!(self.step, UnauthorizedRecoveryStep::Done)
    }

    pub fn unavailable_reason(&self) -> &'static str {
        if !self.manager.has_refreshable_external_auth() {
            return "no_external_auth";
        }
        if matches!(self.step, UnauthorizedRecoveryStep::Done) {
            return "recovery_exhausted";
        }
        "ready"
    }

    pub fn mode_name(&self) -> &'static str {
        "external"
    }

    pub fn step_name(&self) -> &'static str {
        match self.step {
            UnauthorizedRecoveryStep::ExternalRefresh => "external_refresh",
            UnauthorizedRecoveryStep::Done => "done",
        }
    }

    pub async fn next(&mut self) -> Result<UnauthorizedRecoveryStepResult, RefreshTokenError> {
        if !self.has_next() {
            return Err(RefreshTokenError::Permanent(RefreshTokenFailedError::new(
                RefreshTokenFailedReason::Other,
                "No more recovery steps available.",
            )));
        }

        match self.step {
            UnauthorizedRecoveryStep::ExternalRefresh => {
                self.manager.refresh_token_from_authority().await?;
                self.step = UnauthorizedRecoveryStep::Done;
                return Ok(UnauthorizedRecoveryStepResult {
                    auth_state_changed: Some(true),
                });
            }
            UnauthorizedRecoveryStep::Done => {}
        }
        Ok(UnauthorizedRecoveryStepResult {
            auth_state_changed: None,
        })
    }
}

/// Central manager providing a single source of truth for stored
/// authentication data. It loads once (or on preference change) and then
/// hands out cloned `CodexAuth` values so the rest of the program has a
/// consistent snapshot.
///
/// External modifications to stored credentials will NOT be observed until
/// `reload()` is called explicitly. This matches the design goal of avoiding
/// different parts of the program seeing inconsistent auth data mid‑run.
pub struct AuthManager {
    codex_home: PathBuf,
    inner: RwLock<CachedAuth>,
    auth_change_tx: watch::Sender<u64>,
    auth_change_state_tx: watch::Sender<AuthChangeState>,
    enable_codex_api_key_env: bool,
    auth_credentials_store_mode: AuthCredentialsStoreMode,
    keyring_backend_kind: AuthKeyringBackendKind,
    forced_login_method: Option<ForcedLoginMethod>,
    forced_chatgpt_workspace_id: RwLock<Option<Vec<String>>>,
    managed_auth_policy: ManagedAuthPolicy,
    chatgpt_base_url: Option<String>,
    refresh_lock: Arc<Semaphore>,
    external_auth: RwLock<Option<Arc<dyn ExternalAuth>>>,
    auth_route_config: AuthRouteConfig,
    // Provider credentials follow the primary account's policy without owning its lifecycle.
    owns_network_policy: bool,
    allowed_auth_modes: RwLock<Option<Vec<AuthMode>>>,
}

/// Configuration view required to construct a shared [`AuthManager`].
///
/// Implementations should return the auth-related config values for the
/// already-resolved runtime configuration. The primary implementation is
/// `codex_core::config::Config`, but this trait keeps `codex-login` independent
/// from `codex-core`.
pub trait AuthManagerConfig {
    /// Returns the Codex home directory used for auth storage.
    fn codex_home(&self) -> PathBuf;

    /// Returns the CLI auth credential storage mode for auth loading.
    fn cli_auth_credentials_store_mode(&self) -> AuthCredentialsStoreMode;

    /// Returns the backend to use when CLI auth keyring storage is selected.
    fn auth_keyring_backend_kind(&self) -> AuthKeyringBackendKind;

    /// Returns the resolved login-method restriction, if any.
    fn forced_login_method(&self) -> Option<ForcedLoginMethod>;

    /// Returns the workspace IDs that ChatGPT auth should be restricted to, if any.
    fn forced_chatgpt_workspace_id(&self) -> Option<Vec<String>>;

    /// Returns administrator-managed authentication restrictions.
    fn managed_auth_policy(&self) -> ManagedAuthPolicy;

    /// Returns the ChatGPT backend base URL used for first-party backend authorization.
    fn chatgpt_base_url(&self) -> String;

    /// Returns the account-backend path dialect for [`Self::chatgpt_base_url`].
    fn account_path_style(&self) -> codex_config::AccountPathStyle;

    /// Returns route-selection settings for auth-owned clients.
    fn auth_route_config(&self) -> AuthRouteConfig;
}

/// Runtime storage and network policy shared by independent credential managers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AuthRuntimeConfig {
    pub codex_home: PathBuf,
    pub auth_route_config: AuthRouteConfig,
}

impl Debug for AuthManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AuthManager")
            .field("codex_home", &self.codex_home)
            .field("inner", &self.inner)
            .field("enable_codex_api_key_env", &self.enable_codex_api_key_env)
            .field(
                "auth_credentials_store_mode",
                &self.auth_credentials_store_mode,
            )
            .field("keyring_backend_kind", &self.keyring_backend_kind)
            .field("forced_login_method", &self.forced_login_method)
            .field(
                "forced_chatgpt_workspace_id",
                &self.forced_chatgpt_workspace_id,
            )
            .field("managed_auth_policy", &self.managed_auth_policy)
            .field("chatgpt_base_url", &self.chatgpt_base_url)
            .field("auth_route_config", &self.auth_route_config)
            .field("has_external_auth", &self.has_external_auth())
            .finish_non_exhaustive()
    }
}

impl AuthManager {
    /// Returns the application policy associated with this account owner.
    pub fn application_network_policy(&self) -> codex_http_client::NetworkPolicy {
        self.auth_route_config.application_network_policy().clone()
    }

    /// Creates content clients bound to the account currently owned by this manager.
    pub fn http_client_factory(&self) -> HttpClientFactory {
        self.auth_route_config
            .http_client_factory()
            .clone()
            .with_network_policy(self.application_network_policy().for_current_account())
    }
    /// Create a new manager loading the initial auth using the provided
    /// preferred auth method. Errors loading auth are swallowed; `auth()` will
    /// simply return `None` in that case so callers can treat it as an
    /// unauthenticated state.
    pub async fn new(
        codex_home: PathBuf,
        enable_codex_api_key_env: bool,
        auth_credentials_store_mode: AuthCredentialsStoreMode,
        forced_chatgpt_workspace_id: Option<Vec<String>>,
        chatgpt_base_url: Option<String>,
        keyring_backend_kind: AuthKeyringBackendKind,
        auth_route_config: AuthRouteConfig,
    ) -> Self {
        Self::new_from_auth_config(
            AuthConfig {
                codex_home,
                auth_credentials_store_mode,
                keyring_backend_kind,
                forced_login_method: None,
                chatgpt_base_url,
                forced_chatgpt_workspace_id,
                managed_auth_policy: ManagedAuthPolicy::default(),
                auth_route_config,
            },
            enable_codex_api_key_env,
            /*allowed_auth_modes*/ None,
        )
        .await
    }

    async fn new_from_auth_config(
        auth_config: AuthConfig,
        enable_codex_api_key_env: bool,
        allowed_auth_modes: Option<Vec<AuthMode>>,
    ) -> Self {
        let managed_auth = auth_config
            .load_auth_with_modes(enable_codex_api_key_env, allowed_auth_modes.as_deref())
            .await
            .ok()
            .flatten();
        let AuthConfig {
            codex_home,
            auth_credentials_store_mode,
            keyring_backend_kind,
            forced_login_method,
            chatgpt_base_url,
            forced_chatgpt_workspace_id,
            managed_auth_policy,
            auth_route_config,
        } = auth_config;
        let (auth_change_tx, _auth_change_rx) = watch::channel(0);
        Self {
            codex_home,
            inner: RwLock::new(CachedAuth {
                auth: managed_auth,
                permanent_refresh_failure: None,
            }),
            auth_change_tx,
            auth_change_state_tx: watch::channel(AuthChangeState::default()).0,
            enable_codex_api_key_env,
            auth_credentials_store_mode,
            keyring_backend_kind,
            forced_login_method,
            forced_chatgpt_workspace_id: RwLock::new(forced_chatgpt_workspace_id),
            managed_auth_policy,
            chatgpt_base_url,
            refresh_lock: Arc::new(Semaphore::new(/*permits*/ 1)),
            external_auth: RwLock::new(None),
            auth_route_config,
            owns_network_policy: true,
            allowed_auth_modes: RwLock::new(allowed_auth_modes),
        }
    }

    /// Create an AuthManager with a specific CodexAuth, for testing only.
    pub fn from_auth_for_testing(auth: CodexAuth) -> Arc<Self> {
        Self::from_optional_auth_for_testing(Some(auth))
    }

    /// Create an AuthManager with an optional CodexAuth, for testing only.
    pub(crate) fn from_optional_auth_for_testing(auth: Option<CodexAuth>) -> Arc<Self> {
        Self::from_cached_auth(
            auth,
            /*external_auth*/ None,
            AuthRuntimeConfig {
                codex_home: PathBuf::from("non-existent"),
                auth_route_config: crate::test_support::transport_default_auth_route_config(),
            },
            /*owns_network_policy*/ true,
        )
    }

    /// An explicitly selected API key with no ambient auth-storage or refresh fallback.
    pub fn from_api_key(api_key: &str, runtime: AuthRuntimeConfig) -> Arc<Self> {
        let auth = CodexAuth::from_api_key(api_key);
        Self::from_cached_auth(
            Some(auth.clone()),
            Some(Arc::new(FixedApiKeyAuth(auth))),
            runtime,
            /*owns_network_policy*/ false,
        )
    }

    /// Give a command-backed credential source the caller's storage and network context.
    pub fn external_bearer_with_runtime(
        config: ModelProviderAuthInfo,
        runtime: AuthRuntimeConfig,
    ) -> Arc<Self> {
        Self::from_cached_auth(
            /*auth*/ None,
            Some(Arc::new(BearerTokenRefresher::new(config, HashMap::new()))),
            runtime,
            /*owns_network_policy*/ false,
        )
    }

    fn from_cached_auth(
        auth: Option<CodexAuth>,
        external_auth: Option<Arc<dyn ExternalAuth>>,
        runtime: AuthRuntimeConfig,
        owns_network_policy: bool,
    ) -> Arc<Self> {
        let cached = CachedAuth {
            auth,
            permanent_refresh_failure: None,
        };
        let (auth_change_tx, _auth_change_rx) = watch::channel(0);

        Arc::new(Self {
            codex_home: runtime.codex_home,
            inner: RwLock::new(cached),
            auth_change_tx,
            auth_change_state_tx: watch::channel(AuthChangeState::default()).0,
            enable_codex_api_key_env: false,
            auth_credentials_store_mode: if external_auth.is_some() {
                AuthCredentialsStoreMode::Ephemeral
            } else {
                AuthCredentialsStoreMode::File
            },
            keyring_backend_kind: AuthKeyringBackendKind::default(),
            forced_login_method: None,
            forced_chatgpt_workspace_id: RwLock::new(None),
            managed_auth_policy: ManagedAuthPolicy::default(),
            chatgpt_base_url: None,
            refresh_lock: Arc::new(Semaphore::new(/*permits*/ 1)),
            external_auth: RwLock::new(external_auth),
            allowed_auth_modes: RwLock::new(None),
            auth_route_config: runtime.auth_route_config,
            owns_network_policy,
        })
    }

    /// Create an AuthManager with a specific CodexAuth and codex home, for testing only.
    pub fn from_auth_for_testing_with_home(auth: CodexAuth, codex_home: PathBuf) -> Arc<Self> {
        let cached = CachedAuth {
            auth: Some(auth),
            permanent_refresh_failure: None,
        };
        let (auth_change_tx, _auth_change_rx) = watch::channel(0);
        Arc::new(Self {
            codex_home,
            inner: RwLock::new(cached),
            auth_change_tx,
            auth_change_state_tx: watch::channel(AuthChangeState::default()).0,
            enable_codex_api_key_env: false,
            auth_credentials_store_mode: AuthCredentialsStoreMode::File,
            keyring_backend_kind: AuthKeyringBackendKind::default(),
            forced_login_method: None,
            forced_chatgpt_workspace_id: RwLock::new(None),
            managed_auth_policy: ManagedAuthPolicy::default(),
            chatgpt_base_url: None,
            refresh_lock: Arc::new(Semaphore::new(/*permits*/ 1)),
            external_auth: RwLock::new(None),
            allowed_auth_modes: RwLock::new(None),
            auth_route_config: crate::test_support::transport_default_auth_route_config(),
            owns_network_policy: true,
        })
    }

    pub fn external_bearer_only(config: ModelProviderAuthInfo) -> Arc<Self> {
        Self::external_bearer_only_with_env(config, HashMap::new())
    }

    /// Creates a provider-scoped token cache whose command environment overrides are
    /// retained for both initial authentication and every subsequent refresh.
    pub fn external_bearer_only_with_env(
        config: ModelProviderAuthInfo,
        env: HashMap<String, OsString>,
    ) -> Arc<Self> {
        let (auth_change_tx, _auth_change_rx) = watch::channel(0);
        Arc::new(Self {
            codex_home: PathBuf::from("non-existent"),
            inner: RwLock::new(CachedAuth {
                auth: None,
                permanent_refresh_failure: None,
            }),
            auth_change_tx,
            auth_change_state_tx: watch::channel(AuthChangeState::default()).0,
            enable_codex_api_key_env: false,
            auth_credentials_store_mode: AuthCredentialsStoreMode::File,
            keyring_backend_kind: AuthKeyringBackendKind::default(),
            forced_login_method: None,
            forced_chatgpt_workspace_id: RwLock::new(None),
            managed_auth_policy: ManagedAuthPolicy::default(),
            chatgpt_base_url: None,
            refresh_lock: Arc::new(Semaphore::new(/*permits*/ 1)),
            external_auth: RwLock::new(Some(Arc::new(BearerTokenRefresher::new(config, env)))),
            allowed_auth_modes: RwLock::new(None),
            owns_network_policy: false,
            // External bearer auth refreshes by running the provider's command and never makes
            // auth-owned HTTP requests, so this route is intentionally inert.
            auth_route_config: AuthRouteConfig::from_http_client_factory(HttpClientFactory::new(
                OutboundProxyPolicy::ReqwestDefault,
            )),
        })
    }

    /// Current cached auth (clone) without attempting a refresh.
    pub fn auth_cached(&self) -> Option<CodexAuth> {
        self.inner
            .read()
            .ok()
            .and_then(|cached| cached.auth.clone())
    }

    /// Current revisions if the selected credential still belongs to this manager's source.
    pub fn auth_change_state_for(&self, expected: &CodexAuth) -> Option<AuthChangeState> {
        let cached = self.inner.read().ok()?;
        if !super::source::same_source(cached.auth.as_ref(), Some(expected)) {
            return None;
        }
        Some(*self.auth_change_state_tx.borrow())
    }

    /// Identify this credential source's current account owner without refreshing auth.
    pub fn auth_owner(self: &Arc<Self>) -> AuthOwner {
        AuthOwner::new(
            Arc::clone(self),
            self.auth_change_state_tx.borrow().owner_generation,
        )
    }

    /// Retain the owner of a selected credential, rejecting a changed source.
    pub fn auth_owner_for(self: &Arc<Self>, expected: &CodexAuth) -> Option<AuthOwner> {
        let state = self.auth_change_state_for(expected)?;
        Some(AuthOwner::new(Arc::clone(self), state.owner_generation))
    }

    /// Subscribes to cached auth changes that can affect request recovery.
    pub fn auth_change_receiver(&self) -> watch::Receiver<u64> {
        self.auth_change_tx.subscribe()
    }

    /// Subscribes to credential and owner revisions published together, including when changes coalesce.
    pub fn auth_change_state_receiver(&self) -> watch::Receiver<AuthChangeState> {
        self.auth_change_state_tx.subscribe()
    }

    pub fn refresh_failure_for_auth(&self, auth: &CodexAuth) -> Option<RefreshTokenFailedError> {
        self.inner.read().ok().and_then(|cached| {
            cached
                .permanent_refresh_failure
                .as_ref()
                .filter(|failure| Self::auths_equal_for_refresh(Some(auth), Some(&failure.auth)))
                .map(|failure| failure.error.clone())
        })
    }

    /// Current cached auth (clone). May be `None` if not logged in or load failed.
    #[instrument(level = "trace", skip_all)]
    pub async fn auth(&self) -> Option<CodexAuth> {
        if self.has_external_auth() {
            self.reload_inner().await.ok()?;
            return self.auth_cached();
        }

        self.auth_cached()
    }

    /// Refreshes auth, then captures credentials and their account-bound factory together.
    /// The auth read lock prevents an identity change between the two snapshots.
    pub async fn auth_with_http_client_factory(&self) -> Option<(CodexAuth, HttpClientFactory)> {
        self.auth().await?;
        let cached = self.inner.read().ok()?;
        Some((cached.auth.clone()?, self.http_client_factory()))
    }

    /// Reloads auth from the active source. Returns whether the auth value changed.
    pub async fn reload(&self) -> bool {
        self.reload_inner().await.unwrap_or(false)
    }

    async fn reload_inner(&self) -> Result<bool, RefreshTokenError> {
        tracing::info!("Reloading auth");
        loop {
            let generation = self.auth_change_state_tx.borrow().generation;
            let new_auth = self.load_auth().await?;
            if let Some(publication) = self.publish_reloaded_auth(generation, new_auth) {
                return Ok(publication.changed);
            }
            if self.inner.is_poisoned() {
                return Ok(false);
            }
        }
    }

    /// Pins the stored credential kind before any subsequent load or hydration.
    /// This prevents a running client from silently acquiring a different kind of secret.
    pub fn restrict_auth_modes(&self, mut modes: Vec<AuthMode>) -> std::io::Result<()> {
        let mut allowed = self
            .allowed_auth_modes
            .write()
            .map_err(|_| std::io::Error::other("credential policy is unavailable"))?;
        if let Some(previous) = allowed.as_ref() {
            modes.retain(|mode| previous.contains(mode));
        }
        *allowed = Some(modes);
        Ok(())
    }

    /// Reloads stored credentials without hiding storage or hydration failures.
    /// Failed loads retain the last known owner so a temporary outage does not
    /// invalidate application network policy as if the user had logged out.
    /// Leaves host-provided credentials alone and serializes with token refresh.
    /// Callers must reject cached headers on error.
    pub async fn reload_with_cached_auth(&self) -> std::io::Result<bool> {
        let _refresh_guard = self
            .refresh_lock
            .acquire()
            .await
            .map_err(std::io::Error::other)?;
        loop {
            let generation = self.auth_change_state_tx.borrow().generation;
            if self.has_external_auth() {
                return Ok(false);
            }
            let new_auth = self.try_load_managed_auth().await?;
            if let Some(publication) = self.publish_reloaded_auth(generation, new_auth) {
                return Ok(publication.changed);
            }
            if self.inner.is_poisoned() {
                return Err(std::io::Error::other("credential cache is unavailable"));
            }
        }
    }

    fn auths_equal_for_refresh(a: Option<&CodexAuth>, b: Option<&CodexAuth>) -> bool {
        match (a, b) {
            (None, None) => true,
            (Some(a), Some(b)) => match (a.api_auth_mode(), b.api_auth_mode()) {
                (AuthMode::ApiKey, AuthMode::ApiKey) => a.api_key() == b.api_key(),
                (AuthMode::BedrockApiKey, AuthMode::BedrockApiKey) => a == b,
                (AuthMode::BedrockAccessKeys, AuthMode::BedrockAccessKeys) => a == b,
                _ => false,
            },
            _ => false,
        }
    }

    fn auths_equal(a: Option<&CodexAuth>, b: Option<&CodexAuth>) -> bool {
        match (a, b) {
            (None, None) => true,
            (Some(a), Some(b)) => a == b,
            _ => false,
        }
    }

    /// Records a permanent refresh failure only if the failed refresh was
    /// attempted against the auth snapshot that is still cached.
    fn record_permanent_refresh_failure_if_unchanged(
        &self,
        attempted_auth: &CodexAuth,
        error: &RefreshTokenFailedError,
    ) {
        if let Ok(mut guard) = self.inner.write() {
            let current_auth_matches =
                Self::auths_equal_for_refresh(Some(attempted_auth), guard.auth.as_ref());
            if current_auth_matches {
                guard.permanent_refresh_failure = Some(AuthScopedRefreshFailure {
                    auth: attempted_auth.clone(),
                    error: error.clone(),
                });
            }
        }
    }

    async fn load_auth(&self) -> Result<Option<CodexAuth>, RefreshTokenError> {
        if let Some(external_auth) = self.external_auth_provider() {
            let cached_auth = self.auth_cached();
            if cached_auth
                .as_ref()
                .is_some_and(|auth| self.refresh_failure_for_auth(auth).is_some())
            {
                return Ok(cached_auth);
            }
            return match self.resolve_external_auth(external_auth.as_ref()).await {
                Ok(auth) => Ok(Some(auth)),
                Err(err) => {
                    tracing::error!("Failed to resolve external auth: {err}");
                    match err {
                        RefreshTokenError::Permanent(error) => {
                            if let Some(auth) = cached_auth.as_ref() {
                                self.record_permanent_refresh_failure_if_unchanged(auth, &error);
                            }
                            Ok(cached_auth)
                        }
                        // A temporary provider outage is not a logout. Preserve the identity
                        // only while its cached credentials still satisfy the current policy.
                        RefreshTokenError::Transient(_) => Ok(cached_auth.filter(|_| {
                            validate_auth_restrictions(Some(&self.allowed_login_methods())).is_ok()
                        })),
                        RefreshTokenError::Policy(_) => Ok(cached_auth),
                    }
                }
            };
        }

        self.try_load_managed_auth()
            .await
            .map_err(RefreshTokenError::Transient)
    }

    async fn try_load_managed_auth(&self) -> std::io::Result<Option<CodexAuth>> {
        let allowed_auth_modes = self
            .allowed_auth_modes
            .read()
            .map_err(|_| std::io::Error::other("credential policy is unavailable"))?
            .clone();
        let allowed_login_methods = self.allowed_login_methods();
        let auth = load_auth(
            &self.codex_home,
            self.enable_codex_api_key_env,
            self.auth_credentials_store_mode,
            Some(&allowed_login_methods),
            self.keyring_backend_kind,
            allowed_auth_modes.as_deref(),
        )
        .await?;
        Ok(auth.filter(|_| validate_auth_restrictions(Some(&allowed_login_methods)).is_ok()))
    }

    fn set_cached_auth(&self, new_auth: Option<CodexAuth>) -> bool {
        let Ok(mut guard) = self.inner.write() else {
            return false;
        };
        self.set_cached_auth_locked(&mut guard, new_auth)
    }

    fn publish_reloaded_auth(
        &self,
        generation: u64,
        auth: Option<CodexAuth>,
    ) -> Option<AuthReload> {
        let mut guard = self.inner.write().ok()?;
        if self.auth_change_state_tx.borrow().generation != generation {
            return None;
        }
        let changed = self.set_cached_auth_locked(&mut guard, auth);
        Some(AuthReload { changed })
    }

    fn invalidate_pending_auth_reads(&self) {
        if let Ok(_guard) = self.inner.write() {
            self.auth_change_state_tx
                .send_modify(|state| state.generation += 1);
        }
    }

    fn set_cached_auth_locked(&self, guard: &mut CachedAuth, new_auth: Option<CodexAuth>) -> bool {
        let previous = guard.auth.as_ref();
        let changed = !AuthManager::auths_equal(previous, new_auth.as_ref());
        let auth_changed_for_refresh = !Self::auths_equal_for_refresh(previous, new_auth.as_ref());
        let owner_changed = auth_changed_for_refresh;
        let source_changed =
            auth_changed_for_refresh && !super::source::same_source(previous, new_auth.as_ref());
        let login_changed = auth_changed_for_refresh;
        if owner_changed && self.owns_network_policy {
            self.auth_route_config
                .application_network_policy()
                .invalidate();
        }
        if auth_changed_for_refresh {
            guard.permanent_refresh_failure = None;
        }
        guard.auth = new_auth;
        if auth_changed_for_refresh {
            self.auth_change_state_tx.send_modify(|state| {
                state.generation += 1;
                if owner_changed {
                    state.owner_generation += 1;
                }
                if source_changed {
                    state.source_generation += 1;
                }
                if login_changed {
                    state.login_generation += 1;
                }
            });
            self.auth_change_tx.send_modify(|revision| *revision += 1);
        }
        tracing::info!(
            auth_changed_for_refresh,
            "Reloaded auth, changed: {changed}"
        );
        changed
    }

    pub async fn set_external_auth(
        &self,
        external_auth: Arc<dyn ExternalAuth>,
    ) -> Result<(), RefreshTokenError> {
        let auth = self.resolve_external_auth(external_auth.as_ref()).await?;
        let mut external_auth_slot = self.external_auth.write().map_err(|_| {
            RefreshTokenError::Transient(std::io::Error::other("external auth lock is poisoned"))
        })?;
        *external_auth_slot = Some(external_auth);
        drop(external_auth_slot);
        if let Ok(mut guard) = self.inner.write() {
            guard.permanent_refresh_failure = None;
        }
        self.commit_external_auth(auth)
    }

    pub fn clear_external_auth(&self) {
        if let Ok(mut external_auth) = self.external_auth.write()
            && external_auth.take().is_some()
        {
            self.set_cached_auth(/*new_auth*/ None);
        }
    }

    pub fn set_forced_chatgpt_workspace_id(&self, workspace_id: Option<Vec<String>>) {
        // Fence reloads that validated credentials before this policy change.
        let Ok(_auth_guard) = self.inner.write() else {
            return;
        };
        if let Ok(mut guard) = self.forced_chatgpt_workspace_id.write()
            && *guard != workspace_id
        {
            *guard = workspace_id;
            self.auth_change_state_tx
                .send_modify(|state| state.generation += 1);
        }
    }

    pub fn forced_chatgpt_workspace_id(&self) -> Option<Vec<String>> {
        self.forced_chatgpt_workspace_id
            .read()
            .ok()
            .and_then(|guard| guard.clone())
    }

    pub fn effective_chatgpt_workspaces(&self) -> Option<Vec<String>> {
        self.managed_auth_policy
            .effective_chatgpt_workspaces(self.forced_chatgpt_workspace_id().as_deref())
    }

    pub fn is_login_method_allowed(&self, method: ForcedLoginMethod) -> bool {
        self.managed_auth_policy.allows_login_method(
            method,
            self.forced_login_method,
            self.forced_chatgpt_workspace_id().as_deref(),
        )
    }

    /// Returns the login methods permitted by the current effective authentication policy.
    pub fn allowed_login_methods(&self) -> Vec<ForcedLoginMethod> {
        self.managed_auth_policy.allowed_login_methods(
            self.forced_login_method,
            self.forced_chatgpt_workspace_id().as_deref(),
        )
    }

    pub fn has_external_auth(&self) -> bool {
        self.external_auth_provider().is_some()
    }

    pub fn codex_api_key_env_enabled(&self) -> bool {
        self.enable_codex_api_key_env
    }

    /// Returns policy only; independent credential managers own their own state and lifecycle.
    pub fn runtime_config(&self) -> AuthRuntimeConfig {
        AuthRuntimeConfig {
            codex_home: self.codex_home.clone(),
            auth_route_config: self.auth_route_config.clone(),
        }
    }

    /// Convenience constructor returning an `Arc` wrapper.
    pub async fn shared(
        codex_home: PathBuf,
        enable_codex_api_key_env: bool,
        auth_credentials_store_mode: AuthCredentialsStoreMode,
        forced_chatgpt_workspace_id: Option<Vec<String>>,
        chatgpt_base_url: Option<String>,
        keyring_backend_kind: AuthKeyringBackendKind,
        auth_route_config: AuthRouteConfig,
    ) -> Arc<Self> {
        Arc::new(
            Self::new(
                codex_home,
                enable_codex_api_key_env,
                auth_credentials_store_mode,
                forced_chatgpt_workspace_id,
                chatgpt_base_url,
                keyring_backend_kind,
                auth_route_config,
            )
            .await,
        )
    }

    /// Builds a shared manager from resolved configuration.
    pub async fn shared_from_config(
        config: &impl AuthManagerConfig,
        enable_codex_api_key_env: bool,
    ) -> Result<Arc<Self>, AuthManagerInitializationError> {
        Self::shared_from_auth_config(auth_config_from(config), enable_codex_api_key_env).await
    }

    /// Builds a shared manager from the authentication configuration.
    pub async fn shared_from_auth_config(
        auth_config: AuthConfig,
        enable_codex_api_key_env: bool,
    ) -> Result<Arc<Self>, AuthManagerInitializationError> {
        Self::shared_from_auth_config_with_modes(
            auth_config,
            enable_codex_api_key_env,
            /*allowed_auth_modes*/ None,
        )
        .await
    }

    /// Restricts credential modes before loading or hydrating the initial credential.
    pub async fn shared_from_config_with_auth_modes(
        config: &impl AuthManagerConfig,
        enable_codex_api_key_env: bool,
        allowed_auth_modes: Vec<AuthMode>,
    ) -> Result<Arc<Self>, AuthManagerInitializationError> {
        Self::shared_from_auth_config_with_auth_modes(
            auth_config_from(config),
            enable_codex_api_key_env,
            allowed_auth_modes,
        )
        .await
    }

    /// Restricts credential modes during bootstrap, before loading runtime configuration.
    pub async fn shared_from_auth_config_with_auth_modes(
        auth_config: AuthConfig,
        enable_codex_api_key_env: bool,
        allowed_auth_modes: Vec<AuthMode>,
    ) -> Result<Arc<Self>, AuthManagerInitializationError> {
        Self::shared_from_auth_config_with_modes(
            auth_config,
            enable_codex_api_key_env,
            Some(allowed_auth_modes),
        )
        .await
    }

    async fn shared_from_auth_config_with_modes(
        auth_config: AuthConfig,
        enable_codex_api_key_env: bool,
        allowed_auth_modes: Option<Vec<AuthMode>>,
    ) -> Result<Arc<Self>, AuthManagerInitializationError> {
        let manager =
            Self::new_from_auth_config(auth_config, enable_codex_api_key_env, allowed_auth_modes)
                .await;
        let manager = Arc::new(manager);
        Ok(manager)
    }

    pub fn unauthorized_recovery(self: &Arc<Self>) -> UnauthorizedRecovery {
        UnauthorizedRecovery::new(Arc::clone(self))
    }

    fn external_auth_provider(&self) -> Option<Arc<dyn ExternalAuth>> {
        self.external_auth
            .read()
            .ok()
            .and_then(|external_auth| external_auth.clone())
    }

    fn has_refreshable_external_auth(&self) -> bool {
        self.has_external_auth()
            && self
                .auth_cached()
                .as_ref()
                .is_none_or(CodexAuth::is_api_key_auth)
    }

    async fn resolve_external_auth(
        &self,
        external_auth: &dyn ExternalAuth,
    ) -> Result<CodexAuth, RefreshTokenError> {
        let auth = external_auth
            .resolve()
            .await
            .map_err(|error| external_auth.classify_error(error))?;
        self.validate_external_auth(external_auth)?;
        Ok(auth)
    }

    /// Attempt to refresh the token by first performing a guarded reload from
    /// the active auth source. If the loaded token differs from the cached token,
    /// we can assume that the source already refreshed it. Otherwise, ask the
    /// configured external credential owner to refresh. Built-in account renewal is unavailable.
    pub async fn refresh_token(&self) -> Result<(), RefreshTokenError> {
        self.refresh_token_with_expected_auth(/*expected_auth*/ None)
            .await
    }

    /// Refreshes only if the expected credential is still current after acquiring
    /// the existing refresh guard, so queued callers reuse a completed refresh.
    pub async fn refresh_token_if_unchanged(
        &self,
        expected_auth: &CodexAuth,
    ) -> Result<(), RefreshTokenError> {
        self.refresh_token_with_expected_auth(Some(expected_auth))
            .await
    }

    async fn refresh_token_with_expected_auth(
        &self,
        expected_auth: Option<&CodexAuth>,
    ) -> Result<(), RefreshTokenError> {
        let _refresh_guard = self.refresh_lock.acquire().await.map_err(|_| {
            RefreshTokenError::Permanent(RefreshTokenFailedError::new(
                RefreshTokenFailedReason::Other,
                REFRESH_TOKEN_UNKNOWN_MESSAGE.to_string(),
            ))
        })?;
        let auth_before_reload = self.auth_cached();
        if let Some(expected_auth) = expected_auth
            && !Self::auths_equal_for_refresh(auth_before_reload.as_ref(), Some(expected_auth))
        {
            return Ok(());
        }
        if self.has_external_auth() {
            return self.refresh_token_from_authority_impl().await;
        }
        if auth_before_reload
            .as_ref()
            .is_some_and(CodexAuth::is_api_key_auth)
        {
            return Ok(());
        }
        Err(RefreshTokenError::Permanent(RefreshTokenFailedError::new(
            RefreshTokenFailedReason::Other,
            REFRESH_TOKEN_ACCOUNT_MISMATCH_MESSAGE.to_string(),
        )))
    }

    /// Attempt to refresh the current auth token from the authority that issued
    /// it and update the shared cache. If the token refresh fails, returns the
    /// error to the caller.
    pub async fn refresh_token_from_authority(&self) -> Result<(), RefreshTokenError> {
        let _refresh_guard = self.refresh_lock.acquire().await.map_err(|_| {
            RefreshTokenError::Permanent(RefreshTokenFailedError::new(
                RefreshTokenFailedReason::Other,
                REFRESH_TOKEN_UNKNOWN_MESSAGE.to_string(),
            ))
        })?;
        self.refresh_token_from_authority_impl().await
    }

    async fn refresh_token_from_authority_impl(&self) -> Result<(), RefreshTokenError> {
        tracing::info!("Refreshing token");

        let attempted_auth = self.auth_cached();
        if let Some(error) = attempted_auth
            .as_ref()
            .and_then(|auth| self.refresh_failure_for_auth(auth))
        {
            return Err(RefreshTokenError::Permanent(error));
        }

        let result = if self.has_external_auth() {
            self.refresh_external_auth(ExternalAuthRefreshReason::Unauthorized)
                .await
        } else {
            Ok(())
        };
        if let Some(attempted_auth) = attempted_auth.as_ref()
            && let Err(RefreshTokenError::Permanent(error)) = &result
        {
            self.record_permanent_refresh_failure_if_unchanged(attempted_auth, error);
        }
        result
    }

    /// Log out by deleting credentials from the selected storage backend and the
    /// ephemeral store. Returns `Ok(true)` if credentials were removed, `Ok(false)`
    /// if none were present. On success, reloads the in-memory auth cache so
    /// callers immediately observe the unauthenticated state.
    pub async fn logout(&self) -> std::io::Result<bool> {
        let removed = logout_all_stores(
            &self.codex_home,
            self.auth_credentials_store_mode,
            self.keyring_backend_kind,
        )?;
        // Always reload to clear cached auth, even if no stored credentials were present.
        self.invalidate_pending_auth_reads();
        self.clear_external_auth();
        self.reload().await;
        Ok(removed)
    }

    /// Returns the precise kind of credentials backing the current authentication.
    pub fn get_api_auth_mode(&self) -> Option<AuthMode> {
        self.auth_cached().as_ref().map(CodexAuth::api_auth_mode)
    }

    /// Returns the effective backend auth mode for the current authentication.
    pub fn auth_mode(&self) -> Option<AuthMode> {
        self.auth_cached().as_ref().map(CodexAuth::auth_mode)
    }

    async fn refresh_external_auth(
        &self,
        reason: ExternalAuthRefreshReason,
    ) -> Result<(), RefreshTokenError> {
        let Some(external_auth) = self.external_auth_provider() else {
            return Err(RefreshTokenError::Transient(std::io::Error::other(
                "external auth is not configured",
            )));
        };
        let context = ExternalAuthRefreshContext { reason };

        let refreshed = external_auth
            .refresh(context)
            .await
            .map_err(|error| external_auth.classify_error(error))?;
        self.validate_external_auth(external_auth.as_ref())?;
        self.commit_external_auth(refreshed)?;
        Ok(())
    }

    fn commit_external_auth(&self, auth: CodexAuth) -> Result<(), RefreshTokenError> {
        self.set_cached_auth(Some(auth));
        Ok(())
    }

    fn validate_external_auth(
        &self,
        external_auth: &dyn ExternalAuth,
    ) -> Result<(), RefreshTokenError> {
        validate_auth_restrictions(Some(&self.allowed_login_methods()))
            .map_err(|error| external_auth.classify_error(std::io::Error::other(error)))
    }
}

fn auth_config_from(config: &impl AuthManagerConfig) -> AuthConfig {
    AuthConfig {
        codex_home: config.codex_home(),
        auth_credentials_store_mode: config.cli_auth_credentials_store_mode(),
        keyring_backend_kind: config.auth_keyring_backend_kind(),
        forced_login_method: config.forced_login_method(),
        chatgpt_base_url: Some(config.chatgpt_base_url()),
        forced_chatgpt_workspace_id: config.forced_chatgpt_workspace_id(),
        managed_auth_policy: config.managed_auth_policy(),
        auth_route_config: config.auth_route_config(),
    }
}

#[cfg(test)]
#[path = "auth_tests.rs"]
pub(crate) mod tests;

#[cfg(test)]
#[path = "source_tests.rs"]
mod source_tests;
