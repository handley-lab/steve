use std::process::Stdio;
use std::time::Duration;

use anyhow::Context;
use anyhow::Result;
use app_test_support::TestAppServer;
use app_test_support::create_final_assistant_message_sse_response;
use app_test_support::to_response;
use app_test_support::write_mock_responses_config_toml_with_chatgpt_base_url;
use codex_app_server_protocol::CapabilityRootLocation;
use codex_app_server_protocol::EnvironmentAddResponse;
use codex_app_server_protocol::EnvironmentInfoResponse;
use codex_app_server_protocol::RequestId;
use codex_app_server_protocol::SelectedCapabilityRoot;
use codex_app_server_protocol::ThreadStartParams;
use codex_app_server_protocol::ThreadStartResponse;
use codex_app_server_protocol::TurnEnvironmentParams;
use codex_app_server_protocol::TurnStartParams;
use codex_app_server_protocol::TurnStartResponse;
use codex_app_server_protocol::UserInput;
use codex_exec_server::LOCAL_ENVIRONMENT_ID;
use codex_utils_absolute_path::AbsolutePathBuf;
use codex_utils_path_uri::PathUri;
use core_test_support::process::wait_for_pid_file;
use core_test_support::responses;
use core_test_support::responses::ResponsesRequest;
use core_test_support::stdio_server_bin;
use pretty_assertions::assert_eq;
use serde_json::json;
use tempfile::TempDir;
use test_case::test_case;
use tokio::io::AsyncBufReadExt;
use tokio::io::BufReader;
use tokio::process::Child;
use tokio::process::Command;
use tokio::time::timeout;

use super::analytics::mount_analytics_capture;
use super::analytics::wait_for_matching_analytics_event;

const READ_TIMEOUT: Duration = Duration::from_secs(20);
const EXECUTOR_ID: &str = "executor-1";
const EXECUTOR_ENV_NAME: &str = "MCP_EXECUTOR_MARKER";
const EXECUTOR_ENV_VALUE: &str = "executor-only";
const PLUGIN_ID: &str = "executor-demo@1";
const PLUGIN_DISPLAY_NAME: &str = "Executor Demo";
const SKILL_NAME: &str = "executor-demo:deploy";
const SKILL_DESCRIPTION: &str = "Deploy through the selected executor.";
const SKILL_BODY_MARKER: &str = "SELECTED_EXECUTOR_SKILL_BODY";
const LOCAL_SKILL_BODY_MARKER: &str = "COLLIDING_LOCAL_SKILL_BODY";
const NO_SELECTED_SKILLS_MESSAGE: &str = "No selected-environment skills are currently available.";
const RESTORED_SELECTED_SKILLS_MESSAGE: &str =
    "The previously listed selected-environment skills are available again.";
const MCP_SERVER_NAME: &str = "executor_probe";
const CONNECTOR_ID: &str = "calendar";

#[derive(Clone, Copy)]
enum PluginMention {
    Unmentioned,
    Link,
    Structured,
}

#[test_case(PluginMention::Unmentioned, false; "optional direct")]
#[test_case(PluginMention::Unmentioned, true; "optional batched")]
#[test_case(PluginMention::Link, false; "plugin link direct")]
#[test_case(PluginMention::Link, true; "plugin link batched")]
#[test_case(PluginMention::Structured, false; "structured mention direct")]
#[test_case(PluginMention::Structured, true; "structured mention batched")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn selected_plugin_mcp_startup_respects_explicit_mentions(
    mention: PluginMention,
    executor_capability_discovery: bool,
) -> Result<()> {
    let explicitly_mentioned = !matches!(mention, PluginMention::Unmentioned);
    let responses_server = responses::start_mock_server().await;
    let fixture = selected_capability_fixture(&responses_server.uri(), &responses_server.uri())?;
    mount_analytics_capture(&responses_server, fixture.codex_home.path()).await?;
    let config_path = fixture.codex_home.path().join("config.toml");
    let config = std::fs::read_to_string(&config_path)?.replace(
        "executor_capability_discovery = true",
        &format!("executor_capability_discovery = {executor_capability_discovery}"),
    );
    std::fs::write(config_path, config)?;
    let initialize_barrier = fixture.block_mcp_startup()?;
    let response_mock = responses::mount_sse_once(
        &responses_server,
        create_final_assistant_message_sse_response("Done")?,
    )
    .await;
    let mut app_server = TestAppServer::builder()
        .with_codex_home(fixture.codex_home.path())
        // This fixture owns environments.toml and selects its environments explicitly.
        .without_auto_env()
        .build()
        .await?;
    timeout(READ_TIMEOUT, app_server.initialize()).await??;
    let thread_id = start_thread(
        &mut app_server,
        fixture.selected_root,
        fixture.environment_cwd.clone(),
    )
    .await?;
    let mut exec_server =
        spawn_exec_server(fixture.codex_home.path(), &fixture.exec_server_url).await?;
    add_environment(&mut app_server, &fixture.exec_server_url).await?;

    let text_input = |text| UserInput::Text {
        text,
        text_elements: Vec::new(),
    };
    let input = match mention {
        PluginMention::Unmentioned => text_input("Answer without using tools".to_string()),
        PluginMention::Link => {
            text_input(format!("Use [@executor-demo](plugin://{PLUGIN_ID}) now"))
        }
        PluginMention::Structured => UserInput::Mention {
            name: PLUGIN_DISPLAY_NAME.to_string(),
            path: format!("plugin://{PLUGIN_ID}?app=com.example.editor"),
        },
    };
    let request_id = app_server
        .send_turn_start_request(TurnStartParams {
            thread_id: thread_id.clone(),
            input: vec![input],
            environments: Some(vec![
                TurnEnvironmentParams {
                    environment_id: LOCAL_ENVIRONMENT_ID.to_string(),
                    cwd: fixture.environment_cwd.clone().into(),
                    runtime_workspace_roots: None,
                },
                TurnEnvironmentParams {
                    environment_id: EXECUTOR_ID.to_string(),
                    cwd: fixture.environment_cwd.into(),
                    runtime_workspace_roots: None,
                },
            ]),
            ..Default::default()
        })
        .await?;
    let response = timeout(
        READ_TIMEOUT,
        app_server.read_stream_until_response_message(RequestId::Integer(request_id)),
    )
    .await??;
    let TurnStartResponse { turn } = to_response(response)?;
    wait_for_pid_file(&fixture.pid_file).await?;
    if explicitly_mentioned {
        // An explicit mention must outwait the optional one-second grace.
        tokio::time::sleep(Duration::from_secs(3)).await;
        assert!(response_mock.requests().is_empty());
        std::fs::write(&initialize_barrier, "ready")?;
    }
    let turn_timeout = if explicitly_mentioned {
        READ_TIMEOUT
    } else {
        Duration::from_secs(5)
    };
    timeout(
        turn_timeout,
        app_server.read_stream_until_notification_message("turn/completed"),
    )
    .await??;
    let request = response_mock.single_request();
    assert_selected_skill_catalog_available(&request);
    assert_eq!(
        request
            .tool_by_name(&format!("mcp__{MCP_SERVER_NAME}"), "echo")
            .is_some(),
        explicitly_mentioned,
    );

    let event = wait_for_matching_analytics_event(&responses_server, READ_TIMEOUT, |event| {
        event["event_type"] == "codex_turn_event"
            && event["event_params"]["thread_id"] == thread_id
            && event["event_params"]["turn_id"] == turn.id
    })
    .await?;
    assert_eq!(
        event["event_params"]["active_plugin_ids_at_turn_start"],
        json!([PLUGIN_ID])
    );

    exec_server.kill().await?;
    Ok(())
}

struct SelectedCapabilityFixture {
    codex_home: TempDir,
    _plugin: TempDir,
    pid_file: std::path::PathBuf,
    exec_server_url: String,
    selected_root: SelectedCapabilityRoot,
    environment_cwd: AbsolutePathBuf,
}

impl SelectedCapabilityFixture {
    fn block_mcp_startup(&self) -> Result<std::path::PathBuf> {
        let initialize_barrier = self._plugin.path().join("allow-mcp-initialize");
        let mcp_config_path = self._plugin.path().join(".mcp.json");
        let mut mcp_config: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&mcp_config_path)?)?;
        mcp_config["mcpServers"][MCP_SERVER_NAME]["env"]["MCP_TEST_INITIALIZE_BARRIER_FILE"] =
            json!(initialize_barrier);
        mcp_config["mcpServers"][MCP_SERVER_NAME]["startup_timeout_sec"] = json!(30);
        std::fs::write(mcp_config_path, serde_json::to_vec(&mcp_config)?)?;
        Ok(initialize_barrier)
    }
}

fn selected_capability_fixture(
    responses_server_uri: &str,
    apps_url: &str,
) -> Result<SelectedCapabilityFixture> {
    let codex_home = TempDir::new()?;
    write_mock_responses_config_toml_with_chatgpt_base_url(
        codex_home.path(),
        responses_server_uri,
        apps_url,
    )?;
    let config_path = codex_home.path().join("config.toml");
    let config = std::fs::read_to_string(&config_path)?.replacen(
        "model_provider = \"mock_provider\"",
        "mcp_oauth_credentials_store = \"file\"\nmodel_provider = \"mock_provider\"",
        1,
    );
    // These scenarios attach executors between turns, so each turn waits for attachment resolution.
    std::fs::write(
        config_path,
        format!(
            "{config}\n[features]\napps = true\nexecutor_capability_discovery = true\n\n[skills]\ninclude_instructions = true\n"
        ),
    )?;

    // Reserve the URL before app-server starts. The configured environment initially fails to
    // connect, then environment/add points the same stable ID at the same URL once it is live.
    let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
    let exec_server_url = format!("ws://{}", listener.local_addr()?);
    drop(listener);
    std::fs::write(
        codex_home.path().join("environments.toml"),
        format!(
            "default = \"{EXECUTOR_ID}\"\ninclude_local = true\n\n[[environments]]\nid = \"{EXECUTOR_ID}\"\nurl = \"{exec_server_url}\"\nconnect_timeout_sec = 0.05\n"
        ),
    )?;

    let local_skill_dir = codex_home.path().join("skills/local-deploy");
    std::fs::create_dir_all(&local_skill_dir)?;
    std::fs::write(
        local_skill_dir.join("SKILL.md"),
        format!(
            "---\nname: {SKILL_NAME}\ndescription: Colliding local skill.\n---\n\n{LOCAL_SKILL_BODY_MARKER}\n"
        ),
    )?;

    let plugin = TempDir::new()?;
    let manifest_dir = plugin.path().join(".codex-plugin");
    let skill_dir = plugin.path().join("skills/deploy");
    let pid_file = plugin.path().join("executor-mcp.pid");
    std::fs::create_dir_all(&manifest_dir)?;
    std::fs::create_dir_all(&skill_dir)?;
    std::fs::write(
        manifest_dir.join("plugin.json"),
        r#"{"name":"executor-demo","apps":"./.app.json","interface":{"displayName":"Executor Demo"}}"#,
    )?;
    std::fs::write(
        skill_dir.join("SKILL.md"),
        format!(
            "---\nname: deploy\ndescription: {SKILL_DESCRIPTION}\n---\n\n{SKILL_BODY_MARKER}\n"
        ),
    )?;
    std::fs::write(
        plugin.path().join(".app.json"),
        format!(r#"{{"apps":{{"calendar":{{"id":"{CONNECTOR_ID}"}}}}}}"#),
    )?;
    std::fs::write(
        plugin.path().join(".mcp.json"),
        serde_json::to_vec_pretty(&json!({
            "mcpServers": {
                (MCP_SERVER_NAME): {
                    "command": stdio_server_bin()?,
                    "env": {
                        "MCP_TEST_PID_FILE": pid_file.to_string_lossy(),
                    },
                    "env_vars": [EXECUTOR_ENV_NAME],
                    "startup_timeout_sec": 10,
                }
            }
        }))?,
    )?;

    let selected_root = SelectedCapabilityRoot {
        id: PLUGIN_ID.to_string(),
        location: CapabilityRootLocation::Environment {
            environment_id: EXECUTOR_ID.to_string(),
            path: PathUri::from_host_native_path(plugin.path())?,
        },
    };
    let environment_cwd = AbsolutePathBuf::try_from(plugin.path().to_path_buf())?;
    Ok(SelectedCapabilityFixture {
        codex_home,
        _plugin: plugin,
        pid_file,
        exec_server_url,
        selected_root,
        environment_cwd,
    })
}

fn assert_selected_skill_catalog_available(request: &ResponsesRequest) {
    let latest_update = latest_selected_skill_update(request)
        .expect("selected skill availability should be model-visible");
    assert!(!latest_update.contains(NO_SELECTED_SKILLS_MESSAGE));
    let catalog_fragment = request
        .message_input_texts("developer")
        .into_iter()
        .rfind(|text| text.contains(SKILL_DESCRIPTION))
        .expect("the full selected skill catalog should remain in history");
    assert!(catalog_fragment.contains("executor package:"));
}

fn latest_selected_skill_update(request: &ResponsesRequest) -> Option<String> {
    request
        .message_input_texts("developer")
        .into_iter()
        .rfind(|text| {
            text.contains(SKILL_DESCRIPTION)
                || text.contains(NO_SELECTED_SKILLS_MESSAGE)
                || text.contains(RESTORED_SELECTED_SKILLS_MESSAGE)
        })
}

async fn start_thread(
    app_server: &mut TestAppServer,
    selected_root: SelectedCapabilityRoot,
    environment_cwd: AbsolutePathBuf,
) -> Result<String> {
    let request_id = app_server
        .send_thread_start_request(ThreadStartParams {
            model: Some("mock-model".to_string()),
            environments: Some(vec![TurnEnvironmentParams {
                environment_id: LOCAL_ENVIRONMENT_ID.to_string(),
                cwd: environment_cwd.into(),
                runtime_workspace_roots: None,
            }]),
            selected_capability_roots: Some(vec![selected_root]),
            ..Default::default()
        })
        .await?;
    let response = timeout(
        READ_TIMEOUT,
        app_server.read_stream_until_response_message(RequestId::Integer(request_id)),
    )
    .await??;
    let ThreadStartResponse { thread, .. } = to_response(response)?;
    Ok(thread.id)
}

async fn add_environment(app_server: &mut TestAppServer, exec_server_url: &str) -> Result<()> {
    let request_id = app_server
        .send_raw_request(
            "environment/add",
            Some(json!({
                "environmentId": EXECUTOR_ID,
                "execServerUrl": exec_server_url,
                "connectTimeoutMs": 10_000,
            })),
        )
        .await?;
    let response = timeout(
        READ_TIMEOUT,
        app_server.read_stream_until_response_message(RequestId::Integer(request_id)),
    )
    .await??;
    let _: EnvironmentAddResponse = to_response(response)?;
    // Wait for executor attachment, independently of plugin MCP startup.
    let request_id = app_server
        .send_raw_request(
            "environment/info",
            Some(json!({ "environmentId": EXECUTOR_ID })),
        )
        .await?;
    let response = timeout(
        READ_TIMEOUT,
        app_server.read_stream_until_response_message(RequestId::Integer(request_id)),
    )
    .await??;
    let _: EnvironmentInfoResponse = to_response(response)?;
    Ok(())
}

async fn spawn_exec_server(codex_home: &std::path::Path, url: &str) -> Result<Child> {
    let mut child = Command::new(codex_utils_cargo_bin::cargo_bin("steve")?)
        .args(["exec-server", "--listen", url])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .kill_on_drop(true)
        .env("STEVE_HOME", codex_home)
        .env(EXECUTOR_ENV_NAME, EXECUTOR_ENV_VALUE)
        .spawn()?;
    let stdout = child
        .stdout
        .take()
        .context("exec-server stdout was not captured")?;
    let mut lines = BufReader::new(stdout).lines();
    loop {
        let line = timeout(READ_TIMEOUT, lines.next_line())
            .await
            .context("timed out waiting for exec-server URL")??
            .context("exec-server exited before printing its URL")?;
        if line.trim() == url {
            return Ok(child);
        }
    }
}
