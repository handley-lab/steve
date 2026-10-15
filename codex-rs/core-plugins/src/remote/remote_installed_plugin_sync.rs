use super::RemotePluginCapabilities;
use super::RemotePluginCatalogError;
use super::RemotePluginScope;
use super::RemotePluginShareDiscoverability;
use crate::store::PLUGINS_CACHE_DIR;
use crate::store::PluginStoreError;
use codex_plugin::PluginId;
use std::collections::HashMap;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::OnceLock;
use std::sync::Weak;
use tokio::sync::Semaphore;

static REMOTE_INSTALLED_PLUGIN_BUNDLE_SYNC_GATES: OnceLock<
    Mutex<HashMap<PathBuf, Weak<Semaphore>>>,
> = OnceLock::new();
static REMOTE_PLUGIN_CACHE_MUTATIONS_IN_FLIGHT: OnceLock<
    Mutex<HashMap<RemotePluginCacheMutationKey, usize>>,
> = OnceLock::new();

/// A remote plugin bundle newly installed or updated from an authenticated snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemotePluginMaterialization {
    pub plugin_id: PluginId,
    pub scope: RemotePluginScope,
    pub discoverability: Option<RemotePluginShareDiscoverability>,
    pub authenticated_account_id: Option<String>,
}

/// A local plugin and the runtime categories affected by its bundle or installed-state change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemotePluginChange {
    pub plugin_id: String,
    pub capabilities: RemotePluginCapabilities,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RemoteInstalledPluginBundleSyncOutcome {
    /// Internal provenance for materialization-owned hook trust, not runtime change reporting.
    pub materialized_remote_plugins: Vec<RemotePluginMaterialization>,
    /// Affected plugins with capabilities from either side of the change, including removals.
    /// Installed-state removals do not depend on cache cleanup succeeding.
    pub changed_plugins: Vec<RemotePluginChange>,
    pub failed_remote_plugin_ids: Vec<String>,
    /// Failures that leave an otherwise valid installed plugin unavailable locally.
    pub failed_materialization_remote_plugin_ids: Vec<String>,
}


#[derive(Debug, thiserror::Error)]
pub enum RemoteInstalledPluginBundleSyncError {
    #[error("{0}")]
    Catalog(#[from] RemotePluginCatalogError),

    #[error("{0}")]
    Store(#[from] PluginStoreError),

    #[error("timed out waiting for another remote plugin cache mutation; retry")]
    LockTimeout,

    #[error("remote plugin state changed during reconciliation; retry reconciliation")]
    Superseded,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct RemotePluginCacheMutationKey {
    plugin_cache_root: PathBuf,
    marketplace_name: String,
    plugin_name: String,
}

pub struct RemotePluginCacheMutationGuard {
    key: RemotePluginCacheMutationKey,
}

pub(crate) fn remote_installed_plugin_bundle_sync_gate(codex_home: &Path) -> Arc<Semaphore> {
    let plugin_cache_root = remote_plugin_cache_root(codex_home);
    let gates =
        REMOTE_INSTALLED_PLUGIN_BUNDLE_SYNC_GATES.get_or_init(|| Mutex::new(HashMap::new()));
    let mut gates = match gates.lock() {
        Ok(gates) => gates,
        Err(err) => err.into_inner(),
    };
    if let Some(gate) = gates.get(&plugin_cache_root).and_then(Weak::upgrade) {
        return gate;
    }
    let gate = Arc::new(Semaphore::new(/*permits*/ 1));
    gates.insert(plugin_cache_root, Arc::downgrade(&gate));
    gate
}



pub fn mark_remote_plugin_cache_mutation_in_flight(
    codex_home: &Path,
    marketplace_name: &str,
    plugin_name: &str,
) -> RemotePluginCacheMutationGuard {
    let key = RemotePluginCacheMutationKey {
        plugin_cache_root: remote_plugin_cache_root(codex_home),
        marketplace_name: marketplace_name.to_string(),
        plugin_name: plugin_name.to_string(),
    };
    let mutations =
        REMOTE_PLUGIN_CACHE_MUTATIONS_IN_FLIGHT.get_or_init(|| Mutex::new(HashMap::new()));
    let mut mutations = match mutations.lock() {
        Ok(mutations) => mutations,
        Err(err) => err.into_inner(),
    };
    *mutations.entry(key.clone()).or_default() += 1;
    RemotePluginCacheMutationGuard { key }
}

impl Drop for RemotePluginCacheMutationGuard {
    fn drop(&mut self) {
        let Some(mutations) = REMOTE_PLUGIN_CACHE_MUTATIONS_IN_FLIGHT.get() else {
            return;
        };
        let mut mutations = match mutations.lock() {
            Ok(mutations) => mutations,
            Err(err) => err.into_inner(),
        };
        if let Some(count) = mutations.get_mut(&self.key) {
            *count -= 1;
            if *count == 0 {
                mutations.remove(&self.key);
            }
        }
    }
}


fn remote_plugin_cache_root(codex_home: &Path) -> PathBuf {
    codex_home.join(PLUGINS_CACHE_DIR)
}


