use std::time::Duration;

use anyhow::Result;
use app_test_support::MockResponsesConfig;
use app_test_support::TestAppServer;
use codex_app_server_protocol::ThreadStartParams;
use codex_app_server_protocol::TurnStartParams;
use codex_app_server_protocol::UserInput;
use core_test_support::load_default_config_for_test;
use core_test_support::responses;
use pretty_assertions::assert_eq;
use serde_json::Value;
use serde_json::json;
use tempfile::TempDir;
use test_case::test_case;
use tokio::time::timeout;
use wiremock::Mock;
use wiremock::Request;
use wiremock::ResponseTemplate;
use wiremock::matchers::method;
use wiremock::matchers::path;

use super::analytics::mount_analytics_capture;

const DEFAULT_READ_TIMEOUT: Duration = Duration::from_secs(30);

const BRIDGE_HINT: &str = "unstructured notes/thread_hint fixture result";

#[test_case(true, true; "native_hint")]
#[test_case(true, false; "native_hint_without_experimental_capability")]
#[test_case(false, true; "bridge_hint")]
#[tokio::test]
async fn app_server_uses_configured_notes_backend_for_context_window_hints(
    use_history_notes_extension: bool,
    supports_experimental_context: bool,
) -> Result<()> {
    let server = responses::start_mock_server().await;
    let backend = responses::start_mock_server().await;
    let response_mock = responses::mount_sse_once(
        &server,
        responses::sse(vec![
            responses::ev_response_created("resp-1"),
            responses::ev_assistant_message("msg-1", "done"),
            responses::ev_completed("resp-1"),
        ]),
    )
    .await;
    // HTTP MCP stays on the app-server host, including with a remote executor.
    Mock::given(method("POST"))
        .and(path("/mcp"))
        .respond_with(|request: &Request| {
            let body: Value = request.body_json().expect("MCP JSON-RPC request");
            let Some(id) = body.get("id") else {
                return ResponseTemplate::new(202);
            };
            let result = match body["method"].as_str() {
                Some("initialize") => json!({
                    "protocolVersion": body["params"]["protocolVersion"],
                    "capabilities": {"tools": {}},
                    "serverInfo": {"name": "notes-fixture", "version": "1.0.0"},
                }),
                Some("tools/list") => json!({
                    "tools": [{
                        "name": "thread_hint",
                        "inputSchema": {"type": "object", "properties": {}},
                        "annotations": {"readOnlyHint": true},
                        "_meta": {"ui": {"visibility": []}},
                    }],
                }),
                Some("tools/call") => {
                    assert_eq!(body["params"]["name"], "thread_hint");
                    let thread_id = body["params"]["_meta"]["threadId"]
                        .as_str()
                        .expect("threadId metadata");
                    json!({"content": [
                        {"type": "text", "text": format!("manual history hint for thread {thread_id}")},
                        {"type": "text", "text": BRIDGE_HINT},
                    ]})
                }
                _ => {
                    return ResponseTemplate::new(200).set_body_json(json!({
                        "jsonrpc": "2.0",
                        "id": id,
                        "error": {"code": -32601, "message": "Method not found"},
                    }));
                }
            };
            ResponseTemplate::new(200).set_body_json(json!({
                "jsonrpc": "2.0", "id": id, "result": result,
            }))
        })
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/mcp"))
        .respond_with(ResponseTemplate::new(405))
        .mount(&server)
        .await;
    let codex_home = TempDir::new()?;
    let config = load_default_config_for_test(&codex_home).await;
    let mut model = codex_core::test_support::construct_model_info_offline("mock-model", &config);
    model.supports_experimental_context = supports_experimental_context;
    let catalog_path = codex_home.path().join("models.json");
    std::fs::write(
        &catalog_path,
        serde_json::to_vec(&json!({"models": [model]}))?,
    )?;
    MockResponsesConfig::new(&server.uri())
        .with_root_config(&format!("chatgpt_base_url = \"{}\"", backend.uri()))
        .with_root_config(&format!(
            "model_catalog_json = {}",
            serde_json::to_string(&catalog_path)?
        ))
        .with_model_provider("openai-custom")
        .with_provider_name("OpenAI")
        .with_provider_base_url(&format!("{}/backend-api/codex", server.uri()))
        .with_provider_config("supports_websockets = false\nrequires_openai_auth = true")
        .with_extra_config(&format!(
            "[features.context_management]\nexperimental_mode = false\n\n[features.token_budget]\nenabled = true\nuse_history_notes_extension = {use_history_notes_extension}\n\n[mcp_servers.notes]\nurl = \"{}/mcp\"\nstartup_timeout_sec = 10\n",
            server.uri(),
        ))
        .write(codex_home.path())?;
    mount_analytics_capture(&backend, codex_home.path()).await?;

    let mut app_server = TestAppServer::builder()
        .with_codex_home(codex_home.path())
        .with_env_overrides(&[("OPENAI_API_KEY", None)])
        .build_initialized()
        .await?;
    let thread = app_server
        .start_thread(ThreadStartParams::default())
        .await?
        .thread;
    timeout(
        Duration::from_secs(10),
        app_server.start_turn_and_wait_for_completion(TurnStartParams {
            thread_id: thread.id.clone(),
            input: vec![UserInput::Text {
                text: "inspect history and notes".to_string(),
                text_elements: Vec::new(),
            }],
            ..Default::default()
        }),
    )
    .await??;

    let request = response_mock.single_request();
    assert!(request.tool_by_name("notes", "thread_hint").is_none());

    let input = request.input();
    let bridge_hint_present = request
        .message_input_texts("developer")
        .iter()
        .any(|text| text.contains(BRIDGE_HINT));
    assert_eq!(bridge_hint_present, !use_history_notes_extension);
    let requests = server.received_requests().await.expect("recorded requests");
    let bridge_calls = requests
        .iter()
        .filter(|request| request.url.path() == "/mcp")
        .filter(|request| {
            request
                .body_json::<Value>()
                .is_ok_and(|body| body["method"] == "tools/call")
        })
        .count();
    assert_eq!(bridge_calls, usize::from(!use_history_notes_extension));
    assert!(!input.iter().any(|item| {
        item["type"] == "function_call"
            && item["namespace"] == "notes"
            && item["name"] == "thread_hint"
    }));

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn history_notes_and_async_message_emit_control_tool_analytics() -> Result<()> {
    let calls = [
        (
            "functions",
            "request_user_input_async",
            json!({"questions": [{"title": "PRIVATE_MESSAGE"}]}),
        ),
        (
            "functions",
            "request_user_input_async",
            json!({"questions": [{"title": " "}]}),
        ),
    ];
    let server = responses::start_mock_server().await;
    let backend = responses::start_mock_server().await;
    let mut sequence = calls
        .iter()
        .enumerate()
        .map(|(index, (namespace, tool, args))| {
            let response_id = format!("resp-{index}");
            responses::sse(vec![
                responses::ev_response_created(&response_id),
                responses::ev_function_call_with_namespace(
                    &format!("call-{index}"),
                    namespace,
                    tool,
                    &args.to_string(),
                ),
                responses::ev_completed(&response_id),
            ])
        })
        .collect::<Vec<_>>();
    sequence.push(responses::sse(vec![responses::ev_completed("resp-final")]));
    let response_mock = responses::mount_sse_sequence(&server, sequence).await;

    let codex_home = TempDir::new()?;
    let config = load_default_config_for_test(&codex_home).await;
    let mut model = codex_core::test_support::construct_model_info_offline("mock-model", &config);
    model.supports_experimental_context = true;
    model
        .experimental_supported_tools
        .push("send_user_message_async".to_string());
    let catalog_path = codex_home.path().join("models.json");
    std::fs::write(
        &catalog_path,
        serde_json::to_vec(&json!({"models": [model]}))?,
    )?;
    MockResponsesConfig::new(&server.uri())
        .with_model_provider("openai-custom")
        .with_provider_name("OpenAI")
        .with_provider_base_url(&format!("{}/backend-api/codex", server.uri()))
        .with_provider_config("supports_websockets = false\nrequires_openai_auth = true")
        .with_root_config(&format!("chatgpt_base_url = \"{}\"", backend.uri()))
        .with_root_config(&format!(
            "model_catalog_json = {}",
            serde_json::to_string(&catalog_path)?
        ))
        .with_extra_config(
            "[features.token_budget]\nenabled = true\nuse_history_notes_extension = true",
        )
        .write(codex_home.path())?;
    mount_analytics_capture(&backend, codex_home.path()).await?;

    let mut app_server = TestAppServer::builder()
        .with_codex_home(codex_home.path())
        .with_env_overrides(&[("OPENAI_API_KEY", None)])
        .without_managed_config()
        .build_initialized()
        .await?;
    let thread = app_server
        .start_thread(ThreadStartParams::default())
        .await?
        .thread;

    assert_eq!(
        response_mock.requests()[1].function_call_output_text("call-0"),
        Some(r#"{"accepted":true}"#.to_string())
    );

    Ok(())
}
