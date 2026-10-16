use anyhow::Context;
use anyhow::Result;
use app_test_support::MockResponsesConfig;
use app_test_support::TestAppServer;
use app_test_support::create_command_execution_sse_response;
use app_test_support::create_final_assistant_message_sse_response;
use app_test_support::create_mock_responses_server_sequence_unchecked;
use codex_app_server_protocol::CommandExecutionStatus;
use codex_app_server_protocol::ItemCompletedNotification;
use codex_app_server_protocol::ItemStartedNotification;
use codex_app_server_protocol::JSONRPCError;
use codex_app_server_protocol::LoginAccountResponse;
use codex_app_server_protocol::RequestId;
use codex_app_server_protocol::ThreadForkParams;
use codex_app_server_protocol::ThreadForkResponse;
use codex_app_server_protocol::ThreadHistoryMode;
use codex_app_server_protocol::ThreadItem;
use codex_app_server_protocol::ThreadItemsListParams;
use codex_app_server_protocol::ThreadItemsListResponse;
use codex_app_server_protocol::ThreadReadParams;
use codex_app_server_protocol::ThreadReadResponse;
use codex_app_server_protocol::ThreadRealtimeAppendAudioParams;
use codex_app_server_protocol::ThreadRealtimeAppendAudioResponse;
use codex_app_server_protocol::ThreadRealtimeAppendSpeechParams;
use codex_app_server_protocol::ThreadRealtimeAppendSpeechResponse;
use codex_app_server_protocol::ThreadRealtimeAppendTextParams;
use codex_app_server_protocol::ThreadRealtimeAppendTextResponse;
use codex_app_server_protocol::ThreadRealtimeAudioChunk;
use codex_app_server_protocol::ThreadRealtimeClosedNotification;
use codex_app_server_protocol::ThreadRealtimeErrorNotification;
use codex_app_server_protocol::ThreadRealtimeInitialItem;
use codex_app_server_protocol::ThreadRealtimeItem;
use codex_app_server_protocol::ThreadRealtimeItemAddedNotification;
use codex_app_server_protocol::ThreadRealtimeItemCompletedNotification;
use codex_app_server_protocol::ThreadRealtimeItemContent;
use codex_app_server_protocol::ThreadRealtimeItemStartedNotification;
use codex_app_server_protocol::ThreadRealtimeItemTranscriptDeltaNotification;
use codex_app_server_protocol::ThreadRealtimeListVoicesParams;
use codex_app_server_protocol::ThreadRealtimeListVoicesResponse;
use codex_app_server_protocol::ThreadRealtimeOutputAudioDeltaNotification;
use codex_app_server_protocol::ThreadRealtimeSdpNotification;
use codex_app_server_protocol::ThreadRealtimeStartParams;
use codex_app_server_protocol::ThreadRealtimeStartResponse;
use codex_app_server_protocol::ThreadRealtimeStartTransport;
use codex_app_server_protocol::ThreadRealtimeStartedNotification;
use codex_app_server_protocol::ThreadRealtimeStopParams;
use codex_app_server_protocol::ThreadRealtimeStopResponse;
use codex_app_server_protocol::ThreadRealtimeTranscriptDeltaNotification;
use codex_app_server_protocol::ThreadRealtimeTranscriptDoneNotification;
use codex_app_server_protocol::ThreadResumeParams;
use codex_app_server_protocol::ThreadResumeResponse;
use codex_app_server_protocol::ThreadStartParams;
use codex_app_server_protocol::ThreadStartResponse;
use codex_app_server_protocol::ThreadTimelineEntry;
use codex_app_server_protocol::ThreadTimelineListParams;
use codex_app_server_protocol::ThreadTimelineListResponse;
use codex_app_server_protocol::ThreadTurnsListParams;
use codex_app_server_protocol::ThreadTurnsListResponse;
use codex_app_server_protocol::TurnCompletedNotification;
use codex_app_server_protocol::TurnStartParams;
use codex_app_server_protocol::TurnStartResponse;
use codex_app_server_protocol::TurnStartedNotification;
use codex_app_server_protocol::TurnStatus;
use codex_app_server_protocol::TurnSteerParams;
use codex_app_server_protocol::TurnSteerResponse;
use codex_app_server_protocol::UserInput as V2UserInput;
use codex_login::default_client::RESIDENCY_HEADER_NAME;
use codex_protocol::protocol::CodexResponseHandoffMode;
use codex_protocol::protocol::ConversationTextRole;
use codex_protocol::protocol::RealtimeConversationVersion;
use codex_protocol::protocol::RealtimeOutputModality;
use codex_protocol::protocol::RealtimeVoice;
use codex_protocol::protocol::RealtimeVoicesList;
use core_test_support::responses;
use core_test_support::responses::WebSocketConnectionConfig;
use core_test_support::responses::WebSocketRequest;
use core_test_support::responses::WebSocketTestServer;
use core_test_support::responses::start_websocket_server;
use core_test_support::responses::start_websocket_server_with_headers;
use core_test_support::skip_if_no_network;
use core_test_support::skip_if_remote;
use pretty_assertions::assert_eq;
use serde::de::DeserializeOwned;
use serde_json::Value;
use serde_json::json;
use std::collections::BTreeMap;
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::mpsc;
use std::time::Duration;
use tempfile::TempDir;
use test_case::test_case;
use test_case::test_matrix;
use tokio::time::timeout;
use uuid::Uuid;
use wiremock::Mock;
use wiremock::MockServer;
use wiremock::Request as WiremockRequest;
use wiremock::Respond;
use wiremock::ResponseTemplate;
use wiremock::matchers::method;
use wiremock::matchers::path_regex;

const DEFAULT_TIMEOUT: Duration = Duration::from_secs(10);
const DELEGATED_SHELL_TURN_TIMEOUT: Duration = Duration::from_secs(30);
const DELEGATED_SHELL_TOOL_TIMEOUT_MS: u64 = 30_000;
const STARTUP_CONTEXT_HEADER: &str = "Startup context from Codex.";
const V2_STEERING_ACKNOWLEDGEMENT: &str =
    "This was sent to steer the previous background agent task.";
const V2_HANDOFF_COMPLETE_ACKNOWLEDGEMENT: &str =
    "Background agent finished. Use the preceding [BACKEND] messages as the result.";
const RESPONSE_ITEM_PREFIX: &str =
    "Use the following context to inform future responses, but do not speak it to the user.";

#[derive(Debug, Clone, Copy)]
enum StartupContextConfig<'a> {
    Generated,
    Override(&'a str),
}

fn normalized_json_string(raw: &str) -> Result<String> {
    let value: Value = serde_json::from_str(raw).context("expected JSON fixture to parse")?;
    serde_json::to_string(&value).context("expected JSON fixture to serialize")
}

struct GatedSseResponse {
    gate_rx: Mutex<Option<mpsc::Receiver<()>>>,
    response: String,
}

impl Respond for GatedSseResponse {
    fn respond(&self, _: &WiremockRequest) -> ResponseTemplate {
        let gate_rx = self
            .gate_rx
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        if let Some(gate_rx) = gate_rx {
            let _ = gate_rx.recv();
        }
        responses::sse_response(self.response.clone())
    }
}

#[derive(Debug, Clone, Copy)]
enum RealtimeTestVersion {
    V1,
    V2,
}

impl RealtimeTestVersion {
    fn config_value(self) -> &'static str {
        match self {
            RealtimeTestVersion::V1 => "v1",
            RealtimeTestVersion::V2 => "v2",
        }
    }
}

#[derive(Debug, Clone, Copy)]
enum RealtimeTestSandbox {
    ReadOnly,
    DangerFullAccess,
}

impl RealtimeTestSandbox {
    fn config_value(self) -> &'static str {
        match self {
            RealtimeTestSandbox::ReadOnly => "read-only",
            RealtimeTestSandbox::DangerFullAccess => "danger-full-access",
        }
    }
}


// Scripted SSE responses for the normal background agent loop. Realtime can ask for a delegated
// background agent turn; that turn talks to this mock `/responses` endpoint and may request
// ordinary tools.
struct MainLoopResponsesScript {
    responses: Vec<String>,
}

// Scripted server events for the direct realtime sideband WebSocket. This mock is the realtime
// session app-server joins after call creation; it is not the background agent Responses stream.
struct RealtimeSidebandScript {
    connections: Vec<WebSocketConnectionConfig>,
}

struct RealtimeE2eHarness {
    mcp: TestAppServer,
    _codex_home: TempDir,
    main_loop_responses_server: MockServer,
    realtime_server: WebSocketTestServer,
    thread_id: String,
}

impl RealtimeE2eHarness {
    // Owns the full mocked app-server realtime route: MCP client, Responses mocks, WebRTC call
    // creation capture, sideband WebSocket server, login, config, and a started thread.
    async fn new(
        realtime_version: RealtimeTestVersion,
        main_loop: MainLoopResponsesScript,
        realtime_sideband: RealtimeSidebandScript,
    ) -> Result<Self> {
        let main_loop_responses_server =
            create_mock_responses_server_sequence_unchecked(main_loop.responses).await;
        Self::new_with_main_loop_responses_server_and_sandbox(
            realtime_version,
            main_loop_responses_server,
            realtime_sideband,
            RealtimeTestSandbox::ReadOnly,
        )
        .await
    }

    async fn new_with_sandbox(
        realtime_version: RealtimeTestVersion,
        main_loop: MainLoopResponsesScript,
        realtime_sideband: RealtimeSidebandScript,
        sandbox: RealtimeTestSandbox,
    ) -> Result<Self> {
        let main_loop_responses_server =
            create_mock_responses_server_sequence_unchecked(main_loop.responses).await;
        Self::new_with_main_loop_responses_server_and_sandbox(
            realtime_version,
            main_loop_responses_server,
            realtime_sideband,
            sandbox,
        )
        .await
    }

    async fn new_with_main_loop_responses_server(
        realtime_version: RealtimeTestVersion,
        main_loop_responses_server: MockServer,
        realtime_sideband: RealtimeSidebandScript,
    ) -> Result<Self> {
        Self::new_with_main_loop_responses_server_and_sandbox(
            realtime_version,
            main_loop_responses_server,
            realtime_sideband,
            RealtimeTestSandbox::ReadOnly,
        )
        .await
    }

    async fn new_with_main_loop_responses_server_and_sandbox(
        realtime_version: RealtimeTestVersion,
        main_loop_responses_server: MockServer,
        realtime_sideband: RealtimeSidebandScript,
        sandbox: RealtimeTestSandbox,
    ) -> Result<Self> {
        let realtime_server =
            start_websocket_server_with_headers(realtime_sideband.connections).await;
        let codex_home = TempDir::new()?;
        create_config_toml_with_realtime_version(
            codex_home.path(),
            &main_loop_responses_server.uri(),
            realtime_server.uri(),
            StartupContextConfig::Override("startup context"),
            realtime_version,
            sandbox,
        )?;

        let mut mcp = TestAppServer::builder()
            .with_codex_home(codex_home.path())
            .build_initialized_with_timeout(DEFAULT_TIMEOUT)
            .await?;
        login_with_api_key(&mut mcp, "sk-test-key").await?;

        let thread_start_request_id = mcp
            .send_thread_start_request_with_auto_env(ThreadStartParams::default())
            .await?;
        let thread_start: ThreadStartResponse =
            timeout(DEFAULT_TIMEOUT, mcp.read_response(thread_start_request_id)).await??;

        Ok(Self {
            mcp,
            _codex_home: codex_home,
            main_loop_responses_server,
            realtime_server,
            thread_id: thread_start.thread.id,
        })
    }




    async fn start_websocket_realtime(&mut self) -> Result<ThreadRealtimeStartedNotification> {
        self.start_websocket_realtime_with_codex_responses_as_items(
            /*codex_responses_as_items*/ None,
        )
        .await
    }

    async fn start_websocket_realtime_with_codex_response_items(
        &mut self,
    ) -> Result<ThreadRealtimeStartedNotification> {
        self.start_websocket_realtime_with_codex_responses_as_items(
            /*codex_responses_as_items*/ Some(true),
        )
        .await
    }

    async fn start_websocket_realtime_with_codex_responses_as_items(
        &mut self,
        codex_responses_as_items: Option<bool>,
    ) -> Result<ThreadRealtimeStartedNotification> {
        let start_request_id = self
            .mcp
            .send_thread_realtime_start_request(ThreadRealtimeStartParams {
                thread_id: self.thread_id.clone(),
                client_managed_handoffs: None,
                delegation_ack_filler: None,
                flush_transcript_tail_on_session_end: None,
                codex_response_item_prefix: codex_responses_as_items
                    .unwrap_or(false)
                    .then(|| RESPONSE_ITEM_PREFIX.to_string()),
                codex_response_handoff_mode: None,
                backend_reasoning_status: false,
                codex_response_handoff_channel_prefixes: None,
                codex_responses_as_items,
                model: None,
                output_modality: RealtimeOutputModality::Audio,
                include_startup_context: None,
                initial_items: None,
                realtime_start_instructions: None,
                realtime_end_instructions: None,
                prompt: Some(Some("backend prompt".to_string())),
                realtime_session_id: None,
                voice_session_id: None,
                transport: None,
                version: None,
                voice: None,
            })
            .await?;
        let _: ThreadRealtimeStartResponse =
            timeout(DEFAULT_TIMEOUT, self.mcp.read_response(start_request_id)).await??;

        self.read_notification::<ThreadRealtimeStartedNotification>("thread/realtime/started")
            .await
    }

    async fn start_frameless_bidi_realtime(
        &mut self,
        codex_response_handoff_mode: Option<CodexResponseHandoffMode>,
        codex_response_handoff_channel_prefixes: Option<BTreeMap<String, Vec<String>>>,
        initial_items: Option<Vec<ThreadRealtimeInitialItem>>,
    ) -> Result<ThreadRealtimeStartedNotification> {
        let start_request_id = self
            .mcp
            .send_thread_realtime_start_request(ThreadRealtimeStartParams {
                thread_id: self.thread_id.clone(),
                client_managed_handoffs: None,
                delegation_ack_filler: None,
                flush_transcript_tail_on_session_end: None,
                codex_response_item_prefix: None,
                codex_response_handoff_mode,
                backend_reasoning_status: false,
                codex_response_handoff_channel_prefixes,
                codex_responses_as_items: None,
                model: None,
                output_modality: RealtimeOutputModality::Audio,
                include_startup_context: None,
                initial_items,
                realtime_start_instructions: None,
                realtime_end_instructions: None,
                prompt: Some(Some("backend prompt".to_string())),
                realtime_session_id: None,
                voice_session_id: None,
                transport: None,
                version: Some(RealtimeConversationVersion::V3),
                voice: None,
            })
            .await?;
        let _: ThreadRealtimeStartResponse =
            timeout(DEFAULT_TIMEOUT, self.mcp.read_response(start_request_id)).await??;

        self.read_notification::<ThreadRealtimeStartedNotification>("thread/realtime/started")
            .await
    }

    async fn read_notification<T: DeserializeOwned>(&mut self, method: &str) -> Result<T> {
        read_notification(&mut self.mcp, method).await
    }

    async fn complete_turn(&mut self, text: &str) -> Result<()> {
        let request_id = self
            .mcp
            .send_turn_start_request(TurnStartParams {
                thread_id: self.thread_id.clone(),
                input: vec![V2UserInput::Text {
                    text: text.to_string(),
                    text_elements: Vec::new(),
                }],
                ..Default::default()
            })
            .await?;
        let _: TurnStartResponse =
            timeout(DEFAULT_TIMEOUT, self.mcp.read_response(request_id)).await??;
        self.read_notification::<TurnCompletedNotification>("turn/completed")
            .await?;
        Ok(())
    }

    /// Returns the nth JSON message app-server wrote to the fake Realtime API
    /// sideband websocket.
    async fn sideband_outbound_request(&self, request_index: usize) -> Value {
        timeout(
            DEFAULT_TIMEOUT,
            self.realtime_server
                .wait_for_request(/*connection_index*/ 0, request_index),
        )
        .await
        .expect("realtime sideband request should arrive before timeout")
        .body_json()
    }

    async fn append_audio(&mut self, thread_id: String) -> Result<()> {
        let request_id = self
            .mcp
            .send_thread_realtime_append_audio_request(ThreadRealtimeAppendAudioParams {
                thread_id,
                audio: ThreadRealtimeAudioChunk {
                    data: "BQYH".to_string(),
                    sample_rate: 24_000,
                    num_channels: 1,
                    samples_per_channel: Some(480),
                    item_id: None,
                },
            })
            .await?;
        let _: ThreadRealtimeAppendAudioResponse =
            timeout(DEFAULT_TIMEOUT, self.mcp.read_response(request_id)).await??;
        Ok(())
    }

    async fn append_text(&mut self, thread_id: String, text: &str) -> Result<()> {
        let request_id = self
            .mcp
            .send_thread_realtime_append_text_request(ThreadRealtimeAppendTextParams {
                thread_id,
                text: text.to_string(),
                role: ConversationTextRole::User,
            })
            .await?;
        let _: ThreadRealtimeAppendTextResponse =
            timeout(DEFAULT_TIMEOUT, self.mcp.read_response(request_id)).await??;
        Ok(())
    }

    async fn append_speech(&mut self, thread_id: String, text: &str) -> Result<()> {
        let request_id = self
            .mcp
            .send_thread_realtime_append_speech_request(ThreadRealtimeAppendSpeechParams {
                thread_id,
                text: text.to_string(),
            })
            .await?;
        let _: ThreadRealtimeAppendSpeechResponse =
            timeout(DEFAULT_TIMEOUT, self.mcp.read_response(request_id)).await??;
        Ok(())
    }

    async fn main_loop_responses_requests(&self) -> Result<Vec<Value>> {
        responses_requests(&self.main_loop_responses_server).await
    }

    async fn shutdown(self) {
        self.realtime_server.shutdown().await;
    }
}

fn main_loop_responses(responses: Vec<String>) -> MainLoopResponsesScript {
    MainLoopResponsesScript { responses }
}

fn no_main_loop_responses() -> MainLoopResponsesScript {
    main_loop_responses(Vec::new())
}

fn realtime_sideband(connections: Vec<WebSocketConnectionConfig>) -> RealtimeSidebandScript {
    RealtimeSidebandScript { connections }
}

fn realtime_sideband_connection(
    realtime_server_events: Vec<Vec<Value>>,
) -> WebSocketConnectionConfig {
    WebSocketConnectionConfig {
        requests: realtime_server_events,
        response_headers: Vec::new(),
        accept_delay: None,
        close_after_requests: true,
    }
}

fn open_realtime_sideband_connection(
    realtime_server_events: Vec<Vec<Value>>,
) -> WebSocketConnectionConfig {
    WebSocketConnectionConfig {
        close_after_requests: false,
        ..realtime_sideband_connection(realtime_server_events)
    }
}

fn session_updated(realtime_session_id: &str) -> Value {
    json!({
        "type": "session.updated",
        "session": { "id": realtime_session_id, "instructions": "backend prompt" }
    })
}

fn session_started(realtime_session_id: &str) -> Value {
    json!({
        "type": "session.started",
        "session": { "id": realtime_session_id, "instructions": "backend prompt" }
    })
}

fn v2_background_agent_tool_call(call_id: &str, prompt: &str) -> Value {
    json!({
        "type": "conversation.item.done",
        "item": {
            "id": format!("item_{call_id}"),
            "type": "function_call",
            "name": "background_agent",
            "call_id": call_id,
            "arguments": json!({ "prompt": prompt }).to_string()
        }
    })
}

#[tokio::test]
async fn realtime_list_voices_returns_supported_names() -> Result<()> {
    let codex_home = TempDir::new()?;
    create_config_toml(
        codex_home.path(),
        "http://127.0.0.1:1",
        "ws://127.0.0.1:1",
        StartupContextConfig::Generated,
    )?;

    let mut mcp = TestAppServer::builder()
        .with_codex_home(codex_home.path())
        .build_initialized_with_timeout(DEFAULT_TIMEOUT)
        .await?;

    let request_id = mcp
        .send_thread_realtime_list_voices_request(ThreadRealtimeListVoicesParams {})
        .await?;
    let response: ThreadRealtimeListVoicesResponse =
        timeout(DEFAULT_TIMEOUT, mcp.read_response(request_id)).await??;

    assert_eq!(
        response,
        ThreadRealtimeListVoicesResponse {
            voices: RealtimeVoicesList {
                v1: vec![
                    RealtimeVoice::Juniper,
                    RealtimeVoice::Maple,
                    RealtimeVoice::Spruce,
                    RealtimeVoice::Ember,
                    RealtimeVoice::Vale,
                    RealtimeVoice::Breeze,
                    RealtimeVoice::Arbor,
                    RealtimeVoice::Sol,
                    RealtimeVoice::Cove,
                ],
                v2: vec![
                    RealtimeVoice::Alloy,
                    RealtimeVoice::Ash,
                    RealtimeVoice::Ballad,
                    RealtimeVoice::Coral,
                    RealtimeVoice::Echo,
                    RealtimeVoice::Sage,
                    RealtimeVoice::Shimmer,
                    RealtimeVoice::Verse,
                    RealtimeVoice::Marin,
                    RealtimeVoice::Cedar,
                ],
                v3: RealtimeVoicesList::builtin().v3,
                default_v1: RealtimeVoice::Cove,
                default_v2: RealtimeVoice::Marin,
            },
        }
    );

    Ok(())
}

const PROVIDER_RESIDENCY_ENV_VAR: &str = "STEVE_TEST_REALTIME_RESIDENCY_HEADER";

#[derive(Clone, Copy, Debug)]
enum RealtimeTransport {
    Websocket,
}

#[derive(Clone, Copy, Debug)]
enum ResidencyPolicy {
    Required,
    Unmanaged,
}

#[derive(Clone, Copy, Debug)]
enum ProviderHeaders {
    Static,
    StaticAndEnvironment,
}



#[test_case("includeStartupContext"; "rejects startup context")]
#[test_case("prompt"; "rejects prompt")]
#[test_case("initialItems"; "rejects initial items")]
#[test_case("model"; "rejects model")]
#[test_case("voice"; "rejects voice")]
#[test_case("delegationAckFiller"; "rejects delegation acknowledgement filler")]
#[tokio::test]
async fn existing_call_rejects_client_owned_session_configuration(option: &str) -> Result<()> {
    skip_if_no_network!(Ok(()));

    let mut harness = RealtimeE2eHarness::new(
        RealtimeTestVersion::V1,
        no_main_loop_responses(),
        realtime_sideband(Vec::new()),
    )
    .await?;
    let mut params = ThreadRealtimeStartParams {
        thread_id: harness.thread_id.clone(),
        client_managed_handoffs: None,
        delegation_ack_filler: None,
        flush_transcript_tail_on_session_end: None,
        codex_response_item_prefix: None,
        codex_response_handoff_mode: None,
        backend_reasoning_status: false,
        codex_response_handoff_channel_prefixes: None,
        codex_responses_as_items: None,
        model: None,
        output_modality: RealtimeOutputModality::Audio,
        include_startup_context: None,
        initial_items: None,
        realtime_start_instructions: None,
        realtime_end_instructions: None,
        prompt: None,
        realtime_session_id: None,
        voice_session_id: None,
        transport: Some(ThreadRealtimeStartTransport::ExistingCall {
            call_id: "rtc_existing".to_string(),
        }),
        version: Some(RealtimeConversationVersion::V3),
        voice: None,
    };
    match option {
        "includeStartupContext" => params.include_startup_context = Some(true),
        "prompt" => params.prompt = Some(Some("backend prompt".to_string())),
        "initialItems" => {
            params.initial_items = Some(vec![ThreadRealtimeInitialItem {
                role: ConversationTextRole::User,
                text: "client-owned history".to_string(),
            }]);
        }
        "model" => params.model = Some("another-model".to_string()),
        "voice" => params.voice = Some(RealtimeVoice::Cove),
        "delegationAckFiller" => params.delegation_ack_filler = Some(true),
        option => anyhow::bail!("unsupported test option: {option}"),
    }

    let request_id = harness
        .mcp
        .send_thread_realtime_start_request(params)
        .await?;
    let error = timeout(
        DEFAULT_TIMEOUT,
        harness
            .mcp
            .read_stream_until_error_message(RequestId::Integer(request_id)),
    )
    .await??;
    assert_invalid_request(
        error,
        format!("existingCall transport does not support {option}"),
    );
    harness.shutdown().await;
    Ok(())
}







async fn read_notification<T: DeserializeOwned>(
    mcp: &mut TestAppServer,
    method: &str,
) -> Result<T> {
    read_notification_with_timeout(mcp, method, DEFAULT_TIMEOUT).await
}

async fn read_notification_with_timeout<T: DeserializeOwned>(
    mcp: &mut TestAppServer,
    method: &str,
    timeout_duration: Duration,
) -> Result<T> {
    timeout(timeout_duration, mcp.read_notification(method)).await?
}

async fn login_with_api_key(mcp: &mut TestAppServer, api_key: &str) -> Result<()> {
    let request_id = mcp.send_login_account_api_key_request(api_key).await?;
    let login: LoginAccountResponse =
        timeout(DEFAULT_TIMEOUT, mcp.read_response(request_id)).await??;
    assert_eq!(login, LoginAccountResponse::ApiKey {});

    Ok(())
}

async fn wait_for_started_command_execution(
    mcp: &mut TestAppServer,
) -> Result<ItemStartedNotification> {
    loop {
        let started = read_notification::<ItemStartedNotification>(mcp, "item/started").await?;
        if let ThreadItem::CommandExecution { .. } = &started.item {
            return Ok(started);
        }
    }
}

async fn wait_for_completed_command_execution(
    mcp: &mut TestAppServer,
) -> Result<ItemCompletedNotification> {
    loop {
        let completed =
            read_notification::<ItemCompletedNotification>(mcp, "item/completed").await?;
        if let ThreadItem::CommandExecution { .. } = &completed.item {
            return Ok(completed);
        }
    }
}

async fn responses_requests(server: &MockServer) -> Result<Vec<Value>> {
    server
        .received_requests()
        .await
        .context("failed to fetch received requests")?
        .into_iter()
        .filter(|request| request.url.path().ends_with("/responses"))
        .map(|request| {
            request
                .body_json::<Value>()
                .context("Responses request body should be JSON")
        })
        .collect()
}

fn response_request_contains_text(request: &Value, text: &str) -> bool {
    match request {
        Value::String(value) => value.contains(text),
        Value::Array(values) => values
            .iter()
            .any(|value| response_request_contains_text(value, text)),
        Value::Object(map) => map
            .values()
            .any(|value| response_request_contains_text(value, text)),
        Value::Null | Value::Bool(_) | Value::Number(_) => false,
    }
}

fn realtime_tool_ok_command() -> Vec<String> {
    #[cfg(windows)]
    {
        vec![
            "powershell.exe".to_string(),
            "-NoProfile".to_string(),
            "-Command".to_string(),
            "[Console]::Write('realtime-tool-ok')".to_string(),
        ]
    }

    #[cfg(not(windows))]
    {
        vec!["printf".to_string(), "realtime-tool-ok".to_string()]
    }
}

fn assert_v2_function_call_output(request: &Value, call_id: &str, expected_output: &str) {
    assert_eq!(
        request,
        &json!({
            "type": "conversation.item.create",
            "item": {
                "type": "function_call_output",
                "call_id": call_id,
                "output": expected_output,
            }
        })
    );
}

fn assert_v2_progress_update(request: &Value, expected_text: &str) {
    assert_eq!(
        request,
        &json!({
            "type": "conversation.item.create",
            "item": {
                "type": "message",
                "role": "user",
                "content": [{
                    "type": "input_text",
                    "text": format!("[BACKEND] {expected_text}")
                }]
            }
        })
    );
}

fn assert_v2_backend_item_update(request: &Value, expected_text: &str) {
    assert_v2_items_update(request, &format!("[BACKEND] {expected_text}"));
}

fn assert_v2_items_update(request: &Value, expected_text: &str) {
    assert_eq!(
        request,
        &json!({
            "type": "conversation.item.create",
            "item": {
                "type": "message",
                "role": "developer",
                "content": [{
                    "type": "input_text",
                    "text": format!("{RESPONSE_ITEM_PREFIX}\n\n{expected_text}")
                }]
            }
        })
    );
}

fn assert_v2_user_text_item(request: &Value, expected_text: &str) {
    assert_eq!(
        request,
        &json!({
            "type": "conversation.item.create",
            "item": {
                "type": "message",
                "role": "user",
                "content": [{
                    "type": "input_text",
                    "text": format!("[USER] {expected_text}")
                }]
            }
        })
    );
}

fn assert_v2_response_create(request: &Value) {
    assert_eq!(
        request,
        &json!({
            "type": "response.create"
        })
    );
}

fn assert_v1_session_update(request: &Value) -> Result<()> {
    assert_eq!(request["type"].as_str(), Some("session.update"));
    assert_eq!(request["session"]["type"].as_str(), Some("quicksilver"));
    assert!(
        request["session"]["instructions"]
            .as_str()
            .context("v1 session.update instructions")?
            .contains("startup context")
    );
    assert_eq!(
        request["session"]["audio"]["output"]["voice"].as_str(),
        Some("cove")
    );
    assert_eq!(request["session"]["tools"], Value::Null);
    Ok(())
}

fn assert_v2_session_update(request: &Value) -> Result<()> {
    assert_eq!(request["type"].as_str(), Some("session.update"));
    assert_eq!(request["session"]["type"].as_str(), Some("realtime"));
    assert!(
        request["session"]["instructions"]
            .as_str()
            .context("v2 session.update instructions")?
            .contains("startup context")
    );
    assert_eq!(
        request["session"]["tools"][0]["name"].as_str(),
        Some("background_agent")
    );
    assert_eq!(
        request["session"]["tools"][1]["name"].as_str(),
        Some("remain_silent")
    );
    assert_eq!(
        request["session"]["audio"]["input"]["transcription"]["model"].as_str(),
        Some("gpt-4o-mini-transcribe")
    );
    Ok(())
}


fn create_config_toml(
    codex_home: &Path,
    responses_server_uri: &str,
    realtime_server_uri: &str,
    startup_context: StartupContextConfig<'_>,
) -> std::io::Result<()> {
    create_config_toml_with_realtime_version(
        codex_home,
        responses_server_uri,
        realtime_server_uri,
        startup_context,
        RealtimeTestVersion::V2,
        RealtimeTestSandbox::ReadOnly,
    )
}

fn create_config_toml_with_realtime_version(
    codex_home: &Path,
    responses_server_uri: &str,
    realtime_server_uri: &str,
    startup_context: StartupContextConfig<'_>,
    realtime_version: RealtimeTestVersion,
    sandbox: RealtimeTestSandbox,
) -> std::io::Result<()> {
    let mut config = MockResponsesConfig::new(responses_server_uri)
        .with_sandbox_mode(sandbox.config_value())
        .with_root_config(&format!(
            "experimental_realtime_ws_base_url = \"{realtime_server_uri}\"\n\
             experimental_realtime_ws_backend_prompt = \"backend prompt\""
        ))
        .with_extra_config(&format!(
            "[realtime]\nversion = \"{}\"\ntype = \"conversational\"",
            realtime_version.config_value()
        ));

    if let StartupContextConfig::Override(context) = startup_context {
        config = config.with_root_config(&format!(
            "experimental_realtime_ws_startup_context = {context:?}"
        ));
    }
    config.write(codex_home)
}

fn assert_invalid_request(error: JSONRPCError, message: String) {
    assert_eq!(error.error.code, -32600);
    assert_eq!(error.error.message, message);
    assert_eq!(error.error.data, None);
}
