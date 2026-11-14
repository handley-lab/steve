use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use app_test_support::TestAppServer;
use app_test_support::to_response;
use axum::Json;
use axum::Router;
use axum::extract::State;
use axum::http::HeaderMap;
use axum::http::StatusCode;
use axum::http::header::AUTHORIZATION;
use axum::routing::post;
use codex_app_server_protocol::AppInfo;
use codex_app_server_protocol::AppMetadata;
use codex_app_server_protocol::JSONRPCError;
use codex_app_server_protocol::JSONRPCResponse;
use codex_app_server_protocol::PluginReadParams;
use codex_app_server_protocol::PluginReadResponse;
use codex_app_server_protocol::RequestId;
use codex_app_server_protocol::SkillSummary;
use codex_utils_absolute_path::AbsolutePathBuf;
use pretty_assertions::assert_eq;
use serde_json::json;
use tempfile::TempDir;
use test_case::test_case;
use tokio::net::TcpListener;
use tokio::task::JoinHandle;
use tokio::time::timeout;

const DEFAULT_TIMEOUT: Duration = Duration::from_secs(10);

#[test_case(Some("skills/setup/SKILL.md"), "enabled = true", true, "", true, true; "enabled matching skill")]
#[test_case(Some("skills/setup/SKILL.md"), "enabled = false", true, "", false, false; "disabled plugin")]
#[test_case(Some("skills/setup/SKILL.md"), "enabled = true", false, "", true, false; "disabled skill")]
#[test_case(None, "enabled = true", true, "", true, false; "missing declaration")]
#[test_case(Some("skills/missing/SKILL.md"), "enabled = true", true, "", true, false; "unmatched declaration")]
#[test_case(Some("skills/setup/SKILL.md"), "", true, "", true, true; "legacy empty entry")]
#[test_case(Some("skills/setup/SKILL.md"), "", true, "[plugins._default]\nenabled = false", false, false; "default disabled empty entry")]
#[test_case(Some("skills/setup/SKILL.md"), "enabled = true", true, "[plugins._default]\nenabled = false", true, true; "explicitly enabled overrides default")]
#[tokio::test]
async fn plugin_read_selects_local_onboarding_skill(
    onboarding_path: Option<&str>,
    plugin_settings: &str,
    skill_enabled: bool,
    plugin_default: &str,
    expected_plugin_enabled: bool,
    expect_onboarding: bool,
) -> Result<()> {
    let codex_home = TempDir::new()?;
    let repo_root = TempDir::new()?;
    let plugin_root = repo_root.path().join("demo-plugin");
    write_plugin_marketplace(
        repo_root.path(),
        "codex-curated",
        "demo-plugin",
        "./demo-plugin",
    )?;
    std::fs::create_dir_all(plugin_root.join(".codex-plugin"))?;
    let mut manifest = json!({ "name": "demo-plugin" });
    if let Some(onboarding_path) = onboarding_path {
        manifest["extensions"] = json!({
            "com.openai": { "onboardingSkill": onboarding_path }
        });
    }
    std::fs::write(
        plugin_root.join(".codex-plugin/plugin.json"),
        serde_json::to_vec(&manifest)?,
    )?;
    for name in ["other", "setup"] {
        std::fs::create_dir_all(plugin_root.join("skills").join(name))?;
        std::fs::write(
            plugin_root.join("skills").join(name).join("SKILL.md"),
            format!("---\nname: {name}\ndescription: Run {name}\n---\n"),
        )?;
    }
    std::fs::write(
        codex_home.path().join("config.toml"),
        format!(
            r#"[features]
plugins = true

{plugin_default}

[plugins."demo-plugin@codex-curated"]
{plugin_settings}

[[skills.config]]
name = "demo-plugin:setup"
enabled = {skill_enabled}
"#
        ),
    )?;
    write_installed_plugin(&codex_home, "codex-curated", "demo-plugin")?;
    let mut mcp = TestAppServer::builder()
        .with_codex_home(codex_home.path())
        .without_auto_env()
        .build_initialized_with_timeout(DEFAULT_TIMEOUT)
        .await?;
    let request_id = mcp
        .send_plugin_read_request(PluginReadParams {
            marketplace_path: Some(AbsolutePathBuf::try_from(
                repo_root.path().join(".agents/plugins/marketplace.json"),
            )?),
            plugin_name: "demo-plugin".to_string(),
        })
        .await?;
    let response: PluginReadResponse =
        timeout(DEFAULT_TIMEOUT, mcp.read_response(request_id)).await??;

    let setup_skill = SkillSummary {
        name: "demo-plugin:setup".to_string(),
        description: "Run setup".to_string(),
        short_description: None,
        interface: None,
        path: Some(
            AbsolutePathBuf::try_from(std::fs::canonicalize(
                plugin_root.join("skills/setup/SKILL.md"),
            )?)?
            .into(),
        ),
        enabled: skill_enabled,
    };
    assert_eq!(response.plugin.summary.enabled, expected_plugin_enabled);
    assert!(response.plugin.skills.contains(&setup_skill));
    assert_eq!(
        response.plugin.onboarding_skill,
        expect_onboarding.then_some(setup_skill)
    );
    Ok(())
}

#[tokio::test]
async fn plugin_read_rejects_missing_read_source() -> Result<()> {
    let codex_home = TempDir::new()?;
    let mut mcp = TestAppServer::builder()
        .with_codex_home(codex_home.path())
        .without_auto_env()
        .build_initialized_with_timeout(DEFAULT_TIMEOUT)
        .await?;

    let request_id = mcp
        .send_plugin_read_request(PluginReadParams {
            marketplace_path: None,
            plugin_name: "sample-plugin".to_string(),
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
            .contains("plugin/read requires marketplacePath")
    );
    Ok(())
}

#[tokio::test]
async fn plugin_read_returns_canonical_openai_curated_marketplace_name() -> Result<()> {
    let codex_home = TempDir::new()?;
    let repo_root = codex_home.path().join(".tmp/plugins");
    write_plugin_marketplace(&repo_root, "openai-curated", "demo-plugin", "./demo-plugin")?;
    std::fs::create_dir_all(repo_root.join("demo-plugin/.codex-plugin"))?;
    std::fs::write(
        repo_root.join("demo-plugin/.codex-plugin/plugin.json"),
        r#"{
  "name": "demo-plugin",
  "description": "OpenAI curated plugin"
}"#,
    )?;
    std::fs::write(
        codex_home.path().join("config.toml"),
        r#"[features]
plugins = true

[plugins."demo-plugin@openai-curated"]
enabled = true
"#,
    )?;
    write_installed_plugin(&codex_home, "openai-curated", "demo-plugin")?;

    let mut mcp = TestAppServer::builder()
        .with_codex_home(codex_home.path())
        .without_auto_env()
        .build_initialized_with_timeout(DEFAULT_TIMEOUT)
        .await?;

    let marketplace_path =
        AbsolutePathBuf::try_from(repo_root.join(".agents/plugins/marketplace.json"))?;
    let request_id = mcp
        .send_plugin_read_request(PluginReadParams {
            marketplace_path: Some(marketplace_path.clone()),
            plugin_name: "demo-plugin".to_string(),
        })
        .await?;

    let response: JSONRPCResponse = timeout(
        DEFAULT_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(request_id)),
    )
    .await??;
    assert_eq!(response.result["plugin"]["scheduledTasks"], json!(null));
    let response: PluginReadResponse = to_response(response)?;

    assert_eq!(response.plugin.marketplace_name, "openai-curated");
    assert_eq!(response.plugin.marketplace_path, Some(marketplace_path));
    assert_eq!(response.plugin.summary.id, "demo-plugin@openai-curated");
    assert_eq!(response.plugin.summary.name, "demo-plugin");
    Ok(())
}

#[tokio::test]
async fn plugin_read_fails_on_malformed_share_mapping() -> Result<()> {
    let codex_home = TempDir::new()?;
    let repo_root = TempDir::new()?;
    write_plugins_enabled_config(&codex_home)?;
    write_plugin_marketplace(
        repo_root.path(),
        "codex-curated",
        "demo-plugin",
        "./demo-plugin",
    )?;
    write_plugin_source(repo_root.path(), "demo-plugin", &[])?;
    std::fs::create_dir_all(codex_home.path().join(".tmp"))?;
    std::fs::write(
        codex_home
            .path()
            .join(".tmp/plugin-share-local-paths-v1.json"),
        "not valid json\n",
    )?;

    let mut mcp = TestAppServer::builder()
        .with_codex_home(codex_home.path())
        .without_auto_env()
        .build_initialized_with_timeout(DEFAULT_TIMEOUT)
        .await?;

    let request_id = mcp
        .send_plugin_read_request(PluginReadParams {
            marketplace_path: Some(AbsolutePathBuf::try_from(
                repo_root.path().join(".agents/plugins/marketplace.json"),
            )?),
            plugin_name: "demo-plugin".to_string(),
        })
        .await?;

    let error: JSONRPCError = timeout(
        DEFAULT_TIMEOUT,
        mcp.read_stream_until_error_message(RequestId::Integer(request_id)),
    )
    .await??;

    assert_eq!(error.error.code, -32603);
    assert!(
        error
            .error
            .message
            .contains("failed to load plugin share local path mapping")
    );
    Ok(())
}

#[tokio::test]
async fn plugin_read_agent_plugin_excludes_nested_skills() -> Result<()> {
    let codex_home = TempDir::new()?;
    let repo_root = TempDir::new()?;
    let plugin_root = repo_root.path().join("plugins/demo-plugin");
    write_plugins_enabled_config(&codex_home)?;
    write_plugin_marketplace(
        repo_root.path(),
        "codex-curated",
        "demo-plugin",
        "./plugins/demo-plugin",
    )?;
    std::fs::create_dir_all(plugin_root.join("skills/direct"))?;
    std::fs::create_dir_all(plugin_root.join("skills/group/nested"))?;
    std::fs::write(
        plugin_root.join("plugin.json"),
        r#"{"$schema":"https://agent-plugins.org/schemas/1.0.0/plugin.schema.json","name":"demo-plugin"}"#,
    )?;
    let direct_skill_path = plugin_root.join("skills/direct/SKILL.md");
    std::fs::write(
        &direct_skill_path,
        "---\nname: direct\ndescription: Direct skill\n---\n",
    )?;
    std::fs::write(
        plugin_root.join("skills/group/nested/SKILL.md"),
        "---\nname: nested\ndescription: Nested skill\n---\n",
    )?;

    let mut mcp = TestAppServer::builder()
        .with_codex_home(codex_home.path())
        .without_auto_env()
        .build_initialized_with_timeout(DEFAULT_TIMEOUT)
        .await?;
    let request_id = mcp
        .send_plugin_read_request(PluginReadParams {
            marketplace_path: Some(AbsolutePathBuf::try_from(
                repo_root.path().join(".agents/plugins/marketplace.json"),
            )?),
            plugin_name: "demo-plugin".to_string(),
        })
        .await?;
    let response: PluginReadResponse =
        timeout(DEFAULT_TIMEOUT, mcp.read_response(request_id)).await??;

    assert_eq!(
        response.plugin.skills,
        vec![codex_app_server_protocol::SkillSummary {
            name: "demo-plugin:direct".to_string(),
            description: "Direct skill".to_string(),
            short_description: None,
            interface: None,
            path: Some(
                AbsolutePathBuf::try_from(std::fs::canonicalize(direct_skill_path)?)?.into()
            ),
            enabled: true,
        }]
    );
    Ok(())
}

#[tokio::test]
async fn plugin_read_hides_apps_for_api_key_auth() -> Result<()> {
    let connectors = vec![AppInfo {
        id: "alpha".to_string(),
        name: "Alpha".to_string(),
        description: Some("Alpha connector".to_string()),
        logo_url: Some("https://example.com/alpha.png".to_string()),
        logo_url_dark: None,
        icon_assets: None,
        icon_dark_assets: None,
        distribution_channel: Some("featured".to_string()),
        branding: None,
        app_metadata: Some(AppMetadata {
            review: None,
            categories: Some(vec!["Productivity".to_string()]),
            sub_categories: None,
            seo_description: None,
            screenshots: None,
            developer: None,
            version: None,
            version_id: None,
            version_notes: None,
            first_party_requires_install: None,
            show_in_composer_when_unlinked: None,
        }),
        labels: None,
        install_url: None,
        is_accessible: false,
        is_enabled: true,
        plugin_display_names: Vec::new(),
    }];
    let (server_url, server_handle) = start_apps_server(connectors).await?;

    let codex_home = TempDir::new()?;
    write_connectors_config(codex_home.path(), &server_url)?;
    std::fs::write(
        codex_home.path().join("auth.json"),
        r#"{"OPENAI_API_KEY":"sk-test-key","tokens":null,"last_refresh":null}"#,
    )?;

    let repo_root = TempDir::new()?;
    write_plugin_marketplace(
        repo_root.path(),
        "debug",
        "sample-plugin",
        "./sample-plugin",
    )?;
    write_plugin_source(repo_root.path(), "sample-plugin", &["alpha"])?;
    std::fs::write(
        repo_root.path().join("sample-plugin/.mcp.json"),
        r#"{"mcpServers":{"alpha":{"command":"alpha-mcp"}}}"#,
    )?;
    let marketplace_path =
        AbsolutePathBuf::try_from(repo_root.path().join(".agents/plugins/marketplace.json"))?;

    let mut mcp = TestAppServer::builder()
        .with_codex_home(codex_home.path())
        .without_auto_env()
        .with_env_overrides(&[
            ("STEVE_ACCESS_TOKEN", None),
            ("STEVE_API_KEY", None),
            ("OPENAI_API_KEY", None),
        ])
        .build_initialized_with_timeout(DEFAULT_TIMEOUT)
        .await?;

    let request_id = mcp
        .send_plugin_read_request(PluginReadParams {
            marketplace_path: Some(marketplace_path),
            plugin_name: "sample-plugin".to_string(),
        })
        .await?;

    let response: PluginReadResponse =
        timeout(DEFAULT_TIMEOUT, mcp.read_response(request_id)).await??;

    assert!(response.plugin.apps.is_empty());
    assert_eq!(response.plugin.mcp_servers, vec!["alpha".to_string()]);

    server_handle.abort();
    let _ = server_handle.await;
    Ok(())
}

#[tokio::test]
async fn plugin_read_accepts_legacy_string_default_prompt() -> Result<()> {
    let codex_home = TempDir::new()?;
    let repo_root = TempDir::new()?;
    let plugin_root = repo_root.path().join("plugins/demo-plugin");
    std::fs::create_dir_all(repo_root.path().join(".git"))?;
    std::fs::create_dir_all(repo_root.path().join(".agents/plugins"))?;
    std::fs::create_dir_all(plugin_root.join(".codex-plugin"))?;
    std::fs::write(
        repo_root.path().join(".agents/plugins/marketplace.json"),
        r#"{
  "name": "codex-curated",
  "plugins": [
    {
      "name": "demo-plugin",
      "source": {
        "source": "local",
        "path": "./plugins/demo-plugin"
      }
    }
  ]
}"#,
    )?;
    std::fs::write(
        plugin_root.join(".codex-plugin/plugin.json"),
        r##"{
  "name": "demo-plugin",
  "interface": {
    "defaultPrompt": "Starter prompt for trying a plugin"
  }
}"##,
    )?;
    write_plugins_enabled_config(&codex_home)?;

    let mut mcp = TestAppServer::builder()
        .with_codex_home(codex_home.path())
        .without_auto_env()
        .build_initialized_with_timeout(DEFAULT_TIMEOUT)
        .await?;

    let request_id = mcp
        .send_plugin_read_request(PluginReadParams {
            marketplace_path: Some(AbsolutePathBuf::try_from(
                repo_root.path().join(".agents/plugins/marketplace.json"),
            )?),
            plugin_name: "demo-plugin".to_string(),
        })
        .await?;

    let response: PluginReadResponse =
        timeout(DEFAULT_TIMEOUT, mcp.read_response(request_id)).await??;

    assert_eq!(
        response
            .plugin
            .summary
            .interface
            .as_ref()
            .and_then(|interface| interface.default_prompt.clone()),
        Some(vec!["Starter prompt for trying a plugin".to_string()])
    );
    Ok(())
}

#[tokio::test]
async fn plugin_read_describes_uninstalled_git_source_without_cloning() -> Result<()> {
    let codex_home = TempDir::new()?;
    let repo_root = TempDir::new()?;
    let missing_remote_repo = repo_root.path().join("missing-remote-plugin-repo");
    let missing_remote_repo_url = url::Url::from_directory_path(&missing_remote_repo)
        .unwrap()
        .to_string();
    std::fs::create_dir_all(repo_root.path().join(".git"))?;
    std::fs::create_dir_all(repo_root.path().join(".agents/plugins"))?;
    std::fs::write(
        repo_root.path().join(".agents/plugins/marketplace.json"),
        format!(
            r#"{{
  "name": "debug",
  "plugins": [
    {{
      "name": "toolkit",
      "source": {{
        "source": "git-subdir",
        "url": "{missing_remote_repo_url}",
        "path": "plugins/toolkit"
      }}
    }}
  ]
}}"#
        ),
    )?;
    write_plugins_enabled_config(&codex_home)?;

    let mut mcp = TestAppServer::builder()
        .with_codex_home(codex_home.path())
        .without_auto_env()
        .build_initialized_with_timeout(DEFAULT_TIMEOUT)
        .await?;

    let request_id = mcp
        .send_plugin_read_request(PluginReadParams {
            marketplace_path: Some(AbsolutePathBuf::try_from(
                repo_root.path().join(".agents/plugins/marketplace.json"),
            )?),
            plugin_name: "toolkit".to_string(),
        })
        .await?;

    let response: PluginReadResponse =
        timeout(DEFAULT_TIMEOUT, mcp.read_response(request_id)).await??;

    let expected_description = format!(
        "This is a cross-repo plugin. Install it to view more detailed information. The source of the plugin is {missing_remote_repo_url}, path `plugins/toolkit`."
    );
    assert_eq!(
        response.plugin.description.as_deref(),
        Some(expected_description.as_str())
    );
    assert!(!response.plugin.summary.installed);
    assert!(response.plugin.skills.is_empty());
    assert!(response.plugin.apps.is_empty());
    assert!(response.plugin.mcp_servers.is_empty());
    assert!(
        !codex_home
            .path()
            .join("plugins/.marketplace-plugin-source-staging")
            .exists()
    );
    Ok(())
}

#[tokio::test]
async fn plugin_read_returns_invalid_request_when_plugin_is_missing() -> Result<()> {
    let codex_home = TempDir::new()?;
    let repo_root = TempDir::new()?;
    std::fs::create_dir_all(repo_root.path().join(".git"))?;
    std::fs::create_dir_all(repo_root.path().join(".agents/plugins"))?;
    std::fs::write(
        repo_root.path().join(".agents/plugins/marketplace.json"),
        r#"{
  "name": "codex-curated",
  "plugins": [
    {
      "name": "demo-plugin",
      "source": {
        "source": "local",
        "path": "./plugins/demo-plugin"
      }
    }
  ]
}"#,
    )?;
    write_plugins_enabled_config(&codex_home)?;

    let mut mcp = TestAppServer::builder()
        .with_codex_home(codex_home.path())
        .without_auto_env()
        .build_initialized_with_timeout(DEFAULT_TIMEOUT)
        .await?;

    let request_id = mcp
        .send_plugin_read_request(PluginReadParams {
            marketplace_path: Some(AbsolutePathBuf::try_from(
                repo_root.path().join(".agents/plugins/marketplace.json"),
            )?),
            plugin_name: "missing-plugin".to_string(),
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
            .contains("plugin `missing-plugin` was not found")
    );
    Ok(())
}

#[tokio::test]
async fn plugin_read_returns_invalid_request_when_plugin_manifest_is_missing() -> Result<()> {
    let codex_home = TempDir::new()?;
    let repo_root = TempDir::new()?;
    let plugin_root = repo_root.path().join("plugins/demo-plugin");
    std::fs::create_dir_all(repo_root.path().join(".git"))?;
    std::fs::create_dir_all(repo_root.path().join(".agents/plugins"))?;
    std::fs::create_dir_all(&plugin_root)?;
    std::fs::write(
        repo_root.path().join(".agents/plugins/marketplace.json"),
        r#"{
  "name": "codex-curated",
  "plugins": [
    {
      "name": "demo-plugin",
      "source": {
        "source": "local",
        "path": "./plugins/demo-plugin"
      }
    }
  ]
}"#,
    )?;
    write_plugins_enabled_config(&codex_home)?;

    let mut mcp = TestAppServer::builder()
        .with_codex_home(codex_home.path())
        .without_auto_env()
        .build_initialized_with_timeout(DEFAULT_TIMEOUT)
        .await?;

    let request_id = mcp
        .send_plugin_read_request(PluginReadParams {
            marketplace_path: Some(AbsolutePathBuf::try_from(
                repo_root.path().join(".agents/plugins/marketplace.json"),
            )?),
            plugin_name: "demo-plugin".to_string(),
        })
        .await?;

    let err = timeout(
        DEFAULT_TIMEOUT,
        mcp.read_stream_until_error_message(RequestId::Integer(request_id)),
    )
    .await??;

    assert_eq!(err.error.code, -32600);
    assert!(err.error.message.contains("missing or invalid plugin.json"));
    Ok(())
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

fn write_plugins_enabled_config(codex_home: &TempDir) -> Result<()> {
    std::fs::write(
        codex_home.path().join("config.toml"),
        r#"[features]
plugins = true
"#,
    )?;
    Ok(())
}

#[derive(Clone)]
struct AppsServerState {
    connectors: Vec<AppInfo>,
}

async fn start_apps_server(connectors: Vec<AppInfo>) -> Result<(String, JoinHandle<()>)> {
    let state = Arc::new(AppsServerState { connectors });

    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let addr = listener.local_addr()?;
    let router = Router::new()
        .route("/ps/apps/batch", post(batch_apps))
        .with_state(state);

    let handle = tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    });

    Ok((format!("http://{addr}"), handle))
}

async fn batch_apps(
    State(state): State<Arc<AppsServerState>>,
    headers: HeaderMap,
    Json(body): Json<serde_json::Value>,
) -> Result<impl axum::response::IntoResponse, StatusCode> {
    let bearer_ok = headers
        .get(AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value == "Bearer chatgpt-token");
    let account_ok = headers
        .get("chatgpt-account-id")
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value == "account-123");
    let product_sku_ok = headers
        .get("oai-product-sku")
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value == "codex");

    if !bearer_ok || !account_ok || !product_sku_ok {
        Err(StatusCode::UNAUTHORIZED)
    } else {
        let app_ids = body
            .get("app_ids")
            .and_then(serde_json::Value::as_array)
            .ok_or(StatusCode::BAD_REQUEST)?;
        if app_ids.len() > 100 {
            return Err(StatusCode::BAD_REQUEST);
        }
        let apps = state
            .connectors
            .iter()
            .filter(|connector| {
                app_ids
                    .iter()
                    .any(|app_id| app_id.as_str() == Some(connector.id.as_str()))
            })
            .map(|connector| {
                json!({
                    "id": connector.id,
                    "name": connector.name,
                    "description": connector.description,
                    "icon_url": connector.logo_url,
                    "tools": null
                })
            })
            .collect::<Vec<_>>();
        Ok(Json(json!({ "apps": apps })))
    }
}

fn write_connectors_config(codex_home: &std::path::Path, base_url: &str) -> std::io::Result<()> {
    std::fs::write(
        codex_home.join("config.toml"),
        format!(
            r#"
chatgpt_base_url = "{base_url}"
cli_auth_credentials_store = "file"
mcp_oauth_credentials_store = "file"

[features]
plugins = true
connectors = true
"#
        ),
    )
}

fn write_plugin_marketplace(
    repo_root: &std::path::Path,
    marketplace_name: &str,
    plugin_name: &str,
    source_path: &str,
) -> std::io::Result<()> {
    std::fs::create_dir_all(repo_root.join(".git"))?;
    std::fs::create_dir_all(repo_root.join(".agents/plugins"))?;
    std::fs::write(
        repo_root.join(".agents/plugins/marketplace.json"),
        format!(
            r#"{{
  "name": "{marketplace_name}",
  "plugins": [
    {{
      "name": "{plugin_name}",
      "source": {{
        "source": "local",
        "path": "{source_path}"
      }}
    }}
  ]
}}"#
        ),
    )
}

fn write_plugin_source(
    repo_root: &std::path::Path,
    plugin_name: &str,
    app_ids: &[&str],
) -> Result<()> {
    let plugin_root = repo_root.join(plugin_name);
    std::fs::create_dir_all(plugin_root.join(".codex-plugin"))?;
    std::fs::write(
        plugin_root.join(".codex-plugin/plugin.json"),
        format!(r#"{{"name":"{plugin_name}"}}"#),
    )?;

    let apps = app_ids
        .iter()
        .map(|app_id| ((*app_id).to_string(), json!({ "id": app_id })))
        .collect::<serde_json::Map<_, _>>();
    std::fs::write(
        plugin_root.join(".app.json"),
        serde_json::to_vec_pretty(&json!({ "apps": apps }))?,
    )?;
    Ok(())
}
