use anyhow::Result;
use app_test_support::ChatGptAuthFixture;
use app_test_support::DEFAULT_CLIENT_NAME;
use app_test_support::MockResponsesConfig;
use app_test_support::TestAppServer;
use app_test_support::create_final_assistant_message_sse_response;
use app_test_support::create_mock_responses_server_sequence;
use app_test_support::to_response;
use app_test_support::write_chatgpt_auth;
use app_test_support::write_mock_responses_config_toml_with_chatgpt_base_url;
use codex_app_server_protocol::ApprovalsReviewer;
use codex_app_server_protocol::AskForApproval;
use codex_app_server_protocol::ClientRequest;
use codex_app_server_protocol::JSONRPCResponse;
use codex_app_server_protocol::RequestId;
use codex_app_server_protocol::SandboxPolicy;
use codex_app_server_protocol::ServerRequest;
use codex_app_server_protocol::ThreadItem;
use codex_app_server_protocol::ThreadReadParams;
use codex_app_server_protocol::ThreadReadResponse;
use codex_app_server_protocol::ThreadStartParams;
use codex_app_server_protocol::ThreadStartResponse;
use codex_app_server_protocol::TurnSettingsUpdateParams;
use codex_app_server_protocol::TurnSettingsUpdateResponse;
use codex_app_server_protocol::TurnSettingsUpdateStatus;
use codex_app_server_protocol::TurnStartParams;
use codex_app_server_protocol::TurnStartResponse;
use codex_app_server_protocol::UserInput;
use codex_config::types::AuthCredentialsStoreMode;
use codex_config::types::OtelExporterKind;
use codex_config::types::OtelHttpProtocol;
use codex_core::config::ConfigBuilder;
use codex_core_plugins::loader::curated_plugin_cache_version;
use codex_core_plugins::store::PluginStore;
use codex_features::Feature;
use codex_models_manager::bundled_models_response;
use codex_plugin::PluginId;
use codex_protocol::openai_models::ModelsResponse;
use codex_protocol::openai_models::ReasoningEffort;
use core_test_support::responses;
use core_test_support::skip_if_no_network;
use core_test_support::skip_if_remote;
use core_test_support::skip_if_wine_exec;
use pretty_assertions::assert_eq;
use serde_json::Value;
use serde_json::json;
use std::collections::HashMap;
use std::path::Path;
use std::path::PathBuf;
use std::time::Duration;
use tempfile::TempDir;
use tokio::time::timeout;
use wiremock::Mock;
use wiremock::MockServer;
use wiremock::ResponseTemplate;
use wiremock::matchers::method;
use wiremock::matchers::path;
use wiremock::matchers::path_regex;

const SERVICE_VERSION: &str = "0.0.0-test";



#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn multi_agent_v2_tools_emit_collaborator_analytics() -> Result<()> {
    const PARENT_PROMPT: &str = "exercise collaborator analytics";
    const READ_TIMEOUT: Duration = Duration::from_secs(30);
    let calls = [
        (
            "spawn_agent",
            json!({"task_name": "worker", "message": "PRIVATE_CHILD", "fork_turns": "none"}),
        ),
        (
            "send_message",
            json!({"target": "worker", "message": "PRIVATE_MESSAGE"}),
        ),
        (
            "followup_task",
            json!({"target": "worker", "message": "PRIVATE_FOLLOWUP"}),
        ),
        ("interrupt_agent", json!({"target": "worker"})),
        ("list_agents", json!({})),
        (
            "send_message",
            json!({"target": "missing", "message": "PRIVATE_MESSAGE"}),
        ),
        (
            "followup_task",
            json!({"target": "/root", "message": "PRIVATE_FOLLOWUP"}),
        ),
        ("interrupt_agent", json!({"target": "/root"})),
        ("list_agents", json!({"unexpected": true})),
        ("send_message", json!({})),
    ];
    let server = responses::start_mock_server().await;
    for (index, (tool, args)) in calls.iter().enumerate() {
        let response_id = format!("parent-response-{index}");
        responses::mount_sse_once_match(
            &server,
            |request: &wiremock::Request| {
                String::from_utf8_lossy(&request.body).contains(PARENT_PROMPT)
            },
            responses::sse(vec![
                responses::ev_response_created(&response_id),
                responses::ev_function_call_with_namespace(
                    &format!("call-{index}"),
                    "collaboration",
                    tool,
                    &args.to_string(),
                ),
                responses::ev_completed(&response_id),
            ]),
        )
        .await;
    }
    // Child turns and the final parent request only need an ordinary completion.
    Mock::given(method("POST"))
        .and(path_regex(".*/responses$"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(responses::sse(vec![responses::ev_completed(
                    "response-done",
                )])),
        )
        .with_priority(10)
        .mount(&server)
        .await;

    let codex_home = TempDir::new()?;
    MockResponsesConfig::new(&server.uri())
        .enable_feature(Feature::MultiAgentV2)
        .with_root_config(&format!("chatgpt_base_url = \"{}\"", server.uri()))
        .write(codex_home.path())?;
    mount_analytics_capture(&server, codex_home.path()).await?;
    let mut app_server = TestAppServer::builder()
        .with_codex_home(codex_home.path())
        .without_managed_config()
        .build_initialized()
        .await?;
    let thread = app_server
        .start_thread(ThreadStartParams::default())
        .await?
        .thread;
    let completed = app_server
        .start_turn_and_wait_for_completion(TurnStartParams {
            thread_id: thread.id.clone(),
            input: vec![UserInput::Text {
                text: PARENT_PROMPT.to_string(),
                text_elements: Vec::new(),
            }],
            ..Default::default()
        })
        .await?;
    let read: ThreadReadResponse = app_server
        .request(|request_id| ClientRequest::ThreadRead {
            request_id,
            params: ThreadReadParams {
                thread_id: thread.id.clone(),
                include_turns: true,
            },
        })
        .await?;
    let items = &read
        .thread
        .turns
        .iter()
        .find(|turn| turn.id == completed.turn.id)
        .expect("completed parent turn should be readable")
        .items;
    assert!(
        !items
            .iter()
            .any(|item| matches!(item, ThreadItem::CollabAgentToolCall { .. }))
    );
    let activity_ids = items
        .iter()
        .filter_map(|item| match item {
            ThreadItem::SubAgentActivity { id, .. } if id.starts_with("call-") => Some(id.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(activity_ids, vec!["call-0", "call-1", "call-2", "call-3"]);

    Ok(())
}

#[tokio::test]
async fn metrics_exporters_respect_analytics_preferences() -> Result<()> {
    let codex_home = TempDir::new()?;
    let mut config = ConfigBuilder::default()
        .codex_home(codex_home.path().to_path_buf())
        .build()
        .await?;
    for exporter in [
        OtelExporterKind::None,
        OtelExporterKind::Statsig,
        OtelExporterKind::OtlpHttp {
            endpoint: "http://localhost:4318".to_string(),
            headers: HashMap::new(),
            protocol: OtelHttpProtocol::Json,
            tls: None,
        },
        OtelExporterKind::OtlpGrpc {
            endpoint: "http://localhost:4317".to_string(),
            headers: HashMap::new(),
            tls: None,
        },
    ] {
        config.otel.metrics_exporter = exporter;
        for analytics_enabled in [None, Some(false), Some(true)] {
            config.analytics_enabled = analytics_enabled;
            for default_analytics_enabled in [false, true] {
                let expected_metrics = match config.otel.metrics_exporter {
                    OtelExporterKind::None => false,
                    // The built-in exporter is disabled in debug builds even with consent.
                    OtelExporterKind::Statsig => false,
                    OtelExporterKind::OtlpHttp { .. } | OtelExporterKind::OtlpGrpc { .. } => true,
                };
                let provider = codex_core::otel_init::build_provider(
                    &config,
                    SERVICE_VERSION,
                    Some("steve-app-server"),
                )
                .map_err(|err| anyhow::anyhow!(err.to_string()))?;
                assert_eq!(
                    provider.as_ref().and_then(|otel| otel.metrics()).is_some(),
                    expected_metrics,
                    "exporter={:?}, analytics={analytics_enabled:?}, default={default_analytics_enabled}",
                    config.otel.metrics_exporter,
                );
            }
        }
    }

    Ok(())
}

pub(crate) async fn mount_analytics_capture(server: &MockServer, codex_home: &Path) -> Result<()> {

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



