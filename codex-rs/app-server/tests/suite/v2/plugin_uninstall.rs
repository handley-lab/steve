use std::time::Duration;

use anyhow::Result;
use anyhow::bail;
use app_test_support::TestAppServer;
use codex_app_server_protocol::PluginUninstallParams;
use codex_app_server_protocol::PluginUninstallResponse;
use codex_app_server_protocol::RequestId;
use pretty_assertions::assert_eq;
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
async fn plugin_uninstall_rejects_remote_plugin_when_plugins_are_disabled() -> Result<()> {
    let codex_home = TempDir::new()?;
    std::fs::write(
        codex_home.path().join("config.toml"),
        r#"[features]
plugins = false
"#,
    )?;
    let mut mcp = TestAppServer::builder()
        .with_codex_home(codex_home.path())
        .build_initialized_with_timeout(DEFAULT_TIMEOUT)
        .await?;

    let request_id = mcp
        .send_plugin_uninstall_request(PluginUninstallParams {
            plugin_id: "plugins~Plugin_sample".to_string(),
        })
        .await?;

    let err = timeout(
        DEFAULT_TIMEOUT,
        mcp.read_stream_until_error_message(RequestId::Integer(request_id)),
    )
    .await??;

    assert_eq!(err.error.code, -32600);
    assert!(
        err.error
            .message
            .contains("remote plugin uninstall is not enabled")
    );
    Ok(())
}

#[tokio::test]
async fn plugin_uninstall_rejects_remote_plugin_id_with_spaces_before_network_call() -> Result<()> {
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
            plugin_id: "sample plugin".to_string(),
        })
        .await?;

    let err = timeout(
        DEFAULT_TIMEOUT,
        mcp.read_stream_until_error_message(RequestId::Integer(request_id)),
    )
    .await??;

    assert_eq!(err.error.code, -32600);
    assert!(err.error.message.contains("invalid remote plugin id"));
    wait_for_remote_plugin_request_count(
        &server,
        "POST",
        "/ps/plugins/sample plugin/uninstall",
        /*expected_count*/ 0,
    )
    .await?;
    Ok(())
}

#[tokio::test]
async fn plugin_uninstall_rejects_invalid_remote_plugin_id_before_network_call() -> Result<()> {
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
            plugin_id: "linear/../../oops".to_string(),
        })
        .await?;

    let err = timeout(
        DEFAULT_TIMEOUT,
        mcp.read_stream_until_error_message(RequestId::Integer(request_id)),
    )
    .await??;

    assert_eq!(err.error.code, -32600);
    assert!(err.error.message.contains("invalid remote plugin id"));
    wait_for_remote_plugin_request_count(
        &server,
        "POST",
        "/ps/plugins/linear/../../oops/uninstall",
        /*expected_count*/ 0,
    )
    .await?;
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
    assert!(err.error.message.contains("invalid remote plugin id"));

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

async fn wait_for_remote_plugin_request_count(
    server: &MockServer,
    method_name: &str,
    path_suffix: &str,
    expected_count: usize,
) -> Result<()> {
    timeout(DEFAULT_TIMEOUT, async {
        loop {
            let Some(requests) = server.received_requests().await else {
                if expected_count == 0 {
                    return Ok::<(), anyhow::Error>(());
                }
                bail!("wiremock did not record requests");
            };
            let request_count = requests
                .iter()
                .filter(|request| {
                    request.method == method_name && request.url.path().ends_with(path_suffix)
                })
                .count();
            if request_count == expected_count {
                return Ok::<(), anyhow::Error>(());
            }
            if request_count > expected_count {
                bail!(
                    "expected exactly {expected_count} {method_name} {path_suffix} requests, got {request_count}"
                );
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await??;
    Ok(())
}
