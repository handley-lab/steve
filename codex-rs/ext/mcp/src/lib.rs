mod cloud_plugin;
#[cfg(test)]
#[path = "event_stream_tests.rs"]
mod event_stream_tests;
mod plugin;
mod plugin_contributor;
mod plugin_contributor_state;
mod plugin_providers;
mod stream_manager;

pub use codex_core_plugins::PluginListQuery;
pub use codex_core_plugins::PluginProvider;
pub use codex_core_plugins::PluginProviderError;
pub use codex_core_plugins::PluginProviderFuture;
pub use codex_core_plugins::PluginProviderResult;
pub use plugin_contributor::install_plugin_providers;
pub use plugin_contributor::install_plugins;
pub use plugin_contributor_state::PluginsThreadState;
pub use plugin_providers::PluginProviders;
pub use stream_manager::McpEventStreamManager;
pub use stream_manager::McpEventStreamUpdate;

#[cfg(test)]
#[path = "stream_manager_tests.rs"]
mod stream_manager_tests;
