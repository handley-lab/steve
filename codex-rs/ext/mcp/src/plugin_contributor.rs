//! Coordinates executor MCP contributions with thread-owned catalogs.
//!
//! Provider instances are shared, while catalogs and executor metadata live in thread
//! extension data.

use crate::PluginProviders;
use crate::PluginsThreadState;
use codex_core::config::Config;
use codex_core_plugins::ExecutorPluginProvider;
use codex_exec_server::EnvironmentManager;
use codex_extension_api::ExtensionFuture;
use codex_extension_api::ExtensionRegistryBuilder;
use codex_extension_api::ThreadLifecycleContributor;
use codex_extension_api::ThreadStartInput;
use std::sync::Arc;

pub(crate) struct PluginContributor {
    pub(crate) providers: PluginProviders,
}

/// Installs one coordinator for executor MCP callbacks; Core's plugin policy applies.
pub fn install_plugin_providers(
    builder: &mut ExtensionRegistryBuilder<Config>,
    providers: PluginProviders,
) {
    let contributor = Arc::new(PluginContributor { providers });
    builder.thread_lifecycle_contributor(contributor.clone());
    builder.mcp_server_contributor(contributor);
}

/// Installs plugin support backed by selected environment roots.
pub fn install_plugins(
    builder: &mut ExtensionRegistryBuilder<Config>,
    environment_manager: Arc<EnvironmentManager>,
) {
    install_plugin_providers(
        builder,
        PluginProviders::new(Arc::new(ExecutorPluginProvider::new(environment_manager))),
    );
}

impl ThreadLifecycleContributor<Config> for PluginContributor {
    fn on_thread_start<'a>(
        &'a self,
        input: ThreadStartInput<'a, Config>,
    ) -> ExtensionFuture<'a, ()> {
        Box::pin(async move {
            let state = input.thread_store.get_or_init(PluginsThreadState::default);
            state.contributor_state().extension_metrics = input.extension_metrics;
        })
    }
}
