use anyhow::Context;
use anyhow::Result;
use app_test_support::MockResponsesConfig;
use app_test_support::TestAppServer;
use app_test_support::app_server_json_shutdown_event;
use app_test_support::create_escalated_command_execution_sse_response;
use app_test_support::create_exec_command_sse_response;
use app_test_support::create_final_assistant_message_sse_response;
use app_test_support::create_mock_responses_server_sequence;
use codex_app_server_protocol::ApprovalsReviewer;
use codex_app_server_protocol::AskForApproval;
use codex_app_server_protocol::ClientRequest;
use codex_app_server_protocol::SandboxPolicy;
use codex_app_server_protocol::ServerRequest;
use codex_app_server_protocol::ThreadApproveGuardianDeniedActionParams;
use codex_app_server_protocol::ThreadApproveGuardianDeniedActionResponse;
use codex_app_server_protocol::ThreadStartParams;
use codex_app_server_protocol::TurnStartParams;
use codex_app_server_protocol::TurnStartResponse;
use codex_app_server_protocol::UserInput;
use codex_features::Feature;
use codex_state::StateRuntime;
use core_test_support::load_default_config_for_test;
use core_test_support::skip_if_no_network;
use core_test_support::skip_if_wine_exec;
use pretty_assertions::assert_eq;
use serde_json::Value;
use serde_json::json;
use tempfile::TempDir;
use tokio::time::Duration;
use tokio::time::timeout;

const READ_TIMEOUT: Duration = Duration::from_secs(10);

#[tokio::test]
async fn submission_metadata_reaches_persisted_and_feedback_logs() -> Result<()> {
    skip_if_wine_exec!(
        Ok(()),
        "command approval routing requires a host-native cwd under Wine-exec"
    );
    skip_if_no_network!(Ok(()));

    let server = create_mock_responses_server_sequence(vec![
        create_escalated_command_execution_sse_response(
            vec!["echo".into(), "hello".into()],
            /*workdir*/ None,
            /*timeout_ms*/ Some(5000),
            "call-approval",
        )?,
        create_final_assistant_message_sse_response("done")?,
    ])
    .await;
    let codex_home = TempDir::new()?;
    MockResponsesConfig::new(&server.uri())
        .with_approval_policy("on-request")
        .with_provider_config("supports_websockets = false")
        .write(codex_home.path())?;
    let mut app_server = TestAppServer::builder()
        .with_codex_home(codex_home.path())
        // SQLite and feedback diagnostics must remain safe independently of stderr.
        .with_env_overrides(&[("RUST_LOG", Some("off"))])
        .build()
        .await?;
    app_server.initialize().await?;
    let thread = app_server
        .start_thread(ThreadStartParams::default())
        .await?
        .thread;
    let _: ThreadApproveGuardianDeniedActionResponse = app_server
        .request(
            |request_id| ClientRequest::ThreadApproveGuardianDeniedAction {
                request_id,
                params: ThreadApproveGuardianDeniedActionParams {
                    thread_id: thread.id.clone(),
                    event: json!({
                        "id": "guardian-review",
                        "target_item_id": "guardian-item",
                        "turn_id": "guardian-turn",
                        "status": "denied",
                        "action": {
                            "type": "mcp_tool_call",
                            "server": "example",
                            "tool_name": "write",
                        },
                    }),
                },
            },
        )
        .await?;
    let _: TurnStartResponse = app_server
        .request(|request_id| ClientRequest::TurnStart {
            request_id,
            params: TurnStartParams {
                thread_id: thread.id.clone(),
                input: vec![UserInput::Text {
                    text: "run echo".into(),
                    text_elements: Vec::new(),
                }],
                approval_policy: Some(AskForApproval::OnRequest),
                approvals_reviewer: Some(ApprovalsReviewer::User),
                sandbox_policy: Some(SandboxPolicy::ReadOnly {
                    network_access: false,
                }),
                ..Default::default()
            },
        })
        .await?;
    let server_request =
        timeout(READ_TIMEOUT, app_server.read_stream_until_request_message()).await??;
    let ServerRequest::CommandExecutionRequestApproval { request_id, params } = server_request
    else {
        anyhow::bail!("expected command approval request");
    };
    let approval_id = params.approval_id.unwrap_or(params.item_id);
    app_server
        .send_response(request_id, json!({ "decision": "decline" }))
        .await?;
    timeout(
        READ_TIMEOUT,
        app_server.read_stream_until_notification_message("turn/completed"),
    )
    .await??;

    let state = StateRuntime::init(
        load_default_config_for_test(&codex_home).await.sqlite,
        "mock_provider".into(),
    )
    .await?;
    // Feedback is read from SQLite, so this also verifies persistence.
    let feedback = timeout(Duration::from_secs(/*secs*/ 60), async {
        loop {
            let logs =
                String::from_utf8(state.query_feedback_logs_for_threads(&[&thread.id]).await?)?;
            if logs
                .lines()
                .any(|line| line.contains("Submission") && line.contains("exec_approval"))
            {
                break Ok::<_, anyhow::Error>(logs);
            }
            tokio::time::sleep(Duration::from_millis(/*millis*/ 50)).await;
        }
    })
    .await??;
    state.close().await;
    for (operation, fields) in [
        (
            "exec_approval",
            vec![
                format!("thread_id={}", thread.id),
                format!("id: \"{approval_id}\""),
                format!("turn_id: Some(\"{}\")", params.turn_id),
                "decision: denied".into(),
            ],
        ),
        (
            "approve_guardian_denied_action",
            vec![
                "review_id: \"guardian-review\"".into(),
                "target_item_id: Some(\"guardian-item\")".into(),
                "turn_id: \"guardian-turn\"".into(),
                "status: Denied".into(),
            ],
        ),
        (
            "turn_input",
            vec![
                "approval_policy: Some(OnRequest)".into(),
                "approvals_reviewer: Some(User)".into(),
                "sandbox_policy: Some(read-only)".into(),
            ],
        ),
    ] {
        let line = feedback
            .lines()
            .find(|line| line.contains("Submission") && line.contains(operation))
            .with_context(|| format!("missing {operation} submission"))?;
        for field in fields {
            anyhow::ensure!(line.contains(&field), "missing {field}: {line}");
        }
    }
    Ok(())
}

#[test]
fn standalone_app_server_emits_json_info_events() -> Result<()> {
    let codex_home = TempDir::new()?;
    let event = app_server_json_shutdown_event("steve-app-server", &[], codex_home.path())?;

    assert_eq!(
        event,
        json!({
            "level": "INFO",
            "fields": {
                "message": "processor task exited",
                "exit_reason": "stdio_connection_closed",
                "remaining_connection_count": 0,
                "shutdown_forced": false,
            },
            "target": "codex_app_server",
        })
    );

    Ok(())
}

#[tokio::test]
async fn app_server_emits_structured_tool_call_timing_event() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let server = create_mock_responses_server_sequence(vec![
        create_exec_command_sse_response("exec-call-1")?,
        create_final_assistant_message_sse_response("done")?,
    ])
    .await;
    let codex_home = TempDir::new()?;
    MockResponsesConfig::new(&server.uri())
        .enable_feature(Feature::UnifiedExec)
        .with_root_config("compact_prompt = \"compact\"\nmodel_auto_compact_token_limit = 100000")
        .with_provider_config("supports_websockets = false")
        .write(codex_home.path())?;

    let mut app_server = TestAppServer::builder()
        .with_codex_home(codex_home.path())
        .with_json_logging("warn,codex_core::tools::parallel=info")
        .build_initialized()
        .await?;

    let thread = app_server
        .start_thread(ThreadStartParams {
            model: Some("mock-model".to_string()),
            ..Default::default()
        })
        .await?
        .thread;

    let TurnStartResponse { turn } = app_server
        .request(|request_id| ClientRequest::TurnStart {
            request_id,
            params: TurnStartParams {
                thread_id: thread.id.clone(),
                input: vec![UserInput::Text {
                    text: "run a command".to_string(),
                    text_elements: Vec::new(),
                }],
                ..Default::default()
            },
        })
        .await?;

    timeout(
        READ_TIMEOUT,
        app_server.read_stream_until_notification_message("turn/completed"),
    )
    .await??;

    let mut tool_call = app_server
        .wait_for_json_log_event("codex.tool_call", Duration::from_secs(/*secs*/ 10))
        .await?;
    let tool_call_object = tool_call
        .as_object_mut()
        .context("tool call log event must be an object")?;
    // JsonLogCapture already validates the timestamp as RFC 3339.
    tool_call_object
        .remove("timestamp")
        .context("tool call log event must include a timestamp")?;
    let fields = tool_call_object
        .get_mut("fields")
        .and_then(Value::as_object_mut)
        .context("tool call log event fields must be an object")?;
    let trace_id = fields
        .remove("trace_id")
        .context("tool call log event must include trace_id")?;
    anyhow::ensure!(trace_id.is_string(), "trace_id must be a string");
    let dispatch_duration_ms = fields
        .remove("dispatch_duration_ms")
        .and_then(|duration| duration.as_u64())
        .context("dispatch_duration_ms must be a nonnegative integer")?;
    let handler_duration_ms = fields
        .remove("handler_duration_ms")
        .and_then(|duration| duration.as_u64())
        .context("handler_duration_ms must be a nonnegative integer")?;
    let total_duration_ms = fields
        .remove("total_duration_ms")
        .and_then(|duration| duration.as_u64())
        .context("total_duration_ms must be a nonnegative integer")?;
    let accounted_duration_ms = dispatch_duration_ms
        .checked_add(handler_duration_ms)
        .context("dispatch and handler durations must not overflow")?;
    anyhow::ensure!(
        total_duration_ms >= accounted_duration_ms
            && total_duration_ms - accounted_duration_ms <= 1,
        "dispatch and handler durations must account for total duration within integer truncation"
    );

    assert_eq!(
        tool_call,
        json!({
            "level": "INFO",
            "fields": {
                "message": "tool call completed",
                "event.name": "codex.tool_call",
                "conversation.id": thread.id,
                "turn_id": turn.id,
                "tool_name": "exec_command",
                "call_id": "exec-call-1",
                "tool_source": "direct",
                "execution_started": true,
            },
            "target": "codex_core::tools::parallel",
        })
    );

    Ok(())
}
