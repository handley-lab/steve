use anyhow::Result;
use codex_core::config::Config;
use codex_extension_api::ExtensionFuture;
use codex_extension_api::ExtensionRegistryBuilder;
use codex_extension_api::McpServerContribution;
use codex_extension_api::McpServerContributionContext;
use codex_extension_api::McpServerContributor;
use codex_extension_api::ThreadLifecycleContributor;
use codex_extension_api::ThreadStartInput;
use codex_history::RolloutItem;
use codex_mcp::McpResourceClient;
use codex_protocol::protocol::Op;
use codex_protocol::protocol::SessionSource;
use codex_protocol::protocol::SubAgentSource;
use core_test_support::apps_test_server::AppsTestServer;
use core_test_support::responses;
use core_test_support::responses::ResponsesRequest;
use core_test_support::responses::ev_assistant_message;
use core_test_support::responses::ev_completed;
use core_test_support::responses::ev_function_call_with_namespace;
use core_test_support::responses::ev_response_created;
use core_test_support::responses::mount_sse_once_match;
use core_test_support::responses::mount_sse_sequence;
use core_test_support::responses::sse;
use core_test_support::skip_if_no_network;
use pretty_assertions::assert_eq;
use rmcp::model::ReadResourceRequestParams;
use serde::Deserialize;
use serde_json::json;
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;
use std::time::Duration;
use tokio::sync::Semaphore;
use wiremock::Request;

struct McpResourceClientCapture {
    client: Arc<Mutex<Option<McpResourceClient>>>,
}

struct CoalescingMcpContributor {
    block_next: AtomicBool,
    entered: Semaphore,
    release: Semaphore,
    observed_markers: Mutex<Vec<String>>,
}

struct SessionSourceMcpContributor {
    observed_sources: Arc<Mutex<Vec<SessionSource>>>,
}

impl CoalescingMcpContributor {
    fn new() -> Self {
        Self {
            block_next: AtomicBool::new(false),
            entered: Semaphore::new(0),
            release: Semaphore::new(0),
            observed_markers: Mutex::new(Vec::new()),
        }
    }
}

impl McpServerContributor<Config> for CoalescingMcpContributor {
    fn id(&self) -> &'static str {
        "coalescing_mcp_refresh_test"
    }

    fn contribute<'a>(
        &'a self,
        context: McpServerContributionContext<'a, Config>,
    ) -> ExtensionFuture<'a, Vec<McpServerContribution>> {
        Box::pin(async move {
            let marker = context
                .config()
                .mcp_servers
                .get()
                .keys()
                .next()
                .cloned()
                .unwrap_or_else(|| "initial".to_string());
            self.observed_markers
                .lock()
                .expect("observed markers lock should not be poisoned")
                .push(marker.clone());
            if marker != "initial" {
                self.entered.add_permits(1);
            }
            if self.block_next.swap(false, Ordering::SeqCst) {
                self.release
                    .acquire()
                    .await
                    .expect("release semaphore should remain open")
                    .forget();
            }
            Vec::new()
        })
    }
}

impl McpServerContributor<Config> for SessionSourceMcpContributor {
    fn id(&self) -> &'static str {
        "session_source_mcp_test"
    }

    fn contribute<'a>(
        &'a self,
        context: McpServerContributionContext<'a, Config>,
    ) -> ExtensionFuture<'a, Vec<McpServerContribution>> {
        Box::pin(async move {
            self.observed_sources
                .lock()
                .expect("observed sources lock should not be poisoned")
                .push(
                    context
                        .session_source()
                        .expect("thread-scoped MCP resolution should identify its session source")
                        .clone(),
                );
            Vec::new()
        })
    }
}

fn configure_deferred_tool_world_state_without_agents(config: &mut Config) {
    config.update_plan_enabled = true;
    config.agents_enabled = false;
}

fn tools_state_sections(request: &ResponsesRequest) -> Vec<String> {
    request
        .message_input_texts("developer")
        .into_iter()
        .filter(|text| text.starts_with("<tools>"))
        .collect()
}

fn completed_response_sequence(count: usize) -> Vec<String> {
    (1..=count)
        .map(|index| {
            sse(vec![
                ev_response_created(&format!("resp-{index}")),
                ev_assistant_message(&format!("msg-{index}"), "done"),
                ev_completed(&format!("resp-{index}")),
            ])
        })
        .collect()
}

impl ThreadLifecycleContributor<Config> for McpResourceClientCapture {
    fn on_thread_start<'a>(
        &'a self,
        input: ThreadStartInput<'a, Config>,
    ) -> ExtensionFuture<'a, ()> {
        Box::pin(async move {
            let client = input
                .mcp_resource_client
                .as_ref()
                .expect("host should supply an MCP resource client");
            *self
                .client
                .lock()
                .expect("capture lock should not be poisoned") = Some(client.as_ref().clone());
        })
    }
}

fn config_with_mcp_marker(base: &Config, marker: &str) -> Config {
    let mut config = base.clone();
    let server = serde_json::from_value(json!({
        "url": "http://127.0.0.1:1/mcp",
        "enabled": false,
    }))
    .expect("test MCP server config");
    config
        .mcp_servers
        .set(HashMap::from([(marker.to_string(), server)]))
        .expect("test config should allow MCP servers");
    config
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn root_and_spawned_subagent_receive_distinct_mcp_session_sources() -> Result<()> {
    skip_if_no_network!(Ok(()));

    const PARENT_PROMPT: &str = "spawn an agent to verify its MCP session source";
    const CHILD_PROMPT: &str = "child: report that MCP configuration completed";
    const SPAWN_CALL_ID: &str = "mcp-session-source-spawn";

    let server = responses::start_mock_server().await;
    let spawn_args = serde_json::to_string(&json!({ "message": CHILD_PROMPT }))?;
    mount_sse_once_match(
        &server,
        |request: &Request| {
            std::str::from_utf8(&request.body).is_ok_and(|body| body.contains(PARENT_PROMPT))
                && !request.headers.contains_key("x-openai-subagent")
        },
        sse(vec![
            ev_response_created("resp-parent-spawn"),
            ev_function_call_with_namespace(
                SPAWN_CALL_ID,
                "multi_agent_v1",
                "spawn_agent",
                &spawn_args,
            ),
            ev_completed("resp-parent-spawn"),
        ]),
    )
    .await;
    let child_response = mount_sse_once_match(
        &server,
        |request: &Request| {
            std::str::from_utf8(&request.body).is_ok_and(|body| body.contains(CHILD_PROMPT))
                && request
                    .headers
                    .get("x-openai-subagent")
                    .and_then(|value| value.to_str().ok())
                    == Some("collab_spawn")
        },
        sse(vec![
            ev_response_created("resp-child"),
            ev_assistant_message("msg-child", "child done"),
            ev_completed("resp-child"),
        ]),
    )
    .await;
    mount_sse_once_match(
        &server,
        |request: &Request| {
            std::str::from_utf8(&request.body).is_ok_and(|body| body.contains(SPAWN_CALL_ID))
                && !request.headers.contains_key("x-openai-subagent")
        },
        sse(vec![
            ev_response_created("resp-parent-complete"),
            ev_assistant_message("msg-parent", "parent done"),
            ev_completed("resp-parent-complete"),
        ]),
    )
    .await;

    let observed_sources = Arc::new(Mutex::new(Vec::new()));
    let mut extensions = ExtensionRegistryBuilder::<Config>::new();
    extensions.mcp_server_contributor(Arc::new(SessionSourceMcpContributor {
        observed_sources: observed_sources.clone(),
    }));
    let test = core_test_support::test_codex::test_codex()
        .with_extensions(Arc::new(extensions.build()))
        .build(&server)
        .await?;

    test.submit_turn(PARENT_PROMPT).await?;
    tokio::time::timeout(Duration::from_secs(/*secs*/ 10), async {
        while child_response.requests().is_empty() {
            tokio::time::sleep(Duration::from_millis(/*millis*/ 10)).await;
        }
    })
    .await?;

    let observed_sources = observed_sources
        .lock()
        .expect("observed sources lock should not be poisoned");
    assert!(observed_sources.contains(&SessionSource::Exec));
    assert!(observed_sources.iter().any(|source| matches!(
        source,
        SessionSource::SubAgent(SubAgentSource::ThreadSpawn {
            parent_thread_id,
            ..
        }) if *parent_thread_id == test.session_configured.thread_id
    )));

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rapid_mcp_refreshes_coalesce_to_the_latest_config() -> Result<()> {
    let server = responses::start_mock_server().await;
    let response = responses::mount_sse_once(
        &server,
        sse(vec![
            ev_response_created("resp-1"),
            ev_assistant_message("msg-1", "done"),
            ev_completed("resp-1"),
        ]),
    )
    .await;
    let contributor = Arc::new(CoalescingMcpContributor::new());
    let mut extensions = ExtensionRegistryBuilder::<Config>::new();
    extensions.mcp_server_contributor(contributor.clone());
    let test = core_test_support::test_codex::test_codex()
        .with_extensions(Arc::new(extensions.build()))
        .build(&server)
        .await?;

    contributor.block_next.store(true, Ordering::SeqCst);
    let current_config = test.codex.config().await;
    let _ = test
        .codex
        .refresh_runtime_config(
            current_config,
            config_with_mcp_marker(&test.config, "config-a"),
        )
        .await;
    tokio::time::timeout(Duration::from_secs(5), contributor.entered.acquire())
        .await
        .expect("configuration A should enter MCP projection")
        .expect("entered semaphore should remain open")
        .forget();

    let current_config = test.codex.config().await;
    let _ = test
        .codex
        .refresh_runtime_config(
            current_config,
            config_with_mcp_marker(&test.config, "config-b"),
        )
        .await;
    let current_config = test.codex.config().await;
    let _ = test
        .codex
        .refresh_runtime_config(
            current_config,
            config_with_mcp_marker(&test.config, "config-c"),
        )
        .await;
    contributor.release.add_permits(1);

    tokio::time::timeout(Duration::from_secs(5), contributor.entered.acquire())
        .await
        .expect("the coalesced refresh should project the latest configuration")
        .expect("entered semaphore should remain open")
        .forget();
    let observed = contributor
        .observed_markers
        .lock()
        .expect("observed markers lock should not be poisoned")
        .iter()
        .filter(|marker| marker.as_str() != "initial")
        .cloned()
        .collect::<Vec<_>>();
    assert_eq!(
        observed,
        vec!["config-a".to_string(), "config-c".to_string()]
    );

    test.submit_turn("bind the latest MCP state").await?;
    assert!(
        !contributor
            .observed_markers
            .lock()
            .expect("observed markers lock should not be poisoned")
            .iter()
            .any(|marker| marker == "config-b")
    );
    response.single_request();
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn timeout_refresh_replaces_pending_startup_and_reuses_ready_connection() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let server = responses::start_mock_server().await;
    let pending_mock = responses::start_mock_server().await;
    let (pending_server, pending_startup) =
        AppsTestServer::mount_with_startup_control(&pending_mock).await?;
    let release_startup = pending_startup.hold_next_successful_initialize();
    let ready_mock = responses::start_mock_server().await;
    let (ready_server, ready_startup) =
        AppsTestServer::mount_with_startup_control(&ready_mock).await?;
    let test = core_test_support::test_codex::test_codex()
        .with_config(move |config| {
            config
                .mcp_servers
                .set(
                    [("pending", pending_server), ("ready", ready_server)]
                        .into_iter()
                        .map(|(name, server)| {
                            (
                                name.to_string(),
                                serde_json::from_value(json!({
                                    "url": format!("{}/api/codex/ps/mcp", server.chatgpt_base_url),
                                    "startup_timeout_sec": 60,
                                }))
                                .expect("valid MCP config"),
                            )
                        })
                        .collect(),
                )
                .expect("test config should allow MCP servers");
        })
        .build_with_auto_env(&server)
        .await?;
    tokio::time::timeout(Duration::from_secs(5), async {
        while pending_startup.initialize_attempts() == 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("pending startup should begin before refresh");
    let ready_result = test
        .codex
        .call_mcp_tool(
            "ready",
            "calendar_list_events",
            /*arguments*/ None,
            /*meta*/ None,
        )
        .await?;

    let current_config = test.codex.config().await;
    let mut refresh_config = test.config.clone();
    let mut servers = refresh_config.mcp_servers.get().clone();
    for config in servers.values_mut() {
        config.startup_timeout_sec = None;
    }
    refresh_config.mcp_servers.set(servers)?;
    let _ = test
        .codex
        .refresh_mcp_config(current_config, refresh_config)
        .await;
    // Publish without waiting for the held initialize to finish.
    let error = test
        .codex
        .read_mcp_resource("unknown", ReadResourceRequestParams::new("test://resource"))
        .await
        .expect_err("the unknown server should not exist");
    assert_eq!(error.to_string(), "unknown MCP server 'unknown'");
    release_startup
        .send(())
        .expect("the mock initialize should remain held until publication");

    for name in ["pending", "ready"] {
        let result = test
            .codex
            .call_mcp_tool(
                name,
                "calendar_list_events",
                /*arguments*/ None,
                /*meta*/ None,
            )
            .await?;
        assert_eq!(result, ready_result);
    }
    assert_eq!(
        (
            pending_startup.initialize_attempts(),
            ready_startup.initialize_attempts(),
        ),
        (2, 1)
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn out_of_band_resource_read_reconciles_the_published_mcp_runtime() -> Result<()> {
    let server = responses::start_mock_server().await;

    let captured_client = Arc::new(Mutex::new(None));
    let mut extensions = ExtensionRegistryBuilder::<Config>::new();
    extensions.thread_lifecycle_contributor(Arc::new(McpResourceClientCapture {
        client: Arc::clone(&captured_client),
    }));
    let test = core_test_support::test_codex::test_codex()
        .with_extensions(Arc::new(extensions.build()))
        .build(&server)
        .await?;
    let resource_client = captured_client
        .lock()
        .expect("capture lock should not be poisoned")
        .clone()
        .expect("thread start should capture the MCP resource client");
    assert!(!resource_client.has_server("refreshed").await);

    let current_config = test.codex.config().await;
    let mut refresh_config = test.config.clone();
    let user_config_path = refresh_config.codex_home.join("config.toml");
    let user_config: toml::Value = toml::from_str(&format!(
        r#"
[mcp_servers.refreshed]
url = "{}/mcp"
startup_timeout_sec = 0.1
"#,
        server.uri()
    ))?;
    let refreshed_servers = user_config
        .get("mcp_servers")
        .cloned()
        .map(HashMap::<String, codex_config::types::McpServerConfig>::deserialize)
        .transpose()?
        .expect("test config should define MCP servers");
    refresh_config
        .mcp_servers
        .set(refreshed_servers)
        .expect("test config should allow MCP servers");
    refresh_config.config_layer_stack = refresh_config
        .config_layer_stack
        .with_user_config(&user_config_path, user_config)?;
    let _ = test
        .codex
        .refresh_runtime_config(current_config, refresh_config)
        .await;
    test.codex.submit(Op::RefreshMcpServers).await?;

    let _ = test
        .codex
        .read_mcp_resource(
            "refreshed",
            ReadResourceRequestParams::new("test://resource"),
        )
        .await;
    assert!(resource_client.has_server("refreshed").await);
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn initially_empty_deferred_tool_world_state_is_not_rendered_or_persisted() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let server = responses::start_mock_server().await;
    let response = mount_sse_sequence(&server, completed_response_sequence(/*count*/ 1)).await;
    let mut builder = core_test_support::test_codex::test_codex()
        .with_config(configure_deferred_tool_world_state_without_agents);
    let test = builder.build_with_auto_env(&server).await?;
    test.submit_turn("inspect empty deferred tools").await?;

    let request = response.single_request();
    assert!(tools_state_sections(&request).is_empty());
    test.codex.ensure_rollout_materialized().await;
    test.codex.flush_rollout().await?;
    let rollout_path = test.codex.rollout_path().expect("rollout path");
    let world_states = tokio::fs::read_to_string(rollout_path)
        .await?
        .lines()
        .map(codex_rollout::parse_rollout_line)
        .collect::<serde_json::Result<Vec<_>>>()?
        .into_iter()
        .filter_map(|line| match line.item {
            RolloutItem::WorldState(item) => Some(item.state),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert!(!world_states.is_empty());
    assert!(
        world_states
            .iter()
            .all(|state| state.get("tools").is_none())
    );

    Ok(())
}
