use std::sync::Arc;

use codex_core::McpManager;
use codex_core::config::ConfigBuilder;
use codex_core::plugins_manager_for_config;
use codex_login::test_support::auth_manager_from_optional_auth;
use codex_mcp::CODEX_APPS_MCP_SERVER_NAME;

type TestResult = Result<(), Box<dyn std::error::Error>>;

#[tokio::test]
async fn disabled_apps_remove_reserved_server_config_for_all_hosts() -> TestResult {
    let codex_home = tempfile::tempdir()?;
    let config = ConfigBuilder::default()
        .codex_home(codex_home.path().to_path_buf())
        .fallback_cwd(Some(codex_home.path().to_path_buf()))
        .cli_overrides(vec![
            ("features.apps".to_string(), false.into()),
            (
                "mcp_servers.codex_apps.url".to_string(),
                "https://example.com/mcp".into(),
            ),
        ])
        .build()
        .await?;
    let manager = McpManager::new(Arc::new(plugins_manager_for_config(
        &config,
        auth_manager_from_optional_auth(/*auth*/ None),
    )));
    let servers = manager.runtime_servers(&config).await;
    assert!(!servers.contains_key(CODEX_APPS_MCP_SERVER_NAME));
    Ok(())
}
