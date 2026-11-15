pub use codex_connectors::AppBranding;
pub use codex_connectors::AppInfo;
pub use codex_connectors::AppMetadata;

use codex_config::types::ApprovalsReviewer;
use codex_mcp::ToolPluginContext;

#[derive(Debug, Clone)]
pub struct AccessibleConnectorsStatus {
    pub connectors: Vec<AppInfo>,
    pub codex_apps_ready: bool,
}

pub fn with_app_plugin_sources(
    mut connectors: Vec<AppInfo>,
    tool_plugin_context: &ToolPluginContext,
) -> Vec<AppInfo> {
    for connector in &mut connectors {
        connector.plugin_display_names = tool_plugin_context
            .plugin_display_names_for_connector_id(connector.id.as_str())
            .to_vec();
    }
    connectors
}

pub(crate) fn mcp_approvals_reviewer_from_layers(
    config_layer_stack: &codex_config::ConfigLayerStack,
    default_reviewer: ApprovalsReviewer,
    model: Option<&str>,
) -> ApprovalsReviewer {
    let requirements = config_layer_stack.requirements();
    if model.is_some_and(|model| requirements.auto_review_required_for_model(model)) {
        return ApprovalsReviewer::AutoReview;
    }

    default_reviewer
}
