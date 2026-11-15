//! Verifies protocol selection and request metadata for extension-contributed MCP servers.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::Mutex;

use codex_config::McpServerConfig;
use codex_core::config::Config;
use codex_extension_api::ExtensionFuture;
use codex_extension_api::ExtensionRegistryBuilder;
use codex_extension_api::McpProtocolMode;
use codex_extension_api::McpServerContribution;
use codex_extension_api::McpServerContributionContext;
use codex_extension_api::McpServerContributor;
use codex_features::Feature;
use codex_mcp::McpServerSource;
use codex_mcp::McpToolRequestMetaHook;
use codex_mcp::McpToolRequestMetaProvider;
use codex_protocol::ThreadId;
use codex_protocol::models::FunctionCallOutputBody;
use core_test_support::apps_test_server::AppsTestServer;
use core_test_support::apps_test_server::recorded_apps_tool_calls;
use core_test_support::responses;
use core_test_support::skip_if_no_network;
use core_test_support::test_codex::test_codex;
use core_test_support::wait_for_mcp_server;
use pretty_assertions::assert_eq;
use serde_json::Value;
use serde_json::json;
use test_case::test_case;
use wiremock::MockServer;

struct ExtensionServers(Vec<McpServerContribution>);

impl McpServerContributor<Config> for ExtensionServers {
    fn id(&self) -> &'static str {
        "apps_protocol_test"
    }

    fn contribute<'a>(
        &'a self,
        _context: McpServerContributionContext<'a, Config>,
    ) -> ExtensionFuture<'a, Vec<McpServerContribution>> {
        Box::pin(async move { self.0.clone() })
    }
}

async fn mcp_methods(server: &MockServer) -> Vec<String> {
    server
        .received_requests()
        .await
        .expect("mock server should capture MCP startup requests")
        .into_iter()
        .filter(|request| request.url.path() == "/api/codex/ps/mcp")
        .filter_map(|request| {
            let body: Value = serde_json::from_slice(&request.body).ok()?;
            body.get("method")?.as_str().map(str::to_string)
        })
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn extension_protocol_mode_does_not_change_other_http_servers() -> anyhow::Result<()> {
    skip_if_no_network!(Ok(()));

    for (generic_mode, extension_mode) in [
        (McpProtocolMode::Legacy, McpProtocolMode::V20260728),
        (McpProtocolMode::V20260728, McpProtocolMode::Legacy),
    ] {
        let responses_server = responses::start_mock_server().await;
        let extension_server = responses::start_mock_server().await;
        let extension_url = format!(
            "{}/api/codex/ps/mcp",
            AppsTestServer::mount(&extension_server)
                .await?
                .chatgpt_base_url
        );
        let third_party_server = responses::start_mock_server().await;
        let third_party_url = format!(
            "{}/api/codex/ps/mcp",
            AppsTestServer::mount(&third_party_server)
                .await?
                .chatgpt_base_url
        );
        let mut extensions = ExtensionRegistryBuilder::new();
        extensions.mcp_server_contributor(Arc::new(ExtensionServers(vec![
            McpServerContribution::SetWithProtocolMode {
                name: "extension_apps".to_string(),
                config: Box::new(serde_json::from_value(json!({ "url": extension_url }))?),
                protocol_mode: extension_mode,
            },
        ])));

        let fixture = test_codex()
            .with_extensions(Arc::new(extensions.build()))
            .with_config(move |config| {
                config
                    .features
                    .disable(Feature::Apps)
                    .expect("test config should disable hosted Apps");
                if generic_mode == McpProtocolMode::V20260728 {
                    config
                        .features
                        .enable(Feature::Mcp20260728)
                        .expect("test config should enable generic MCP protocol");
                } else {
                    config
                        .features
                        .disable(Feature::Mcp20260728)
                        .expect("test config should disable generic MCP protocol");
                }
                let third_party: McpServerConfig =
                    serde_json::from_value(json!({ "url": third_party_url }))
                        .expect("third-party MCP config");
                config
                    .mcp_servers
                    .set(HashMap::from([("third_party".to_string(), third_party)]))
                    .expect("test config should accept MCP server");
            })
            .build_with_auto_env(&responses_server)
            .await?;

        wait_for_mcp_server(&fixture.codex, "extension_apps").await?;
        let legacy = vec!["initialize", "notifications/initialized", "tools/list"];
        let modern = vec![
            "server/discover",
            "initialize",
            "notifications/initialized",
            "tools/list",
        ];
        assert_eq!(
            mcp_methods(&extension_server).await,
            if extension_mode == McpProtocolMode::V20260728 {
                modern.clone()
            } else {
                legacy.clone()
            }
        );
        assert_eq!(
            mcp_methods(&third_party_server).await,
            if generic_mode == McpProtocolMode::V20260728 {
                modern
            } else {
                legacy
            }
        );
    }

    Ok(())
}

struct InvocationMetadata;

impl McpToolRequestMetaProvider for InvocationMetadata {
    fn value<'a>(
        &'a self,
        thread_id: ThreadId,
        turn_id: &'a str,
        call_id: &'a str,
    ) -> ExtensionFuture<'a, anyhow::Result<Value>> {
        Box::pin(async move {
            anyhow::ensure!(call_id == "prepared", "cannot prepare call {call_id}");
            Ok(json!([thread_id, turn_id, call_id]))
        })
    }
}

enum HookOutcome {
    Applies,
    Skipped,
    KeyConflict,
}

#[test_case("apps_protocol_test", "approved", HookOutcome::Applies; "matching registration")]
#[test_case("other_contributor", "approved", HookOutcome::Skipped; "different contributor")]
#[test_case("apps_protocol_test", "other", HookOutcome::Skipped; "different routing header")]
#[test_case("apps_protocol_test", "approved", HookOutcome::KeyConflict; "existing metadata key")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn request_metadata_is_scoped_to_the_selected_tool_and_registration(
    contributor: &str,
    target: &str,
    outcome: HookOutcome,
) -> anyhow::Result<()> {
    skip_if_no_network!(Ok(()));

    let model = responses::start_mock_server().await;
    let gateway = responses::start_mock_server().await;
    let tools = ["prepare", "read"].map(|name| {
        json!({
            "name": name,
            "inputSchema": {"type": "object"},
            "annotations": {"readOnlyHint": true},
        })
    });
    AppsTestServer::mount_with_tools(&gateway, Arc::new(Mutex::new(Vec::from(tools)))).await?;
    let config_for = |target: &str| -> serde_json::Result<McpServerConfig> {
        serde_json::from_value(json!({
            "url": format!("{}/api/codex/ps/mcp", gateway.uri()),
            "http_headers": {"x-test-target": target},
        }))
    };
    // The two registrations differ only by name, as when a gateway hosts several tools.
    let config = config_for("approved")?;
    let servers = ["selected", "other"].map(|name| McpServerContribution::Set {
        name: name.into(),
        config: Box::new(config.clone()),
    });
    let mut extensions = ExtensionRegistryBuilder::new();
    extensions.mcp_server_contributor(Arc::new(ExtensionServers(Vec::from(servers))));
    let fixture = test_codex()
        .with_extensions(Arc::new(extensions.build()))
        .build_with_auto_env(&model)
        .await?;
    fixture
        .codex
        .thread_extension_data()
        .insert(McpToolRequestMetaHook {
            server_name: "selected".into(),
            source: McpServerSource::Extension {
                id: contributor.into(),
                host_owned_apps: false,
            },
            transport: config_for(target)?.transport,
            tool_name: "prepare".into(),
            key: if matches!(outcome, HookOutcome::KeyConflict) {
                "callId"
            } else {
                "x-test-context"
            }
            .into(),
            provider: Arc::new(InvocationMetadata),
        });
    wait_for_mcp_server(&fixture.codex, "selected").await?;

    let mut events = vec![responses::ev_response_created("calls")];
    events.extend(
        [
            ("prepared", "mcp__selected", "prepare"),
            ("refused", "mcp__selected", "prepare"),
            ("other-tool", "mcp__selected", "read"),
            ("other-server", "mcp__other", "prepare"),
        ]
        .map(|(id, ns, tool)| responses::ev_function_call_with_namespace(id, ns, tool, "{}")),
    );
    events.push(responses::ev_completed("calls"));
    let mock = responses::mount_sse_sequence(
        &model,
        vec![
            responses::sse(events),
            responses::sse(vec![
                responses::ev_assistant_message("done", "done"),
                responses::ev_completed("end"),
            ]),
        ],
    )
    .await;
    fixture.submit_turn("Call the tools").await?;

    let calls = recorded_apps_tool_calls(&gateway).await;
    let mut dispatched: Vec<_> = calls
        .iter()
        .map(|call| {
            let meta = &call["params"]["_meta"];
            (
                meta["callId"].as_str().expect("dispatch call ID"),
                meta.get("x-test-context").cloned(),
            )
        })
        .collect();
    dispatched.sort_unstable_by_key(|(id, _)| *id);
    let mut expected = vec![("other-server", None), ("other-tool", None)];
    let request = mock.last_request().expect("tool outputs");
    let error = match outcome {
        HookOutcome::Applies => {
            let body = request.body_json();
            let turn_id = body["client_metadata"]["turn_id"]
                .as_str()
                .expect("model turn ID");
            expected.push((
                "prepared",
                Some(json!([
                    fixture.session_configured.thread_id,
                    turn_id,
                    "prepared"
                ])),
            ));
            Some("cannot prepare call refused")
        }
        HookOutcome::Skipped => {
            expected.extend([("prepared", None), ("refused", None)]);
            None
        }
        HookOutcome::KeyConflict => Some("MCP request metadata already contains callId"),
    };
    if let Some(message) = error {
        let item = request.function_call_output("refused");
        let body: FunctionCallOutputBody = serde_json::from_value(item["output"].clone())?;
        let output = body.to_text().expect("tool error");
        assert!(output.contains(message), "{output}");
    }
    assert_eq!(dispatched, expected);
    Ok(())
}
