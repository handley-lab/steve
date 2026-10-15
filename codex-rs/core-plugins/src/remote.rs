use crate::error_subtype::http_status_sub_error_type;
use crate::http_client_selector::HttpClientSelector;
use chrono::DateTime;
use chrono::Utc;
use codex_app_server_protocol::JSONRPCErrorError;
use codex_app_server_protocol::PluginAuthPolicy;
use codex_app_server_protocol::PluginAvailability;
use codex_app_server_protocol::PluginDisabledReason;
use codex_app_server_protocol::PluginInstallPolicy;
use codex_app_server_protocol::PluginInstallPolicySource;
use codex_app_server_protocol::PluginInterface;
use codex_app_server_protocol::ScheduledTaskSummary;
use codex_app_server_protocol::SkillInterface;
use codex_http_client::ClientRouteClass;
use codex_http_client::HttpClientFactory;
use codex_http_client::RequestBuilder;
use codex_http_client::RouteAwareClientPool;
use codex_http_client::RouteAwareRequestError;
use codex_login::CodexAuth;
use codex_login::default_client::default_headers;
use codex_plugin::PluginCapabilitySummary;
use codex_plugin::PluginId;
use http::Method;
use http::StatusCode;
use serde::Deserialize;
use serde::Serialize;
use serde_json::Value as JsonValue;
use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;
use url::Url;

pub(crate) mod catalog_cache;
mod plugin_capabilities;
mod remote_installed_plugin_sync;
mod share;

#[cfg(test)]
#[path = "remote_tests.rs"]
mod tests;

pub use plugin_capabilities::RemotePluginCapabilities;
pub use remote_installed_plugin_sync::RemoteInstalledPluginBundleSyncError;
pub use remote_installed_plugin_sync::RemoteInstalledPluginBundleSyncOutcome;
pub use remote_installed_plugin_sync::RemotePluginCacheMutationGuard;
pub use remote_installed_plugin_sync::RemotePluginChange;
pub use remote_installed_plugin_sync::RemotePluginMaterialization;
pub use remote_installed_plugin_sync::mark_remote_plugin_cache_mutation_in_flight;
pub use share::RemotePluginShareDiscoverability;
pub use share::RemotePluginSharePrincipal;
pub use share::RemotePluginSharePrincipalRole;
pub use share::RemotePluginSharePrincipalType;
pub use share::load_plugin_share_remote_ids_by_local_path;

pub const REMOTE_GLOBAL_MARKETPLACE_NAME: &str = "openai-curated-remote";
pub const REMOTE_CREATED_BY_ME_MARKETPLACE_NAME: &str = "created-by-me-remote";
pub const REMOTE_WORKSPACE_MARKETPLACE_NAME: &str = "workspace-directory";
pub const REMOTE_WORKSPACE_SHARED_WITH_ME_MARKETPLACE_NAME: &str = "workspace-shared-with-me";
pub const REMOTE_WORKSPACE_SHARED_WITH_ME_PRIVATE_MARKETPLACE_NAME: &str =
    "workspace-shared-with-me-private";
pub const REMOTE_WORKSPACE_SHARED_WITH_ME_UNLISTED_MARKETPLACE_NAME: &str =
    "workspace-shared-with-me-unlisted";
pub const REMOTE_GLOBAL_MARKETPLACE_DISPLAY_NAME: &str = "OpenAI Curated Remote";
pub const REMOTE_CREATED_BY_ME_MARKETPLACE_DISPLAY_NAME: &str = "Created by me";
pub const REMOTE_WORKSPACE_MARKETPLACE_DISPLAY_NAME: &str = "Workspace Directory";
pub const REMOTE_WORKSPACE_SHARED_WITH_ME_PRIVATE_MARKETPLACE_DISPLAY_NAME: &str = "Shared with me";
pub const REMOTE_WORKSPACE_SHARED_WITH_ME_UNLISTED_MARKETPLACE_DISPLAY_NAME: &str =
    "Shared with me (unlisted)";

const OPENAI_CURATED_REMOTE_COLLECTION_KEY: &str = "vertical";
const OAI_PRODUCT_SKU_HEADER: &str = "OAI-Product-Sku";
pub(crate) const CODEX_PRODUCT_SKU: &str = "codex";
const REMOTE_PLUGIN_CATALOG_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_REMOTE_DEFAULT_PROMPT_COUNT: usize = 3;
const MAX_REMOTE_DEFAULT_PROMPT_LEN: usize = 128;
const INVALID_REQUEST_ERROR_CODE: i64 = -32600;
const REMOTE_INSTALLED_MARKETPLACE_DISPLAY_ORDER: [(&str, &str); 6] = [
    (
        REMOTE_GLOBAL_MARKETPLACE_NAME,
        REMOTE_GLOBAL_MARKETPLACE_DISPLAY_NAME,
    ),
    (
        REMOTE_CREATED_BY_ME_MARKETPLACE_NAME,
        REMOTE_CREATED_BY_ME_MARKETPLACE_DISPLAY_NAME,
    ),
    (
        REMOTE_WORKSPACE_MARKETPLACE_NAME,
        REMOTE_WORKSPACE_MARKETPLACE_DISPLAY_NAME,
    ),
    (
        REMOTE_WORKSPACE_SHARED_WITH_ME_MARKETPLACE_NAME,
        REMOTE_WORKSPACE_SHARED_WITH_ME_PRIVATE_MARKETPLACE_DISPLAY_NAME,
    ),
    (
        REMOTE_WORKSPACE_SHARED_WITH_ME_PRIVATE_MARKETPLACE_NAME,
        REMOTE_WORKSPACE_SHARED_WITH_ME_PRIVATE_MARKETPLACE_DISPLAY_NAME,
    ),
    (
        REMOTE_WORKSPACE_SHARED_WITH_ME_UNLISTED_MARKETPLACE_NAME,
        REMOTE_WORKSPACE_SHARED_WITH_ME_UNLISTED_MARKETPLACE_DISPLAY_NAME,
    ),
];

#[derive(Debug, Clone)]
pub struct RemotePluginServiceConfig {
    pub chatgpt_base_url: String,
    pub(crate) product_sku: String,
    pub(crate) http_clients: Arc<dyn HttpClientSelector>,
}

impl RemotePluginServiceConfig {
    /// Creates remote plugin service state from the effective application HTTP configuration.
    ///
    /// Keeping the factory mandatory ensures every catalog, mutation, upload, and bundle request
    /// follows the same outbound proxy policy.
    pub fn new(
        chatgpt_base_url: String,
        http_client_factory: HttpClientFactory,
        product_sku: Option<String>,
    ) -> Self {
        let http_clients =
            RouteAwareClientPool::with_chatgpt_cloudflare_cookies_without_request_logging(
                http_client_factory,
                ClientRouteClass::Api,
            );
        Self {
            chatgpt_base_url,
            product_sku: product_sku.unwrap_or_else(|| CODEX_PRODUCT_SKU.to_string()),
            http_clients: Arc::new(http_clients),
        }
    }

    pub(crate) fn http_request(&self, method: Method, url: &str) -> RequestBuilder {
        self.http_clients
            .request(method, url)
            .headers(default_headers())
    }
}

impl PartialEq for RemotePluginServiceConfig {
    fn eq(&self, other: &Self) -> bool {
        self.chatgpt_base_url == other.chatgpt_base_url
            && self.product_sku == other.product_sku
            && self.http_clients.outbound_proxy_policy()
                == other.http_clients.outbound_proxy_policy()
    }
}

impl Eq for RemotePluginServiceConfig {}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemotePluginUninstallTarget {
    pub plugin_id: PluginId,
    pub remote_plugin_id: String,
    pub fallback_capability_summary: PluginCapabilitySummary,
}

#[derive(Debug, Clone, PartialEq)]
pub struct RemoteMarketplace {
    pub name: String,
    pub display_name: String,
    pub plugins: Vec<RemotePluginSummary>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RemoteMarketplaceSource {
    Global,
    CreatedByMeRemote,
    WorkspaceDirectory,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RemotePluginCatalogCacheMode {
    PreferCache,
    /// Reuse fresh entries and synchronously refresh missing or stale entries.
    PreferFreshCache,
    ForceRefetch,
}

#[derive(Debug, Clone, PartialEq)]
pub struct RemoteMarketplacesFetchOutcome {
    pub marketplaces: Vec<RemoteMarketplace>,
    pub catalog_cache_refresh_scopes: BTreeSet<RemotePluginScope>,
    /// Whether any requested directory was served from disk cache, even if it was empty.
    pub catalog_cache_used: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct RemoteMarketplaceFetchOutcome {
    pub marketplace: Option<RemoteMarketplace>,
    /// Whether the requested directory was served from disk cache, even if it was empty.
    pub catalog_cache_used: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct RemoteInstalledPlugin {
    pub marketplace_name: String,
    pub id: String,
    pub version: Option<String>,
    pub name: String,
    pub canonical_app_id: Option<String>,
    pub installed_at: Option<DateTime<Utc>>,
    pub enabled: bool,
    pub install_policy: PluginInstallPolicy,
    pub install_policy_source: Option<PluginInstallPolicySource>,
    pub must_show_installation_interstitial: Option<bool>,
    pub auth_policy: PluginAuthPolicy,
    pub availability: PluginAvailability,
    pub disabled_reason: Option<PluginDisabledReason>,
    pub eligible_plan_types: Option<Vec<String>>,
    pub interface: Option<PluginInterface>,
    pub keywords: Vec<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct RemotePluginSummary {
    pub id: String,
    pub remote_plugin_id: String,
    pub version: Option<String>,
    pub local_version: Option<String>,
    pub name: String,
    pub share_context: Option<RemotePluginShareContext>,
    pub installed: bool,
    pub installed_at: Option<DateTime<Utc>>,
    pub enabled: bool,
    pub install_policy: PluginInstallPolicy,
    pub install_policy_source: Option<PluginInstallPolicySource>,
    pub must_show_installation_interstitial: Option<bool>,
    pub auth_policy: PluginAuthPolicy,
    pub availability: PluginAvailability,
    pub disabled_reason: Option<PluginDisabledReason>,
    pub eligible_plan_types: Option<Vec<String>>,
    pub interface: Option<PluginInterface>,
    pub keywords: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemotePluginShareContext {
    pub remote_plugin_id: String,
    pub remote_version: Option<String>,
    pub discoverability: RemotePluginShareDiscoverability,
    pub share_url: Option<String>,
    pub creator_account_user_id: Option<String>,
    pub creator_name: Option<String>,
    pub share_principals: Option<Vec<RemotePluginSharePrincipal>>,
    pub can_publish_to_workspace: Option<bool>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct RemotePluginDetail {
    pub marketplace_name: String,
    pub marketplace_display_name: String,
    pub summary: RemotePluginSummary,
    pub share_url: Option<String>,
    pub description: Option<String>,
    pub release_version: Option<String>,
    pub bundle_download_url: Option<String>,
    pub app_manifest: Option<JsonValue>,
    pub skills: Vec<RemotePluginSkill>,
    pub onboarding_skill_name: Option<String>,
    pub app_ids: Vec<String>,
    pub app_templates: Vec<RemoteAppTemplate>,
    pub mcp_servers: Vec<String>,
    pub scheduled_tasks: Option<Vec<ScheduledTaskSummary>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteAppTemplate {
    pub template_id: String,
    pub name: String,
    pub description: Option<String>,
    pub category: Option<String>,
    pub canonical_connector_id: Option<String>,
    pub logo_url: Option<String>,
    pub logo_url_dark: Option<String>,
    pub materialized_app_ids: Vec<String>,
    pub reason: Option<RemoteAppTemplateUnavailableReason>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum RemoteAppTemplateUnavailableReason {
    NotConfiguredForWorkspace,
    NoActiveWorkspace,
}

#[derive(Debug, Clone, PartialEq)]
pub struct RemotePluginSkill {
    pub name: String,
    pub description: String,
    pub short_description: Option<String>,
    pub interface: Option<SkillInterface>,
    pub enabled: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct RemotePluginSkillDetail {
    pub contents: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteDiscoverablePlugin {
    pub config_id: String,
    pub remote_plugin_id: String,
    pub name: String,
    pub description: Option<String>,
    pub has_skills: bool,
    pub app_ids: Vec<String>,
    pub install_policy: PluginInstallPolicy,
    pub availability: PluginAvailability,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecommendedPlugin {
    pub config_id: String,
    pub remote_plugin_id: String,
    pub display_name: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecommendedPluginsMode {
    Legacy,
    Endpoint { plugins: Vec<RecommendedPlugin> },
}

pub fn is_valid_remote_plugin_id(plugin_id: &str) -> bool {
    !plugin_id.is_empty()
        && plugin_id
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' || ch == '~')
}

pub fn validate_remote_plugin_id(plugin_id: &str) -> Result<(), JSONRPCErrorError> {
    if !is_valid_remote_plugin_id(plugin_id) {
        return Err(JSONRPCErrorError {
            code: INVALID_REQUEST_ERROR_CODE,
            message:
                "invalid remote plugin id: only ASCII letters, digits, `_`, `-`, and `~` are allowed"
                    .to_string(),
            data: None,
        });
    }

    Ok(())
}

#[derive(Debug, thiserror::Error)]
pub enum RemotePluginCatalogError {
    #[error("chatgpt authentication required for remote plugin catalog")]
    AuthRequired,

    #[error(
        "chatgpt authentication required for remote plugin catalog; api key auth is not supported"
    )]
    UnsupportedAuthMode,

    #[error("failed to read auth token for remote plugin catalog: {0}")]
    AuthToken(#[source] std::io::Error),

    #[error("failed to send remote plugin catalog request to {url}: {source}")]
    Request {
        url: String,
        #[source]
        source: RouteAwareRequestError,
    },

    #[error("remote plugin catalog request to {url} failed with status {status}: {body}")]
    UnexpectedStatus {
        url: String,
        status: StatusCode,
        body: String,
    },

    #[error("failed to parse remote plugin catalog response from {url}: {source}")]
    Decode {
        url: String,
        #[source]
        source: serde_json::Error,
    },

    #[error("invalid remote plugin catalog base URL: {0}")]
    InvalidBaseUrl(#[source] url::ParseError),

    #[error("invalid remote plugin catalog base URL path")]
    InvalidBaseUrlPath,

    #[error("remote marketplace `{marketplace_name}` is not supported")]
    UnknownMarketplace { marketplace_name: String },

    #[error(
        "remote plugin mutation returned unexpected plugin id: expected `{expected}`, got `{actual}`"
    )]
    UnexpectedPluginId { expected: String, actual: String },

    #[error(
        "remote plugin skill response returned unexpected skill name: expected `{expected}`, got `{actual}`"
    )]
    UnexpectedSkillName { expected: String, actual: String },

    #[error(
        "remote plugin mutation returned unexpected enabled state for `{plugin_id}`: expected {expected_enabled}, got {actual_enabled}"
    )]
    UnexpectedEnabledState {
        plugin_id: String,
        expected_enabled: bool,
        actual_enabled: bool,
    },

    #[error("{0}")]
    UnexpectedResponse(String),

    #[error("{0}")]
    CacheRemove(String),
}

impl RemotePluginCatalogError {
    /// Stable low-cardinality detail for plugin-install failure telemetry.
    pub fn sub_error_type(&self) -> Option<String> {
        match self {
            Self::UnexpectedStatus { status, .. } => {
                Some(http_status_sub_error_type(*status).to_string())
            }
            Self::AuthRequired
            | Self::UnsupportedAuthMode
            | Self::AuthToken(_)
            | Self::Request { .. }
            | Self::Decode { .. }
            | Self::InvalidBaseUrl(_)
            | Self::InvalidBaseUrlPath
            | Self::UnknownMarketplace { .. }
            | Self::UnexpectedPluginId { .. }
            | Self::UnexpectedSkillName { .. }
            | Self::UnexpectedEnabledState { .. }
            | Self::UnexpectedResponse(_)
            | Self::CacheRemove(_) => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Deserialize, Serialize)]
pub enum RemotePluginScope {
    #[serde(rename = "GLOBAL")]
    Global,
    #[serde(rename = "USER")]
    User,
    #[serde(rename = "WORKSPACE")]
    Workspace,
}


impl RemotePluginScope {
    #[cfg(test)]
    const CATALOG_CACHE_SCOPES: [Self; 3] = [Self::Global, Self::User, Self::Workspace];


    fn marketplace_name(self) -> &'static str {
        match self {
            Self::Global => REMOTE_GLOBAL_MARKETPLACE_NAME,
            Self::User => REMOTE_CREATED_BY_ME_MARKETPLACE_NAME,
            Self::Workspace => REMOTE_WORKSPACE_MARKETPLACE_NAME,
        }
    }

    fn marketplace_display_name(self) -> &'static str {
        match self {
            Self::Global => REMOTE_GLOBAL_MARKETPLACE_DISPLAY_NAME,
            Self::User => REMOTE_CREATED_BY_ME_MARKETPLACE_DISPLAY_NAME,
            Self::Workspace => REMOTE_WORKSPACE_MARKETPLACE_DISPLAY_NAME,
        }
    }

    pub(crate) fn from_marketplace_name(name: &str) -> Option<Self> {
        match name {
            REMOTE_GLOBAL_MARKETPLACE_NAME => Some(Self::Global),
            REMOTE_CREATED_BY_ME_MARKETPLACE_NAME => Some(Self::User),
            REMOTE_WORKSPACE_MARKETPLACE_NAME
            | REMOTE_WORKSPACE_SHARED_WITH_ME_MARKETPLACE_NAME
            | REMOTE_WORKSPACE_SHARED_WITH_ME_PRIVATE_MARKETPLACE_NAME
            | REMOTE_WORKSPACE_SHARED_WITH_ME_UNLISTED_MARKETPLACE_NAME => Some(Self::Workspace),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
struct RemotePluginSkillInterfaceResponse {
    display_name: Option<String>,
    short_description: Option<String>,
    brand_color: Option<String>,
    default_prompt: Option<String>,
    icon_small_url: Option<String>,
    icon_large_url: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
struct RemotePluginSkillResponse {
    name: String,
    description: String,
    interface: Option<RemotePluginSkillInterfaceResponse>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
struct RemotePluginSkillDetailResponse {
    plugin_id: String,
    name: String,
    skill_md_contents: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
struct RemotePluginReleaseInterfaceResponse {
    short_description: Option<String>,
    long_description: Option<String>,
    developer_name: Option<String>,
    category: Option<String>,
    #[serde(default)]
    capabilities: Vec<String>,
    website_url: Option<String>,
    privacy_policy_url: Option<String>,
    terms_of_service_url: Option<String>,
    brand_color: Option<String>,
    default_prompt: Option<String>,
    default_prompts: Option<Vec<String>>,
    composer_icon_url: Option<String>,
    logo_url: Option<String>,
    logo_url_dark: Option<String>,
    #[serde(default)]
    screenshot_urls: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
struct RemotePluginReleaseResponse {
    #[serde(default)]
    version: Option<String>,
    display_name: String,
    description: String,
    #[serde(default)]
    app_ids: Vec<String>,
    #[serde(default)]
    app_manifest: Option<JsonValue>,
    #[serde(default, alias = "unavailable_app_templates")]
    app_templates: Vec<RemoteAppTemplateResponse>,
    #[serde(default)]
    keywords: Vec<String>,
    interface: RemotePluginReleaseInterfaceResponse,
    #[serde(default)]
    skills: Vec<RemotePluginSkillResponse>,
    #[serde(default)]
    onboarding_skill_name: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    mcp_servers: Vec<RemotePluginMcpServerResponse>,
    scheduled_tasks: Option<Vec<ScheduledTaskSummary>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
struct RemotePluginMcpServerResponse {
    key: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
struct RemoteAppTemplateResponse {
    template_id: String,
    name: String,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    category: Option<String>,
    #[serde(default)]
    canonical_connector_id: Option<String>,
    #[serde(default)]
    logo_url: Option<String>,
    #[serde(default)]
    logo_url_dark: Option<String>,
    #[serde(default)]
    materialized_app_ids: Vec<String>,
    #[serde(default)]
    reason: Option<RemoteAppTemplateUnavailableReason>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
enum RemotePluginInstallPolicySource {
    #[serde(rename = "WORKSPACE_SETTING")]
    WorkspaceSetting,
    #[serde(rename = "IMPLICIT_CANONICAL_APP")]
    ImplicitCanonicalApp,
    #[serde(other)]
    Unknown,
}

impl RemotePluginInstallPolicySource {
    fn into_protocol(self) -> Option<PluginInstallPolicySource> {
        match self {
            Self::WorkspaceSetting => Some(PluginInstallPolicySource::WorkspaceSetting),
            Self::ImplicitCanonicalApp => Some(PluginInstallPolicySource::ImplicitCanonicalApp),
            Self::Unknown => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub(crate) struct RemotePluginDirectoryItem {
    id: String,
    name: String,
    #[serde(default)]
    canonical_app_id: Option<String>,
    scope: RemotePluginScope,
    #[serde(default)]
    discoverability: Option<RemotePluginShareDiscoverability>,
    #[serde(default)]
    creator_account_user_id: Option<String>,
    #[serde(default)]
    creator_name: Option<String>,
    #[serde(default)]
    share_url: Option<String>,
    #[serde(default)]
    share_principals: Option<Vec<RemotePluginDirectorySharePrincipal>>,
    #[serde(default)]
    can_publish_to_workspace: Option<bool>,
    installation_policy: PluginInstallPolicy,
    installation_policy_source: Option<RemotePluginInstallPolicySource>,
    #[serde(default)]
    must_show_installation_interstitial: Option<bool>,
    authentication_policy: PluginAuthPolicy,
    #[serde(rename = "status", default)]
    availability: PluginAvailability,
    #[serde(default)]
    disabled_reason: Option<PluginDisabledReason>,
    #[serde(default)]
    eligible_plan_types: Option<Vec<String>>,
    release: RemotePluginReleaseResponse,
}

fn remote_plugin_canonical_marketplace_name(
    plugin: &RemotePluginDirectoryItem,
) -> Result<&'static str, RemotePluginCatalogError> {
    match plugin.scope {
        RemotePluginScope::Global => Ok(REMOTE_GLOBAL_MARKETPLACE_NAME),
        RemotePluginScope::User => Ok(REMOTE_CREATED_BY_ME_MARKETPLACE_NAME),
        RemotePluginScope::Workspace => match workspace_plugin_discoverability(plugin)? {
            RemotePluginShareDiscoverability::Listed => Ok(REMOTE_WORKSPACE_MARKETPLACE_NAME),
            RemotePluginShareDiscoverability::Private
            | RemotePluginShareDiscoverability::Unlisted => {
                Ok(REMOTE_WORKSPACE_SHARED_WITH_ME_MARKETPLACE_NAME)
            }
        },
    }
}

fn workspace_plugin_discoverability(
    plugin: &RemotePluginDirectoryItem,
) -> Result<RemotePluginShareDiscoverability, RemotePluginCatalogError> {
    plugin.discoverability.ok_or_else(|| {
        RemotePluginCatalogError::UnexpectedResponse(format!(
            "workspace plugin `{}` did not include discoverability",
            plugin.id
        ))
    })
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
struct RemotePluginDirectorySharePrincipal {
    principal_type: RemotePluginSharePrincipalType,
    principal_id: String,
    role: RemotePluginSharePrincipalRole,
    name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
struct RemotePluginInstalledItem {
    #[serde(flatten)]
    plugin: RemotePluginDirectoryItem,
    #[serde(default)]
    installed_at: Option<DateTime<Utc>>,
    enabled: bool,
    #[serde(default)]
    disabled_skill_names: Vec<String>,
}

pub async fn fetch_remote_marketplaces(
    config: &RemotePluginServiceConfig,
    auth: Option<&CodexAuth>,
    sources: &[RemoteMarketplaceSource],
    catalog_cache_root: Option<&Path>,
    catalog_cache_mode: RemotePluginCatalogCacheMode,
) -> Result<RemoteMarketplacesFetchOutcome, RemotePluginCatalogError> {
    let auth = ensure_chatgpt_auth(auth)?;
    let mut marketplaces = Vec::new();
    let mut catalog_cache_refresh_scopes = BTreeSet::new();
    let mut catalog_cache_used = false;

    for source in sources {
        match source {
            RemoteMarketplaceSource::Global => {
                let scope = RemotePluginScope::Global;
                let directory_plugins = fetch_directory_plugins_for_scope_with_cache(
                    catalog_cache_root,
                    config,
                    auth,
                    scope,
                    /*collection*/ None,
                    catalog_cache_mode,
                )
                .await?;
                if directory_plugins.cache_refresh_needed {
                    catalog_cache_refresh_scopes.insert(scope);
                }
                catalog_cache_used |= directory_plugins.catalog_cache_used;
                if let Some(marketplace) = build_remote_marketplace(
                    scope.marketplace_name(),
                    scope.marketplace_display_name(),
                    directory_plugins.plugins,
                    Vec::new(),
                    /*include_installed_only*/ false,
                )? {
                    marketplaces.push(marketplace);
                }
            }
            RemoteMarketplaceSource::CreatedByMeRemote => {
                let scope = RemotePluginScope::User;
                let directory_plugins = fetch_directory_plugins_for_scope_with_cache(
                    catalog_cache_root,
                    config,
                    auth,
                    scope,
                    /*collection*/ None,
                    catalog_cache_mode,
                )
                .await?;
                if directory_plugins.cache_refresh_needed {
                    catalog_cache_refresh_scopes.insert(scope);
                }
                catalog_cache_used |= directory_plugins.catalog_cache_used;
                if let Some(marketplace) = build_remote_marketplace(
                    scope.marketplace_name(),
                    scope.marketplace_display_name(),
                    directory_plugins.plugins,
                    Vec::new(),
                    /*include_installed_only*/ false,
                )? {
                    marketplaces.push(marketplace);
                }
            }
            RemoteMarketplaceSource::WorkspaceDirectory => {
                let scope = RemotePluginScope::Workspace;
                let directory_plugins = fetch_directory_plugins_for_scope_with_cache(
                    catalog_cache_root,
                    config,
                    auth,
                    scope,
                    /*collection*/ None,
                    catalog_cache_mode,
                )
                .await?;
                if directory_plugins.cache_refresh_needed {
                    catalog_cache_refresh_scopes.insert(scope);
                }
                catalog_cache_used |= directory_plugins.catalog_cache_used;
                if let Some(marketplace) = build_remote_marketplace(
                    scope.marketplace_name(),
                    scope.marketplace_display_name(),
                    directory_plugins.plugins,
                    Vec::new(),
                    /*include_installed_only*/ false,
                )? {
                    marketplaces.push(marketplace);
                }
            }
        }
    }

    Ok(RemoteMarketplacesFetchOutcome {
        marketplaces,
        catalog_cache_refresh_scopes,
        catalog_cache_used,
    })
}

pub fn invalidate_cached_remote_plugin_catalog_scopes(
    codex_home: &Path,
    config: &RemotePluginServiceConfig,
    auth: Option<&CodexAuth>,
    scopes: &[RemotePluginScope],
) {
    let Ok(auth) = ensure_chatgpt_auth(auth) else {
        return;
    };
    for scope in scopes {
        catalog_cache::remove_cached_directory_plugins(
            codex_home, config, auth, *scope, /*collection*/ None,
        );
        if *scope == RemotePluginScope::Global {
            catalog_cache::remove_cached_directory_plugins(
                codex_home,
                config,
                auth,
                *scope,
                Some(OPENAI_CURATED_REMOTE_COLLECTION_KEY),
            );
        }
    }
}


#[cfg(test)]
pub(crate) fn cached_remote_plugin_catalog_scopes(
    codex_home: &Path,
    config: &RemotePluginServiceConfig,
    auth: Option<&CodexAuth>,
) -> BTreeSet<RemotePluginScope> {
    let Ok(auth) = ensure_chatgpt_auth(auth) else {
        return BTreeSet::new();
    };
    RemotePluginScope::CATALOG_CACHE_SCOPES
        .into_iter()
        .filter(|scope| {
            catalog_cache::load_cached_directory_plugins(
                codex_home, config, auth, *scope, /*collection*/ None,
            )
            .is_some()
        })
        .collect()
}

pub fn cached_global_remote_discoverable_plugins(
    codex_home: &Path,
    config: &RemotePluginServiceConfig,
    auth: &CodexAuth,
) -> Vec<RemoteDiscoverablePlugin> {
    catalog_cache::load_cached_directory_plugins(
        codex_home,
        config,
        auth,
        RemotePluginScope::Global,
        /*collection*/ None,
    )
    .map(|cached| cached.plugins)
    .unwrap_or_default()
    .into_iter()
    .filter_map(
        |plugin| match remote_discoverable_plugin_from_directory_item(&plugin) {
            Ok(plugin) => Some(plugin),
            Err(err) => {
                tracing::warn!(error = %err, "ignoring cached remote plugin recommendation entry");
                None
            }
        },
    )
    .collect()
}

pub async fn fetch_openai_curated_remote_collection_marketplace(
    config: &RemotePluginServiceConfig,
    auth: Option<&CodexAuth>,
    catalog_cache_root: Option<&Path>,
    catalog_cache_mode: RemotePluginCatalogCacheMode,
) -> Result<RemoteMarketplaceFetchOutcome, RemotePluginCatalogError> {
    let auth = ensure_chatgpt_auth(auth)?;
    let scope = RemotePluginScope::Global;
    let directory_plugins = fetch_directory_plugins_for_scope_with_cache(
        catalog_cache_root,
        config,
        auth,
        scope,
        Some(OPENAI_CURATED_REMOTE_COLLECTION_KEY),
        catalog_cache_mode,
    )
    .await?;

    let marketplace = build_remote_marketplace(
        REMOTE_GLOBAL_MARKETPLACE_NAME,
        REMOTE_GLOBAL_MARKETPLACE_DISPLAY_NAME,
        directory_plugins.plugins,
        Vec::new(),
        /*include_installed_only*/ false,
    )?;
    Ok(RemoteMarketplaceFetchOutcome {
        marketplace,
        catalog_cache_used: directory_plugins.catalog_cache_used,
    })
}

fn build_remote_marketplace(
    name: &str,
    display_name: &str,
    directory_plugins: Vec<RemotePluginDirectoryItem>,
    installed_plugins: Vec<RemotePluginInstalledItem>,
    include_installed_only: bool,
) -> Result<Option<RemoteMarketplace>, RemotePluginCatalogError> {
    let mut installed_plugins = installed_plugins
        .into_iter()
        .map(|plugin| (plugin.plugin.id.clone(), plugin))
        .collect::<BTreeMap<_, _>>();
    let mut plugins = directory_plugins
        .into_iter()
        .map(|plugin| {
            let installed_plugin = installed_plugins.remove(&plugin.id);
            build_remote_plugin_summary(&plugin, installed_plugin.as_ref())
        })
        .collect::<Result<Vec<_>, _>>()?;
    if include_installed_only {
        plugins.extend(
            installed_plugins
                .into_values()
                .map(|plugin| build_remote_plugin_summary(&plugin.plugin, Some(&plugin)))
                .collect::<Result<Vec<_>, _>>()?,
        );
    }
    if plugins.is_empty() {
        return Ok(None);
    }

    Ok(Some(RemoteMarketplace {
        name: name.to_string(),
        display_name: display_name.to_string(),
        plugins,
    }))
}


pub fn group_remote_installed_plugins_by_marketplaces(
    plugins: &[RemoteInstalledPlugin],
    visible_marketplaces: &[&str],
) -> Vec<RemoteMarketplace> {
    let mut plugins_by_marketplace = BTreeMap::<String, Vec<RemotePluginSummary>>::new();

    for plugin in plugins {
        if !visible_marketplaces.contains(&plugin.marketplace_name.as_str()) {
            continue;
        }
        let Ok(plugin_id) = PluginId::new(plugin.name.clone(), plugin.marketplace_name.clone())
        else {
            continue;
        };
        let plugin_summary = RemotePluginSummary {
            id: plugin_id.as_key(),
            remote_plugin_id: plugin.id.clone(),
            version: plugin.version.clone(),
            local_version: None,
            name: plugin.name.clone(),
            share_context: None,
            installed: true,
            installed_at: plugin.installed_at,
            enabled: plugin.enabled,
            install_policy: plugin.install_policy,
            install_policy_source: plugin.install_policy_source,
            must_show_installation_interstitial: plugin.must_show_installation_interstitial,
            auth_policy: plugin.auth_policy,
            availability: plugin.availability,
            disabled_reason: plugin.disabled_reason,
            eligible_plan_types: plugin.eligible_plan_types.clone(),
            interface: plugin.interface.clone(),
            keywords: plugin.keywords.clone(),
        };
        plugins_by_marketplace
            .entry(plugin.marketplace_name.clone())
            .or_default()
            .push(plugin_summary);
    }

    REMOTE_INSTALLED_MARKETPLACE_DISPLAY_ORDER
        .into_iter()
        .filter_map(|(marketplace_name, display_name)| {
            let mut marketplace_plugins = plugins_by_marketplace.remove(marketplace_name)?;
            sort_remote_plugin_summaries_by_display_name(&mut marketplace_plugins);
            Some(RemoteMarketplace {
                name: marketplace_name.to_string(),
                display_name: display_name.to_string(),
                plugins: marketplace_plugins,
            })
        })
        .collect()
}




pub async fn fetch_remote_plugin_skill_detail(
    config: &RemotePluginServiceConfig,
    auth: Option<&CodexAuth>,
    marketplace_name: &str,
    plugin_id: &str,
    skill_name: &str,
) -> Result<RemotePluginSkillDetail, RemotePluginCatalogError> {
    let auth = ensure_chatgpt_auth(auth)?;
    if RemotePluginScope::from_marketplace_name(marketplace_name).is_none() {
        return Err(RemotePluginCatalogError::UnknownMarketplace {
            marketplace_name: marketplace_name.to_string(),
        });
    }

    let url = remote_plugin_skill_detail_url(config, plugin_id, skill_name)?;
    let request = authenticated_request(
        config.http_request(Method::GET, &url),
        auth,
        &config.product_sku,
    );
    let response: RemotePluginSkillDetailResponse = send_and_decode(request, &url).await?;
    if response.plugin_id != plugin_id {
        return Err(RemotePluginCatalogError::UnexpectedPluginId {
            expected: plugin_id.to_string(),
            actual: response.plugin_id,
        });
    }
    if response.name != skill_name {
        return Err(RemotePluginCatalogError::UnexpectedSkillName {
            expected: skill_name.to_string(),
            actual: response.name,
        });
    }

    Ok(RemotePluginSkillDetail {
        contents: response.skill_md_contents,
    })
}


fn build_remote_plugin_summary(
    plugin: &RemotePluginDirectoryItem,
    installed_plugin: Option<&RemotePluginInstalledItem>,
) -> Result<RemotePluginSummary, RemotePluginCatalogError> {
    let marketplace_name = remote_plugin_canonical_marketplace_name(plugin)?;
    let plugin_id =
        PluginId::new(plugin.name.clone(), marketplace_name.to_string()).map_err(|err| {
            RemotePluginCatalogError::UnexpectedResponse(format!(
                "invalid remote plugin config id for `{}` in `{marketplace_name}`: {err}",
                plugin.name
            ))
        })?;
    Ok(RemotePluginSummary {
        id: plugin_id.as_key(),
        remote_plugin_id: plugin.id.clone(),
        version: plugin.release.version.clone(),
        local_version: installed_plugin
            .and_then(|installed| installed.plugin.release.version.clone()),
        name: plugin.name.clone(),
        share_context: remote_plugin_share_context(plugin)?,
        installed: installed_plugin.is_some(),
        installed_at: installed_plugin.and_then(|installed| installed.installed_at),
        enabled: installed_plugin.is_some_and(|plugin| plugin.enabled),
        install_policy: plugin.installation_policy,
        install_policy_source: plugin
            .installation_policy_source
            .and_then(RemotePluginInstallPolicySource::into_protocol),
        must_show_installation_interstitial: plugin.must_show_installation_interstitial,
        auth_policy: plugin.authentication_policy,
        availability: plugin.availability,
        disabled_reason: plugin.disabled_reason,
        eligible_plan_types: plugin.eligible_plan_types.clone(),
        interface: remote_plugin_interface_to_info(plugin),
        keywords: plugin.release.keywords.clone(),
    })
}

fn remote_discoverable_plugin_from_directory_item(
    plugin: &RemotePluginDirectoryItem,
) -> Result<RemoteDiscoverablePlugin, RemotePluginCatalogError> {
    let marketplace_name = remote_plugin_canonical_marketplace_name(plugin)?;
    let plugin_id =
        PluginId::new(plugin.name.clone(), marketplace_name.to_string()).map_err(|err| {
            RemotePluginCatalogError::UnexpectedResponse(format!(
                "invalid remote plugin config id for `{}` in `{marketplace_name}`: {err}",
                plugin.name
            ))
        })?;
    let display_name =
        non_empty_string(Some(&plugin.release.display_name)).unwrap_or_else(|| plugin.name.clone());
    let description = non_empty_string(plugin.release.interface.short_description.as_deref())
        .or_else(|| non_empty_string(Some(&plugin.release.description)));

    Ok(RemoteDiscoverablePlugin {
        config_id: plugin_id.as_key(),
        remote_plugin_id: plugin.id.clone(),
        name: display_name,
        description,
        has_skills: !plugin.release.skills.is_empty(),
        app_ids: plugin.release.app_ids.clone(),
        install_policy: plugin.installation_policy,
        availability: plugin.availability,
    })
}

fn remote_plugin_share_context(
    plugin: &RemotePluginDirectoryItem,
) -> Result<Option<RemotePluginShareContext>, RemotePluginCatalogError> {
    match plugin.scope {
        RemotePluginScope::Global | RemotePluginScope::User => Ok(None),
        RemotePluginScope::Workspace => {
            let discoverability = workspace_plugin_discoverability(plugin)?;
            Ok(Some(RemotePluginShareContext {
                remote_plugin_id: plugin.id.clone(),
                remote_version: plugin.release.version.clone(),
                discoverability,
                share_url: plugin.share_url.clone(),
                creator_account_user_id: plugin.creator_account_user_id.clone(),
                creator_name: plugin.creator_name.clone(),
                share_principals: plugin.share_principals.as_ref().map(|share_principals| {
                    share_principals
                        .iter()
                        .map(|principal| RemotePluginSharePrincipal {
                            principal_type: principal.principal_type,
                            principal_id: principal.principal_id.clone(),
                            role: principal.role,
                            name: principal.name.clone(),
                        })
                        .collect()
                }),
                can_publish_to_workspace: plugin.can_publish_to_workspace,
            }))
        }
    }
}

#[cfg(test)]
fn remote_installed_plugin_to_cache_entry(
    installed_plugin: &RemotePluginInstalledItem,
) -> Result<RemoteInstalledPlugin, RemotePluginCatalogError> {
    let plugin = &installed_plugin.plugin;
    let marketplace_name = remote_plugin_canonical_marketplace_name(plugin)?.to_string();
    PluginId::new(plugin.name.clone(), marketplace_name.clone()).map_err(|err| {
        RemotePluginCatalogError::UnexpectedResponse(format!(
            "invalid remote plugin config id for `{}` in `{marketplace_name}`: {err}",
            plugin.name
        ))
    })?;
    // Remote per-skill disabled state (`disabled_skill_names`) is intentionally
    // not projected into skills/list yet; local skills.config remains the
    // supported source for skill enablement.
    Ok(RemoteInstalledPlugin {
        marketplace_name,
        id: plugin.id.clone(),
        version: plugin.release.version.clone(),
        name: plugin.name.clone(),
        canonical_app_id: plugin.canonical_app_id.clone(),
        installed_at: installed_plugin.installed_at,
        enabled: installed_plugin.enabled,
        install_policy: plugin.installation_policy,
        install_policy_source: plugin
            .installation_policy_source
            .and_then(RemotePluginInstallPolicySource::into_protocol),
        must_show_installation_interstitial: plugin.must_show_installation_interstitial,
        auth_policy: plugin.authentication_policy,
        availability: plugin.availability,
        disabled_reason: plugin.disabled_reason,
        eligible_plan_types: plugin.eligible_plan_types.clone(),
        interface: remote_plugin_interface_to_info(plugin),
        keywords: plugin.release.keywords.clone(),
    })
}

fn remote_plugin_interface_to_info(plugin: &RemotePluginDirectoryItem) -> Option<PluginInterface> {
    let interface = &plugin.release.interface;
    let display_name = non_empty_string(Some(&plugin.release.display_name));
    let default_prompt = interface
        .default_prompts
        .as_deref()
        .and_then(normalize_remote_default_prompts)
        .or_else(|| {
            interface
                .default_prompt
                .as_deref()
                .and_then(normalize_remote_default_prompt)
                .map(|prompt| vec![prompt])
        });
    let result = PluginInterface {
        display_name,
        short_description: interface.short_description.clone(),
        long_description: interface.long_description.clone(),
        developer_name: interface.developer_name.clone(),
        category: interface.category.clone(),
        capabilities: interface.capabilities.clone(),
        website_url: interface.website_url.clone(),
        privacy_policy_url: interface.privacy_policy_url.clone(),
        terms_of_service_url: interface.terms_of_service_url.clone(),
        default_prompt,
        brand_color: interface.brand_color.clone(),
        composer_icon: None,
        composer_icon_url: interface.composer_icon_url.clone(),
        logo: None,
        logo_dark: None,
        logo_url: interface.logo_url.clone(),
        logo_url_dark: interface.logo_url_dark.clone(),
        screenshots: Vec::new(),
        screenshot_urls: interface.screenshot_urls.clone(),
    };
    let has_fields = result.display_name.is_some()
        || result.short_description.is_some()
        || result.long_description.is_some()
        || result.developer_name.is_some()
        || result.category.is_some()
        || !result.capabilities.is_empty()
        || result.website_url.is_some()
        || result.privacy_policy_url.is_some()
        || result.terms_of_service_url.is_some()
        || result.default_prompt.is_some()
        || result.brand_color.is_some()
        || result.composer_icon_url.is_some()
        || result.logo_url.is_some()
        || result.logo_url_dark.is_some()
        || !result.screenshot_urls.is_empty();
    has_fields.then_some(result)
}

fn remote_plugin_display_name(plugin: &RemotePluginSummary) -> &str {
    plugin
        .interface
        .as_ref()
        .and_then(|interface| interface.display_name.as_deref())
        .unwrap_or(&plugin.name)
}

fn sort_remote_plugin_summaries_by_display_name(plugins: &mut [RemotePluginSummary]) {
    plugins.sort_by(|left, right| {
        let left_display_name = remote_plugin_display_name(left);
        let right_display_name = remote_plugin_display_name(right);
        left_display_name
            .to_ascii_lowercase()
            .cmp(&right_display_name.to_ascii_lowercase())
            .then_with(|| left_display_name.cmp(right_display_name))
            .then_with(|| left.id.cmp(&right.id))
    });
}

fn non_empty_string(value: Option<&str>) -> Option<String> {
    value.and_then(|value| {
        let value = value.trim();
        (!value.is_empty()).then(|| value.to_string())
    })
}

fn normalize_remote_default_prompts(prompts: &[String]) -> Option<Vec<String>> {
    let prompts = prompts
        .iter()
        .filter_map(|prompt| normalize_remote_default_prompt(prompt))
        .take(MAX_REMOTE_DEFAULT_PROMPT_COUNT)
        .collect::<Vec<_>>();
    (!prompts.is_empty()).then_some(prompts)
}

fn normalize_remote_default_prompt(prompt: &str) -> Option<String> {
    let prompt = prompt.trim();
    if prompt.is_empty() || prompt.chars().count() > MAX_REMOTE_DEFAULT_PROMPT_LEN {
        return None;
    }
    Some(prompt.to_string())
}

struct DirectoryPluginsFetchOutcome {
    plugins: Vec<RemotePluginDirectoryItem>,
    cache_refresh_needed: bool,
    catalog_cache_used: bool,
}

async fn fetch_directory_plugins_for_scope_with_cache(
    codex_home: Option<&Path>,
    config: &RemotePluginServiceConfig,
    auth: &CodexAuth,
    scope: RemotePluginScope,
    collection: Option<&str>,
    cache_mode: RemotePluginCatalogCacheMode,
) -> Result<DirectoryPluginsFetchOutcome, RemotePluginCatalogError> {
    let _ = cache_mode;
    let Some(codex_home) = codex_home else {
        return Ok(DirectoryPluginsFetchOutcome {
            plugins: Vec::new(),
            cache_refresh_needed: false,
            catalog_cache_used: false,
        });
    };
    let Some(cached) =
        catalog_cache::load_cached_directory_plugins(codex_home, config, auth, scope, collection)
    else {
        return Ok(DirectoryPluginsFetchOutcome {
            plugins: Vec::new(),
            cache_refresh_needed: false,
            catalog_cache_used: false,
        });
    };
    Ok(DirectoryPluginsFetchOutcome {
        plugins: cached.plugins,
        cache_refresh_needed: false,
        catalog_cache_used: true,
    })
}








fn remote_plugin_skill_detail_url(
    config: &RemotePluginServiceConfig,
    plugin_id: &str,
    skill_name: &str,
) -> Result<String, RemotePluginCatalogError> {
    let mut url = Url::parse(config.chatgpt_base_url.trim_end_matches('/'))
        .map_err(RemotePluginCatalogError::InvalidBaseUrl)?;
    {
        let mut segments = url
            .path_segments_mut()
            .map_err(|()| RemotePluginCatalogError::InvalidBaseUrlPath)?;
        segments.pop_if_empty();
        segments.push("ps");
        segments.push("plugins");
        segments.push(plugin_id);
        segments.push("skills");
        segments.push(skill_name);
    }
    Ok(url.to_string())
}

fn ensure_chatgpt_auth(auth: Option<&CodexAuth>) -> Result<&CodexAuth, RemotePluginCatalogError> {
    let Some(auth) = auth else {
        return Err(RemotePluginCatalogError::AuthRequired);
    };
    if !auth.uses_codex_backend() {
        return Err(RemotePluginCatalogError::UnsupportedAuthMode);
    }
    Ok(auth)
}

fn authenticated_request(
    request: RequestBuilder,
    auth: &CodexAuth,
    product_sku: &str,
) -> RequestBuilder {
    request
        .timeout(REMOTE_PLUGIN_CATALOG_TIMEOUT)
        .headers(codex_model_provider::auth_provider_from_auth(auth).to_auth_headers())
        .header(OAI_PRODUCT_SKU_HEADER, product_sku)
}

async fn send_and_decode<T: for<'de> Deserialize<'de>>(
    request: RequestBuilder,
    url: &str,
) -> Result<T, RemotePluginCatalogError> {
    let response = request
        .send()
        .await
        .map_err(|source| RemotePluginCatalogError::Request {
            url: url.to_string(),
            source,
        })?;
    let status = response.status();
    let body = response
        .text()
        .await
        .map_err(|source| RemotePluginCatalogError::Request {
            url: url.to_string(),
            source,
        })?;
    if !status.is_success() {
        return Err(RemotePluginCatalogError::UnexpectedStatus {
            url: url.to_string(),
            status,
            body,
        });
    }

    serde_json::from_str(&body).map_err(|source| RemotePluginCatalogError::Decode {
        url: url.to_string(),
        source,
    })
}
