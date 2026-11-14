use super::*;
use crate::McpPluginAttribution;
use crate::McpServerPermissions;
use crate::McpServerRegistration;
use crate::server::McpCredentialPolicy;
use codex_config::Constrained;
use codex_config::types::AppToolApproval;
use codex_config::types::AuthKeyringBackendKind;
use codex_plugin::PluginCapabilitySummary;
use codex_protocol::models::ManagedFileSystemPermissions;
use codex_protocol::models::PermissionProfile;
use codex_protocol::permissions::NetworkSandboxPolicy;
use codex_protocol::protocol::AskForApproval;
use codex_protocol::protocol::GranularApprovalConfig;
use pretty_assertions::assert_eq;
use std::collections::HashMap;
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;

pub(crate) fn test_mcp_config(codex_home: PathBuf) -> McpConfig {
    McpConfig {
        chatgpt_base_url: "https://chatgpt.com".to_string(),
        apps_mcp_product_sku: None,
        requires_read_only_mcp_tools: false,
        codex_home,
        mcp_oauth_credentials_store_mode: OAuthCredentialsStoreMode::default(),
        oauth_refresh_mode: McpOAuthRefreshMode::Legacy,
        auth_keyring_backend_kind: AuthKeyringBackendKind::default(),
        mcp_oauth_callback_port: None,
        mcp_oauth_callback_url: None,
        optional_mcp_startup_grace: DEFAULT_OPTIONAL_MCP_STARTUP_GRACE,
        skill_mcp_dependency_install_enabled: true,
        approval_policy: Constrained::allow_any(AskForApproval::OnRequest),
        permission_profile: PermissionProfile::default(),
        host_cwd: AbsolutePathBuf::current_dir().expect("test working directory"),
        config_layer_stack: codex_config::ConfigLayerStack::default(),
        plugins: Default::default(),
        approvals_reviewer: codex_config::types::ApprovalsReviewer::default(),
        environment_cwds: HashMap::new(),
        environment_use_mxc: HashMap::new(),
        server_permissions: HashMap::new(),
        codex_self_exe: None,
        codex_linux_sandbox_exe: None,
        use_legacy_landlock: false,
        apps_enabled: false,
        apps_early_resource_discovery: false,
        prefix_mcp_tool_names: true,
        non_prefixed_mcp_tool_servers: Vec::new(),
        protocol_mode: McpProtocolMode::Legacy,
        discover_local_stdio: false,
        mcp_tool_list_changed: true,
        host_owned_apps_protocol_mode: McpProtocolMode::Legacy,
        client_elicitation_capability: ElicitationCapability::default(),
        mcp_server_catalog: ResolvedMcpCatalog::default(),
        connector_snapshot: codex_connectors::ConnectorSnapshot::default(),
    }
}

pub(crate) fn test_elicitation_config(
    server_name: &str,
    approval_policy: AskForApproval,
    permission_profile: PermissionProfile,
) -> Arc<McpConfig> {
    let mut config = test_mcp_config(PathBuf::new());
    config.approval_policy = Constrained::allow_any(approval_policy);
    config.permission_profile = permission_profile;
    config
        .server_permissions
        .insert(server_name.to_string(), McpServerPermissions::Thread);
    Arc::new(config)
}

#[test]
fn qualified_mcp_tool_name_prefix_sanitizes_server_names_without_lowercasing() {
    assert_eq!(
        qualified_mcp_tool_name_prefix("Some-Server"),
        "mcp__Some_Server__".to_string()
    );
}

#[test]
fn mcp_server_permissions_handle_unattached_and_threadless_servers() {
    let mut config = test_mcp_config(PathBuf::new());
    config.permission_profile = PermissionProfile::Disabled;
    let mut missing_server = serde_json::from_value::<McpServerConfig>(
        serde_json::json!({"url": "https://example.com"}),
    )
    .expect("valid MCP server config");
    missing_server.environment_id = "missing".to_string();
    let mut selected_server = missing_server.clone();
    selected_server.environment_id = "unattached".to_string();
    let mut catalog = ResolvedMcpCatalog::builder();
    catalog.register(McpServerRegistration::from_config(
        "missing".to_string(),
        missing_server,
    ));
    catalog.register(McpServerRegistration::from_selected_plugin(
        "selected".to_string(),
        McpPluginAttribution::new("selected@test".to_string(), "Selected".to_string()),
        /*selection_order*/ 0,
        codex_config::DEFAULT_MCP_SERVER_ENVIRONMENT_ID,
        selected_server,
    ));
    config.mcp_server_catalog = catalog.build();
    let servers = effective_mcp_servers(&config);
    assert_eq!(config.permission_profile_for_server("selected"), None);
    config.set_server_permissions(&servers, std::iter::empty());

    assert_eq!(
        config.permission_profile_for_server("selected"),
        Some(&PermissionProfile::Disabled)
    );
    assert_eq!(config.permission_profile_for_server("missing"), None);

    let config = config.for_threadless_operations(&servers);
    assert_eq!(
        config.permission_profile_for_server("selected"),
        Some(&PermissionProfile::default())
    );
}

#[test]
fn mcp_prompt_auto_approval_honors_unrestricted_managed_profiles() {
    assert!(mcp_permission_prompt_is_auto_approved(
        AskForApproval::Never,
        &PermissionProfile::Managed {
            file_system: ManagedFileSystemPermissions::Unrestricted,
            network: NetworkSandboxPolicy::Enabled,
        },
        McpPermissionPromptAutoApproveContext::default(),
    ));
    assert!(mcp_permission_prompt_is_auto_approved(
        AskForApproval::Never,
        &PermissionProfile::Managed {
            file_system: ManagedFileSystemPermissions::Unrestricted,
            network: NetworkSandboxPolicy::Restricted,
        },
        McpPermissionPromptAutoApproveContext::default(),
    ));
    assert!(!mcp_permission_prompt_is_auto_approved(
        AskForApproval::Never,
        &PermissionProfile::read_only(),
        McpPermissionPromptAutoApproveContext::default(),
    ));
    assert!(!mcp_permission_prompt_is_auto_approved(
        AskForApproval::OnRequest,
        &PermissionProfile::Managed {
            file_system: ManagedFileSystemPermissions::Unrestricted,
            network: NetworkSandboxPolicy::Enabled,
        },
        McpPermissionPromptAutoApproveContext::default(),
    ));
}

#[test]
fn mcp_prompt_auto_approval_honors_approved_tools_in_all_permission_modes() {
    for approval_policy in [
        AskForApproval::UnlessTrusted,
        AskForApproval::OnRequest,
        AskForApproval::Granular(GranularApprovalConfig {
            sandbox_approval: true,
            rules: true,
            skill_approval: true,
            request_permissions: true,
            mcp_elicitations: true,
        }),
        AskForApproval::Never,
    ] {
        assert!(mcp_permission_prompt_is_auto_approved(
            approval_policy,
            &PermissionProfile::read_only(),
            McpPermissionPromptAutoApproveContext {
                tool_approval_mode: Some(AppToolApproval::Approve),
            },
        ));
    }

    assert!(!mcp_permission_prompt_is_auto_approved(
        AskForApproval::OnRequest,
        &PermissionProfile::read_only(),
        McpPermissionPromptAutoApproveContext {
            tool_approval_mode: Some(AppToolApproval::Auto),
        },
    ));
}

#[test]
fn selected_mcp_attribution_does_not_join_an_unrelated_local_summary() {
    let mut config = test_mcp_config(PathBuf::new());
    let mut catalog = ResolvedMcpCatalog::builder();
    catalog.register(McpServerRegistration::from_selected_plugin(
        "github".to_string(),
        McpPluginAttribution::new(
            "shared-plugin-id".to_string(),
            "Executor GitHub".to_string(),
        ),
        /*selection_order*/ 0,
        "executor-1",
        serde_json::from_value::<McpServerConfig>(
            serde_json::json!({"url": "https://github.example"}),
        )
        .expect("valid MCP server config"),
    ));
    config.mcp_server_catalog = catalog.build();
    config.connector_snapshot =
        codex_connectors::ConnectorSnapshot::from_plugin_capability_summaries(&[
            PluginCapabilitySummary {
                config_name: "shared-plugin-id".to_string(),
                display_name: "Local GitHub".to_string(),
                plugin_namespace: None,
                mcp_server_names: vec!["github".to_string()],
                ..PluginCapabilitySummary::default()
            },
        ]);

    let provenance = tool_plugin_context(&config);

    assert_eq!(
        provenance,
        ToolPluginContext {
            disabled_connector_ids: HashSet::new(),
            plugin_display_names_by_connector_id: HashMap::new(),
            plugin_display_names_by_mcp_server_name: HashMap::from([(
                "github".to_string(),
                vec!["Executor GitHub".to_string()],
            )]),
            plugin_ids_by_mcp_server_name: HashMap::from([(
                "github".to_string(),
                "shared-plugin-id".to_string(),
            )]),
            selected_plugin_mcp_server_names: HashSet::from(["github".to_string()]),
        }
    );
    assert!(provenance.is_selected_plugin_mcp_server("github"));
}

#[test]
fn effective_mcp_servers_preserve_executor_credential_policy() {
    let mut config = test_mcp_config(PathBuf::new());
    let server: McpServerConfig = serde_json::from_value(serde_json::json!({
        "url": "https://executor.example/mcp",
        "environment_id": "remote",
        "bearer_token_env_var": "MCP_EXECUTOR_CREDENTIAL_CANARY",
    }))
    .expect("valid MCP server config");
    let mut catalog = ResolvedMcpCatalog::builder();
    catalog.register(McpServerRegistration::from_executor_config(
        "executor".to_string(),
        server.clone(),
    ));
    catalog.register(McpServerRegistration::from_config(
        "host".to_string(),
        server,
    ));
    config.mcp_server_catalog = catalog.build();
    let effective = effective_mcp_servers(&config);

    assert_eq!(
        effective["executor"].credential_policy(),
        McpCredentialPolicy::ExecutorOnly,
    );
    assert_eq!(
        effective["host"].credential_policy(),
        McpCredentialPolicy::HostFallbackAllowed,
    );
}

#[test]
#[should_panic(expected = "materialized MCP server must have a catalog registration")]
fn effective_mcp_servers_require_catalog_registration() {
    let config = test_mcp_config(PathBuf::new());
    let server: McpServerConfig = serde_json::from_value(serde_json::json!({
        "url": "https://executor.example/mcp",
        "environment_id": "remote",
        "bearer_token_env_var": "MCP_EXECUTOR_CREDENTIAL_CANARY",
    }))
    .expect("valid MCP server config");

    effective_mcp_servers_from_configured(
        HashMap::from([("unregistered".to_string(), server)]),
        &config,
    );
}
