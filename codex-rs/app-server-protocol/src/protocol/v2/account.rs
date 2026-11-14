use crate::JsonSchema;
use crate::TS;
use crate::protocol::common::AuthMode;
use codex_experimental_api_macros::ExperimentalApi;
use codex_protocol::account::PlanType;
use codex_protocol::account::ProviderAccount;
use codex_protocol::protocol::CreditsSnapshot as CoreCreditsSnapshot;
use codex_protocol::protocol::RateLimitReachedType as CoreRateLimitReachedType;
use codex_protocol::protocol::RateLimitSnapshot as CoreRateLimitSnapshot;
use codex_protocol::protocol::RateLimitWindow as CoreRateLimitWindow;
use codex_protocol::protocol::SpendControlLimitSnapshot as CoreSpendControlLimitSnapshot;
use serde::Deserialize;
use serde::Serialize;

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(tag = "type", rename_all = "camelCase")]
#[ts(tag = "type")]
#[ts(export_to = "v2/")]
pub enum Account {
    #[serde(rename = "apiKey", rename_all = "camelCase")]
    #[ts(rename = "apiKey", rename_all = "camelCase")]
    ApiKey {},

    #[serde(rename = "amazonBedrock", rename_all = "camelCase")]
    #[ts(rename = "amazonBedrock", rename_all = "camelCase")]
    AmazonBedrock {
        #[serde(default)]
        uses_codex_managed_credentials: bool,
    },

    /// Google Cloud Vertex AI with externally managed credentials.
    #[serde(rename = "gcp", rename_all = "camelCase")]
    #[ts(rename = "gcp", rename_all = "camelCase")]
    Gcp {},
}

impl From<ProviderAccount> for Account {
    fn from(account: ProviderAccount) -> Self {
        match account {
            ProviderAccount::ApiKey => Self::ApiKey {},
            ProviderAccount::AmazonBedrock {
                uses_codex_managed_credentials,
            } => Self::AmazonBedrock {
                uses_codex_managed_credentials,
            },
            ProviderAccount::Gcp => Self::Gcp {},
        }
    }
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS, ExperimentalApi)]
#[serde(tag = "type")]
#[ts(tag = "type")]
#[ts(export_to = "v2/")]
pub enum LoginAccountParams {
    #[serde(rename = "apiKey", rename_all = "camelCase")]
    #[ts(rename = "apiKey", rename_all = "camelCase")]
    ApiKey {
        #[serde(rename = "apiKey")]
        #[ts(rename = "apiKey")]
        api_key: String,
    },
    /// [UNSTABLE] Managed Amazon Bedrock login is experimental.
    #[experimental("account/login/start.amazonBedrock")]
    #[serde(rename = "amazonBedrock", rename_all = "camelCase")]
    #[ts(rename = "amazonBedrock", rename_all = "camelCase")]
    AmazonBedrock { api_key: String, region: String },
    /// [UNSTABLE] Managed Amazon Bedrock AWS access key login is experimental.
    #[experimental("account/login/start.amazonBedrockAccessKeys")]
    #[serde(rename = "amazonBedrockAccessKeys", rename_all = "camelCase")]
    #[ts(rename = "amazonBedrockAccessKeys", rename_all = "camelCase")]
    AmazonBedrockAccessKeys {
        access_key_id: String,
        secret_access_key: String,
        #[ts(optional = nullable)]
        session_token: Option<String>,
        region: String,
    },
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(tag = "type", rename_all = "camelCase")]
#[ts(tag = "type")]
#[ts(export_to = "v2/")]
pub enum LoginAccountResponse {
    #[serde(rename = "apiKey", rename_all = "camelCase")]
    #[ts(rename = "apiKey", rename_all = "camelCase")]
    ApiKey {},
    #[serde(rename = "amazonBedrock", rename_all = "camelCase")]
    #[ts(rename = "amazonBedrock", rename_all = "camelCase")]
    AmazonBedrock {},
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct CancelLoginAccountParams {
    pub login_id: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub enum CancelLoginAccountStatus {
    Canceled,
    NotFound,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct CancelLoginAccountResponse {
    pub status: CancelLoginAccountStatus,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct LogoutAccountResponse {}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct GetAccountParams {
    /// When `true`, reloads credentials and asks the configured external credential
    /// owner to refresh them before returning. Built-in account renewal is unavailable.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub refresh_token: bool,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS, ExperimentalApi)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct GetAccountResponse {
    pub account: Option<Account>,
    pub requires_openai_auth: bool,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct AccountUpdatedNotification {
    pub auth_mode: Option<AuthMode>,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/", rename_all = "camelCase")]
pub enum GatewayOAuthStatus {
    NotReady,
    Started,
    Succeeded,
    Failed,
}

#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct GatewayOAuthChangedNotification {
    /// Identifies the completed device login attempt.
    pub login_id: Option<String>,
    /// Authorization handoff, sent only to the connection that started login.
    pub auth_url: Option<String>,
    pub provider_id: String,
    pub status: GatewayOAuthStatus,
    pub error: Option<String>,
}

impl std::fmt::Debug for GatewayOAuthChangedNotification {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GatewayOAuthChangedNotification")
            .field("auth_url", &self.auth_url.as_ref().map(|_| "[REDACTED]"))
            .field("provider_id", &self.provider_id)
            .field("status", &self.status)
            .field("error", &self.error)
            .finish()
    }
}

/// Read credential readiness for a configured provider without starting browser login.
#[derive(Serialize, Deserialize, Debug, Clone, Default, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct GatewayOAuthReadParams {
    /// Defaults to the current provider when omitted.
    #[ts(optional = nullable)]
    pub model_provider: Option<String>,
}

pub type NullableGatewayOAuthReadParams = Option<GatewayOAuthReadParams>;

/// Authorize a configured provider without changing the default or any thread's provider.
#[derive(Serialize, Deserialize, Debug, Clone, Default, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct GatewayOAuthLoginParams {
    /// Defaults to the current provider when omitted.
    #[ts(optional = nullable)]
    pub model_provider: Option<String>,
    /// Select device authorization when the server advertises support for it.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub device_auth: bool,
}

pub type NullableGatewayOAuthLoginParams = Option<GatewayOAuthLoginParams>;

/// Current effective gateway policy and credential readiness; never contains credentials.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct GatewayOAuthReadResponse {
    pub provider_id: String,
    pub provider_name: String,
    /// Whether the selected provider uses gateway OAuth, even when already signed in.
    pub required: bool,
    /// Whether the administrator configured a device authorization endpoint.
    #[serde(default)]
    #[ts(optional, as = "Option<bool>")]
    pub supports_device_auth: bool,
    /// Null when the effective provider does not use gateway OAuth.
    pub status: Option<GatewayOAuthStatus>,
    pub error: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct GatewayOAuthLoginResponse {
    pub prompt: Option<GatewayOAuthPrompt>,
}

#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct GatewayOAuthPrompt {
    pub login_id: String,
    pub verification_url: String,
    pub user_code: String,
}

impl std::fmt::Debug for GatewayOAuthPrompt {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GatewayOAuthPrompt")
            .field("login_id", &self.login_id)
            .finish_non_exhaustive()
    }
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct GatewayOAuthCancelResponse {}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
/// Sparse rolling rate-limit update.
///
/// Clients should merge available values into the most recent snapshot. Nullable metadata may be
/// unavailable in a rolling update and does not clear a previously observed value.
pub struct AccountRateLimitsUpdatedNotification {
    pub rate_limits: RateLimitSnapshot,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct RateLimitSnapshot {
    pub limit_id: Option<String>,
    pub limit_name: Option<String>,
    /// Normal model whose display name and reasoning options describe this quota alias.
    pub normal_model_slug: Option<String>,
    pub primary: Option<RateLimitWindow>,
    pub secondary: Option<RateLimitWindow>,
    pub credits: Option<CreditsSnapshot>,
    pub individual_limit: Option<SpendControlLimitSnapshot>,
    /// Backend-reported spend-control state. `None` is unavailable, not a sparse-update recovery.
    pub spend_control_reached: Option<bool>,
    pub plan_type: Option<PlanType>,
    pub rate_limit_reached_type: Option<RateLimitReachedType>,
}

impl From<CoreRateLimitSnapshot> for RateLimitSnapshot {
    fn from(value: CoreRateLimitSnapshot) -> Self {
        Self {
            limit_id: value.limit_id,
            limit_name: value.limit_name,
            normal_model_slug: value.normal_model_slug,
            primary: value.primary.map(RateLimitWindow::from),
            secondary: value.secondary.map(RateLimitWindow::from),
            credits: value.credits.map(CreditsSnapshot::from),
            individual_limit: value.individual_limit.map(SpendControlLimitSnapshot::from),
            spend_control_reached: value.spend_control_reached,
            plan_type: value.plan_type,
            rate_limit_reached_type: value
                .rate_limit_reached_type
                .map(RateLimitReachedType::from),
        }
    }
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export_to = "v2/", rename_all = "snake_case")]
pub enum RateLimitReachedType {
    RateLimitReached,
    WorkspaceOwnerCreditsDepleted,
    WorkspaceMemberCreditsDepleted,
    WorkspaceOwnerUsageLimitReached,
    WorkspaceMemberUsageLimitReached,
}

impl From<CoreRateLimitReachedType> for RateLimitReachedType {
    fn from(value: CoreRateLimitReachedType) -> Self {
        match value {
            CoreRateLimitReachedType::RateLimitReached => Self::RateLimitReached,
            CoreRateLimitReachedType::WorkspaceOwnerCreditsDepleted => {
                Self::WorkspaceOwnerCreditsDepleted
            }
            CoreRateLimitReachedType::WorkspaceMemberCreditsDepleted => {
                Self::WorkspaceMemberCreditsDepleted
            }
            CoreRateLimitReachedType::WorkspaceOwnerUsageLimitReached => {
                Self::WorkspaceOwnerUsageLimitReached
            }
            CoreRateLimitReachedType::WorkspaceMemberUsageLimitReached => {
                Self::WorkspaceMemberUsageLimitReached
            }
        }
    }
}

impl From<RateLimitReachedType> for CoreRateLimitReachedType {
    fn from(value: RateLimitReachedType) -> Self {
        match value {
            RateLimitReachedType::RateLimitReached => Self::RateLimitReached,
            RateLimitReachedType::WorkspaceOwnerCreditsDepleted => {
                Self::WorkspaceOwnerCreditsDepleted
            }
            RateLimitReachedType::WorkspaceMemberCreditsDepleted => {
                Self::WorkspaceMemberCreditsDepleted
            }
            RateLimitReachedType::WorkspaceOwnerUsageLimitReached => {
                Self::WorkspaceOwnerUsageLimitReached
            }
            RateLimitReachedType::WorkspaceMemberUsageLimitReached => {
                Self::WorkspaceMemberUsageLimitReached
            }
        }
    }
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct RateLimitWindow {
    pub used_percent: i32,
    #[ts(type = "number | null")]
    pub window_duration_mins: Option<i64>,
    #[ts(type = "number | null")]
    pub resets_at: Option<i64>,
}

impl From<CoreRateLimitWindow> for RateLimitWindow {
    fn from(value: CoreRateLimitWindow) -> Self {
        Self {
            used_percent: value.used_percent.round() as i32,
            window_duration_mins: value.window_minutes,
            resets_at: value.resets_at,
        }
    }
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct CreditsSnapshot {
    pub has_credits: bool,
    pub unlimited: bool,
    pub balance: Option<String>,
}

impl From<CoreCreditsSnapshot> for CreditsSnapshot {
    fn from(value: CoreCreditsSnapshot) -> Self {
        Self {
            has_credits: value.has_credits,
            unlimited: value.unlimited,
            balance: value.balance,
        }
    }
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct SpendControlLimitSnapshot {
    pub limit: String,
    pub used: String,
    pub remaining_percent: i32,
    #[ts(type = "number")]
    pub resets_at: i64,
}

impl From<CoreSpendControlLimitSnapshot> for SpendControlLimitSnapshot {
    fn from(value: CoreSpendControlLimitSnapshot) -> Self {
        Self {
            limit: value.limit,
            used: value.used,
            remaining_percent: value.remaining_percent,
            resets_at: value.resets_at,
        }
    }
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct AccountLoginCompletedNotification {
    // Use plain String for identifiers to avoid TS/JSON Schema quirks around uuid-specific types.
    // Convert to/from UUIDs at the application layer as needed.
    pub login_id: Option<String>,
    pub success: bool,
    pub error: Option<String>,
}
