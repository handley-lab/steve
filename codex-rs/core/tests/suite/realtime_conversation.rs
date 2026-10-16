use anyhow::Result;
use codex_config::config_toml::RealtimeWsVersion;
use codex_login::CodexAuth;
use codex_login::OPENAI_API_KEY_ENV_VAR;
use codex_protocol::protocol::CodexErrorInfo;
use codex_protocol::protocol::ConversationAudioParams;
use codex_protocol::protocol::ConversationStartParams;
use codex_protocol::protocol::ConversationTextParams;
use codex_protocol::protocol::ConversationTextRole;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::Op;
use codex_protocol::protocol::RealtimeAudioFrame;
use codex_protocol::protocol::RealtimeConversationRealtimeEvent;
use codex_protocol::protocol::RealtimeEvent;
use codex_protocol::protocol::RealtimeOutputModality;
use core_test_support::responses::start_websocket_server;
use core_test_support::skip_if_no_network;
use core_test_support::test_codex::test_codex;
use core_test_support::wait_for_event_match;
use std::process::Command;
use std::time::Duration;
use tokio::time::timeout;

const REALTIME_CONVERSATION_TEST_SUBPROCESS_ENV_VAR: &str =
    "STEVE_REALTIME_CONVERSATION_TEST_SUBPROCESS";

fn run_realtime_conversation_test_in_subprocess(
    test_name: &str,
    openai_api_key: Option<&str>,
) -> Result<()> {
    let mut command = Command::new(std::env::current_exe()?);
    command
        .arg("--exact")
        .arg(test_name)
        .env(REALTIME_CONVERSATION_TEST_SUBPROCESS_ENV_VAR, "1");
    // The child talks to a loopback websocket server; parent proxy settings can
    // route that connection away from the test server in Bazel environments.
    for &key in codex_network_proxy::PROXY_ENV_KEYS {
        command.env_remove(key);
    }
    match openai_api_key {
        Some(openai_api_key) => {
            command.env(OPENAI_API_KEY_ENV_VAR, openai_api_key);
        }
        None => {
            command.env_remove(OPENAI_API_KEY_ENV_VAR);
        }
    }
    let output = command.output()?;
    assert!(
        output.status.success(),
        "subprocess test `{test_name}` failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn conversation_audio_before_start_emits_error() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let server = start_websocket_server(vec![]).await;
    let mut builder = test_codex();
    let test = builder.build_with_websocket_server(&server).await?;

    test.codex
        .submit(Op::RealtimeConversationAudio(ConversationAudioParams {
            frame: RealtimeAudioFrame {
                data: "AQID".to_string(),
                sample_rate: 24000,
                num_channels: 1,
                samples_per_channel: Some(480),
                item_id: None,
            },
        }))
        .await?;

    let err = wait_for_event_match(&test.codex, |msg| match msg {
        EventMsg::Error(err) => Some(err.clone()),
        _ => None,
    })
    .await;
    assert_eq!(err.codex_error_info, Some(CodexErrorInfo::BadRequest));
    assert_eq!(err.message, "conversation is not running");

    server.shutdown().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn conversation_start_preflight_failure_emits_realtime_error_only() -> Result<()> {
    if std::env::var_os(REALTIME_CONVERSATION_TEST_SUBPROCESS_ENV_VAR).is_none() {
        return run_realtime_conversation_test_in_subprocess(
            "suite::realtime_conversation::conversation_start_preflight_failure_emits_realtime_error_only",
            /*openai_api_key*/ None,
        );
    }

    skip_if_no_network!(Ok(()));

    let server = start_websocket_server(vec![]).await;
    let mut builder = test_codex().with_auth(CodexAuth::create_dummy_chatgpt_auth_for_testing());
    let test = builder.build_with_websocket_server(&server).await?;

    test.codex
        .submit(Op::RealtimeConversationStart(ConversationStartParams {
            client_managed_handoffs: false,
            delegation_ack_filler: None,
            flush_transcript_tail_on_session_end: false,
            codex_responses_as_items: false,
            codex_response_item_prefix: None,
            codex_response_handoff_mode:
                codex_protocol::protocol::CodexResponseHandoffMode::Thinking,
            backend_reasoning_status: false,
            codex_response_handoff_channel_prefixes: None,
            model: None,
            output_modality: RealtimeOutputModality::Audio,
            include_startup_context: true,
            initial_items: Vec::new(),
            realtime_start_instructions: None,
            realtime_end_instructions: None,
            prompt: Some(Some("backend prompt".to_string())),
            realtime_session_id: None,
            voice_session_id: None,
            transport: None,
            version: None,
            voice: None,
        }))
        .await?;

    let err = wait_for_event_match(&test.codex, |msg| match msg {
        EventMsg::RealtimeConversationRealtime(RealtimeConversationRealtimeEvent {
            payload: RealtimeEvent::Error(message),
        }) => Some(message.clone()),
        _ => None,
    })
    .await;
    assert_eq!(err, "realtime conversation requires API key auth");

    let closed = timeout(Duration::from_millis(200), async {
        wait_for_event_match(&test.codex, |msg| match msg {
            EventMsg::RealtimeConversationClosed(closed) => Some(closed.clone()),
            _ => None,
        })
        .await
    })
    .await;
    assert!(closed.is_err(), "preflight failure should not emit closed");

    server.shutdown().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn conversation_start_connect_failure_emits_realtime_error_only() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let server = start_websocket_server(vec![]).await;
    let mut builder = test_codex().with_config(|config| {
        config.experimental_realtime_ws_base_url = Some("http://127.0.0.1:1".to_string());
        config.realtime.version = RealtimeWsVersion::V1;
    });
    let test = builder.build_with_websocket_server(&server).await?;

    test.codex
        .submit(Op::RealtimeConversationStart(ConversationStartParams {
            client_managed_handoffs: false,
            delegation_ack_filler: None,
            flush_transcript_tail_on_session_end: false,
            codex_responses_as_items: false,
            codex_response_item_prefix: None,
            codex_response_handoff_mode:
                codex_protocol::protocol::CodexResponseHandoffMode::Thinking,
            backend_reasoning_status: false,
            codex_response_handoff_channel_prefixes: None,
            model: None,
            output_modality: RealtimeOutputModality::Audio,
            include_startup_context: true,
            initial_items: Vec::new(),
            realtime_start_instructions: None,
            realtime_end_instructions: None,
            prompt: Some(Some("backend prompt".to_string())),
            realtime_session_id: None,
            voice_session_id: None,
            transport: None,
            version: None,
            voice: None,
        }))
        .await?;

    let err = wait_for_event_match(&test.codex, |msg| match msg {
        EventMsg::RealtimeConversationRealtime(RealtimeConversationRealtimeEvent {
            payload: RealtimeEvent::Error(message),
        }) => Some(message.clone()),
        _ => None,
    })
    .await;
    assert!(!err.is_empty());

    let closed = timeout(Duration::from_millis(200), async {
        wait_for_event_match(&test.codex, |msg| match msg {
            EventMsg::RealtimeConversationClosed(closed) => Some(closed.clone()),
            _ => None,
        })
        .await
    })
    .await;
    assert!(closed.is_err(), "connect failure should not emit closed");

    server.shutdown().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn conversation_text_before_start_emits_error() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let server = start_websocket_server(vec![]).await;
    let mut builder = test_codex();
    let test = builder.build_with_websocket_server(&server).await?;

    test.codex
        .submit(Op::RealtimeConversationText(ConversationTextParams {
            text: "hello".to_string(),
            role: ConversationTextRole::User,
        }))
        .await?;

    let err = wait_for_event_match(&test.codex, |msg| match msg {
        EventMsg::Error(err) => Some(err.clone()),
        _ => None,
    })
    .await;
    assert_eq!(err.codex_error_info, Some(CodexErrorInfo::BadRequest));
    assert_eq!(err.message, "conversation is not running");

    server.shutdown().await;
    Ok(())
}
