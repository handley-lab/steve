use super::*;
use codex_core::config::ConfigBuilder;
use codex_extension_api::ExtensionData;
use codex_extension_api::ThreadStartInput;
use codex_features::Feature;
use codex_history::ResponseItemEnvelope;
use codex_models_manager::manager::SharedModelsManager;
use codex_protocol::models::ExecutedToolCall;
use codex_protocol::models::FunctionCallOutputPayload;
use codex_protocol::models::ResponseInputItem;
use codex_protocol::models::ToolResultMetadata;
use codex_protocol::openai_models::ModelInfo;
use codex_protocol::openai_models::ModelsResponse;
use pretty_assertions::assert_eq;

const LITE_KEY: &str = "ws_request_header_x_openai_internal_codex_responses_lite";

// Use the production catalog constructor and startup path, not the test-only getter.
async fn configured_sampler(
    url: &str,
    model: Option<ModelInfo>,
    continuation: bool,
    metadata: bool,
    kinds: bool,
    websocket: bool,
) -> Result<LunaSamplerConfig> {
    let home = tempfile::tempdir()?;
    let models = if let Some(model) = model {
        vec![model]
    } else {
        let mut other = codex_models_manager::bundled_models_response()?
            .models
            .remove(0);
        other.slug = "different-catalog-model".into();
        vec![other]
    };
    std::fs::write(
        home.path().join("catalog.json"),
        serde_json::to_vec(&ModelsResponse { models })?,
    )?;
    std::fs::write(
        home.path().join("config.toml"),
        format!(
            r#"
model_provider = "selected"
model = "classifier-test"
model_catalog_json = "catalog.json"
service_tier = "experimental-tier-id"
[features]
api_key_model_discovery = false
content_item_kinds = {kinds}
[features.guardianv2]
enabled = true
model = "classifier-test"
[model_providers.selected]
name = "Cambridge"
base_url = "{url}"
experimental_bearer_token = "selected-classifier-key"
supports_websockets = {websocket}
accepts_response_continuation = {continuation}
include_internal_metadata = {metadata}
"#
        ),
    )?;
    let config = ConfigBuilder::default()
        .codex_home(home.path().to_path_buf())
        .loader_overrides(codex_config::LoaderOverrides::without_managed_config_for_tests())
        .build()
        .await?;
    assert_eq!(config.features.enabled(Feature::ContentItemKinds), kinds);
    let auth = AuthManager::from_auth_for_testing(CodexAuth::from_api_key("unused-key"));
    let scoped: SharedModelsManager = codex_core::build_models_manager(&config, auth.clone());
    let thread = ExtensionData::new("classifier-thread");
    thread.insert(scoped);
    let session = ExtensionData::new("classifier-session");
    let input = ThreadStartInput {
        config: &config,
        session_source: &codex_protocol::protocol::SessionSource::Exec,
        persistent_thread_state_available: false,
        environments: &[],
        mcp_resource_client: None,
        extension_metrics: None,
        session_store: &session,
        thread_store: &thread,
    };
    let guardian = crate::async_scorer::config::GuardianV2Config::resolve(&config).unwrap();
    crate::async_scorer::startup::sampler_config(&input, &guardian, auth, None)
        .await
        .map_err(anyhow::Error::msg)
}

fn model(lite: bool, original: bool) -> Result<ModelInfo> {
    let mut model = codex_models_manager::bundled_models_response()?
        .models
        .remove(0);
    model.slug = "classifier-test".into();
    model.use_responses_lite = lite;
    model.supports_image_detail_original = original;
    model.comp_hash = Some("classifier-scope-hash".into());
    model.context_window = Some(262_144);
    model.effective_context_window_percent = 50;
    Ok(model)
}

fn evidence() -> Result<Vec<ResponseItem>> {
    let message = serde_json::from_value(json!({
        "type":"message", "id":"server-message", "role":"user",
        "encrypted_content":"message-ciphertext",
        "content":[{"type":"input_text","text":"Inspect only."},
            {"type":"input_image","image_url":"data:image/png;base64,Zm9v","detail":"original"}],
        "internal_chat_message_metadata_passthrough":{
            "turn_id":"evidence-turn", "content_item_kinds":["user","user"],
            "content_item_metadata":[{"provenance":{"type":"harness","hook":false}},
                {"provenance":{"type":"harness","hook":false}}]
        }
    }))?;
    let mut output = ResponseItem::from(ResponseInputItem::FunctionCallOutput {
        call_id: "tool-call".into(),
        output: FunctionCallOutputPayload::from_text("tool evidence".into()),
    });
    let mut call = ExecutedToolCall::new("read_file".into(), json!({"path":"README.md"}));
    call.set_tool_result_metadata(ToolResultMetadata::new(&json!({"selected":"metadata"})));
    output.append_executed_tool_calls(vec![call]);
    Ok(vec![
        message,
        serde_json::from_value(json!({"type":"function_call","id":"server-function",
            "call_id":"tool-call","name":"read_file","arguments":"{}",
            "encrypted_function_args":["function-ciphertext"]}))?,
        output,
        serde_json::from_value(json!({"type":"reasoning","id":"server-reasoning",
            "summary":[],"encrypted_content":"reasoning-ciphertext"}))?,
        serde_json::from_value(json!({"type":"compaction","id":"server-compaction",
            "encrypted_content":"compaction-ciphertext"}))?,
        serde_json::from_value(json!({"type":"context_compaction","id":"server-context",
            "encrypted_content":"context-ciphertext"}))?,
        ResponseItem::AdditionalTools {
            id: Some(ResponseItemId::from_server("server-nonempty-tools".into())),
            role: "developer".into(),
            tools: vec![
                json!({"type":"function","name":"read_file","parameters":{"type":"object"}}),
            ],
        },
    ])
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn classifier_protocol_controls_reach_http_and_websocket() -> Result<()> {
    skip_if_no_network!(Ok(()));
    let mut failures = Vec::new();
    for websocket in [false, true] {
        for lite in [false, true] {
            for continuation in [false, true] {
                for metadata in [false, true] {
                    for kinds in [false, true] {
                        let events =
                            vec![ev_assistant_message("score", "low"), ev_completed("score")];
                        let http = responses::start_mock_server().await;
                        let observer =
                            responses::mount_sse_once(&http, responses::sse(events.clone())).await;
                        let ws =
                            responses::start_websocket_server(vec![Vec::new(), vec![events]]).await;
                        let url = if websocket {
                            format!("http://{}/v1", ws.uri().trim_start_matches("ws://"))
                        } else {
                            http.uri()
                        };
                        let config = configured_sampler(
                            &url,
                            Some(model(lite, true)?),
                            continuation,
                            metadata,
                            kinds,
                            websocket,
                        )
                        .await?;
                        let sampler = LunaSampler::new(config);
                        if websocket {
                            sampler.prewarm().await;
                            sampler.wait_for_prewarm(Duration::from_secs(2)).await?;
                        }
                        let mut request = sample_request("parent");
                        request.input = evidence()?;
                        assert_eq!(sampler.sample(request).await?, "low");
                        let (body, header, authorization) = if websocket {
                            let bodies = ws.connections().into_iter().flatten().collect::<Vec<_>>();
                            assert_eq!(bodies.len(), 1);
                            let handshake = ws.handshakes().last().unwrap().clone();
                            (
                                bodies[0].body_json(),
                                handshake.header("x-openai-internal-codex-responses-lite"),
                                handshake.header("authorization"),
                            )
                        } else {
                            let requests = observer.requests();
                            assert_eq!(requests.len(), 1);
                            (
                                requests[0].body_json(),
                                requests[0].header("x-openai-internal-codex-responses-lite"),
                                requests[0].header("authorization"),
                            )
                        };
                        let items = body["input"].as_array().unwrap();
                        let message = items.iter().find(|i| i["id"] == "server-message").unwrap();
                        let output = items
                            .iter()
                            .find(|i| i["type"] == "function_call_output")
                            .unwrap();
                        let function = items.iter().find(|i| i["id"] == "server-function").unwrap();
                        let passthrough = &message["internal_chat_message_metadata_passthrough"];
                        let tool_metadata = &output["internal_chat_message_metadata_passthrough"]["executed_tool_calls"]
                            [0]["tool_result_metadata"];
                        let valid = header.as_deref() == lite.then_some("true")
                            && body["client_metadata"][LITE_KEY].as_str() == lite.then_some("true")
                            && body["reasoning"]["context"].as_str() == lite.then_some("all_turns")
                            && (items[0]["type"] == "additional_tools") == lite
                            && passthrough.is_object() == continuation
                            && passthrough["content_item_kinds"].is_array() == (continuation && kinds)
                            && passthrough["content_item_metadata"].is_array() == (continuation && kinds && lite)
                            && tool_metadata.is_object() == (continuation && metadata)
                            && message["encrypted_content"].as_str() == continuation.then_some("message-ciphertext")
                            && function["encrypted_function_args"].as_array().is_some() == continuation
                            && message["content"][1]["detail"].as_str() == (!lite).then_some("original")
                            && body["service_tier"] == "experimental-tier-id"
                            && items.iter().any(|item| item["id"] == "server-nonempty-tools"
                                && item["tools"][0]["name"] == "read_file")
                            && authorization.as_deref() == Some("Bearer selected-classifier-key")
                            && ["server-reasoning", "server-compaction", "server-context"].iter().all(|id| {
                                items.iter().find(|i| i["id"] == *id).unwrap()["encrypted_content"].is_string()
                            });
                        eprintln!(
                            "T20_EVIDENCE {}",
                            json!({"kind":"protocol","websocket":websocket,"lite":lite,
                            "continuation":continuation,"metadata":metadata,"kinds":kinds,"valid":valid,
                            "lite_header":header,"authorization":authorization,"body":body})
                        );
                        if !valid {
                            failures.push((websocket, lite, continuation, metadata, kinds));
                        }
                        ws.shutdown().await;
                    }
                }
            }
        }
    }
    assert!(
        failures.is_empty(),
        "protocol controls bypassed: {failures:?}"
    );
    Ok(())
}

#[tokio::test]
async fn classifier_disabled_websocket_never_upgrades_during_prewarm_or_classification()
-> Result<()> {
    skip_if_no_network!(Ok(()));
    let http = responses::start_mock_server().await;
    let observer = responses::mount_sse_once(
        &http,
        responses::sse(vec![
            ev_assistant_message("score", "low"),
            ev_completed("score"),
        ]),
    )
    .await;
    let config = configured_sampler(
        &http.uri(),
        Some(model(false, true)?),
        false,
        false,
        false,
        false,
    )
    .await?;
    let sampler = LunaSampler::new(config);
    sampler.prewarm().await;
    let before = http.received_requests().await.unwrap();
    assert_eq!(sampler.sample(sample_request("parent")).await?, "low");
    let after = http.received_requests().await.unwrap();
    let upgrades = after
        .iter()
        .filter(|r| r.headers.contains_key("upgrade"))
        .count();
    eprintln!(
        "T20_EVIDENCE {}",
        json!({"kind":"disabled-websocket","before":before.len(),
        "upgrades":upgrades,"requests":after.iter().map(|r|json!({"method":r.method.as_str(),"url":r.url.as_str(),
            "headers":format!("{:?}",r.headers),"body":String::from_utf8_lossy(&r.body)})).collect::<Vec<_>>() })
    );
    assert_eq!(observer.requests().len(), 1);
    assert!(before.is_empty(), "prewarm attempted a disabled transport");
    assert_eq!(upgrades, 0);
    Ok(())
}

#[tokio::test]
async fn classifier_startup_uses_the_thread_catalog_without_a_process_manager() -> Result<()> {
    let mut record = model(false, true)?;
    record.context_window = Some(16_384);
    let config = configured_sampler(
        "http://127.0.0.1:9/v1",
        Some(record),
        false,
        false,
        false,
        false,
    )
    .await?;
    eprintln!(
        "T20_EVIDENCE {}",
        json!({"kind":"thread-catalog-budget","actual":config.max_input_tokens,
        "expected":8192})
    );
    assert_eq!(config.max_input_tokens, 8192);
    let sampler = LunaSampler::new(config);
    assert!(sampler.supports_parent_compaction(Some("classifier-scope-hash")));
    Ok(())
}

#[tokio::test]
async fn classifier_catalog_scope_does_not_alias_process_or_independent_provider() -> Result<()> {
    skip_if_no_network!(Ok(()));
    let server = responses::start_mock_server().await;
    let test = core_test_support::test_codex::test_codex()
        .with_pre_build_hook(|home| {
            std::fs::write(home.join("config.toml"), "model_provider = \"openai\"\n").unwrap();
        })
        .with_model_info_override("classifier-test", |model| {
            model.comp_hash = Some("process-hash".into());
            model.context_window = Some(32_768);
            model.effective_context_window_percent = 50;
            model.use_responses_lite = true;
        })
        .build_with_auto_env(&server)
        .await?;
    let auth = test.thread_manager.auth_manager();
    let mut failures = Vec::new();
    for (scope, expected_hash, expected_window) in [
        ("scoped", "scoped-hash", 8192),
        ("process", "process-hash", 16_384),
        ("independent", "independent-hash", 24_576),
        ("no-manager", "", 128_000),
    ] {
        let mut config = test.config.clone();
        let mut scoped_record = model(false, true)?;
        scoped_record.comp_hash = Some("scoped-hash".into());
        scoped_record.context_window = Some(16_384);
        config.model_catalog = Some(ModelsResponse {
            models: vec![scoped_record],
        });
        let scoped: SharedModelsManager = codex_core::build_models_manager(&config, auth.clone());
        let thread = ExtensionData::new("scope-thread");
        if matches!(scope, "scoped" | "independent") {
            thread.insert(scoped);
        }
        let session = ExtensionData::new("scope-session");
        let mut guardian = crate::async_scorer::config::GuardianV2Config::resolve(&config).unwrap();
        guardian.local_overrides.model = Some("classifier-test".into());
        if scope == "independent" {
            let mut independent_record = model(true, true)?;
            independent_record.comp_hash = Some("independent-hash".into());
            independent_record.context_window = Some(49_152);
            config.model_catalog = Some(ModelsResponse {
                models: vec![independent_record],
            });
            config.model_providers.insert(
                "independent".into(),
                ModelProviderInfo {
                    name: "Independent".into(),
                    base_url: Some(server.uri()),
                    ..config.model_provider.clone()
                },
            );
            guardian.local_overrides.model_provider = Some("independent".into());
        }
        let input = ThreadStartInput {
            config: &config,
            session_source: &codex_protocol::protocol::SessionSource::Exec,
            persistent_thread_state_available: false,
            environments: &[],
            mcp_resource_client: None,
            extension_metrics: None,
            session_store: &session,
            thread_store: &thread,
        };
        let selected = crate::async_scorer::startup::sampler_config(
            &input,
            &guardian,
            auth.clone(),
            (scope != "no-manager").then(|| test.thread_manager.clone()),
        )
        .await
        .unwrap();
        let window = selected.max_input_tokens;
        let sampler = LunaSampler::new(selected);
        let hash_matches = if scope == "no-manager" {
            !sampler.supports_parent_compaction(Some("scoped-hash"))
        } else {
            sampler.supports_parent_compaction(Some(expected_hash))
        };
        let response = responses::mount_sse_once(
            &server,
            responses::sse(vec![
                ev_assistant_message("scope-score", "low"),
                ev_completed("scope-score"),
            ]),
        )
        .await;
        assert_eq!(sampler.sample(sample_request("scope-parent")).await?, "low");
        let body = response.requests()[0].body_json();
        let lite_matches = body["client_metadata"][LITE_KEY].as_str()
            == matches!(scope, "process" | "independent").then_some("true");
        eprintln!(
            "T20_EVIDENCE {}",
            json!({"kind":"catalog-scope","scope":scope,
            "expected_hash":expected_hash,"hash_matches":hash_matches,"expected_window":expected_window,"window":window,
            "lite_matches":lite_matches,"body":body})
        );
        if window != expected_window || !hash_matches || !lite_matches {
            failures.push(scope);
        }
    }
    assert!(failures.is_empty(), "catalog scope bypassed: {failures:?}");
    Ok(())
}

#[tokio::test]
async fn classifier_image_detail_uses_resolved_capabilities_without_rewriting_evidence()
-> Result<()> {
    skip_if_no_network!(Ok(()));
    let mut failures = Vec::new();
    for (label, record, expected_original) in [
        ("lite", Some(model(true, true)?), None),
        ("original", Some(model(false, true)?), Some("original")),
        ("ordinary", Some(model(false, false)?), Some("high")),
        ("fallback", None, Some("original")),
        ("no-manager", Some(model(false, false)?), Some("original")),
    ] {
        let http = responses::start_mock_server().await;
        let observer = responses::mount_sse_once(
            &http,
            responses::sse(vec![
                ev_assistant_message("score", "low"),
                ev_completed("score"),
            ]),
        )
        .await;
        let mut config = configured_sampler(&http.uri(), record, true, true, true, false).await?;
        if label == "no-manager" {
            config.model_info = None;
        }
        let sampler = LunaSampler::new(config);
        let mut request = sample_request("parent");
        request.input = evidence()?;
        if let ResponseItem::Message { content, .. } = &mut request.input[0] {
            content.push(serde_json::from_value(json!({"type":"input_image",
                "image_url":"data:image/png;base64,YmFy","detail":"high"}))?);
        }
        let original = request.input.clone();
        let prepared = LunaSampler::prepare_input(
            &request,
            None,
            request
                .input
                .iter()
                .cloned()
                .map(ResponseItemEnvelope::new)
                .collect(),
        )
        .0;
        let prepared_message = prepared
            .iter()
            .find(|e| {
                e.item
                    .id()
                    .is_some_and(|id| id.as_str() == "server-message")
            })
            .unwrap();
        let logical_unchanged = prepared_message.item == original[0];
        assert_eq!(sampler.sample(request).await?, "low");
        let body = observer.requests()[0].body_json();
        let message = body["input"]
            .as_array()
            .unwrap()
            .iter()
            .find(|i| i["id"] == "server-message")
            .unwrap();
        let valid = logical_unchanged
            && message["content"][1]["detail"].as_str() == expected_original
            && message["content"][2]["detail"].as_str() == (label != "lite").then_some("high");
        eprintln!(
            "T20_EVIDENCE {}",
            json!({"kind":"images","case":label,"logical_unchanged":logical_unchanged,
            "valid":valid,"body":body})
        );
        if !valid {
            failures.push(label);
        }
    }
    assert!(failures.is_empty(), "image controls bypassed: {failures:?}");
    Ok(())
}

#[tokio::test]
async fn classifier_nonlite_wire_preserves_retained_envelope_alignment() -> Result<()> {
    use crate::async_scorer::request::PreparedRequest;
    use codex_guardian_context::TranscriptCursor;
    use codex_history::CodexHarnessMetadata;
    skip_if_no_network!(Ok(()));
    let http = responses::start_mock_server().await;
    let observer = responses::mount_sse_sequence(
        &http,
        vec![
            responses::sse(vec![
                ev_assistant_message("score-one", "low"),
                ev_completed("response-one"),
            ]),
            responses::sse(vec![
                ev_assistant_message("score-two", "low"),
                ev_completed("response-two"),
            ]),
        ],
    )
    .await;
    let config = configured_sampler(
        &http.uri(),
        Some(model(false, true)?),
        false,
        false,
        false,
        false,
    )
    .await?;
    let sampler = LunaSampler::new(config);
    let mut request = sample_request("parent");
    request.input = evidence()?;
    let source = serde_json::from_value(
        json!({"id":{"message_id":"source-message","turn_id":"source-turn","role":"user"},
        "revision":"source-revision","complete":true}),
    )?;
    let mut evidence = request
        .input
        .iter()
        .cloned()
        .map(ResponseItemEnvelope::new)
        .collect::<Vec<_>>();
    evidence[0].metadata = Some(CodexHarnessMetadata {
        guardian_sources: vec![source],
        ..Default::default()
    });
    let (mut committed, _) = LunaSampler::prepare_input(&request, None, evidence);
    let original = committed.clone();
    for _ in 0..2 {
        let input_tokens = committed
            .iter()
            .map(|e| codex_guardian_context::estimate_input_tokens(&e.item))
            .sum();
        let before = committed.clone();
        let prepared = PreparedRequest {
            sampling: &request,
            input: committed,
            input_tokens,
            existing_context_tokens: 0,
            cursor: TranscriptCursor {
                parent_history_version: 0,
                transcript_entry_count: 0,
            },
            truncations: vec![],
            section_costs: vec![],
        };
        let (ready, score) = tokio::sync::oneshot::channel();
        let (history, score) = tokio::join!(sampler.sample_retained(prepared, ready), score);
        assert_eq!(score??, "low");
        committed = history.expect("completed retained history");
        assert_eq!(&committed[..before.len()], before);
    }
    let requests = observer.requests();
    assert_eq!(requests.len(), 2);
    let bodies = requests.iter().map(|r| r.body_json()).collect::<Vec<_>>();
    let valid = bodies
        .iter()
        .all(|body| body["input"][0]["type"] != "additional_tools")
        && committed
            .iter()
            .find(|e| {
                e.item
                    .id()
                    .is_some_and(|id| id.as_str() == "server-message")
            })
            .unwrap()
            .metadata
            == original
                .iter()
                .find(|e| {
                    e.item
                        .id()
                        .is_some_and(|id| id.as_str() == "server-message")
                })
                .unwrap()
                .metadata;
    eprintln!(
        "T20_EVIDENCE {}",
        json!({"kind":"retained-alignment","valid":valid,"bodies":bodies,
        "original":original.iter().map(|e|json!({"item":e.item,"metadata":e.metadata})).collect::<Vec<_>>(),
        "committed":committed.iter().map(|e|json!({"item":e.item,"metadata":e.metadata})).collect::<Vec<_>>() })
    );
    assert!(
        valid,
        "wire normalization changed history or retained its Lite prefix"
    );
    Ok(())
}
