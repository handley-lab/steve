use std::collections::HashMap;
use std::sync::Arc;

use codex_mcp::CODEX_APPS_MCP_SERVER_NAME;
use codex_mcp::McpPluginAttribution;
use codex_mcp::McpServerRegistration;
use codex_mcp::ResolvedMcpCatalog;
use codex_mcp::ToolInfo;
use codex_tools::ToolExposure;
use codex_tools::ToolName;
use pretty_assertions::assert_eq;
use rmcp::model::JsonObject;
use rmcp::model::MetaObject;
use rmcp::model::Tool;

use super::*;
use crate::config::CONFIG_TOML_FILE;
use crate::config::ConfigBuilder;
use tempfile::tempdir;

fn make_mcp_tool(
    server_name: &str,
    tool_name: &str,
    callable_namespace: &str,
    callable_name: &str,
    connector_id: Option<&str>,
    connector_name: Option<&str>,
) -> ToolInfo {
    ToolInfo {
        server_name: server_name.to_string(),
        supports_parallel_tool_calls: false,
        server_origin: None,
        callable_name: callable_name.to_string(),
        callable_namespace: callable_namespace.to_string(),
        namespace_description: None,
        tool: Tool::new(
            tool_name.to_string(),
            format!("Test tool: {tool_name}"),
            Arc::new(JsonObject::default()),
        ),
        connector_id: connector_id.map(str::to_string),
        connector_name: connector_name.map(str::to_string),
        plugin_display_names: Vec::new(),
    }
}

fn numbered_mcp_tools(count: usize) -> Vec<ToolInfo> {
    (0..count)
        .map(|index| {
            let tool_name = format!("tool_{index}");
            make_mcp_tool(
                "rmcp",
                &tool_name,
                "mcp__rmcp",
                &tool_name,
                /*connector_id*/ None,
                /*connector_name*/ None,
            )
        })
        .collect()
}

fn expected_runtimes(
    tools: &[ToolInfo],
    exposure: ToolExposure,
) -> HashMap<ToolName, ToolExposure> {
    tools
        .iter()
        .map(|tool| (tool.canonical_tool_name(), exposure))
        .collect()
}

fn runtimes_by_name(
    tools: &[ToolInfo],
    search_tool_enabled: bool,
) -> HashMap<ToolName, ToolExposure> {
    runtimes_by_name_with_catalog(tools, &ResolvedMcpCatalog::default(), search_tool_enabled)
}

fn runtimes_by_name_with_catalog(
    tools: &[ToolInfo],
    mcp_server_catalog: &ResolvedMcpCatalog,
    search_tool_enabled: bool,
) -> HashMap<ToolName, ToolExposure> {
    let mut handlers = HashMap::new();
    let mut registry = ToolRegistry::default();
    append_mcp_tools(
        tools,
        mcp_server_catalog,
        search_tool_enabled,
        &mut handlers,
        &SharedMcpDefinitions::default(),
        &mut registry,
    );
    registry
        .entries()
        .map(|tool| (tool.runtime.tool_name(), tool.exposure))
        .collect()
}

#[tokio::test]
async fn agent_plugin_budget_hides_only_overflow_agent_tools() {
    let codex_home = tempdir().expect("tempdir should succeed");
    std::fs::write(
        codex_home.path().join(CONFIG_TOML_FILE),
        "[mcp_servers.agent]\ncommand = \"echo\"\n",
    )
    .expect("write config");
    let config = ConfigBuilder::default()
        .codex_home(codex_home.path().to_path_buf())
        .build()
        .await
        .expect("config should build");
    let agent_config = config.mcp_servers.get()["agent"].clone();
    let legacy_config = agent_config.clone();
    let mut catalog = ResolvedMcpCatalog::builder();
    catalog.register(McpServerRegistration::from_plugin(
        "agent".to_string(),
        McpPluginAttribution::agent_plugin("agent@test".to_string(), "Agent".to_string()),
        /*plugin_order*/ 0,
        agent_config,
    ));
    catalog.register(McpServerRegistration::from_plugin(
        "legacy".to_string(),
        McpPluginAttribution::new("legacy@test".to_string(), "Legacy".to_string()),
        /*plugin_order*/ 1,
        legacy_config,
    ));
    let catalog = catalog.build();
    let mut tools = (0..40)
        .map(|index| {
            let name = format!("tool_{index}");
            let mut tool = make_mcp_tool(
                "agent",
                &name,
                "mcp__agent",
                &name,
                /*connector_id*/ None,
                /*connector_name*/ None,
            );
            tool.namespace_description = Some("n".repeat(1_000));
            tool.tool.description = Some("d".repeat(1_000).into());
            tool
        })
        .collect::<Vec<_>>();
    let oversized_name = "x".repeat(MAX_AGENT_PLUGIN_MCP_SPEC_BYTES);
    let oversized_agent_tool = make_mcp_tool(
        "agent",
        "oversized_agent_tool",
        "mcp__agent",
        &oversized_name,
        /*connector_id*/ None,
        /*connector_name*/ None,
    );
    tools.push(oversized_agent_tool.clone());
    let legacy_tool = make_mcp_tool(
        "legacy",
        "legacy_tool",
        "mcp__legacy",
        &oversized_name,
        /*connector_id*/ None,
        /*connector_name*/ None,
    );
    tools.push(legacy_tool.clone());

    let runtimes =
        runtimes_by_name_with_catalog(&tools, &catalog, /*search_tool_enabled*/ false);
    let agent_exposures = tools[..40]
        .iter()
        .map(|tool| runtimes[&tool.canonical_tool_name()])
        .collect::<Vec<_>>();

    assert!(agent_exposures.contains(&ToolExposure::Direct));
    assert!(agent_exposures.contains(&ToolExposure::Hidden));
    assert_eq!(
        runtimes[&oversized_agent_tool.canonical_tool_name()],
        ToolExposure::Hidden
    );
    assert_eq!(
        runtimes[&legacy_tool.canonical_tool_name()],
        ToolExposure::Direct
    );
}

fn with_visibility(mut tool: ToolInfo, visibility: &[&str]) -> ToolInfo {
    tool.tool.meta = Some(MetaObject(
        serde_json::json!({ "ui": { "visibility": visibility } })
            .as_object()
            .expect("metadata object")
            .clone(),
    ));
    tool
}

#[tokio::test]
async fn directly_exposes_effective_tool_sets_when_search_is_unavailable() {
    let mcp_tools = numbered_mcp_tools(/*count*/ 2);

    let runtimes = runtimes_by_name(&mcp_tools, /*search_tool_enabled*/ false);

    assert_eq!(
        runtimes,
        expected_runtimes(&mcp_tools, ToolExposure::Direct)
    );
}

#[tokio::test]
async fn excludes_tools_hidden_from_model_exposure() {
    let visible_tool = make_mcp_tool(
        "rmcp",
        "visible_tool",
        "mcp__rmcp",
        "visible_tool",
        /*connector_id*/ None,
        /*connector_name*/ None,
    );
    let hidden_tool = with_visibility(
        make_mcp_tool(
            "rmcp",
            "hidden_tool",
            "mcp__rmcp",
            "hidden_tool",
            /*connector_id*/ None,
            /*connector_name*/ None,
        ),
        &["app"],
    );
    let empty_visibility_tool = with_visibility(
        make_mcp_tool(
            "rmcp",
            "empty_visibility_tool",
            "mcp__rmcp",
            "empty_visibility_tool",
            /*connector_id*/ None,
            /*connector_name*/ None,
        ),
        &[],
    );
    let visible_app_tool = with_visibility(
        make_mcp_tool(
            CODEX_APPS_MCP_SERVER_NAME,
            "calendar_read",
            "mcp__codex_apps__calendar",
            "read",
            Some("calendar"),
            Some("Calendar"),
        ),
        &["app", "model"],
    );
    let hidden_app_tool = with_visibility(
        make_mcp_tool(
            CODEX_APPS_MCP_SERVER_NAME,
            "calendar_open",
            "mcp__codex_apps__calendar",
            "open",
            Some("calendar"),
            Some("Calendar"),
        ),
        &["app"],
    );
    let mcp_tools = vec![
        visible_tool.clone(),
        hidden_tool,
        empty_visibility_tool,
        visible_app_tool.clone(),
        hidden_app_tool,
    ];
    let runtimes = runtimes_by_name(&mcp_tools, /*search_tool_enabled*/ false);

    assert_eq!(
        runtimes,
        expected_runtimes(&[visible_tool, visible_app_tool], ToolExposure::Direct)
    );
}

#[tokio::test]
async fn defers_effective_tool_sets_when_search_is_available() {
    let mcp_tools = numbered_mcp_tools(/*count*/ 2);

    let runtimes = runtimes_by_name(&mcp_tools, /*search_tool_enabled*/ true);

    assert_eq!(
        runtimes,
        expected_runtimes(&mcp_tools, ToolExposure::Deferred)
    );
}

#[tokio::test]
async fn defers_apps_and_non_app_mcp_tools() {
    let mcp_tools = vec![
        make_mcp_tool(
            "rmcp",
            "tool",
            "mcp__rmcp",
            "tool",
            /*connector_id*/ None,
            /*connector_name*/ None,
        ),
        make_mcp_tool(
            CODEX_APPS_MCP_SERVER_NAME,
            "calendar_create_event",
            "mcp__codex_apps__calendar",
            "_create_event",
            Some("calendar"),
            Some("Calendar"),
        ),
    ];
    let runtimes = runtimes_by_name(&mcp_tools, /*search_tool_enabled*/ true);

    assert_eq!(
        runtimes,
        expected_runtimes(&mcp_tools, ToolExposure::Deferred)
    );
}
