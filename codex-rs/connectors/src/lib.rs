use std::sync::LazyLock;
use std::sync::Mutex as StdMutex;
use std::time::Duration;

use serde::Deserialize;
use serde::Serialize;

pub mod accessible;
mod app_info;
mod app_tool_policy;
mod connector_runtime;
mod directory_cache;
pub mod filter;
pub mod merge;
pub mod metadata;
mod metadata_store;
mod plugin_config;
mod runtime_projection;
mod snapshot;

pub use app_info::AppBranding;
pub use app_info::AppInfo;
pub use app_info::AppMetadata;
pub use app_info::AppReview;
pub use app_info::AppScreenshot;
pub use app_tool_policy::AppToolPolicy;
pub use app_tool_policy::AppToolPolicyEvaluator;
pub use app_tool_policy::AppToolPolicyInput;
pub use app_tool_policy::app_is_enabled;
pub use app_tool_policy::apps_config_from_layer_stack;
pub use connector_runtime::ConnectorRuntimeContext;
pub use connector_runtime::ConnectorRuntimeContextKey;
pub use connector_runtime::ConnectorRuntimeFetchSource;
pub use connector_runtime::ConnectorRuntimeFetchTicket;
pub use connector_runtime::ConnectorRuntimeManager;
pub use connector_runtime::ConnectorRuntimePayload;
pub use connector_runtime::ConnectorRuntimeSnapshot;
pub use connector_runtime::connector_runtime_cache_path;
pub use connector_runtime::connector_runtime_context_key;
pub use directory_cache::ConnectorDirectoryCacheContext;
pub use metadata_store::ConnectorMetadata;
pub use metadata_store::ConnectorMetadataStore;
pub use metadata_store::ConnectorToolSummary;
pub use plugin_config::parse_plugin_app_config;
pub use plugin_config::parse_plugin_app_config_value;
pub use runtime_projection::ConnectorRuntimeTool;
pub use runtime_projection::InstalledConnectorRuntime;
pub use runtime_projection::connector_tool_is_synthetic;
pub use runtime_projection::installed_connector_runtime;
pub use snapshot::ConnectorSnapshot;
pub use snapshot::PluginConnectorSource;

pub const CONNECTORS_CACHE_TTL: Duration = Duration::from_secs(3600);
/// TTL for app/read metadata; it starts aligned with the connector directory cache.
pub const CONNECTOR_METADATA_CACHE_TTL: Duration = CONNECTORS_CACHE_TTL;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConnectorDirectoryCacheKey {
    chatgpt_base_url: String,
    account_id: Option<String>,
    chatgpt_user_id: Option<String>,
    is_workspace_account: bool,
}

impl ConnectorDirectoryCacheKey {
    pub fn new(
        chatgpt_base_url: String,
        account_id: Option<String>,
        chatgpt_user_id: Option<String>,
        is_workspace_account: bool,
    ) -> Self {
        Self {
            chatgpt_base_url,
            account_id,
            chatgpt_user_id,
            is_workspace_account,
        }
    }
}

#[derive(Clone)]
struct CachedConnectorDirectory {
    key: ConnectorDirectoryCacheKey,
    connectors: Vec<AppInfo>,
}

static CONNECTOR_DIRECTORY_CACHE: LazyLock<StdMutex<Option<CachedConnectorDirectory>>> =
    LazyLock::new(|| StdMutex::new(None));

pub fn cached_directory_connectors(
    cache_context: &ConnectorDirectoryCacheContext,
) -> Option<Vec<AppInfo>> {
    if let Some(cached_connectors) = cached_directory_connectors_in_memory(&cache_context.cache_key)
    {
        return Some(cached_connectors);
    }

    let directory_cache::CachedConnectorDirectoryDiskLoad::Hit { connectors } =
        directory_cache::load_cached_directory_connectors_from_disk(cache_context)
    else {
        return None;
    };
    write_cached_directory_connectors_in_memory(cache_context.cache_key.clone(), &connectors);
    Some(connectors)
}

fn cached_directory_connectors_in_memory(
    cache_key: &ConnectorDirectoryCacheKey,
) -> Option<Vec<AppInfo>> {
    let cache_guard = CONNECTOR_DIRECTORY_CACHE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    cache_guard
        .as_ref()
        .filter(|cached| cached.key == *cache_key)
        .map(|cached| cached.connectors.clone())
}

fn write_cached_directory_connectors_in_memory(
    cache_key: ConnectorDirectoryCacheKey,
    connectors: &[AppInfo],
) {
    let mut cache_guard = CONNECTOR_DIRECTORY_CACHE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    *cache_guard = Some(CachedConnectorDirectory {
        key: cache_key,
        connectors: connectors.to_vec(),
    });
}

fn connector_install_url(name: &str, connector_id: &str) -> String {
    let chatgpt_base_url = std::env::var("STEVE_APP_SERVER_CHATGPT_BASE_URL")
        .unwrap_or_else(|_| "https://chatgpt.com".to_string());
    let chatgpt_origin = chatgpt_base_url
        .trim_end_matches('/')
        .trim_end_matches("/backend-api");
    let slug = connector_name_slug(name);
    format!("{chatgpt_origin}/apps/{slug}/{connector_id}")
}

fn connector_name_slug(name: &str) -> String {
    let mut normalized = String::with_capacity(name.len());
    for character in name.chars() {
        if character.is_ascii_alphanumeric() {
            normalized.push(character.to_ascii_lowercase());
        } else {
            normalized.push('-');
        }
    }
    let normalized = normalized.trim_matches('-');
    if normalized.is_empty() {
        "app".to_string()
    } else {
        normalized.to_string()
    }
}

fn normalize_connector_value(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    use tempfile::TempDir;

    static CONNECTOR_DIRECTORY_CACHE_TEST_LOCK: LazyLock<tokio::sync::Mutex<()>> =
        LazyLock::new(|| tokio::sync::Mutex::new(()));

    fn cache_key(id: &str) -> ConnectorDirectoryCacheKey {
        ConnectorDirectoryCacheKey::new(
            "https://chatgpt.example".to_string(),
            Some(format!("account-{id}")),
            Some(format!("user-{id}")),
            /*is_workspace_account*/ true,
        )
    }

    fn cache_context(codex_home: &TempDir, id: &str) -> ConnectorDirectoryCacheContext {
        ConnectorDirectoryCacheContext::new(codex_home.path().to_path_buf(), cache_key(id))
    }

    #[test]
    fn connector_install_url_uses_configured_origin() {
        let chatgpt_base_url = std::env::var("STEVE_APP_SERVER_CHATGPT_BASE_URL")
            .unwrap_or_else(|_| "https://chatgpt.com".to_string());
        let chatgpt_origin = chatgpt_base_url
            .trim_end_matches('/')
            .trim_end_matches("/backend-api");
        assert_eq!(
            connector_install_url("Google Calendar", "calendar"),
            format!("{chatgpt_origin}/apps/google-calendar/calendar"),
        );
    }

    fn clear_directory_memory_cache() {
        let mut cache_guard = CONNECTOR_DIRECTORY_CACHE
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *cache_guard = None;
    }

    #[tokio::test]
    async fn cached_directory_connectors_reads_directory_disk_cache() -> anyhow::Result<()> {
        let _cache_guard = CONNECTOR_DIRECTORY_CACHE_TEST_LOCK.lock().await;
        clear_directory_memory_cache();
        let codex_home = TempDir::new()?;
        let cache_context = cache_context(&codex_home, "disk");
        let cache_path = cache_context.cache_path();
        std::fs::create_dir_all(cache_path.parent().expect("cache parent"))?;
        let connectors = vec![AppInfo {
            id: "alpha".to_string(),
            name: "Alpha".to_string(),
            description: None,
            logo_url: None,
            logo_url_dark: None,
            icon_assets: None,
            icon_dark_assets: None,
            distribution_channel: None,
            branding: None,
            app_metadata: None,
            labels: None,
            install_url: None,
            is_accessible: false,
            is_enabled: true,
            plugin_display_names: Vec::new(),
        }];
        std::fs::write(
            &cache_path,
            serde_json::to_vec_pretty(&serde_json::json!({
                "schema_version": 1,
                "connectors": connectors,
            }))?,
        )?;
        let loaded = cached_directory_connectors(&cache_context).expect("disk cache should load");
        assert_eq!(loaded, connectors);
        Ok(())
    }

    #[tokio::test]
    async fn cached_directory_connectors_drops_stale_disk_schema() -> anyhow::Result<()> {
        let _cache_guard = CONNECTOR_DIRECTORY_CACHE_TEST_LOCK.lock().await;

        clear_directory_memory_cache();
        let codex_home = TempDir::new()?;
        let cache_context = cache_context(&codex_home, "stale-schema");
        let cache_path = cache_context.cache_path();
        std::fs::create_dir_all(cache_path.parent().expect("cache parent"))?;
        std::fs::write(
            &cache_path,
            serde_json::to_vec_pretty(&serde_json::json!({
                "schema_version": 0,
                "connectors": [],
            }))?,
        )?;

        assert_eq!(cached_directory_connectors(&cache_context), None);
        assert!(!cache_path.exists());
        Ok(())
    }
}
