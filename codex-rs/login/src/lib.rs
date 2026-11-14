pub mod auth;
pub mod auth_env_telemetry;
pub mod test_support;

mod credential_export;
mod oauth;
mod oauth_session;
pub use credential_export::run_credential_export;
mod outbound_proxy;

pub use codex_config::types::AuthCredentialsStoreMode;
pub use codex_http_client::BuildCustomCaTransportError as BuildLoginHttpClientError;

pub use auth::AuthChangeState;
pub use auth::AuthConfig;
pub use auth::AuthDotJson;
pub use auth::AuthKeyringBackendKind;
pub use auth::AuthManager;
pub use auth::AuthManagerConfig;
pub use auth::AuthManagerInitializationError;
pub use auth::AuthOwner;
pub use auth::AuthRuntimeConfig;
pub use auth::CODEX_API_KEY_ENV_VAR;
pub use auth::CodexAuth;
pub use auth::ExternalAuth;
pub use auth::ExternalAuthFuture;
pub use auth::ExternalAuthRefreshContext;
pub use auth::ExternalAuthRefreshReason;
pub use auth::OPENAI_API_KEY_ENV_VAR;
pub use auth::RefreshTokenError;
pub use auth::RequestAuthChange;
pub use auth::UnauthorizedRecovery;
pub use auth::default_client;
pub use auth::enforce_login_restrictions;
pub use auth::load_auth_dot_json;
pub use auth::login_with_api_key;
pub use auth::login_with_bedrock_access_keys;
pub use auth::login_with_bedrock_api_key;
pub use auth::logout;
pub use auth::logout_all_stores;
pub use auth::read_openai_api_key_from_env;
pub use auth::save_auth;
pub use auth_env_telemetry::AuthEnvTelemetry;
pub use auth_env_telemetry::collect_auth_env_telemetry;
pub use oauth::OAuthDeviceCodePrompt;
pub use outbound_proxy::AuthRouteConfig;

// Existing inference-gateway names continue to refer to the same grant manager.
pub use oauth_session::OAuthLoginControl as GatewayLoginControl;
pub use oauth_session::OAuthSessionConfig as GatewayAuthConfig;
pub use oauth_session::OAuthSessionError as GatewayAuthError;
pub use oauth_session::OAuthSessionStatus as GatewayAuthStatus;
pub use oauth_session::OAuthSessionStatusChange as GatewayAuthStatusChange;
pub use oauth_session::subscribe_oauth_session_status as subscribe_gateway_auth_status;

pub use oauth_session::OAuthLoginControl;
pub use oauth_session::OAuthRequestCredentials;
pub use oauth_session::OAuthSessionConfig;
pub use oauth_session::OAuthSessionError;
pub use oauth_session::OAuthSessionManager;
pub use oauth_session::OAuthSessionRequest;
pub use oauth_session::OAuthSessionStatus;
pub use oauth_session::OAuthSessionStatusChange;
pub use oauth_session::subscribe_oauth_session_status;
