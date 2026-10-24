#![cfg(unix)]

use anyhow::Result;
use app_test_support::TestAppServer;
use app_test_support::create_command_execution_sse_response;
use app_test_support::create_final_assistant_message_sse_response;
use codex_app_server_protocol::AskForApproval;
use codex_app_server_protocol::ClientRequest;
use codex_app_server_protocol::RequestId;
use codex_app_server_protocol::SandboxMode;
use codex_app_server_protocol::ThreadResumeParams;
use codex_app_server_protocol::ThreadResumeResponse;
use codex_app_server_protocol::ThreadSettingsUpdateParams;
use codex_app_server_protocol::ThreadSettingsUpdateResponse;
use codex_app_server_protocol::ThreadStartParams;
use codex_app_server_protocol::TurnStartParams;
use codex_app_server_protocol::TurnStartResponse;
use codex_app_server_protocol::UserInput;
use serde_json::Value;
use serde_json::json;
use std::io::Read;
use std::sync::Arc;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;
use std::time::Duration;
use tempfile::TempDir;
use tokio::sync::Notify;
use wiremock::Mock;
use wiremock::MockServer;
use wiremock::Request;
use wiremock::ResponseTemplate;
use wiremock::matchers::method;
use wiremock::matchers::path;

fn input(text: &str) -> Vec<UserInput> {
    vec![UserInput::Text {
        text: text.to_string(),
        text_elements: Vec::new(),
    }]
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn switch_captures_native_route_tools_and_cold_resume() -> Result<()> {
    let server = MockServer::start().await;
    let home = TempDir::new()?;
    let cwd = TempDir::new()?;
    let mut nonce_bytes = [0u8; 16];
    std::fs::File::open("/dev/urandom")?.read_exact(&mut nonce_bytes)?;
    let nonce: String = nonce_bytes
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    std::fs::write(cwd.path().join("nonce.txt"), &nonce)?;
    for provider in ["cambridge", "turing"] {
        Mock::given(method("GET"))
            .and(path(format!("/{provider}/v1/models")))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"data":[
                {"id":"turing/shared-id"}, {"id":"excluded", "mode":"decisions"}
            ]})))
            .mount(&server)
            .await;
    }
    let started = Arc::new(Notify::new());
    let first_count = Arc::new(AtomicUsize::new(0));
    let compactions = Arc::new(AtomicUsize::new(0));
    for provider in ["cambridge", "turing"] {
        let nonce = nonce.clone();
        let cwd = cwd.path().to_path_buf();
        let started = Arc::clone(&started);
        let first_count = Arc::clone(&first_count);
        let compactions = Arc::clone(&compactions);
        let count = AtomicUsize::new(0);
        Mock::given(method("POST"))
            .and(path(format!("/{provider}/v1/responses")))
            .respond_with(move |request: &Request| {
                let body: Value = serde_json::from_slice(&request.body).unwrap();
                assert_eq!(body["model"], "turing/shared-id");
                assert_eq!(body["store"], false);
                if provider == "cambridge" {
                    assert_eq!(request.headers["authorization"], "Bearer route-secret");
                    first_count.fetch_add(1, Ordering::SeqCst);
                } else {
                    assert!(!request.headers.contains_key("authorization"));
                }
                if body["input"].as_array().unwrap().iter()
                    .any(|item| item["type"] == "compaction_trigger") {
                    assert_eq!(provider, "cambridge", "previous model compacts on its own provider");
                    compactions.fetch_add(1, Ordering::SeqCst);
                    let events = [
                        json!({"type":"response.output_item.done", "item": {
                            "type":"compaction", "encrypted_content":"fixture-summary"
                        }}),
                        json!({"type":"response.completed", "response":{"id":"compact"}}),
                    ];
                    let response: String = events.iter()
                        .map(|event| format!("data: {event}\n\n")).collect();
                    return ResponseTemplate::new(200)
                        .insert_header("content-type", "text/event-stream")
                        .set_body_string(response);
                }
                let ordinal = count.fetch_add(1, Ordering::SeqCst);
                let response = if ordinal == 0 {
                    if provider == "cambridge" {
                        assert!(!String::from_utf8_lossy(&request.body).contains(&nonce));
                    }
                    create_command_execution_sse_response(
                        vec!["cat".to_string(), "nonce.txt".to_string()],
                        Some(&cwd),
                        Some(1000),
                        &format!("{provider}-read"),
                    )
                    .unwrap()
                } else {
                    let outputs: Vec<_> = body["input"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .filter(|item| item["type"] == "function_call_output")
                        .collect();
                    assert!(
                        outputs
                            .iter()
                            .any(|item| item["call_id"] == format!("{provider}-read")
                                && item.to_string().contains(&nonce))
                    );
                    create_final_assistant_message_sse_response(&format!("{provider}: {nonce}"))
                        .unwrap()
                };
                let template = ResponseTemplate::new(200)
                    .insert_header("content-type", "text/event-stream")
                    .set_body_string(response);
                if provider == "cambridge" && ordinal == 0 {
                    started.notify_one();
                    template.set_delay(Duration::from_secs(1))
                } else {
                    template
                }
            })
            .mount(&server)
            .await;
    }
    let metadata = ["cambridge", "turing"].into_iter().map(|provider| {
        let mut model = codex_protocol::openai_models::ModelInfo::from_discovery_id(
            format!("{provider}/turing/shared-id"), None,
        );
        model.comp_hash = Some(provider.to_string());
        model
    }).collect::<Vec<_>>();
    std::fs::write(home.path().join("models.json"),
        serde_json::to_vec(&json!({"models":metadata}))?)?;
    std::fs::write(
        home.path().join("config.toml"),
        format!(
            r#"
model_provider = "cambridge"
model = "turing/shared-id"
model_catalog_json = "{metadata}"
check_for_update_on_startup = false
[analytics]
enabled = false
[feedback]
enabled = false
[model_providers.cambridge]
name = "Cambridge"
base_url = "{base}/cambridge/v1"
env_key = "STEVE_ROUTE_FIXTURE_KEY"
default_model = "turing/shared-id"
[model_providers.cambridge.capabilities]
remote_compaction = "v2"
[model_providers.turing]
name = "Turing"
base_url = "{base}/turing/v1"
default_model = "turing/shared-id"
"#,
            base = server.uri(),
            metadata = home.path().join("models.json").display(),
        ),
    )?;
    let binary = codex_utils_cargo_bin::cargo_bin("steve")?;
    let mut app = TestAppServer::builder()
        .with_program(&binary)
        .with_plugin_startup_tasks()
        .with_args(&["app-server"])
        .with_codex_home(home.path())
        .with_env_overrides(&[("STEVE_ROUTE_FIXTURE_KEY", Some("route-secret"))])
        .build_initialized()
        .await?;
    // The explicit native pair remains native even though its prefix is another provider key.
    let thread = app
        .start_thread(ThreadStartParams {
            model: Some("turing/shared-id".to_string()),
            model_provider: Some("cambridge".to_string()),
            cwd: Some(cwd.path().to_string_lossy().into_owned()),
            approval_policy: Some(AskForApproval::Never),
            sandbox: Some(SandboxMode::DangerFullAccess),
            ..Default::default()
        })
        .await?;
    assert_eq!(thread.model, "cambridge/turing/shared-id");
    assert_eq!(thread.model_provider, "cambridge");
    assert_eq!(thread.thread.model.as_deref(), Some("cambridge/turing/shared-id"));
    let started_notification = app.read_stream_until_notification_message("thread/started").await?;
    assert_eq!(started_notification.params.as_ref().unwrap()["thread"]["model"],
        "cambridge/turing/shared-id");
    let thread_id = thread.thread.id;
    let first = app
        .send_turn_start_request(TurnStartParams {
            thread_id: thread_id.clone(),
            input: input("Read nonce.txt using the shell."),
            ..Default::default()
        })
        .await?;
    let _: TurnStartResponse = app.read_response(first).await?;
    tokio::time::timeout(Duration::from_secs(10), started.notified()).await?;
    let _: ThreadSettingsUpdateResponse = app
        .request(|request_id| ClientRequest::ThreadSettingsUpdate {
            request_id,
            params: ThreadSettingsUpdateParams {
                thread_id: thread_id.clone(),
                model: Some("turing/turing/shared-id".to_string()),
                ..Default::default()
            },
        })
        .await?;
    let updated = app.read_stream_until_matching_notification(
        "selected provider acknowledgement",
        |notification| notification.method == "thread/settings/updated"
            && notification.params.as_ref().is_some_and(|params|
                params["threadSettings"]["modelProvider"] == "turing"),
    ).await?;
    let settings = &updated.params.as_ref().unwrap()["threadSettings"];
    assert_eq!(settings["model"], "turing/turing/shared-id");
    assert_eq!(settings["collaborationMode"]["settings"]["model"], "turing/turing/shared-id");
    assert_eq!(
        first_count.load(Ordering::SeqCst),
        1,
        "provider update must precede the tool round trip"
    );
    let completed = app
        .read_stream_until_notification_message("turn/completed")
        .await?;
    assert_eq!(
        completed.params.as_ref().unwrap()["turn"]["status"],
        "completed"
    );
    let second = app
        .start_turn_and_wait_for_completion(TurnStartParams {
            thread_id: thread_id.clone(),
            input: input("Read nonce.txt again using the shell."),
            ..Default::default()
        })
        .await?;
    assert_eq!(serde_json::to_value(&second.turn)?["status"], "completed");
    app.shutdown_gracefully().await?;
    let mut app = TestAppServer::builder()
        .with_program(&binary)
        .with_plugin_startup_tasks()
        .with_args(&["app-server"])
        .with_codex_home(home.path())
        .with_env_overrides(&[("STEVE_ROUTE_FIXTURE_KEY", Some("route-secret"))])
        .build_initialized()
        .await?;
    let resumed: ThreadResumeResponse = app
        .request(|request_id| ClientRequest::ThreadResume {
            request_id,
            params: ThreadResumeParams {
                thread_id: thread_id.clone(),
                ..Default::default()
            },
        })
        .await?;
    assert_eq!(resumed.thread.id, thread_id);
    assert_eq!(resumed.model_provider, "turing");
    assert_eq!(resumed.model, "turing/turing/shared-id");
    assert_eq!(resumed.thread.model.as_deref(), Some("turing/turing/shared-id"));
    assert_eq!(resumed.thread.model_provider, "turing");
    assert_eq!(resumed.collaboration_mode.as_ref().unwrap().model(), "turing/turing/shared-id");
    let warm: ThreadResumeResponse = app.request(|request_id| ClientRequest::ThreadResume {
        request_id,
        params: ThreadResumeParams { thread_id: thread_id.clone(), ..Default::default() },
    }).await?;
    assert_eq!(warm.model, "turing/turing/shared-id");
    let fork = app.send_thread_fork_request(codex_app_server_protocol::ThreadForkParams {
        thread_id: thread_id.clone(), ..Default::default()
    }).await?;
    let forked: codex_app_server_protocol::ThreadForkResponse = app.read_response(fork).await?;
    assert_ne!(forked.thread.id, thread_id);
    assert_eq!(forked.model, "turing/turing/shared-id");
    assert_eq!(forked.model_provider, "turing");

    let third = app
        .start_turn_and_wait_for_completion(TurnStartParams {
            thread_id: thread_id.clone(),
            input: input("Keep the same route and retained conversation."),
            ..Default::default()
        })
        .await?;
    assert_eq!(serde_json::to_value(&third.turn)?["status"], "completed");
    assert_eq!(compactions.load(Ordering::SeqCst), 1);
    let requests = server.received_requests().await.unwrap();
    let responses: Vec<_> = requests
        .iter()
        .filter(|request| request.method == "POST")
        .collect();
    assert_eq!(
        responses
            .iter()
            .filter(|request| request.url.path().starts_with("/cambridge/"))
            .count(),
        3
    );
    assert_eq!(
        responses
            .iter()
            .filter(|request| request.url.path().starts_with("/turing/"))
            .count(),
        3
    );
    let last: Value = serde_json::from_slice(&responses.last().unwrap().body)?;
    assert!(
        last["input"]
            .to_string()
            .contains("Read nonce.txt using the shell.")
    );
    assert!(
        last["input"]
            .to_string()
            .contains("Read nonce.txt again using the shell.")
    );
    let bad = app
        .send_raw_request(
            "thread/settings/update",
            Some(json!({
                "threadId":thread_id, "model":"turing/excluded"
            })),
        )
        .await?;
    let error = app
        .read_stream_until_error_message(RequestId::Integer(bad))
        .await?;
    assert!(error.error.message.contains("unavailable for coding"));
    app.shutdown_gracefully().await?;
    Ok(())
}
