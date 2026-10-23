//! Supplied credentials must bootstrap configured services through the system proxy.

use anyhow::Context;
use anyhow::Result;
use app_test_support::ChatGptIdTokenClaims;
use app_test_support::MockResponsesConfig;
use app_test_support::encode_id_token;
use app_test_support::mount_workspace_routing;
use codex_app_server::in_process;
use codex_app_server::in_process::InProcessServerEvent;
use codex_app_server::in_process::InProcessStartArgs;
use codex_app_server_protocol::ClientInfo;
use codex_app_server_protocol::ClientRequest;
use codex_app_server_protocol::InitializeCapabilities;
use codex_app_server_protocol::InitializeParams;
use codex_app_server_protocol::LoginAccountParams;
use codex_app_server_protocol::LoginAccountResponse;
use codex_app_server_protocol::RequestId;
use codex_app_server_protocol::ServerNotification;
use codex_app_server_protocol::ThreadStartParams;
use codex_app_server_protocol::ThreadStartResponse;
use codex_app_server_protocol::TurnStartParams;
use codex_app_server_protocol::TurnStatus;
use codex_app_server_protocol::UserInput;
use codex_arg0::Arg0DispatchPaths;
use codex_config::CloudConfigBundleLoader;
use codex_config::LoaderOverrides;
use codex_core::config::ConfigBuilder;
use codex_exec_server::EnvironmentManager;
use codex_feedback::CodexFeedback;
use codex_http_client::cache_system_proxy_route_for_test;
use codex_protocol::protocol::SessionSource;
use core_test_support::responses;
use core_test_support::skip_if_no_network;
use pretty_assertions::assert_eq;
use serde_json::json;
use std::sync::Arc;
use std::time::Duration;
use tempfile::TempDir;
use tokio::process::Command;
use tokio::time::timeout;
use wiremock::Mock;
use wiremock::MockServer;
use wiremock::ResponseTemplate;
use wiremock::matchers::method;
use wiremock::matchers::path;

const TEST_NAME: &str =
    "suite::v2::account_system_proxy::supplied_token_bootstraps_through_system_proxy";
const TEST_MODE: &str = "STEVE_APP_SERVER_SYSTEM_PROXY_TEST_MODE";
const BLOCKED_ORIGIN: &str = "http://127.0.0.1:0";
const READ_TIMEOUT: Duration = Duration::from_secs(60);

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn supplied_token_bootstraps_through_system_proxy() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let Ok(mode) = std::env::var(TEST_MODE) else {
        // The in-process server shares the proxy cache. Isolate environment-derived
        // routing and the login issuer from the other integration tests.
        for mode in ["bootstrap-only", "cloud-enables-proxy"] {
            let mut command = Command::new(std::env::current_exe()?);
            command.arg("--exact").arg(TEST_NAME);
            for key in codex_network_proxy::PROXY_ENV_KEYS {
                command.env_remove(key);
            }
            let output = timeout(
                Duration::from_secs(120),
                command
                    .kill_on_drop(true)
                    .env(TEST_MODE, mode)
                    .env_remove("OPENAI_API_KEY")
                    .env_remove("STEVE_API_KEY")
                    .output(),
            )
            .await??;
            assert!(
                output.status.success(),
                "{mode} failed\nstdout:\n{}\nstderr:\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr),
            );
        }
        return Ok(());
    };

    let cloud_enables_proxy = mode == "cloud-enables-proxy";
    let proxy = MockServer::start().await;
    let provider = MockServer::start().await;
    let codex_home = TempDir::new()?;
    let model_catalog = codex_protocol::openai_models::ModelsResponse {
        models: vec![codex_models_manager::test_support::model_info("gpt-5.5")],
    };
    let model_catalog_path = codex_home.path().join("models.json");
    std::fs::write(&model_catalog_path, serde_json::to_vec(&model_catalog)?)?;
    MockResponsesConfig::new(&provider.uri())
        .with_model(&model_catalog.models[0].slug)
        .with_provider_base_url(&format!(
            "{}/v1",
            if cloud_enables_proxy {
                BLOCKED_ORIGIN.to_string()
            } else {
                provider.uri()
            }
        ))
        .with_provider_config("requires_openai_auth = true\nsupports_websockets = false")
        .with_root_config(&format!(
            "chatgpt_base_url = \"{BLOCKED_ORIGIN}/backend-api\"\ncli_auth_credentials_store = \"file\"\nmodel_catalog_json = {}",
            serde_json::to_string(&model_catalog_path)?,
        ))
        .write(codex_home.path())?;
    let id_token = encode_id_token(
        &ChatGptIdTokenClaims::new()
            .email("proxy@example.com")
            .plan_type("enterprise")
            .chatgpt_user_id("proxy-user")
            .chatgpt_account_id("proxy-workspace"),
    )?;
    let cloud_config = if cloud_enables_proxy {
        json!({"config_toml": {"enterprise_managed": [{
            "id": "proxy-rollout",
            "name": "System proxy rollout",
            "contents": "[features]\nrespect_system_proxy = true\n",
        }]}})
    } else {
        json!({})
    };
    Mock::given(method("GET"))
        .and(path("/backend-api/wham/config/bundle"))
        .respond_with(ResponseTemplate::new(200).set_body_json(cloud_config))
        .expect(1..)
        .mount(&proxy)
        .await;
    mount_workspace_routing(&proxy).await;
    for route in [
        "/backend-api/wham/config/bundle",
        "/backend-api/wham/accounts/check",
        "/v1/responses",
    ] {
        cache_system_proxy_route_for_test(&format!("{BLOCKED_ORIGIN}{route}"), proxy.uri());
    }
    let model_requests = responses::mount_sse_once(
        if cloud_enables_proxy {
            &proxy
        } else {
            &provider
        },
        responses::sse(vec![responses::ev_completed("proxy-response")]),
    )
    .await;

    let loader_overrides = LoaderOverrides::without_managed_config_for_tests();
    let config = ConfigBuilder::default()
        .codex_home(codex_home.path().to_path_buf())
        .fallback_cwd(Some(codex_home.path().to_path_buf()))
        .loader_overrides(loader_overrides.clone())
        .build()
        .await?;
    assert!(!config.respect_system_proxy);
    let mut client = in_process::start(InProcessStartArgs {
        arg0_paths: Arg0DispatchPaths::default(),
        config: Arc::new(config),
        cli_overrides: Vec::new(),
        loader_overrides,
        strict_config: false,
        cloud_config_bundle: CloudConfigBundleLoader::default(),
        embedded_network_policy: Default::default(),
        thread_config_loader: Arc::new(codex_config::NoopThreadConfigLoader),
        feedback: CodexFeedback::new(),
        log_db: None,
        state_db: None,
        environment_manager: Arc::new(EnvironmentManager::default_for_tests()),
        config_warnings: Vec::new(),
        session_source: SessionSource::Cli,
        enable_codex_api_key_env: false,
        initialize: InitializeParams {
            client_info: ClientInfo {
                name: "codex-app-server-tests".to_string(),
                title: None,
                version: "0.1.0".to_string(),
            },
            capabilities: Some(InitializeCapabilities {
                experimental_api: true,
                ..Default::default()
            }),
        },
        channel_capacity: in_process::DEFAULT_IN_PROCESS_CHANNEL_CAPACITY,
    })
    .await?;
    let login: LoginAccountResponse = serde_json::from_value(
        client
            .request(ClientRequest::LoginAccount {
                request_id: RequestId::Integer(1),
                params: LoginAccountParams::ChatgptAuthTokens {
                    access_token: id_token,
                    chatgpt_account_id: "proxy-workspace".to_string(),
                    chatgpt_plan_type: Some("enterprise".to_string()),
                },
            })
            .await?
            .expect("supplied credentials accepted"),
    )?;
    assert_eq!(login, LoginAccountResponse::ChatgptAuthTokens {});
    timeout(READ_TIMEOUT, async {
        loop {
            let event = client
                .next_event()
                .await
                .context("login completion event")?;
            if let InProcessServerEvent::ServerNotification(notification) = event
                && let ServerNotification::AccountLoginCompleted(completed) = notification.as_ref()
            {
                assert_eq!(completed.login_id, None);
                assert!(completed.success, "login failed: {completed:?}");
                return anyhow::Ok(());
            }
        }
    })
    .await??;
    let started: ThreadStartResponse = serde_json::from_value(
        client
            .request(ClientRequest::ThreadStart {
                request_id: RequestId::Integer(2),
                params: ThreadStartParams::default(),
            })
            .await?
            .expect("first thread starts without restarting"),
    )?;
    client
        .request(ClientRequest::TurnStart {
            request_id: RequestId::Integer(3),
            params: TurnStartParams {
                thread_id: started.thread.id.clone(),
                input: vec![UserInput::Text {
                    text: "Hello through the proxy".to_string(),
                    text_elements: Vec::new(),
                }],
                ..Default::default()
            },
        })
        .await?
        .expect("first turn starts without restarting");
    timeout(READ_TIMEOUT, async {
        loop {
            let event = client.next_event().await.context("turn completion event")?;
            if let InProcessServerEvent::ServerNotification(notification) = event
                && let ServerNotification::TurnCompleted(completed) = notification.as_ref()
                && completed.thread_id == started.thread.id
            {
                assert_eq!(completed.turn.status, TurnStatus::Completed);
                return anyhow::Ok(());
            }
        }
    })
    .await??;
    model_requests.single_request();
    client.shutdown().await?;
    proxy.verify().await;
    Ok(())
}
