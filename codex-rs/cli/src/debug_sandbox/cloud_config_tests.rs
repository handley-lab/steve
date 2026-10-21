use anyhow::Result;
use app_test_support::ChatGptAuthFixture;
use app_test_support::write_chatgpt_auth;
use codex_app_server::in_process::EmbeddedNetworkPolicy;
use codex_cloud_config::cloud_config_bundle_loader_for_storage;
use codex_config::CloudConfigBundleLoader;
use codex_config::ConfigLoadOptions;
use codex_config::ConfigRequirementsToml;
use codex_config::LoaderOverrides;
use codex_config::types::AuthCredentialsStoreMode;
use codex_core::config::bootstrap_account_path_style;
use codex_core::config::bootstrap_auth_config;
use codex_core::config::load_config_toml_with_layer_stack;
use codex_protocol::permissions::NetworkSandboxPolicy;
use codex_utils_absolute_path::AbsolutePathBuf;
use pretty_assertions::assert_eq;
use serde_json::Value;
use serde_json::json;
use tempfile::TempDir;
use wiremock::Mock;
use wiremock::MockServer;
use wiremock::ResponseTemplate;
use wiremock::matchers::header;
use wiremock::matchers::method;
use wiremock::matchers::path;

use super::super::DebugSandboxConfigOptions;
use super::super::ManagedRequirementsMode;
use super::super::load_debug_sandbox_config_with_codex_home;

const CLOUD_MANAGED_PERMISSION_PROFILE_REQUIREMENTS: &str = r#"
default_permissions = "managed-cloud"

[allowed_permission_profiles]
managed-cloud = true

[permissions.managed-cloud]
extends = ":workspace"

[permissions.managed-cloud.network]
enabled = true
"#;

#[tokio::test]
async fn debug_sandbox_bootstraps_cloud_managed_permission_profile_from_backend() -> Result<()> {
    for (style_toml, request_path) in [
        ("", "/backend-api/api/codex/config/bundle"),
        (
            "chatgpt_path_style = \"chatgpt_api\"\n",
            "/backend-api/wham/config/bundle",
        ),
    ] {
    let server = MockServer::start().await;
    let expected_requirements = json!([{
        "id": "req-managed-cloud",
        "name": "Managed permissions",
        "contents": CLOUD_MANAGED_PERMISSION_PROFILE_REQUIREMENTS,
    }]);
    Mock::given(method("GET"))
        .and(path(request_path))
        .and(header("authorization", "Bearer chatgpt-token"))
        .and(header("chatgpt-account-id", "workspace-123"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "requirements_toml": {
                "enterprise_managed": expected_requirements.clone(),
            },
        })))
        .expect(1)
        .mount(&server)
        .await;

    let codex_home = TempDir::new()?;
    std::fs::write(
        codex_home.path().join("config.toml"),
        format!(
            "model_provider = \"openai\"\ncli_auth_credentials_store = \"file\"\nchatgpt_base_url = \"{}/backend-api\"\n{style_toml}",
            server.uri(),
        ),
    )?;
    write_chatgpt_auth(
        codex_home.path(),
        ChatGptAuthFixture::new("chatgpt-token")
            .account_id("workspace-123")
            .chatgpt_account_id("workspace-123")
            .chatgpt_user_id("user-123")
            .plan_type("enterprise"),
        AuthCredentialsStoreMode::File,
    )?;

    let options = DebugSandboxConfigOptions {
        sandbox_state: Default::default(),
        permissions_profile: Some("managed-cloud".to_string()),
        cwd: Some(codex_home.path().to_path_buf()),
        managed_requirements_mode: ManagedRequirementsMode::Include,
        loader_overrides: LoaderOverrides::without_managed_config_for_tests(),
    };
    let cwd = AbsolutePathBuf::from_absolute_path(codex_home.path())?;
    let bootstrap_config = load_config_toml_with_layer_stack(
        codex_home.path(),
        Some(&cwd),
        Vec::new(),
        ConfigLoadOptions {
            loader_overrides: options.loader_overrides.clone(),
            strict_config: false,
            cloud_config_bundle: CloudConfigBundleLoader::default(),
        },
    )
    .await?;
    let path_style = bootstrap_account_path_style(&bootstrap_config);
    let cloud_config_bundle = cloud_config_bundle_loader_for_storage(
        EmbeddedNetworkPolicy::load(&options.loader_overrides)
            .await
            .bind_bootstrap_auth(
                bootstrap_auth_config(codex_home.path(), &bootstrap_config)?,
                path_style,
            ),
        path_style,
        /*enable_codex_api_key_env*/ false,
    )
    .await?;
    let config = load_debug_sandbox_config_with_codex_home(
        Vec::new(),
        /*codex_linux_sandbox_exe*/ None,
        options,
        Some(codex_home.path().to_path_buf()),
        cloud_config_bundle,
        /*strict_config*/ false,
    )
    .await?;

    assert_eq!(
        config
            .permissions
            .active_permission_profile()
            .map(|profile| profile.id),
        Some("managed-cloud".to_string()),
    );
    assert_eq!(
        config.permissions.network_sandbox_policy(),
        NetworkSandboxPolicy::Enabled,
    );
    assert_eq!(
        config.config_layer_stack.requirements_toml(),
        &toml::from_str::<ConfigRequirementsToml>(CLOUD_MANAGED_PERMISSION_PROFILE_REQUIREMENTS,)?,
    );

    let cache: Value = serde_json::from_slice(&std::fs::read(
        codex_home.path().join("cloud-config-bundle-cache.json"),
    )?)?;
    assert_eq!(
        json!({
            "chatgpt_user_id": cache["signed_payload"]["chatgpt_user_id"],
            "account_id": cache["signed_payload"]["account_id"],
            "requirements_toml": cache["signed_payload"]["bundle"]["requirements_toml"],
        }),
        json!({
            "chatgpt_user_id": "user-123",
            "account_id": "workspace-123",
            "requirements_toml": {
                "enterprise_managed": expected_requirements,
            },
        }),
    );
    server.verify().await;
    }

    Ok(())
}
