use anyhow::Result;
use anyhow::bail;
use app_test_support::ChatGptAuthFixture;
use app_test_support::TestAppServer;
use app_test_support::to_response;
use app_test_support::write_chatgpt_auth;
use codex_app_server_protocol::AttestationGenerateResponse;
use codex_app_server_protocol::ClientInfo;
use codex_app_server_protocol::InitializeCapabilities;
use codex_app_server_protocol::JSONRPCMessage;
use codex_app_server_protocol::RequestId;
use codex_app_server_protocol::ServerRequest;
use codex_app_server_protocol::ThreadStartParams;
use codex_app_server_protocol::ThreadStartResponse;
use codex_app_server_protocol::TurnStartParams;
use codex_app_server_protocol::TurnStartResponse;
use codex_app_server_protocol::UserInput as V2UserInput;
use codex_config::types::AuthCredentialsStoreMode;
use core_test_support::responses;
use core_test_support::responses::WebSocketConnectionConfig;
use core_test_support::responses::start_websocket_server_with_headers;
use core_test_support::skip_if_no_network;
use pretty_assertions::assert_eq;
use std::path::Path;
use tempfile::TempDir;
use test_case::test_case;
use tokio::time::Duration;
use tokio::time::timeout;

const DEFAULT_READ_TIMEOUT: Duration = Duration::from_secs(60);
const ATTESTATION_HEADER: &str = "v1.integration-test";
const APP_SERVER_ATTESTATION_HEADER: &str = r#"{"v":1,"s":0,"t":"v1.integration-test"}"#;

#[test_case(ThreadStartParams::default(), "account", true; "account")]
#[test_case(ThreadStartParams::default(), "account", false; "account_no_opt_in")]
#[test_case(ThreadStartParams::default(), "bearer", true; "provider_bearer_without_account")]
#[test_case(ThreadStartParams::default(), "bearer", false; "provider_bearer_without_account_no_opt_in")]
#[test_case(ThreadStartParams::default(), "bearer-unused-account", true; "provider_bearer")]
#[test_case(ThreadStartParams::default(), "bearer-unused-account", false; "provider_bearer_no_opt_in")]
#[test_case(ThreadStartParams::default(), "anonymous", true; "anonymous")]
#[test_case(ThreadStartParams::default(), "anonymous", false; "anonymous_no_opt_in")]
#[test_case(ThreadStartParams { ephemeral: Some(true), ..Default::default() }, "account", true; "ephemeral")]
#[tokio::test]
async fn attestation_generate_round_trip_adds_header_to_responses_websocket_handshake(
    thread_start_params: ThreadStartParams,
    credential: &str,
    opted_in: bool,
) -> Result<()> {
    skip_if_no_network!(Ok(()));

    let websocket_server = start_websocket_server_with_headers(vec![WebSocketConnectionConfig {
        requests: vec![
            vec![
                responses::ev_response_created("warm-1"),
                responses::ev_completed("warm-1"),
            ],
            vec![
                responses::ev_response_created("resp-1"),
                responses::ev_assistant_message("msg-1", "Done"),
                responses::ev_completed("resp-1"),
            ],
        ],
        response_headers: Vec::new(),
        accept_delay: None,
        close_after_requests: true,
    }])
    .await;

    let codex_home = TempDir::new()?;
    create_configured_websocket_config(
        codex_home.path(),
        &websocket_server.uri().replacen("ws://", "http://", 1),
        credential,
    )?;
    if matches!(credential, "account" | "bearer-unused-account") {
        write_chatgpt_auth(
            codex_home.path(),
            ChatGptAuthFixture::new("access-chatgpt")
                .account_id("attestation-account")
                .plan_type("pro"),
            AuthCredentialsStoreMode::File,
        )?;
    }

    let mut mcp = TestAppServer::builder()
        .with_codex_home(codex_home.path())
        .with_env_overrides(&[("OPENAI_API_KEY", None), ("STEVE_API_KEY", None)])
        .build()
        .await?;
    let initialized = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.initialize_with_capabilities(
            ClientInfo {
                name: "codex_desktop".to_string(),
                title: Some("Codex Desktop".to_string()),
                version: "0.1.0".to_string(),
            },
            Some(InitializeCapabilities {
                explicit_gateway_oauth: false,
                experimental_api: true,
                request_attestation: opted_in,
                opt_out_notification_methods: None,
                mcp_server_openai_form_elicitation: false,
                extensions: None,
            }),
        ),
    )
    .await??;
    let JSONRPCMessage::Response(_) = initialized else {
        bail!("expected initialize response, got {initialized:?}");
    };

    let thread_request_id = mcp
        .send_thread_start_request_with_auto_env(thread_start_params)
        .await?;
    let mut turn_request_id = None;
    let mut turn_started = false;
    let mut turn_completed = false;
    let mut attestation_requests = 0;
    // Prewarming can request attestation before thread/start or turn/start responds.
    // Service those requests immediately instead of buffering them behind RPC responses.
    timeout(DEFAULT_READ_TIMEOUT, async {
        loop {
            match mcp.read_next_message().await? {
                JSONRPCMessage::Response(response)
                    if response.id == RequestId::Integer(thread_request_id) =>
                {
                    let ThreadStartResponse { thread, .. } = to_response(response)?;
                    turn_request_id = Some(RequestId::Integer(
                        mcp.send_turn_start_request(TurnStartParams {
                            thread_id: thread.id,
                            client_user_message_id: None,
                            input: vec![V2UserInput::Text {
                                text: "Hello".to_string(),
                                text_elements: Vec::new(),
                            }],
                            ..Default::default()
                        })
                        .await?,
                    ));
                }
                JSONRPCMessage::Response(response)
                    if turn_request_id.as_ref() == Some(&response.id) =>
                {
                    let _: TurnStartResponse = to_response(response)?;
                    turn_started = true;
                }
                JSONRPCMessage::Request(request) => {
                    let request = ServerRequest::try_from(request)?;
                    let ServerRequest::AttestationGenerate { request_id, .. } = request else {
                        bail!("expected attestation/generate request, got {request:?}");
                    };
                    attestation_requests += 1;
                    mcp.send_response(
                        request_id,
                        serde_json::to_value(AttestationGenerateResponse {
                            token: ATTESTATION_HEADER.into(),
                        })?,
                    )
                    .await?;
                }
                JSONRPCMessage::Notification(notification)
                    if notification.method == "turn/completed" =>
                {
                    turn_completed = true;
                }
                _ => {}
            }
            if turn_started && turn_completed {
                break Ok(());
            }
        }
    })
    .await??;
    assert!(
        websocket_server
            .wait_for_handshakes(/*expected*/ 1, DEFAULT_READ_TIMEOUT)
            .await
    );
    let handshake = websocket_server.single_handshake();
    let expected_authorization = match credential {
        "account" => Some("Bearer access-chatgpt"),
        "bearer" | "bearer-unused-account" => Some("Bearer selected-bearer"),
        "anonymous" => None,
        _ => unreachable!(),
    };
    eprintln!(
        "T19_EVIDENCE {}",
        serde_json::json!({"kind": "app-server-attestation",
            "credential": credential, "opted_in": opted_in, "attestation_requests": attestation_requests,
            "handshake_uri": handshake.uri(), "attestation": handshake.header("x-oai-attestation"),
            "authorization": handshake.header("authorization"), "account_id": handshake.header("chatgpt-account-id"),
            "requests": websocket_server.connections().iter().map(|connection| connection.iter().map(|request| request.body_json()).collect::<Vec<_>>()).collect::<Vec<_>>(),
        })
    );

    assert_eq!(attestation_requests > 0, opted_in);
    assert_eq!(
        handshake.header("x-oai-attestation").as_deref(),
        opted_in.then_some(APP_SERVER_ATTESTATION_HEADER)
    );
    assert_eq!(
        handshake.header("authorization").as_deref(),
        expected_authorization
    );
    assert_eq!(
        handshake.header("chatgpt-account-id").as_deref(),
        (credential == "account").then_some("attestation-account")
    );
    websocket_server.shutdown().await;
    Ok(())
}

fn create_configured_websocket_config(
    codex_home: &Path,
    server_uri: &str,
    credential: &str,
) -> std::io::Result<()> {
    let requires_auth = credential == "account";
    let bearer = if matches!(credential, "bearer" | "bearer-unused-account") {
        "experimental_bearer_token = \"selected-bearer\""
    } else {
        ""
    };
    std::fs::write(
        codex_home.join("config.toml"),
        format!(
            r#"
model = "mock-model"
approval_policy = "never"
sandbox_mode = "read-only"

model_provider = "mock_provider"

[model_providers.mock_provider]
name = "Cambridge"
base_url = "{server_uri}/v1"
wire_api = "responses"
request_max_retries = 0
stream_max_retries = 0
requires_openai_auth = {requires_auth}
supports_websockets = true
{bearer}

[features]
api_key_model_discovery = false
guardian_approval = false
"#
        ),
    )
}
