use anyhow::Context;
use anyhow::Result;
use codex_core::TurnInputRequest;
use codex_core::config::Constrained;
use codex_otel::MetricsClient;
use codex_otel::MetricsConfig;
use codex_protocol::config_types::ApprovalsReviewer;
use codex_protocol::items::TurnItem;
use codex_protocol::openai_models::AutoReviewMessages;
use codex_protocol::protocol::AskForApproval;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::GuardianAssessmentStatus;
use codex_protocol::protocol::Op;
use codex_protocol::protocol::ThreadSettingsOverrides;
use codex_protocol::user_input::UserInput;
use core_test_support::responses;
use core_test_support::skip_if_no_network;
use core_test_support::skip_if_wine_exec;
use core_test_support::test_codex::local_requests;
use core_test_support::test_codex::test_codex;
use core_test_support::wait_for_event;
use core_test_support::wait_for_mcp_server;
use opentelemetry_sdk::metrics::data::AggregatedMetrics;
use opentelemetry_sdk::metrics::data::MetricData;
use pretty_assertions::assert_eq;
use serde_json::Value;
use serde_json::json;
use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;
use test_case::test_case;
use wiremock::matchers::body_partial_json;

// The tool itself needs no approval. Its server requests a separate Guardian
// review after tools/call, exercising the ordinary MCP elicitation path.
pub(super) const ELICITATION_SERVER: &str = r#"
import json
import sys

def send(message):
    print(json.dumps({"jsonrpc": "2.0", **message}), flush=True)

pending_call = None
approval_meta = json.loads(sys.argv[1]) if len(sys.argv) > 1 else {}
multiple = isinstance(approval_meta, list)
approval_metas = approval_meta if multiple else [approval_meta]
browser = len(sys.argv) > 2 and sys.argv[2] == "browser"
connector_id = sys.argv[4] if len(sys.argv) > 4 else "connector_openai_browser"
pending_elicitations = []
approval_results = {}
for line in sys.stdin:
    request = json.loads(line)
    method = request.get("method")
    if method == "initialize":
        result = {"protocolVersion": request["params"]["protocolVersion"],
                  "capabilities": {"tools": {}},
                  "serverInfo": {"name": "guardian-elicitation-test", "version": "1"}}
    elif method == "tools/list":
        result = {"tools": [{"name": "browser_js" if browser else "js" if multiple else "request_approval",
                  "inputSchema": {"type": "object", "properties": {}},
                  "annotations": {"readOnlyHint": True},
                  "_meta": {"connector_id": connector_id, "connector_name": "Browser",
                            "_codex_apps": {"resource_uri": f"/{connector_id}/browser-link/js",
                                            "contains_mcp_source": True}} if browser else {}}]}
    elif method == "tools/call":
        if request["params"].get("arguments", {}).get("seed"):
            send({"id": request["id"], "result": {"content": [{"type": "text", "text": "seeded"}]}})
            continue
        pending_call = request["id"]
        approval_results = {}
        for index, meta in enumerate(approval_metas):
            invocation_meta = {}
            if len(sys.argv) > 2 and sys.argv[2] in ("forward", "browser"):
                key = "_codex_apps" if browser else "callId"
                invocation_meta[key] = request["params"]["_meta"][key]
            pending_elicitations.append({
                "id": f"server-approval-{index}" if multiple else "server-approval",
                "method": "elicitation/create", "params": {
                    "message": "Approve the server-side action?",
                    "requestedSchema": {"type": "object", "properties": {}},
                    "_meta": {"codex_request_type": "approval_request",
                              "codex_approval_kind": "mcp_tool_call", "tool_name": "write_record",
                              **meta, **invocation_meta}}})
        if len(sys.argv) > 3:
            for elicitation in pending_elicitations:
                params = elicitation["params"]
                params.update(json.loads(sys.argv[3]))
                if params.get("mode") == "url":
                    params.pop("requestedSchema")
        send(pending_elicitations.pop(0))
        continue
    elif method is None and str(request.get("id", "")).startswith("server-approval"):
        approval_results[request["id"]] = request.get("result")
        if pending_elicitations:
            send(pending_elicitations.pop(0))
        else:
            result = approval_results if multiple else request.get("result")
            send({"id": pending_call, "result": {"content": [
                {"type": "text", "text": json.dumps(result)}]}})
        continue
    elif method == "resources/list":
        result = {"resources": []}
    elif method == "resources/templates/list":
        result = {"resourceTemplates": []}
    elif "id" not in request:
        continue
    else:
        result = {}
    send({"id": request["id"], "result": result})
"#;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn interrupt_aborts_server_initiated_mcp_guardian_review() -> Result<()> {
    skip_if_no_network!(Ok(()));
    skip_if_wine_exec!(Ok(()), "the MCP fixture requires a host Python interpreter");

    let server = responses::start_mock_server().await;
    let mcp_servers = serde_json::from_value(json!({
        "elicitation": {
            "command": if cfg!(windows) { "python" } else { "python3" },
            "args": ["-u", "-c", ELICITATION_SERVER],
            "default_tools_approval_mode": "approve",
        }
    }))?;
    let mut builder = test_codex().with_config(move |config| {
        config.permissions.approval_policy = Constrained::allow_any(AskForApproval::OnRequest);
        config.approvals_reviewer = ApprovalsReviewer::AutoReview;
        config
            .mcp_servers
            .set(mcp_servers)
            .expect("set MCP fixture");
    });
    let test = builder.build_with_auto_env(&server).await?;
    wait_for_mcp_server(&test.codex, "elicitation").await?;
    responses::mount_sse_once(
        &server,
        responses::sse(vec![
            responses::ev_function_call_with_namespace(
                "eliciting-tool",
                "mcp__elicitation",
                "request_approval",
                "{}",
            ),
            responses::ev_completed("parent-tool"),
        ]),
    )
    .await;
    let pending_guardian = responses::mount_response_once_match(
        &server,
        |request: &wiremock::Request| {
            serde_json::from_slice::<Value>(&request.body)
                .expect("Responses request body should be valid JSON")
                .pointer("/client_metadata/x-openai-subagent")
                .and_then(Value::as_str)
                == Some("guardian")
        },
        responses::sse_response(responses::sse(vec![
            responses::ev_assistant_message(
                "review-result",
                &json!({
                    "risk_level": "low",
                    "user_authorization": "high",
                    "outcome": "allow",
                    "rationale": "This response must be interrupted.",
                })
                .to_string(),
            ),
            responses::ev_completed("pending-review"),
        ]))
        .set_delay(Duration::from_secs(60)),
    )
    .await;

    test.codex
        .start_or_steer_turn(
            TurnInputRequest::user_input(vec![UserInput::Text {
                text: "Run the tool that requests server-side approval.".into(),
                text_elements: Vec::new(),
            }])
            .with_thread_settings(ThreadSettingsOverrides {
                environments: Some(local_requests(test.config.cwd.clone())),
                ..Default::default()
            }),
        )
        .await?;
    tokio::time::timeout(Duration::from_secs(10), async {
        while pending_guardian.requests().is_empty() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .context("server-initiated Guardian review did not start")?;
    assert!(
        pending_guardian
            .single_request()
            .body_contains_text("write_record")
    );

    test.codex.submit(Op::Interrupt).await?;
    tokio::time::timeout(Duration::from_secs(10), async {
        let mut guardian_aborted = false;
        let mut parent_aborted = false;
        while !guardian_aborted || !parent_aborted {
            match test.codex.next_event().await?.msg {
                EventMsg::GuardianAssessment(assessment)
                    if assessment.status == GuardianAssessmentStatus::Aborted =>
                {
                    guardian_aborted = true;
                }
                EventMsg::TurnAborted(_) => parent_aborted = true,
                _ => {}
            }
        }
        anyhow::Ok(())
    })
    .await
    .context("turn interruption did not abort the MCP elicitation review")??;
    test.codex.shutdown_and_wait().await?;
    Ok(())
}

#[test_case(None, false; "ordinary_absent")]
#[test_case(Some(false), false; "ordinary_false")]
#[test_case(Some(true), false; "ordinary_true")]
#[test_case(None, true; "strict_absent")]
#[test_case(Some(false), true; "strict_false")]
#[test_case(Some(true), true; "strict_true")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn server_initiated_mcp_elicitation_can_require_synchronous_auto_review(
    sensitive_action: Option<bool>,
    strict_auto_review: bool,
) -> Result<()> {
    skip_if_no_network!(Ok(()));
    skip_if_wine_exec!(Ok(()), "the MCP fixture requires a host Python interpreter");

    const RATIONALE: &str = "The server-side action is not authorized.";
    const REJECTION_INSTRUCTIONS: &str = "Do not retry the rejected action.";

    struct AutoApprovingReviewContributor;

    impl codex_extension_api::ApprovalReviewContributor for AutoApprovingReviewContributor {
        fn decide<'a>(
            &'a self,
            _input: &'a codex_extension_api::ApprovalDecisionInput<'_>,
        ) -> codex_extension_api::ExtensionFuture<'a, Option<codex_extension_api::ApprovalDecision>>
        {
            Box::pin(async { Some(codex_extension_api::ApprovalDecision::Allow) })
        }
    }

    let mut extensions = codex_extension_api::ExtensionRegistryBuilder::new();
    extensions.approval_review_contributor(Arc::new(AutoApprovingReviewContributor));
    let mut meta = serde_json::Map::new();
    if strict_auto_review {
        meta.insert("codex_strict_auto_review".to_string(), json!(true));
    }
    if let Some(sensitive_action) = sensitive_action {
        meta.insert(
            "codex_sensitive_action".to_string(),
            json!(sensitive_action),
        );
    }

    let server = responses::start_mock_server().await;
    let mcp_servers = serde_json::from_value(json!({
        "elicitation": {
            "command": if cfg!(windows) { "python" } else { "python3" },
            "args": ["-u", "-c", ELICITATION_SERVER, serde_json::to_string(&meta)?],
            "default_tools_approval_mode": "approve",
        }
    }))?;
    let mut builder = test_codex()
        .with_extensions(Arc::new(extensions.build()))
        .with_model_info_override("gpt-5.5", |model| {
            model
                .model_messages
                .as_mut()
                .expect("acting model messages")
                .auto_review = Some(AutoReviewMessages {
                policy: None,
                policy_template: None,
                node_repl_policy: None,
                rejection_instructions: Some(REJECTION_INSTRUCTIONS.to_string()),
                timeout_instructions: None,
            });
        })
        .with_config(move |config| {
            config.permissions.approval_policy = Constrained::allow_any(AskForApproval::OnRequest);
            config.approvals_reviewer = if strict_auto_review {
                ApprovalsReviewer::User
            } else {
                ApprovalsReviewer::AutoReview
            };
            config
                .mcp_servers
                .set(mcp_servers)
                .expect("set MCP fixture");
        });
    let test = builder.build_with_auto_env(&server).await?;
    wait_for_mcp_server(&test.codex, "elicitation").await?;
    responses::mount_sse_once(
        &server,
        responses::sse(vec![
            responses::ev_function_call_with_namespace(
                "eliciting-tool",
                "mcp__elicitation",
                "request_approval",
                "{}",
            ),
            responses::ev_completed("parent-tool"),
        ]),
    )
    .await;
    let guardian = responses::mount_sse_once_match(
        &server,
        body_partial_json(json!({"client_metadata": {"x-openai-subagent": "guardian"}})),
        responses::sse(vec![
            responses::ev_assistant_message(
                "review-result",
                &json!({
                    "risk_level": "high",
                    "user_authorization": "low",
                    "outcome": "deny",
                    "rationale": RATIONALE,
                })
                .to_string(),
            ),
            responses::ev_completed("guardian-review"),
        ]),
    )
    .await;
    let parent = responses::mount_sse_once(
        &server,
        responses::sse(vec![responses::ev_completed("parent-complete")]),
    )
    .await;

    test.submit_text_turn("Run the tool that requests server-side approval.")
        .await?;

    let output = parent
        .single_request()
        .function_call_output("eliciting-tool");
    let response: Value = serde_json::from_str(
        output["output"][1]["text"]
            .as_str()
            .expect("MCP fixture should echo the wire response"),
    )?;
    let requires_sync = sensitive_action == Some(true);
    let expected_response = if requires_sync {
        json!({
            "action": "decline",
            "_meta": {
                "approvals_reviewer": "auto_review",
                "message": format!(
                    "This action was rejected due to unacceptable risk.\nReason: {RATIONALE}\n{REJECTION_INSTRUCTIONS}"
                ),
            },
        })
    } else {
        json!({
            "action": "accept",
            "content": {},
            "_meta": {"approvals_reviewer": "auto_review"},
        })
    };
    assert_eq!(response, expected_response);

    let guardian_requests = guardian
        .requests()
        .into_iter()
        .filter(|request| request.body_json()["client_metadata"]["x-openai-subagent"] == "guardian")
        .collect::<Vec<_>>();
    assert_eq!(guardian_requests.len(), usize::from(requires_sync));
    if requires_sync {
        assert!(guardian_requests[0].body_contains_text("write_record"));
    }
    test.codex.shutdown_and_wait().await?;
    Ok(())
}

#[test_case("form"; "nonempty_form")]
#[test_case("url"; "url")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn user_review_preserves_unsupported_guardian_elicitations(mode: &str) -> Result<()> {
    skip_if_no_network!(Ok(()));
    skip_if_wine_exec!(Ok(()), "the MCP fixture requires a host Python interpreter");
    let server = responses::start_mock_server().await;
    let form_schema = json!({
        "type": "object", "properties": {"name": {"type": "string"}}, "required": ["name"]
    });
    let params = if mode == "url" {
        json!({"mode": "url", "url": "https://example.com/approve", "elicitationId": "approval"})
    } else {
        json!({"requestedSchema": form_schema})
    };
    let mcp_servers = serde_json::from_value(json!({
        "elicitation": {
            "command": if cfg!(windows) { "python" } else { "python3" },
            "args": ["-u", "-c", ELICITATION_SERVER, "{}", "", params.to_string()],
            "default_tools_approval_mode": "approve",
        }
    }))?;
    let test = test_codex()
        .with_config(move |config| {
            config.approvals_reviewer = ApprovalsReviewer::User;
            config.permissions.approval_policy = Constrained::allow_any(AskForApproval::Granular(
                codex_protocol::protocol::GranularApprovalConfig {
                    sandbox_approval: false,
                    rules: false,
                    skill_approval: false,
                    request_permissions: false,
                    mcp_elicitations: true,
                },
            ));
            config
                .mcp_servers
                .set(mcp_servers)
                .expect("set MCP fixture");
        })
        .build_with_auto_env(&server)
        .await?;
    wait_for_mcp_server(&test.codex, "elicitation").await?;
    responses::mount_sse_once(
        &server,
        responses::sse(vec![
            responses::ev_function_call_with_namespace(
                "eliciting-tool",
                "mcp__elicitation",
                "request_approval",
                "{}",
            ),
            responses::ev_completed("parent-tool"),
        ]),
    )
    .await;
    let parent = responses::mount_sse_once(
        &server,
        responses::sse(vec![responses::ev_completed("parent-complete")]),
    )
    .await;
    test.codex
        .start_or_steer_turn(TurnInputRequest::user_input(vec![UserInput::Text {
            text: "Run the tool and ask me for its approval.".into(),
            text_elements: Vec::new(),
        }]))
        .await?;
    let event = wait_for_event(&test.codex, |event| {
        matches!(
            event,
            EventMsg::ElicitationRequest(_)
                | EventMsg::GuardianAssessment(_)
                | EventMsg::TurnComplete(_)
        )
    })
    .await;
    let EventMsg::ElicitationRequest(request) = event else {
        panic!("expected a user elicitation, got {event:?}");
    };
    match &request.request {
        codex_protocol::approvals::ElicitationRequest::Form {
            requested_schema, ..
        } => {
            assert_eq!(requested_schema, &form_schema);
        }
        codex_protocol::approvals::ElicitationRequest::Url { url, .. } => {
            assert_eq!(url, "https://example.com/approve");
        }
        other => panic!("unexpected elicitation: {other:?}"),
    }
    test.codex
        .submit(Op::ResolveElicitation {
            server_name: request.server_name,
            request_id: request.id,
            decision: codex_protocol::approvals::ElicitationAction::Decline,
            content: None,
            meta: None,
        })
        .await?;
    wait_for_event(&test.codex, |event| {
        matches!(event, EventMsg::TurnComplete(_))
    })
    .await;
    assert!(
        parent
            .single_request()
            .function_call_output("eliciting-tool")
            .to_string()
            .contains("decline")
    );
    test.codex.shutdown_and_wait().await?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn yielded_code_mode_elicitation_keeps_live_invocation_metadata() -> Result<()> {
    skip_if_no_network!(Ok(()));
    skip_if_wine_exec!(Ok(()), "the MCP fixture requires a host Python interpreter");

    let server = responses::start_mock_server().await;
    let meta = json!([{
        "codex_strict_auto_review": true,
        "codex_sensitive_action": true,
        "tool_name": "write_record",
        "tool_params": {"value": 42},
        "connector_id": "inner-connector",
    }]);
    let mcp_servers = serde_json::from_value(json!({
        "node_repl": {
            "command": if cfg!(windows) { "python" } else { "python3" },
            "args": ["-u", "-c", ELICITATION_SERVER, meta.to_string(), "forward"],
            "default_tools_approval_mode": "approve",
        }
    }))?;
    let mut builder = test_codex()
        .with_model_info_override("gpt-5.2", |model| {
            model.tool_mode = Some(codex_protocol::openai_models::ToolMode::CodeMode);
            model.experimental_supported_tools = vec!["test_sync_tool".to_string()];
        })
        .with_model_info_override("gpt-5.4", |model| {
            model.tool_mode = Some(codex_protocol::openai_models::ToolMode::CodeMode);
            model.experimental_supported_tools = vec!["test_sync_tool".to_string()];
        })
        .with_config(move |config| {
            config
                .features
                .enable(codex_features::Feature::CodeMode)
                .unwrap();
            config.permissions.approval_policy = Constrained::allow_any(AskForApproval::OnRequest);
            config.approvals_reviewer = ApprovalsReviewer::User;
            config.mcp_servers.set(mcp_servers).unwrap();
        });
    let mut test = builder.build_with_auto_env(&server).await?;
    let metrics = MetricsClient::new(
        MetricsConfig::in_memory("test", "test", "1", Default::default()).with_runtime_reader(),
    )?;
    let mut options = test.start_thread_options().await;
    options.history_mode = Some(codex_protocol::protocol::ThreadHistoryMode::Legacy);
    options.thread_extension_init.insert(metrics.clone());
    let thread = test.thread_manager.start_thread(options).await?;
    test.codex.shutdown_and_wait().await?;
    test.codex = thread.thread;
    wait_for_mcp_server(&test.codex, "node_repl").await?;
    let barrier = r#"await tools.test_sync_tool({barrier: {
        id: "elicitation-origin", participants: 2, timeout_ms: 60000
    }});"#;
    responses::mount_sse_once(&server, responses::sse(vec![
        responses::ev_response_created("origin-a"),
        responses::ev_custom_tool_call("cell-a", "exec", &format!(
            "// @exec: {{\"yield_time_ms\": 1}}\n{barrier}\ntext(await tools.mcp__node_repl__js({{code: 'write_record()'}}));"
        )),
        responses::ev_completed("origin-a"),
    ])).await;
    let yielded = responses::mount_sse_once(
        &server,
        responses::sse(vec![responses::ev_completed("a-finished")]),
    )
    .await;
    test.submit_text_turn("Start A and leave its cell running.")
        .await?;
    let output = yielded.single_request().custom_tool_call_output("cell-a");
    let text = output["output"]
        .as_str()
        .or_else(|| output["output"][0]["text"].as_str())
        .context("A should return text")?;
    let cell_id = text
        .strip_prefix("Script running with cell ID ")
        .and_then(|text| text.lines().next())
        .context("A should yield a running cell")?
        .to_string();

    responses::mount_sse_once(
        &server,
        responses::sse(vec![
            responses::ev_custom_tool_call("cell-b", "exec", barrier),
            responses::ev_completed("b-started"),
        ]),
    )
    .await;
    let guardian = responses::mount_sse_once_match(
        &server,
        body_partial_json(json!({"client_metadata": {"x-openai-subagent": "guardian"}})),
        responses::sse(vec![
            responses::ev_assistant_message(
                "review-result",
                &json!({
                    "risk_level": "low", "user_authorization": "high", "outcome": "allow",
                    "rationale": "The user requested the action.",
                })
                .to_string(),
            ),
            responses::ev_completed("review-done"),
        ]),
    )
    .await;
    responses::mount_sse_once(
        &server,
        responses::sse(vec![
            responses::ev_function_call(
                "wait-a",
                "wait",
                &json!({
                    "cell_id": cell_id, "yield_time_ms": 60000,
                })
                .to_string(),
            ),
            responses::ev_completed("b-wait"),
        ]),
    )
    .await;
    responses::mount_sse_once(
        &server,
        responses::sse(vec![responses::ev_completed("b-finished")]),
    )
    .await;
    test.codex
        .start_or_steer_turn(
            TurnInputRequest::user_input(vec![UserInput::Text {
                text: "Run B and wait for A.".into(),
                text_elements: Vec::new(),
            }])
            .with_thread_settings(ThreadSettingsOverrides {
                model: Some("gpt-5.2".into()),
                ..Default::default()
            }),
        )
        .await?;
    let mut invocation_id = None;
    let mut assessment_target = None;
    loop {
        let event =
            tokio::time::timeout(Duration::from_secs(60), test.codex.next_event()).await??;
        match event.msg {
            EventMsg::ItemCompleted(event) => {
                if let TurnItem::McpToolCall(item) = event.item {
                    invocation_id = Some(item.id);
                }
            }
            EventMsg::GuardianAssessment(assessment)
                if assessment.status == GuardianAssessmentStatus::Approved =>
            {
                assessment_target = assessment.target_item_id;
            }
            EventMsg::ElicitationRequest(_) => panic!("strict review must not prompt the user"),
            EventMsg::TurnComplete(_) => break,
            _ => {}
        }
    }
    assert!(invocation_id.is_some(), "A's tool must complete during B");
    assert_eq!(assessment_target, invocation_id);
    assert_eq!(
        guardian
            .requests()
            .iter()
            .filter(
                |request| request.body_json()["client_metadata"]["x-openai-subagent"] == "guardian"
            )
            .count(),
        1
    );
    test.codex.shutdown_and_wait().await?;
    let mut samples = guardian_blocking_samples(&metrics, "model")?;
    assert_eq!(samples.remove("gpt-5.4"), Some(0.0));
    assert!(samples.remove("gpt-5.2").unwrap() > 0.0);
    assert!(samples.is_empty());
    Ok(())
}

/// Reads per-turn samples shared by command and MCP approval regressions.
pub(super) fn guardian_blocking_samples(
    metrics: &MetricsClient,
    label: &str,
) -> Result<BTreeMap<String, f64>> {
    let snapshot = metrics.snapshot()?;
    let metric = snapshot
        .scope_metrics()
        .flat_map(opentelemetry_sdk::metrics::data::ScopeMetrics::metrics)
        .find(|metric| metric.name() == "codex.guardian.turn.blocking_ms")
        .context("blocking histogram")?;
    let AggregatedMetrics::F64(MetricData::Histogram(histogram)) = metric.data() else {
        panic!("expected blocking histogram");
    };
    histogram
        .data_points()
        .map(|point| {
            let attribute = point
                .attributes()
                .find(|tag| tag.key.as_str() == label)
                .context("blocking metric label")?;
            assert_eq!(point.count(), 1);
            Ok((attribute.value.as_str().to_string(), point.sum()))
        })
        .collect()
}
