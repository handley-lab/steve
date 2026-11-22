use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use codex_core::GuardianRootMessage;
use codex_core::TurnInputRequest;
use codex_core::config::Constrained;
use codex_extension_api::ExtensionRegistryBuilder;
use codex_features::Feature;
use codex_history::CompactedItem;
use codex_history::RolloutItem;
use codex_protocol::ThreadId;
use codex_protocol::config_types::ApprovalsReviewer;
use codex_protocol::models::PermissionProfile;
use codex_protocol::protocol::AskForApproval;
use codex_protocol::protocol::EventMsg;
use codex_protocol::user_input::UserInput;
use core_test_support::ThreadIdle;
use core_test_support::responses::ResponseMock;
use core_test_support::responses::ResponsesRequest;
use core_test_support::responses::ev_assistant_message;
use core_test_support::responses::ev_completed;
use core_test_support::responses::ev_function_call;
use core_test_support::responses::ev_function_call_with_namespace;
use core_test_support::responses::mount_response_once_match;
use core_test_support::responses::mount_sse_once_match;
use core_test_support::responses::sse;
use core_test_support::responses::sse_response;
use core_test_support::responses::start_mock_server;
use core_test_support::skip_if_no_network;
use core_test_support::test_codex::TestCodex;
use core_test_support::test_codex::test_codex;
use core_test_support::wait_for_event;
use serde_json::Value;
use serde_json::json;

#[path = "guardian_root_handoff.rs"]
pub(super) mod root_handoff;

#[path = "guardian_heartbeat_authorization.rs"]
mod heartbeat;

const INITIAL_TASK: &str = "Inspect the reviewed production deployment.";
const SPAWN_CALL_ID: &str = "spawn-authorization-worker";

fn request_body(request: &wiremock::Request) -> Option<Value> {
    let compressed = request
        .headers
        .get("content-encoding")
        .and_then(|value| value.to_str().ok())
        .is_some_and(|encoding| encoding.eq_ignore_ascii_case("zstd"));
    let bytes = if compressed {
        zstd::stream::decode_all(std::io::Cursor::new(&request.body)).ok()?
    } else {
        request.body.clone()
    };
    serde_json::from_slice(&bytes).ok()
}

fn is_root_request(request: &wiremock::Request, root_thread_id: ThreadId) -> bool {
    request_body(request)
        .is_some_and(|body| body["client_metadata"]["thread_id"] == json!(root_thread_id))
}

fn is_worker_request(request: &wiremock::Request, root_thread_id: ThreadId) -> bool {
    request_body(request).is_some_and(|body| {
        body["client_metadata"]["x-codex-parent-thread-id"] == json!(root_thread_id)
            && body["client_metadata"]["x-openai-subagent"] != "guardian"
    })
}

fn contains_text(request: &wiremock::Request, text: &str) -> bool {
    request_body(request).is_some_and(|body| body.to_string().contains(text))
}

fn has_call_output(request: &wiremock::Request, call_id: &str) -> bool {
    request_body(request).is_some_and(|body| {
        body["input"].as_array().is_some_and(|items| {
            items.iter().any(|item| {
                (item["type"] == "function_call_output"
                    || item["type"] == "custom_tool_call_output")
                    && item["call_id"] == call_id
            })
        })
    })
}

async fn mount_completion(
    server: &wiremock::MockServer,
    root_thread_id: ThreadId,
    call_id: &'static str,
) -> ResponseMock {
    mount_sse_once_match(
        server,
        move |request: &wiremock::Request| {
            is_root_request(request, root_thread_id) && has_call_output(request, call_id)
        },
        sse(vec![ev_completed(&format!("response-{call_id}-completed"))]),
    )
    .await
}
