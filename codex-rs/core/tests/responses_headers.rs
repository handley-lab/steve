use std::process::Command;

use core_test_support::responses;
use core_test_support::test_codex::test_codex;
use pretty_assertions::assert_eq;

fn normalize_git_remote_url(url: &str) -> String {
    let normalized = url.trim().trim_end_matches('/');
    normalized
        .strip_suffix(".git")
        .unwrap_or(normalized)
        .to_string()
}

#[tokio::test]
async fn responses_stream_includes_turn_metadata_header_for_git_workspace_e2e() {
    core_test_support::skip_if_no_network!();

    let server = responses::start_mock_server().await;
    let response_body = responses::sse(vec![
        responses::ev_response_created("resp-1"),
        responses::ev_completed("resp-1"),
    ]);

    let test = test_codex()
        .with_config(|config| config.analytics_enabled = Some(true))
        .build(&server)
        .await
        .expect("build test codex");
    let cwd = test.cwd_path();

    let first_request = responses::mount_sse_once(&server, response_body.clone()).await;
    test.submit_turn("hello")
        .await
        .expect("submit first turn prompt");
    let initial_request = first_request.single_request();
    let initial_header = initial_request
        .header("x-codex-turn-metadata")
        .expect("x-codex-turn-metadata header should be present");
    let initial_parsed: serde_json::Value =
        serde_json::from_str(&initial_header).expect("x-codex-turn-metadata should be valid JSON");
    let initial_turn_id = initial_parsed
        .get("turn_id")
        .and_then(serde_json::Value::as_str)
        .expect("turn_id should be present")
        .to_string();
    assert!(
        !initial_turn_id.is_empty(),
        "turn_id should not be empty in x-codex-turn-metadata"
    );
    let initial_turn_started_at_unix_ms = initial_parsed
        .get("turn_started_at_unix_ms")
        .and_then(serde_json::Value::as_i64)
        .expect("turn_started_at_unix_ms should be present");
    assert!(
        initial_turn_started_at_unix_ms > 0,
        "turn_started_at_unix_ms should be positive"
    );
    assert_eq!(
        initial_parsed
            .get("sandbox")
            .and_then(serde_json::Value::as_str),
        Some("none")
    );
    assert_eq!(
        initial_parsed
            .get("sandbox_mode")
            .and_then(serde_json::Value::as_str),
        Some("danger-full-access")
    );
    let body_metadata: serde_json::Value = serde_json::from_str(
        initial_request.body_json()["client_metadata"]["x-codex-turn-metadata"]
            .as_str()
            .expect("request body should include x-codex-turn-metadata"),
    )
    .expect("body x-codex-turn-metadata should be valid JSON");
    assert_eq!(
        (
            initial_parsed["analytics_enabled"].as_bool(),
            body_metadata["analytics_enabled"].as_bool(),
        ),
        (Some(true), Some(true)),
    );
    assert_eq!(
        body_metadata
            .get("sandbox_mode")
            .and_then(serde_json::Value::as_str),
        Some("danger-full-access")
    );
    assert_eq!(
        initial_parsed
            .get("thread_source")
            .and_then(serde_json::Value::as_str),
        None
    );

    let git_config_global = cwd.join("empty-git-config");
    std::fs::write(&git_config_global, "").expect("write empty git config");
    let run_git = |args: &[&str]| {
        let output = Command::new("git")
            .env("GIT_CONFIG_GLOBAL", &git_config_global)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .args(args)
            .current_dir(cwd)
            .output()
            .expect("git command should run");
        assert!(
            output.status.success(),
            "git {:?} failed: stdout={} stderr={}",
            args,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        output
    };

    run_git(&["init"]);
    run_git(&["config", "user.name", "Test User"]);
    run_git(&["config", "user.email", "test@example.com"]);
    std::fs::write(cwd.join("README.md"), "hello").expect("write README");
    run_git(&["add", "."]);
    run_git(&["commit", "-m", "initial commit"]);
    run_git(&[
        "remote",
        "add",
        "origin",
        "https://github.com/openai/codex.git",
    ]);

    let expected_head = String::from_utf8(run_git(&["rev-parse", "HEAD"]).stdout)
        .expect("git rev-parse output should be valid UTF-8")
        .trim()
        .to_string();
    let expected_origin = String::from_utf8(run_git(&["remote", "get-url", "origin"]).stdout)
        .expect("git remote get-url output should be valid UTF-8")
        .trim()
        .to_string();

    let first_response = responses::sse(vec![
        responses::ev_response_created("resp-2"),
        responses::ev_reasoning_item("rsn-1", &["thinking"], &[]),
        responses::ev_exec_command_call("call-1", "echo turn-metadata"),
        responses::ev_completed("resp-2"),
    ]);
    let follow_up_response = responses::sse(vec![
        responses::ev_response_created("resp-3"),
        responses::ev_assistant_message("msg-1", "done"),
        responses::ev_completed("resp-3"),
    ]);
    let request_log = responses::mount_response_sequence(
        &server,
        vec![
            responses::sse_response(first_response),
            responses::sse_response(follow_up_response),
        ],
    )
    .await;

    test.submit_turn("hello")
        .await
        .expect("submit post-git turn prompt");

    let requests = request_log.requests();
    assert_eq!(requests.len(), 2, "expected two requests in one turn");

    let first_parsed: serde_json::Value = serde_json::from_str(
        &requests[0]
            .header("x-codex-turn-metadata")
            .expect("first request should include turn metadata"),
    )
    .expect("first metadata should be valid json");
    let second_parsed: serde_json::Value = serde_json::from_str(
        &requests[1]
            .header("x-codex-turn-metadata")
            .expect("second request should include turn metadata"),
    )
    .expect("second metadata should be valid json");

    let first_turn_id = first_parsed
        .get("turn_id")
        .and_then(serde_json::Value::as_str)
        .expect("first turn_id should be present");
    let second_turn_id = second_parsed
        .get("turn_id")
        .and_then(serde_json::Value::as_str)
        .expect("second turn_id should be present");
    let first_turn_started_at_unix_ms = first_parsed
        .get("turn_started_at_unix_ms")
        .and_then(serde_json::Value::as_i64)
        .expect("first turn_started_at_unix_ms should be present");
    let second_turn_started_at_unix_ms = second_parsed
        .get("turn_started_at_unix_ms")
        .and_then(serde_json::Value::as_i64)
        .expect("second turn_started_at_unix_ms should be present");
    assert!(
        first_turn_started_at_unix_ms > 0,
        "first turn_started_at_unix_ms should be positive"
    );
    assert_eq!(
        first_turn_started_at_unix_ms, second_turn_started_at_unix_ms,
        "requests in the same turn should share turn_started_at_unix_ms"
    );
    assert_eq!(
        first_parsed
            .get("thread_source")
            .and_then(serde_json::Value::as_str),
        None
    );
    assert_eq!(
        second_parsed
            .get("thread_source")
            .and_then(serde_json::Value::as_str),
        None
    );
    assert_eq!(
        first_turn_id, second_turn_id,
        "requests should share turn_id"
    );
    assert_ne!(
        second_turn_id,
        initial_turn_id.as_str(),
        "post-git turn should have a new turn_id"
    );

    assert_eq!(
        second_parsed
            .get("sandbox")
            .and_then(serde_json::Value::as_str),
        Some("none")
    );

    let workspace = second_parsed
        .get("workspaces")
        .and_then(serde_json::Value::as_object)
        .and_then(|workspaces| workspaces.values().next())
        .cloned()
        .expect("second request should include git workspace metadata");
    assert_eq!(
        workspace
            .get("latest_git_commit_hash")
            .and_then(serde_json::Value::as_str),
        Some(expected_head.as_str())
    );
    if let Some(actual_origin) = workspace
        .get("associated_remote_urls")
        .and_then(serde_json::Value::as_object)
        .and_then(|remotes| remotes.get("origin"))
        .and_then(serde_json::Value::as_str)
    {
        assert_eq!(
            normalize_git_remote_url(actual_origin),
            normalize_git_remote_url(&expected_origin)
        );
    }
    assert_eq!(
        workspace
            .get("has_changes")
            .and_then(serde_json::Value::as_bool),
        Some(false)
    );
}

/// Credential-bearing Git remotes must be redacted from both metadata transports.
#[tokio::test]
async fn responses_stream_redacts_git_remote_credentials_from_turn_metadata() {
    core_test_support::skip_if_no_network!();

    let server = responses::start_mock_server().await;
    let test = test_codex()
        .with_model("test-gpt-5.1-codex")
        .build(&server)
        .await
        .expect("build test codex");
    let cwd = test.cwd_path();
    let remote_token = "test-git-secret-token";
    let credential_bearing_origin =
        "https://alice:test-git-secret-token@example.invalid/openai/codex.git";

    for args in [
        ["init"].as_slice(),
        ["remote", "add", "origin", credential_bearing_origin].as_slice(),
    ] {
        let output = Command::new("git")
            .args(args)
            .current_dir(cwd)
            .output()
            .expect("git command should run");
        assert!(
            output.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    let first_response = responses::sse(vec![
        responses::ev_response_created("resp-1"),
        responses::ev_function_call(
            "wait-for-git",
            "test_sync_tool",
            r#"{"wait_for_git_enrichment":true}"#,
        ),
        responses::ev_completed("resp-1"),
    ]);
    let follow_up_response = responses::sse(vec![
        responses::ev_response_created("resp-2"),
        responses::ev_assistant_message("msg-1", "done"),
        responses::ev_completed("resp-2"),
    ]);
    let request_log = responses::mount_response_sequence(
        &server,
        vec![
            responses::sse_response(first_response),
            responses::sse_response(follow_up_response),
        ],
    )
    .await;

    test.submit_turn("inspect the workspace")
        .await
        .expect("submit turn prompt");

    let requests = request_log.requests();
    assert_eq!(requests.len(), 2, "expected a follow-up model request");
    let header_metadata = requests[1]
        .header("x-codex-turn-metadata")
        .expect("request header should include turn metadata");
    let request_body = requests[1].body_json();
    let body_metadata = request_body["client_metadata"]["x-codex-turn-metadata"]
        .as_str()
        .expect("request body should include turn metadata");

    for (location, metadata) in [
        ("request header", header_metadata.as_str()),
        ("request body", body_metadata),
    ] {
        assert!(
            !metadata.contains(remote_token),
            "{location} should not expose git remote credentials"
        );
        let metadata: serde_json::Value =
            serde_json::from_str(metadata).expect("turn metadata should be valid JSON");
        let remotes = metadata
            .get("workspaces")
            .and_then(serde_json::Value::as_object)
            .and_then(|workspaces| workspaces.values().next())
            .and_then(|workspace| workspace.get("associated_remote_urls"))
            .expect("turn metadata should include git remote URLs");
        assert_eq!(
            remotes,
            &serde_json::json!({ "origin": "https://example.invalid/openai/codex.git" }),
            "{location} should contain the sanitized remote"
        );
    }
}
