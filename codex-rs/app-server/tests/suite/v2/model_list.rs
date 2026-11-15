use std::time::Duration;

use anyhow::Result;
use app_test_support::TestAppServer;
use app_test_support::write_models_cache;
use codex_app_server_protocol::ClientRequest;
use codex_app_server_protocol::ExperimentalFeatureEnablementSetParams;
use codex_app_server_protocol::ExperimentalFeatureEnablementSetResponse;
use codex_app_server_protocol::JSONRPCError;
use codex_app_server_protocol::Model;
use codex_app_server_protocol::ModelListParams;
use codex_app_server_protocol::ModelListResponse;
use codex_app_server_protocol::ModelServiceTier;
use codex_app_server_protocol::ModelUpgradeInfo;
use codex_app_server_protocol::ReasoningEffortOption;
use codex_app_server_protocol::RequestId;
use codex_config::types::AuthCredentialsStoreMode;
use codex_login::AuthKeyringBackendKind;
use codex_login::login_with_api_key;
use codex_protocol::openai_models::ModelPreset;
use codex_protocol::openai_models::ModelsResponse;
use pretty_assertions::assert_eq;
use tempfile::TempDir;
use test_case::test_case;
use tokio::time::timeout;
use wiremock::Mock;
use wiremock::MockServer;
use wiremock::ResponseTemplate;
use wiremock::matchers::method;
use wiremock::matchers::path;

const DEFAULT_TIMEOUT: Duration = Duration::from_secs(10);
const INVALID_REQUEST_ERROR_CODE: i64 = -32600;

#[test_case(None, None, true; "default on")]
#[test_case(None, Some(false), true; "app rollback")]
#[test_case(None, Some(true), true; "app rollout")]
#[test_case(Some(false), Some(true), true; "user opt out")]
#[test_case(Some(true), Some(false), true; "user opt in")]
#[test_case(Some(true), Some(false), false; "base URL without catalog URL")]
#[tokio::test]
async fn api_key_model_discovery_startup_enablement_respects_user_config(
    user_enablement: Option<bool>,
    app_enablement: Option<bool>,
    catalog_opt_in: bool,
) -> Result<()> {
    let server = MockServer::start().await;
    let mut remote_model = codex_models_manager::test_support::model_info("test-model");
    remote_model.slug = "rollout-model".into();
    remote_model.visibility = codex_protocol::openai_models::ModelVisibility::List;
    remote_model.supported_in_api = true;
    Mock::given(method("GET"))
        .and(path("/v1/models"))
        .respond_with(
            ResponseTemplate::new(/*s*/ 200).set_body_json(ModelsResponse {
                models: vec![remote_model.clone()],
            }),
        )
        .mount(&server)
        .await;
    let codex_home = TempDir::new()?;
    let server_uri = server.uri();
    let feature_config = user_enablement
        .map(|enabled| format!("[features]\napi_key_model_discovery = {enabled}\n"))
        .unwrap_or_default();
    let catalog_config = if catalog_opt_in {
        format!("model_catalog_url = \"{server_uri}/v1/models\"\n")
    } else {
        String::new()
    };
    std::fs::write(
        codex_home.path().join("config.toml"),
        format!(
            r#"
model_provider = "catalog-test"
{feature_config}
[model_providers.catalog-test]
name = "OpenAI"
base_url = "{server_uri}/v1"
requires_openai_auth = true
{catalog_config}
"#
        ),
    )?;
    login_with_api_key(
        codex_home.path(),
        "test-key",
        AuthCredentialsStoreMode::File,
        AuthKeyringBackendKind::default(),
    )?;
    let mut mcp = TestAppServer::builder()
        .with_codex_home(codex_home.path())
        .with_env_overrides(&[("OPENAI_API_KEY", None), ("STEVE_API_KEY", None)])
        .build_initialized()
        .await?;
    // Exercise runtime toggles after login has replaced the startup catalog manager.
    if let Some(enabled) = app_enablement {
        let _: ExperimentalFeatureEnablementSetResponse = mcp
            .request(
                |request_id| ClientRequest::ExperimentalFeatureEnablementSet {
                    request_id,
                    params: ExperimentalFeatureEnablementSetParams {
                        enablement: [("api_key_model_discovery".to_string(), !enabled)].into(),
                    },
                },
            )
            .await?;
    }
    let config_path = codex_home.path().join("config.toml");
    let config = std::fs::read_to_string(&config_path)?
        .replace("name = \"OpenAI\"", "name = \"After login\"");
    std::fs::write(config_path, config)?;
    let login = mcp.send_login_account_api_key_request("test-key").await?;
    let _: codex_app_server_protocol::LoginAccountResponse = mcp.read_response(login).await?;
    let _: ModelListResponse = mcp
        .request(|request_id| ClientRequest::ModelList {
            request_id,
            params: ModelListParams::default(),
        })
        .await?;
    let mut remote = vec![ModelPreset::from(remote_model)];
    ModelPreset::mark_default_by_picker_visibility(&mut remote);
    let _: ExperimentalFeatureEnablementSetResponse = mcp
        .request(
            |request_id| ClientRequest::ExperimentalFeatureEnablementSet {
                request_id,
                params: ExperimentalFeatureEnablementSetParams {
                    enablement: app_enablement
                        .into_iter()
                        .map(|enabled| ("api_key_model_discovery".to_string(), enabled))
                        .collect(),
                },
            },
        )
        .await?;
    let response: ModelListResponse = mcp
        .request(|request_id| ClientRequest::ModelList {
            request_id,
            params: ModelListParams {
                thread_id: None,
                limit: Some(100),
                include_hidden: Some(true),
                cursor: None,
                ..Default::default()
            },
        })
        .await?;
    let enabled = user_enablement.or(app_enablement).unwrap_or(true);
    let expected: &[ModelPreset] = if enabled { &remote } else { &[] };
    assert_eq!(
        response,
        ModelListResponse {
            provider_errors: Vec::new(),
            data: expected.iter().map(model_from_preset).collect(),
            next_cursor: None,
            cyber_access_programs_enabled: None,
        }
    );
    let requests = server
        .received_requests()
        .await
        .expect("request recording is enabled");
    let model_requests: Vec<_> = requests
        .iter()
        .filter(|request| request.url.path() == "/v1/models")
        .collect();
    // Only startup opt-outs prevent fetches before the app sends its enablement.
    if user_enablement == Some(false) {
        assert!(model_requests.is_empty());
    } else if enabled {
        assert!(!model_requests.is_empty());
        assert!(model_requests.iter().all(|request| request.method == "GET"));
    }
    Ok(())
}

fn model_from_preset(preset: &ModelPreset) -> Model {
    Model {
        id: preset.id.clone(),
        model: preset.model.clone(),
        upgrade: preset.upgrade.as_ref().map(|upgrade| upgrade.id.clone()),
        upgrade_info: preset.upgrade.as_ref().map(|upgrade| ModelUpgradeInfo {
            model: upgrade.id.clone(),
            upgrade_copy: upgrade.upgrade_copy.clone(),
            model_link: upgrade.model_link.clone(),
            migration_markdown: upgrade.migration_markdown.clone(),
            retirement_at: upgrade
                .retirement_at
                .as_ref()
                .map(chrono::DateTime::timestamp),
        }),
        availability_nux: preset.availability_nux.clone().map(Into::into),
        display_name: preset.display_name.clone(),
        description: preset.description.clone(),
        model_specialty: preset.model_specialty.clone(),
        hidden: !preset.show_in_picker,
        supported_reasoning_efforts: preset
            .supported_reasoning_efforts
            .iter()
            .map(|preset| ReasoningEffortOption {
                reasoning_effort: preset.effort.clone(),
                description: preset.description.clone(),
            })
            .collect(),
        default_reasoning_effort: preset.default_reasoning_effort.clone(),
        input_modalities: preset.input_modalities.clone(),
        supports_personality: false,
        multi_agent_version: preset.multi_agent_version.map(Into::into),
        additional_speed_tiers: preset.additional_speed_tiers.clone(),
        service_tiers: preset
            .service_tiers
            .iter()
            .map(|service_tier| ModelServiceTier {
                id: service_tier.id.clone(),
                name: service_tier.name.clone(),
                description: service_tier.description.clone(),
            })
            .collect(),
        default_service_tier: preset.default_service_tier.clone(),
        available_access_programs: preset.available_access_programs.clone().map(Into::into),
        is_default: preset.is_default,
        is_provider_default: preset.is_provider_default,
    }
}

fn expected_visible_models() -> Vec<Model> {
    let mut presets = vec![ModelPreset::from(app_test_support::cached_model())];

    // Mirror `ModelsManager::build_available_models()` default selection.
    ModelPreset::mark_default_by_picker_visibility(&mut presets);

    presets
        .iter()
        .filter(|preset| preset.show_in_picker)
        .map(model_from_preset)
        .collect()
}

#[tokio::test]
async fn list_models_returns_all_models_with_large_limit() -> Result<()> {
    let codex_home = TempDir::new()?;
    write_models_cache(codex_home.path()).await?;
    let mut mcp = TestAppServer::builder()
        .with_codex_home(codex_home.path())
        .without_auto_env()
        .build_initialized()
        .await?;
    let ModelListResponse {
        data: items,
        next_cursor,
        ..
    } = mcp
        .request(|request_id| ClientRequest::ModelList {
            request_id,
            params: ModelListParams {
                thread_id: None,
                limit: Some(100),
                cursor: None,
                include_hidden: None,
                ..Default::default()
            },
        })
        .await?;

    let expected_models = expected_visible_models();

    assert_eq!(items, expected_models);
    assert!(next_cursor.is_none());
    Ok(())
}

#[tokio::test]
async fn list_models_includes_hidden_models() -> Result<()> {
    let codex_home = TempDir::new()?;
    let mut hidden = codex_models_manager::test_support::model_info("hidden-model");
    hidden.visibility = codex_protocol::openai_models::ModelVisibility::Hide;
    app_test_support::write_models_cache_with_models(codex_home.path(), vec![hidden]).await?;
    let mut mcp = TestAppServer::builder()
        .with_codex_home(codex_home.path())
        .without_auto_env()
        .build_initialized()
        .await?;
    let ModelListResponse {
        data: items,
        next_cursor,
        ..
    } = mcp
        .request(|request_id| ClientRequest::ModelList {
            request_id,
            params: ModelListParams {
                thread_id: None,
                limit: Some(100),
                cursor: None,
                include_hidden: Some(true),
                ..Default::default()
            },
        })
        .await?;

    assert!(items.iter().any(|item| item.hidden));
    assert!(next_cursor.is_none());
    Ok(())
}

#[tokio::test]
async fn list_models_pagination_works() -> Result<()> {
    let codex_home = TempDir::new()?;
    let models = vec![
        app_test_support::cached_model(),
        codex_models_manager::test_support::model_info("second-model"),
    ];
    app_test_support::write_models_cache_with_models(codex_home.path(), models.clone()).await?;
    let mut presets: Vec<ModelPreset> = models.into_iter().map(Into::into).collect();
    ModelPreset::mark_default_by_picker_visibility(&mut presets);
    let mut mcp = TestAppServer::builder()
        .with_codex_home(codex_home.path())
        .without_auto_env()
        .build_initialized()
        .await?;

    let expected_models: Vec<Model> = presets.iter().map(model_from_preset).collect();
    let mut cursor = None;
    let mut items = Vec::new();

    for _ in 0..expected_models.len() {
        let ModelListResponse {
            data: page_items,
            next_cursor,
            ..
        } = mcp
            .request(|request_id| ClientRequest::ModelList {
                request_id,
                params: ModelListParams {
                    thread_id: None,
                    limit: Some(1),
                    cursor: cursor.clone(),
                    include_hidden: None,
                    ..Default::default()
                },
            })
            .await?;

        assert_eq!(page_items.len(), 1);
        items.extend(page_items);

        if let Some(next_cursor) = next_cursor {
            cursor = Some(next_cursor);
        } else {
            assert_eq!(items, expected_models);
            return Ok(());
        }
    }

    panic!(
        "model pagination did not terminate after {} pages",
        expected_models.len()
    );
}

#[tokio::test]
async fn list_models_rejects_invalid_cursor() -> Result<()> {
    let codex_home = TempDir::new()?;
    write_models_cache(codex_home.path()).await?;
    let mut mcp = TestAppServer::builder()
        .with_codex_home(codex_home.path())
        .without_auto_env()
        .build_initialized()
        .await?;

    let request_id = mcp
        .send_list_models_request(ModelListParams {
            thread_id: None,
            limit: None,
            cursor: Some("invalid".to_string()),
            include_hidden: None,
            ..Default::default()
        })
        .await?;

    let error: JSONRPCError = timeout(
        DEFAULT_TIMEOUT,
        mcp.read_stream_until_error_message(RequestId::Integer(request_id)),
    )
    .await??;

    assert_eq!(error.id, RequestId::Integer(request_id));
    assert_eq!(error.error.code, INVALID_REQUEST_ERROR_CODE);
    assert_eq!(error.error.message, "invalid cursor: invalid");
    Ok(())
}
