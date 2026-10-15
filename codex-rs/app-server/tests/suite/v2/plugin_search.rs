use std::time::Duration;

use anyhow::Result;
use app_test_support::ChatGptAuthFixture;
use app_test_support::TestAppServer;
use app_test_support::write_chatgpt_auth;
use codex_app_server_protocol::PluginSearchParams;
use codex_app_server_protocol::PluginSearchResponse;
use codex_app_server_protocol::PluginSearchScope;
use codex_config::types::AuthCredentialsStoreMode;
use codex_login::AuthKeyringBackendKind;
use codex_login::login_with_api_key;
use codex_utils_absolute_path::AbsolutePathBuf;
use pretty_assertions::assert_eq;
use serde_json::json;
use tempfile::TempDir;
use tokio::time::timeout;
use wiremock::MockServer;

const DEFAULT_TIMEOUT: Duration = Duration::from_secs(10);

#[tokio::test]
async fn plugin_search_returns_local_matches_for_api_key_auth() -> Result<()> {
    for remote_plugin_enabled in [false, true] {
        let codex_home = TempDir::new()?;
        let repo_root = TempDir::new()?;
        let curated_root = codex_home.path().join(".tmp/plugins");
        let bundled_alpha_root = codex_home
            .path()
            .join(".tmp/bundled-marketplaces/openai-bundled-alpha");
        let server = MockServer::start().await;
        write_local_marketplace(
            &curated_root,
            "openai-api-curated",
            "api_marketplace.json",
            &[LocalPluginFixture {
                name: "calendar-built-in",
                display_name: "Calendar Built In",
                keywords: &[],
                description: "Built-in calendar",
            }],
        )?;
        write_local_marketplace(
            repo_root.path(),
            "personal-tools",
            "marketplace.json",
            &[
                LocalPluginFixture {
                    name: "developer-tools",
                    display_name: "Developer Tools",
                    keywords: &["api-key"],
                    description: "Manage credentials",
                },
                LocalPluginFixture {
                    name: "japanese-notes",
                    display_name: "日本語メモ",
                    keywords: &[],
                    description: "Japanese notes",
                },
            ],
        )?;
        write_local_marketplace(
            &bundled_alpha_root,
            "openai-bundled-alpha",
            "marketplace.json",
            &[LocalPluginFixture {
                name: "alpha-calendar",
                display_name: "Alpha Calendar",
                keywords: &[],
                description: "Built-in alpha calendar",
            }],
        )?;
        write_remote_plugin_search_config(codex_home.path(), &server, remote_plugin_enabled)?;
        login_with_api_key(
            codex_home.path(),
            "sk-test-key",
            AuthCredentialsStoreMode::File,
            AuthKeyringBackendKind::default(),
        )?;

        let mut app_server = TestAppServer::builder()
            .with_codex_home(codex_home.path())
            .without_auto_env()
            .build_initialized_with_timeout(DEFAULT_TIMEOUT)
            .await?;
        let config_before = std::fs::read_to_string(codex_home.path().join("config.toml"))?;

        for (scope, search_term, expected_names) in [
            (
                Some(PluginSearchScope::Global),
                "calendar",
                vec!["calendar-built-in", "alpha-calendar"],
            ),
            (
                Some(PluginSearchScope::Personal),
                "API key",
                vec!["developer-tools"],
            ),
            (
                Some(PluginSearchScope::Personal),
                "日本語",
                vec!["japanese-notes"],
            ),
            (Some(PluginSearchScope::Personal), "calendar", Vec::new()),
            (Some(PluginSearchScope::Workspace), "calendar", Vec::new()),
            (
                None,
                "calendar",
                vec!["calendar-built-in", "alpha-calendar"],
            ),
            (None, "!!!", Vec::new()),
        ] {
            let request_id = app_server
                .send_plugin_search_request(PluginSearchParams {
                    search_term: search_term.to_string(),
                    scope,
                    cwds: Some(vec![
                        AbsolutePathBuf::try_from(repo_root.path().canonicalize()?)?,
                        AbsolutePathBuf::try_from(bundled_alpha_root.canonicalize()?)?,
                    ]),
                    cursor: None,
                    limit: None,
                })
                .await?;
            let response: PluginSearchResponse =
                timeout(DEFAULT_TIMEOUT, app_server.read_response(request_id)).await??;

            assert_eq!(
                response
                    .data
                    .iter()
                    .map(|result| result.plugin.name.as_str())
                    .collect::<Vec<_>>(),
                expected_names
            );
            assert_eq!(response.next_cursor, None);
        }

        assert!(
            server
                .received_requests()
                .await
                .expect("wiremock should record requests")
                .is_empty()
        );
        assert_eq!(
            std::fs::read_to_string(codex_home.path().join("config.toml"))?,
            config_before
        );
        assert!(config_before.contains(&format!(
            r#"chatgpt_base_url = "{}/backend-api/""#,
            server.uri()
        )));
    }
    Ok(())
}

struct LocalPluginFixture<'a> {
    name: &'a str,
    display_name: &'a str,
    keywords: &'a [&'a str],
    description: &'a str,
}

fn write_local_marketplace(
    root: &std::path::Path,
    marketplace_name: &str,
    manifest_name: &str,
    plugins: &[LocalPluginFixture<'_>],
) -> Result<AbsolutePathBuf> {
    std::fs::create_dir_all(root.join(".git"))?;
    std::fs::create_dir_all(root.join(".agents/plugins"))?;
    let marketplace_path = root.join(".agents/plugins").join(manifest_name);
    let entries = plugins
        .iter()
        .map(|plugin| {
            json!({
                "name": plugin.name,
                "source": {
                    "source": "local",
                    "path": format!("./plugins/{}", plugin.name),
                },
            })
        })
        .collect::<Vec<_>>();
    std::fs::write(
        &marketplace_path,
        serde_json::to_string(&json!({
            "name": marketplace_name,
            "plugins": entries,
        }))?,
    )?;

    for plugin in plugins {
        let plugin_manifest = root.join("plugins").join(plugin.name).join(".codex-plugin");
        std::fs::create_dir_all(&plugin_manifest)?;
        std::fs::write(
            plugin_manifest.join("plugin.json"),
            serde_json::to_string(&json!({
                "name": plugin.name,
                "keywords": plugin.keywords,
                "interface": {
                    "displayName": plugin.display_name,
                    "shortDescription": plugin.description,
                },
            }))?,
        )?;
    }

    Ok(AbsolutePathBuf::try_from(marketplace_path)?)
}

fn write_remote_plugin_search_config(
    codex_home: &std::path::Path,
    server: &MockServer,
    remote_plugin: bool,
) -> Result<()> {
    std::fs::write(
        codex_home.join("config.toml"),
        format!(
            r#"chatgpt_base_url = "{}/backend-api/"

[features]
plugins = true
remote_plugin = {remote_plugin}
"#,
            server.uri()
        ),
    )?;
    Ok(())
}

fn write_chatgpt_search_auth(codex_home: &std::path::Path) -> Result<()> {
    write_chatgpt_auth(
        codex_home,
        ChatGptAuthFixture::new("chatgpt-token")
            .account_id("account-123")
            .chatgpt_user_id("user-123")
            .chatgpt_account_id("account-123"),
        AuthCredentialsStoreMode::File,
    )?;
    Ok(())
}

fn write_installed_plugin(
    codex_home: &std::path::Path,
    marketplace_name: &str,
    plugin_name: &str,
) -> Result<()> {
    let plugin_manifest = codex_home
        .join("plugins/cache")
        .join(marketplace_name)
        .join(plugin_name)
        .join("1.2.3")
        .join(".codex-plugin");
    std::fs::create_dir_all(&plugin_manifest)?;
    std::fs::write(
        plugin_manifest.join("plugin.json"),
        serde_json::to_string(&json!({
            "name": plugin_name,
            "version": "1.2.3",
            "interface": {"displayName": plugin_name},
        }))?,
    )?;
    Ok(())
}
