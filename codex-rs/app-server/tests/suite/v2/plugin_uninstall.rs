use std::time::Duration;

use anyhow::Result;
use app_test_support::ChatGptAuthFixture;
use app_test_support::DEFAULT_CLIENT_NAME;
use app_test_support::TestAppServer;
use app_test_support::start_analytics_events_server;
use app_test_support::write_chatgpt_auth;
use codex_app_server_protocol::PluginUninstallParams;
use codex_app_server_protocol::PluginUninstallResponse;
use codex_app_server_protocol::RequestId;
use codex_config::types::AuthCredentialsStoreMode;
use pretty_assertions::assert_eq;
use serde_json::json;
use tempfile::TempDir;
use tokio::time::timeout;
use wiremock::MockServer;

const DEFAULT_TIMEOUT: Duration = Duration::from_secs(10);

#[tokio::test]
async fn plugin_uninstall_removes_plugin_cache_and_config_entry() -> Result<()> {
    let codex_home = TempDir::new()?;
    write_installed_plugin(&codex_home, "debug", "sample-plugin")?;
    std::fs::write(
        codex_home.path().join("config.toml"),
        r#"[features]
plugins = true

[plugins."sample-plugin@debug"]
enabled = true
"#,
    )?;

    let mut mcp = TestAppServer::builder()
        .with_codex_home(codex_home.path())
        .build_initialized_with_timeout(DEFAULT_TIMEOUT)
        .await?;

    let response = uninstall_plugin(&mut mcp, "sample-plugin@debug").await?;
    assert_eq!(response, PluginUninstallResponse {});

    assert!(
        !codex_home
            .path()
            .join("plugins/cache/debug/sample-plugin")
            .exists()
    );
    let config = std::fs::read_to_string(codex_home.path().join("config.toml"))?;
    assert!(!config.contains(r#"[plugins."sample-plugin@debug"]"#));

    let response = uninstall_plugin(&mut mcp, "sample-plugin@debug").await?;
    assert_eq!(response, PluginUninstallResponse {});

    Ok(())
}

#[tokio::test]
async fn plugin_uninstall_tracks_analytics_event() -> Result<()> {
    let analytics_server = start_analytics_events_server().await?;
    let codex_home = TempDir::new()?;
    write_installed_plugin(&codex_home, "debug", "sample-plugin")?;
    std::fs::write(
        codex_home.path().join("config.toml"),
        format!(
            "chatgpt_base_url = \"{}\"\n\n[features]\nplugins = true\n\n[plugins.\"sample-plugin@debug\"]\nenabled = true\n",
            analytics_server.uri()
        ),
    )?;
    write_chatgpt_auth(
        codex_home.path(),
        ChatGptAuthFixture::new("chatgpt-token")
            .account_id("account-123")
            .chatgpt_user_id("user-123")
            .chatgpt_account_id("account-123"),
        AuthCredentialsStoreMode::File,
    )?;

    let mut mcp = TestAppServer::builder()
        .with_codex_home(codex_home.path())
        .build_initialized_with_timeout(DEFAULT_TIMEOUT)
        .await?;

    let response = uninstall_plugin(&mut mcp, "sample-plugin@debug").await?;
    assert_eq!(response, PluginUninstallResponse {});

    let payload = timeout(DEFAULT_TIMEOUT, async {
        loop {
            let Some(requests) = analytics_server.received_requests().await else {
                tokio::time::sleep(Duration::from_millis(25)).await;
                continue;
            };
            if let Some(request) = requests.iter().find(|request| {
                request.method == "POST" && request.url.path() == "/codex/analytics-events/events"
            }) {
                break request.body.clone();
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await?;
    let payload: serde_json::Value = serde_json::from_slice(&payload).expect("analytics payload");
    assert_eq!(
        payload,
        json!({
            "events": [{
                "event_type": "codex_plugin_uninstalled",
                "event_params": {
                    "plugin_id": "sample-plugin@debug",
                    "remote_plugin_id": null,
                    "plugin_name": "sample-plugin",
                    "marketplace_name": "debug",
                    "has_skills": false,
                    "mcp_server_count": 0,
                    "connector_ids": [],
                    "product_client_id": DEFAULT_CLIENT_NAME,
                }
            }]
        })
    );
    Ok(())
}

#[tokio::test]
async fn plugin_uninstall_rejects_remote_plugin_id_with_spaces_before_network_call() -> Result<()> {
    let codex_home = TempDir::new()?;
    let mut mcp = TestAppServer::builder()
        .with_codex_home(codex_home.path())
        .build_initialized_with_timeout(DEFAULT_TIMEOUT)
        .await?;

    let request_id = mcp
        .send_plugin_uninstall_request(PluginUninstallParams {
            plugin_id: "sample plugin".to_string(),
        })
        .await?;

    let err = timeout(
        DEFAULT_TIMEOUT,
        mcp.read_stream_until_error_message(RequestId::Integer(request_id)),
    )
    .await??;

    assert_eq!(err.error.code, -32600);
    assert!(err.error.message.contains("invalid plugin key"));
    Ok(())
}

#[tokio::test]
async fn plugin_uninstall_rejects_invalid_remote_plugin_id_before_network_call() -> Result<()> {
    let codex_home = TempDir::new()?;
    let mut mcp = TestAppServer::builder()
        .with_codex_home(codex_home.path())
        .build_initialized_with_timeout(DEFAULT_TIMEOUT)
        .await?;

    let request_id = mcp
        .send_plugin_uninstall_request(PluginUninstallParams {
            plugin_id: "linear/../../oops".to_string(),
        })
        .await?;

    let err = timeout(
        DEFAULT_TIMEOUT,
        mcp.read_stream_until_error_message(RequestId::Integer(request_id)),
    )
    .await??;

    assert_eq!(err.error.code, -32600);
    assert!(err.error.message.contains("invalid plugin key"));
    Ok(())
}

#[tokio::test]
async fn plugin_uninstall_rejects_empty_remote_plugin_id() -> Result<()> {
    let codex_home = TempDir::new()?;
    let server = MockServer::start().await;
    write_remote_plugin_catalog_config(
        codex_home.path(),
        &format!("{}/backend-api/", server.uri()),
    )?;
    let mut mcp = TestAppServer::builder()
        .with_codex_home(codex_home.path())
        .build_initialized_with_timeout(DEFAULT_TIMEOUT)
        .await?;

    let request_id = mcp
        .send_plugin_uninstall_request(PluginUninstallParams {
            plugin_id: String::new(),
        })
        .await?;
    let err = timeout(
        DEFAULT_TIMEOUT,
        mcp.read_stream_until_error_message(RequestId::Integer(request_id)),
    )
    .await??;

    assert_eq!(err.error.code, -32600);
    assert!(err.error.message.contains("invalid plugin key"));

    Ok(())
}

async fn uninstall_plugin(
    mcp: &mut TestAppServer,
    plugin_id: &str,
) -> Result<PluginUninstallResponse> {
    let request_id = mcp
        .send_plugin_uninstall_request(PluginUninstallParams {
            plugin_id: plugin_id.to_string(),
        })
        .await?;
    timeout(DEFAULT_TIMEOUT, mcp.read_response(request_id)).await?
}

fn write_installed_plugin(
    codex_home: &TempDir,
    marketplace_name: &str,
    plugin_name: &str,
) -> Result<()> {
    let plugin_root = codex_home
        .path()
        .join("plugins/cache")
        .join(marketplace_name)
        .join(plugin_name)
        .join("local/.codex-plugin");
    std::fs::create_dir_all(&plugin_root)?;
    std::fs::write(
        plugin_root.join("plugin.json"),
        format!(r#"{{"name":"{plugin_name}"}}"#),
    )?;
    Ok(())
}

fn write_remote_plugin_catalog_config(
    codex_home: &std::path::Path,
    base_url: &str,
) -> std::io::Result<()> {
    std::fs::write(
        codex_home.join("config.toml"),
        format!(
            r#"
chatgpt_base_url = "{base_url}"

[features]
plugins = true
"#
        ),
    )
}
