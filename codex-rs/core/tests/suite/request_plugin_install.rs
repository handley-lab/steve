#![allow(clippy::unwrap_used)]

use anyhow::Context;
use anyhow::Result;
use codex_config::types::ToolSuggestDiscoverable;
use codex_config::types::ToolSuggestDiscoverableType;
use codex_core::NewThread;
use codex_core::StartThreadOptions;
use codex_core::TurnInputRequest;
use codex_core::config::Config;
use codex_core_plugins::startup_sync::curated_plugins_repo_path;
use codex_extension_api::ExtensionFuture;
use codex_extension_api::ExtensionRegistryBuilder;
use codex_extension_api::McpServerContribution;
use codex_extension_api::McpServerContributionContext;
use codex_extension_api::McpServerContributor;
use codex_features::Feature;
use codex_login::CodexAuth;
use codex_models_manager::bundled_models_response;
use codex_protocol::models::PermissionProfile;
use codex_protocol::protocol::AskForApproval;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::Op;
use codex_protocol::protocol::SessionSource;
use codex_protocol::protocol::SubAgentSource;
use codex_protocol::protocol::ThreadSettingsOverrides;
use codex_protocol::user_input::UserInput;
use codex_utils_path_uri::PathUri;
use core_test_support::apps_test_server::AppsTestServer;
use core_test_support::responses::ev_assistant_message;
use core_test_support::responses::ev_completed;
use core_test_support::responses::ev_function_call;
use core_test_support::responses::ev_response_created;
use core_test_support::responses::mount_sse_once;
use core_test_support::responses::mount_sse_sequence;
use core_test_support::responses::sse;
use core_test_support::responses::start_mock_server;
use core_test_support::skip_if_no_network;
use core_test_support::skip_if_wine_exec;
use core_test_support::test_codex::TestCodex;
use core_test_support::test_codex::test_codex;
use core_test_support::test_codex::turn_permission_fields;
use core_test_support::wait_for_event;
use pretty_assertions::assert_eq;
use serde_json::Value;
use serde_json::json;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;
use std::time::Duration;
use tempfile::TempDir;
use test_case::test_case;
use tokio::sync::Notify;
use wiremock::Mock;
use wiremock::MockServer;
use wiremock::ResponseTemplate;
use wiremock::matchers::method;
use wiremock::matchers::path;
use wiremock::matchers::query_param;

use super::rmcp_client::remote_aware_environment_id;
use super::rmcp_client::remote_aware_stdio_server_bin;

const TOOL_SEARCH_TOOL_NAME: &str = "tool_search";
const LIST_AVAILABLE_PLUGINS_TO_INSTALL_TOOL_NAME: &str = "list_available_plugins_to_install";
const REQUEST_PLUGIN_INSTALL_TOOL_NAME: &str = "request_plugin_install";
const DISCOVERABLE_GMAIL_ID: &str = "connector_68df038e0ba48191908c8434991bbac2";
const REMOTE_CALENDAR_PLUGIN_CONFIG_ID: &str = "calendar@openai-curated-remote";
const REMOTE_CALENDAR_PLUGIN_ID: &str = "plugin_calendar";
const STEP_PREPARATION_MCP_SERVER: &str = "step_preparation";

struct StartupMcpBarrier {
    block_next: AtomicBool,
    entered: Notify,
    release: Notify,
}

impl McpServerContributor<Config> for StartupMcpBarrier {
    fn id(&self) -> &'static str {
        "startup_recommendation_barrier"
    }

    fn contribute<'a>(
        &'a self,
        _context: McpServerContributionContext<'a, Config>,
    ) -> ExtensionFuture<'a, Vec<McpServerContribution>> {
        Box::pin(async move {
            if self.block_next.swap(false, Ordering::SeqCst) {
                self.entered.notify_one();
                self.release.notified().await;
            }
            Vec::new()
        })
    }
}

fn tool_names(body: &Value) -> Vec<String> {
    body.get("tools")
        .and_then(Value::as_array)
        .map(|tools| {
            tools
                .iter()
                .filter_map(|tool| {
                    tool.get("name")
                        .or_else(|| tool.get("type"))
                        .and_then(Value::as_str)
                        .map(str::to_string)
                })
                .collect()
        })
        .unwrap_or_default()
}

fn configure_apps_without_search_tool(config: &mut Config, apps_base_url: &str) {
    for feature in [
        Feature::Apps,
        Feature::Plugins,
        Feature::RemotePlugin,
        Feature::ToolSuggest,
    ] {
        config
            .features
            .enable(feature)
            .expect("test config should allow feature update");
    }
    let mut model_catalog = bundled_models_response()
        .unwrap_or_else(|err| panic!("bundled models.json should parse: {err}"));
    let model = model_catalog
        .models
        .iter_mut()
        .find(|model| model.slug == "gpt-5.5")
        .expect("gpt-5.5 exists in bundled models.json");
    config.chatgpt_base_url = apps_base_url.to_string();
    config.model = Some("gpt-5.5".to_string());
    config.tool_suggest.discoverables = vec![ToolSuggestDiscoverable {
        kind: ToolSuggestDiscoverableType::Connector,
        id: DISCOVERABLE_GMAIL_ID.to_string(),
    }];
    model.supports_search_tool = false;
    config.model_catalog = Some(model_catalog);
}

async fn mount_recommendations(server: &wiremock::MockServer, response: ResponseTemplate) {
    Mock::given(method("GET"))
        .and(path("/ps/plugins/suggested/codex"))
        .and(query_param("scope", "GLOBAL"))
        .respond_with(response)
        .mount(server)
        .await;
}

fn assert_legacy_tools(body: &Value) {
    let tools = tool_names(body);
    assert!(!tools.iter().any(|name| name == TOOL_SEARCH_TOOL_NAME));
    assert!(
        tools
            .iter()
            .any(|name| name == LIST_AVAILABLE_PLUGINS_TO_INSTALL_TOOL_NAME),
        "legacy mode should expose {LIST_AVAILABLE_PLUGINS_TO_INSTALL_TOOL_NAME}: {tools:?}"
    );
    assert!(
        tools
            .iter()
            .any(|name| name == REQUEST_PLUGIN_INSTALL_TOOL_NAME),
        "legacy mode should expose {REQUEST_PLUGIN_INSTALL_TOOL_NAME}: {tools:?}"
    );
}

async fn build_test(
    server: &wiremock::MockServer,
    apps_server: &AppsTestServer,
) -> Result<TestCodex> {
    let mut builder = test_codex()
        .with_auth(CodexAuth::create_dummy_chatgpt_auth_for_testing())
        .with_config({
            let apps_base_url = apps_server.chatgpt_base_url.clone();
            move |config| {
                config
                    .permissions
                    .set_permission_profile(PermissionProfile::Disabled)
                    .expect("test config should allow disabled permissions");
                configure_apps_without_search_tool(config, apps_base_url.as_str());
            }
        });
    builder.build_with_auto_env(server).await
}

async fn build_gated_step_preparation_test(
    server: &MockServer,
    apps_server: &AppsTestServer,
) -> Result<TestCodex> {
    let command = remote_aware_stdio_server_bin()?;
    let environment_id = remote_aware_environment_id();
    let apps_base_url = apps_server.chatgpt_base_url.clone();
    let mut builder = test_codex()
        .with_auth(CodexAuth::create_dummy_chatgpt_auth_for_testing())
        .with_config(move |config| {
            config
                .permissions
                .set_permission_profile(PermissionProfile::Disabled)
                .expect("test config should allow disabled permissions");
            configure_apps_without_search_tool(config, apps_base_url.as_str());

            let barrier_file = config.cwd.join("allow-step-preparation-initialize");
            let pid_file = config.cwd.join("step-preparation.pid");
            let mut servers = config.mcp_servers.get().clone();
            servers.insert(
                STEP_PREPARATION_MCP_SERVER.to_string(),
                serde_json::from_value(json!({
                    "command": command.clone(),
                    "environment_id": environment_id.clone(),
                    "cwd": config.cwd,
                    "env": {
                        "MCP_TEST_INITIALIZE_BARRIER_FILE": barrier_file,
                        "MCP_TEST_PID_FILE": pid_file,
                    },
                    "enabled_tools": ["echo"],
                    "startup_timeout_sec": 10,
                }))
                .expect("test MCP server configuration"),
            );
            config
                .mcp_servers
                .set(servers)
                .expect("test config should allow MCP servers");
        });
    builder.build_with_auto_env(server).await
}

async fn start_gated_step_preparation(test: &TestCodex, server: &MockServer) -> Result<PathUri> {
    let plugins_manager = test.thread_manager.plugins_manager();
    let auth = test.thread_manager.auth_manager().auth().await;
    assert_eq!(
        plugins_manager
            .recommended_plugins_mode_for_config(&test.config.plugins_config_input(), auth.as_ref())
            .await,
        codex_core_plugins::RecommendedPluginsMode::Legacy
    );
    let (sandbox_policy, permission_profile) =
        turn_permission_fields(PermissionProfile::Disabled, test.config.cwd.as_path());
    test.codex
        .start_or_steer_turn(
            TurnInputRequest::user_input(vec![UserInput::Text {
                text: "prepare MCP and plugin recommendations".to_string(),
                text_elements: Vec::new(),
            }])
            .with_thread_settings(ThreadSettingsOverrides {
                approval_policy: Some(AskForApproval::Never),
                sandbox_policy: Some(sandbox_policy),
                permission_profile,
                ..Default::default()
            }),
        )
        .await?;

    let fs = test.fs();
    let pid_file = PathUri::from_host_native_path(test.config.cwd.join("step-preparation.pid"))?;
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let mcp_started = fs
                .read_file_text(&pid_file, Default::default(), /*sandbox*/ None)
                .await
                .is_ok_and(|pid| !pid.trim().is_empty());
            if mcp_started {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .context("MCP startup should begin before MCP is released")?;

    PathUri::from_host_native_path(test.config.cwd.join("allow-step-preparation-initialize"))
        .map_err(Into::into)
}

#[test_case(Feature::ToolSuggest, None; "tool suggest")]
#[test_case(Feature::RecommendedPlugins, None; "recommended plugins")]
#[test_case(Feature::RecommendedPlugins, Some(Feature::Apps); "apps disabled")]
#[test_case(Feature::RecommendedPlugins, Some(Feature::Plugins); "plugins disabled")]
#[test_case(Feature::RecommendedPlugins, Some(Feature::RemotePlugin); "remote plugins disabled")]
#[test_case(Feature::RecommendedPlugins, Some(Feature::RecommendedPlugins); "recommendations disabled")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn startup_recommendations_use_developer_message(
    feature: Feature,
    disabled_feature: Option<Feature>,
) -> Result<()> {
    skip_if_no_network!(Ok(()));

    let expose_legacy_install_tool = feature == Feature::ToolSuggest && disabled_feature.is_none();
    let server = start_mock_server().await;
    let apps_server = AppsTestServer::mount(&server).await?;
    Mock::given(method("GET"))
        .and(path("/ps/plugins/suggested/codex"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "enabled": true,
            "plugins": [{
                "id": "plugin_github",
                "name": "github",
                "display_name": "GitHub"
            }]
        })))
        .expect(0)
        .mount(&server)
        .await;
    let model_response = mount_sse_once(
        &server,
        sse(vec![
            ev_response_created("startup-recommendations"),
            ev_assistant_message("startup-recommendations-message", "done"),
            ev_completed("startup-recommendations"),
        ]),
    )
    .await;
    let barrier = Arc::new(StartupMcpBarrier {
        block_next: AtomicBool::new(true),
        entered: Notify::new(),
        release: Notify::new(),
    });
    let mut extensions = ExtensionRegistryBuilder::<Config>::new();
    extensions.mcp_server_contributor(barrier.clone());
    let mut builder = test_codex()
        .with_extensions(Arc::new(extensions.build()))
        .with_auth(CodexAuth::create_dummy_chatgpt_auth_for_testing())
        .with_config(move |config| {
            configure_apps_without_search_tool(config, &apps_server.chatgpt_base_url);
            config.model_provider.supports_websockets = false;
            for suggestion_feature in [Feature::ToolSuggest, Feature::RecommendedPlugins] {
                config
                    .features
                    .disable(suggestion_feature)
                    .expect("test config should allow feature update");
            }
            config
                .features
                .enable(feature)
                .expect("test config should allow feature update");
            if let Some(disabled_feature) = disabled_feature {
                config
                    .features
                    .disable(disabled_feature)
                    .expect("test config should allow feature update");
            }
        });
    let startup = builder.build_with_auto_env(&server);
    tokio::pin!(startup);

    tokio::select! {
        result = &mut startup => {
            result?;
            anyhow::bail!("session startup should remain blocked in MCP configuration");
        }
        _ = barrier.entered.notified() => {}
    }
    assert!(futures::poll!(&mut startup).is_pending());
    assert!(model_response.requests().is_empty());
    barrier.release.notify_one();
    let test = startup.await?;

    test.submit_turn("suggest a plugin").await?;

    let request = model_response.single_request();
    let developer_context = request.message_input_texts("developer").join("\n");
    assert!(!developer_context.contains("<recommended_plugins>"));
    assert!(!developer_context.contains("- GitHub (github@openai-curated-remote)"));
    let tools = tool_names(&request.body_json());
    assert_eq!(
        tools
            .iter()
            .any(|name| name == REQUEST_PLUGIN_INSTALL_TOOL_NAME),
        expose_legacy_install_tool,
    );
    test.codex.shutdown_and_wait().await?;
    server.verify().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mcp_discovery_overlaps_endpoint_plugin_recommendations() -> Result<()> {
    skip_if_wine_exec!(
        Ok(()),
        "requires a Windows test_stdio_server in the Wine-exec environment"
    );
    skip_if_no_network!(Ok(()));

    let server = start_mock_server().await;
    let apps_server = AppsTestServer::mount(&server).await?;
    mount_recommendations(
        &server,
        ResponseTemplate::new(200).set_body_json(json!({
            "enabled": true,
            "plugins": [{
                "id": "plugin_github",
                "name": "github",
                "display_name": "GitHub"
            }]
        })),
    )
    .await;
    let response = mount_sse_once(
        &server,
        sse(vec![
            ev_response_created("concurrent-endpoint"),
            ev_assistant_message("endpoint-message", "done"),
            ev_completed("concurrent-endpoint"),
        ]),
    )
    .await;
    let test = build_gated_step_preparation_test(&server, &apps_server).await?;

    let barrier = start_gated_step_preparation(&test, &server).await?;
    assert!(
        response.requests().is_empty(),
        "sampling should wait for the complete MCP catalog"
    );
    test.fs()
        .write_file(
            &barrier,
            b"ready".to_vec(),
            Default::default(),
            /*sandbox*/ None,
        )
        .await?;
    wait_for_event(&test.codex, |event| {
        matches!(event, EventMsg::TurnComplete(_))
    })
    .await;

    let request = response.single_request();
    assert!(
        !request
            .message_input_texts("developer")
            .join("\n")
            .contains("github@openai-curated-remote"),
        "the completed request should not include suggested plugins"
    );
    assert!(
        request
            .tool_by_name("mcp__step_preparation", "echo")
            .is_some(),
        "the completed request should expose the live gated MCP tool"
    );

    test.codex.shutdown_and_wait().await?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn interrupting_concurrent_step_preparation_prevents_sampling() -> Result<()> {
    skip_if_wine_exec!(
        Ok(()),
        "requires a Windows test_stdio_server in the Wine-exec environment"
    );
    skip_if_no_network!(Ok(()));

    let server = start_mock_server().await;
    let apps_server = AppsTestServer::mount(&server).await?;
    mount_recommendations(
        &server,
        ResponseTemplate::new(200).set_body_json(json!({"enabled": true, "plugins": []})),
    )
    .await;
    let response = mount_sse_once(
        &server,
        sse(vec![
            ev_response_created("cancelled-step-preparation"),
            ev_assistant_message("cancelled-message", "unexpected"),
            ev_completed("cancelled-step-preparation"),
        ]),
    )
    .await;
    let test = build_gated_step_preparation_test(&server, &apps_server).await?;

    let barrier = start_gated_step_preparation(&test, &server).await?;
    assert!(response.requests().is_empty());
    test.codex.submit(Op::Interrupt).await?;
    wait_for_event(&test.codex, |event| {
        matches!(event, EventMsg::TurnAborted(_))
    })
    .await;
    assert!(
        response.requests().is_empty(),
        "cancelling concurrent step preparation must prevent model sampling"
    );

    test.fs()
        .write_file(
            &barrier,
            b"ready".to_vec(),
            Default::default(),
            /*sandbox*/ None,
        )
        .await?;
    test.codex.shutdown_and_wait().await?;
    assert!(
        response.requests().is_empty(),
        "releasing the cancelled MCP startup must not revive the aborted turn"
    );
    Ok(())
}

#[test_case(ResponseTemplate::new(200).set_body_json(json!({"enabled": false, "plugins": []})); "endpoint disabled")]
#[test_case(ResponseTemplate::new(503); "fetch failure")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unavailable_recommendations_preserve_legacy_workflow(
    response: ResponseTemplate,
) -> Result<()> {
    skip_if_no_network!(Ok(()));

    let server = start_mock_server().await;
    let apps_server = AppsTestServer::mount(&server).await?;
    mount_recommendations(&server, response).await;
    let call_id = "list-installable-tools";
    let mock = mount_sse_sequence(
        &server,
        vec![
            sse(vec![
                ev_response_created("resp-1"),
                ev_function_call(call_id, LIST_AVAILABLE_PLUGINS_TO_INSTALL_TOOL_NAME, "{}"),
                ev_completed("resp-1"),
            ]),
            sse(vec![
                ev_response_created("resp-2"),
                ev_assistant_message("msg-1", "done"),
                ev_completed("resp-2"),
            ]),
        ],
    )
    .await;
    let test = build_test(&server, &apps_server).await?;
    test.submit_turn_with_approval_and_permission_profile(
        "list tools",
        AskForApproval::Never,
        PermissionProfile::Disabled,
    )
    .await?;

    let requests = mock.requests();
    assert_eq!(requests.len(), 2);
    let request = &requests[0];
    assert!(
        !request
            .message_input_texts("developer")
            .join("\n")
            .contains("<recommended_plugins>")
    );
    assert_legacy_tools(&request.body_json());
    let output = requests[1]
        .function_call_output_text(call_id)
        .expect("list tool output");
    let output: Value = serde_json::from_str(&output)?;
    assert!(output["tools"].as_array().is_some_and(|tools| {
        tools
            .iter()
            .any(|tool| tool["id"] == DISCOVERABLE_GMAIL_ID && tool["tool_type"] == "connector")
    }));
    Ok(())
}

#[test_case(false; "legacy connector")]
#[test_case(true; "recommended plugin")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn subagent_install_request_returns_root_only_error(
    recommended_plugins_enabled: bool,
) -> Result<()> {
    skip_if_no_network!(Ok(()));

    let server = start_mock_server().await;
    let apps_server = AppsTestServer::mount(&server).await?;
    let (recommendations, arguments) = if recommended_plugins_enabled {
        (
            json!({
                "enabled": true,
                "plugins": [{
                    "id": REMOTE_CALENDAR_PLUGIN_ID,
                    "name": "calendar",
                    "display_name": "Calendar"
                }]
            }),
            json!({
                "plugin_id": REMOTE_CALENDAR_PLUGIN_CONFIG_ID,
                "suggest_reason": "Use Calendar for this task"
            }),
        )
    } else {
        (
            json!({"enabled": false, "plugins": []}),
            json!({
                "tool_type": "connector",
                "action_type": "install",
                "tool_id": DISCOVERABLE_GMAIL_ID,
                "suggest_reason": "Use Gmail for this task"
            }),
        )
    };
    mount_recommendations(
        &server,
        ResponseTemplate::new(200).set_body_json(recommendations),
    )
    .await;
    let call_id = "subagent-plugin-install";
    let mock = mount_sse_sequence(
        &server,
        vec![
            sse(vec![
                ev_response_created("resp-1"),
                ev_function_call(
                    call_id,
                    REQUEST_PLUGIN_INSTALL_TOOL_NAME,
                    &arguments.to_string(),
                ),
                ev_completed("resp-1"),
            ]),
            sse(vec![
                ev_response_created("resp-2"),
                ev_assistant_message("msg-1", "done"),
                ev_completed("resp-2"),
            ]),
        ],
    )
    .await;
    let test = build_test(&server, &apps_server).await?;
    let NewThread {
        thread: subagent, ..
    } = test
        .thread_manager
        .start_thread(StartThreadOptions {
            session_source: Some(SessionSource::SubAgent(SubAgentSource::ThreadSpawn {
                parent_thread_id: test.session_configured.thread_id,
                depth: 1,
                agent_path: None,
                agent_nickname: None,
                agent_role: None,
            })),
            ..StartThreadOptions::new(test.config.clone())
        })
        .await?;
    subagent
        .start_or_steer_turn(
            TurnInputRequest::user_input(vec![UserInput::Text {
                text: "Use the requested integration.".to_string(),
                text_elements: Vec::new(),
            }])
            .with_thread_settings(ThreadSettingsOverrides {
                approval_policy: Some(AskForApproval::Never),
                permission_profile: Some(PermissionProfile::Disabled),
                ..Default::default()
            }),
        )
        .await?;
    wait_for_event(&subagent, |event| {
        assert!(
            !matches!(event, EventMsg::ElicitationRequest(_)),
            "subagents must not request plugin installation"
        );
        matches!(event, EventMsg::TurnComplete(_))
    })
    .await;

    let requests = mock.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(
        requests[1].function_call_output_text(call_id).as_deref(),
        Some("request_plugin_install can only be used by the root thread")
    );
    subagent.shutdown_and_wait().await?;
    test.codex.shutdown_and_wait().await?;
    Ok(())
}

#[test_case(true; "enabled plugin skill")]
#[test_case(false; "disabled plugin skill")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn local_plugin_skill_availability_reaches_tool_suggestion_candidates(
    skill_enabled: bool,
) -> Result<()> {
    skip_if_no_network!(Ok(()));

    let server = start_mock_server().await;
    let apps_server = AppsTestServer::mount(&server).await?;
    let codex_home = Arc::new(TempDir::new()?);
    let curated_root = curated_plugins_repo_path(codex_home.path());
    let plugin_root = curated_root.join("plugins/sample");
    std::fs::create_dir_all(curated_root.join(".agents/plugins"))?;
    std::fs::create_dir_all(plugin_root.join(".codex-plugin"))?;
    std::fs::create_dir_all(plugin_root.join("skills/search"))?;
    std::fs::write(
        curated_root.join(".agents/plugins/marketplace.json"),
        r#"{
  "name": "openai-curated",
  "plugins": [{
    "name": "sample",
    "source": {"source": "local", "path": "./plugins/sample"}
  }]
}"#,
    )?;
    std::fs::write(
        plugin_root.join(".codex-plugin/plugin.json"),
        r#"{"name":"sample","description":"Search sample data"}"#,
    )?;
    std::fs::write(
        plugin_root.join("skills/search/SKILL.md"),
        "---\nname: search\ndescription: Search sample data\n---\n",
    )?;
    std::fs::write(
        codex_home.path().join("config.toml"),
        format!(
            "[features]\nplugins = true\n\n[[skills.config]]\nname = \"sample:search\"\nenabled = {skill_enabled}\n"
        ),
    )?;

    let call_id = "list-skill-backed-plugin-candidates";
    let mock = mount_sse_sequence(
        &server,
        vec![
            sse(vec![
                ev_response_created("resp-1"),
                ev_function_call(call_id, LIST_AVAILABLE_PLUGINS_TO_INSTALL_TOOL_NAME, "{}"),
                ev_completed("resp-1"),
            ]),
            sse(vec![
                ev_response_created("resp-2"),
                ev_assistant_message("msg-1", "done"),
                ev_completed("resp-2"),
            ]),
        ],
    )
    .await;
    let mut builder = test_codex()
        .with_home(codex_home)
        .with_auth(CodexAuth::create_dummy_chatgpt_auth_for_testing())
        .with_config({
            let apps_base_url = apps_server.chatgpt_base_url.clone();
            move |config| {
                config
                    .permissions
                    .set_permission_profile(PermissionProfile::Disabled)
                    .expect("test config should allow disabled permissions");
                configure_apps_without_search_tool(config, apps_base_url.as_str());
                config
                    .features
                    .disable(Feature::RemotePlugin)
                    .expect("test config should allow local plugin suggestions");
                config.tool_suggest.discoverables = vec![ToolSuggestDiscoverable {
                    kind: ToolSuggestDiscoverableType::Plugin,
                    id: "sample@openai-curated".to_string(),
                }];
            }
        });
    let test = builder.build_with_auto_env(&server).await?;
    test.submit_turn_with_approval_and_permission_profile(
        "list available plugins",
        AskForApproval::Never,
        PermissionProfile::Disabled,
    )
    .await?;

    let requests = mock.requests();
    assert_eq!(requests.len(), 2);
    assert_legacy_tools(&requests[0].body_json());
    let output: Value = serde_json::from_str(
        &requests[1]
            .function_call_output_text(call_id)
            .expect("list tool output"),
    )?;
    assert_eq!(
        output,
        json!({
            "tools": [{
                "id": "sample@openai-curated",
                "name": "sample",
                "description": "Search sample data",
                "tool_type": "plugin",
                "has_skills": skill_enabled,
                "mcp_server_names": [],
                "app_connector_ids": []
            }]
        })
    );
    Ok(())
}
