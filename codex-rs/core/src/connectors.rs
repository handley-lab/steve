pub use codex_connectors::AppBranding;
pub use codex_connectors::AppInfo;
pub use codex_connectors::AppMetadata;
use codex_connectors::apps_config_from_layer_stack;

use codex_config::types::ApprovalsReviewer;
use codex_mcp::CODEX_APPS_MCP_SERVER_NAME;
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
    server_name: &str,
    connector_id: Option<&str>,
    link_id: Option<&str>,
) -> ApprovalsReviewer {
    let requirements = config_layer_stack.requirements();
    if model.is_some_and(|model| requirements.auto_review_required_for_model(model)) {
        return ApprovalsReviewer::AutoReview;
    }

    let app_reviewer = if server_name == CODEX_APPS_MCP_SERVER_NAME {
        apps_config_from_layer_stack(config_layer_stack).and_then(|apps_config| {
            let app = connector_id.and_then(|connector_id| apps_config.apps.get(connector_id));
            link_id
                .and_then(|link_id| app?.links.as_ref()?.links.get(link_id))
                .and_then(|link| link.approvals_reviewer)
                .or_else(|| app.and_then(|app| app.approvals_reviewer))
                .or_else(|| {
                    apps_config
                        .default
                        .and_then(|defaults| defaults.approvals_reviewer)
                })
        })
    } else {
        None
    };

    if let Some(reviewer) = app_reviewer
        && requirements.approvals_reviewer.can_set(&reviewer).is_ok()
    {
        return reviewer;
    }

    default_reviewer
}

#[cfg(test)]
#[path = "connectors_tests.rs"]
mod tests;
