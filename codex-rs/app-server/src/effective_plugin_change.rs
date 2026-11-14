use codex_core::McpRefreshReason;
use std::sync::Arc;

use codex_core::ThreadManager;

/// Refresh plugin consumers after the effective plugin set changed.
pub(crate) fn effective_plugins_changed_callback(
    thread_manager: Arc<ThreadManager>,
) -> Arc<dyn Fn() + Send + Sync> {
    Arc::new(move || {
        thread_manager.plugins_manager().clear_cache();
        thread_manager.skills_service().clear_cache();

        let refresh_thread_manager = Arc::clone(&thread_manager);
        tokio::spawn(async move {
            refresh_thread_manager
                .invalidate_mcp_runtimes_for(McpRefreshReason::EffectivePluginsChanged)
                .await;
            refresh_thread_manager.refresh_hook_runtimes().await;
        });
    })
}
