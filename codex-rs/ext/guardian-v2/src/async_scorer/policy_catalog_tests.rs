use super::*;
use codex_protocol::models::PermissionProfile;
use codex_protocol::openai_models::ModelsResponse;
use codex_protocol::protocol::AskForApproval;

const PARENT: &str = "parent A policy";
const PROCESS: &str = "process catalog policy";
const SCOPED: &str = "scoped session policy";
const REVIEWER: &str = "reviewer B policy";
const CLASSIFIER: &str = "classifier C policy must not be used";
const EXPLICIT: &str = "configured user policy";

fn reservation_slots(backend: &crate::async_scorer::conversation::ConversationBackend) -> usize {
    let mut held = Vec::new();
    while let Ok(reservation) = backend.reserve(|| 0).1 {
        held.push(reservation);
    }
    held.len()
}

fn policy_model(slug: &str, policy: &str) -> Result<ModelInfo> {
    let mut model = codex_models_manager::test_support::model_info("test-model");
    model.slug = slug.into();
    model.use_responses_lite = false;
    model.auto_review_model_override = None;
    model.guardian = Some(legacy_policy(Some(&GuardianV2ReviewScopeConfigToml {
        computer_use_only: Some(false),
        ..Default::default()
    })));
    model.model_messages = Some(serde_json::from_value(
        json!({"auto_review":{"policy":policy}}),
    )?);
    Ok(model)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn classifier_policy_follows_review_catalog_and_resumed_credential_owner() -> Result<()> {
    skip_if_no_network!(Ok(()));
    let mut failures = Vec::new();
    for case in [
        "scoped",
        "independent",
        "high",
        "process_miss",
        "explicit",
        "missing",
        "failure",
        "no_model",
        "no_parent",
        "parent_override",
        "blank",
        "owned_credential",
        "off",
        "before_expired",
        "inflight_expired",
    ] {
        let parent = responses::start_mock_server().await;
        let reviewer = responses::start_mock_server().await;
        let classifier = responses::start_mock_server().await;
        let command_home = tempfile::tempdir()?;
        let command_marker = command_home.path().join("called");
        let independent = !matches!(case, "scoped" | "parent_override" | "blank");
        let no_id = matches!(case, "no_model" | "no_parent" | "blank");
        let mut parent_model = policy_model("gpt-5.5", PARENT)?;
        if matches!(case, "parent_override" | "blank") {
            parent_model.auto_review_model_override = Some("review-R".into());
        }
        let classifier_model = policy_model("classifier-C", CLASSIFIER)?;
        let mut process_models = vec![parent_model.clone(), classifier_model.clone()];
        if case != "process_miss" {
            process_models.push(policy_model("review-R", PROCESS)?);
        }
        let scoped_models = ModelsResponse {
            models: vec![
                parent_model.clone(),
                policy_model("review-R", SCOPED)?,
                classifier_model.clone(),
            ],
        };
        let mut reviewer_model = policy_model(
            if case == "missing" {
                "different-model"
            } else {
                "review-R"
            },
            REVIEWER,
        )?;
        reviewer_model.auto_review_model_override = Some("must-not-follow".into());
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::path("/models"))
            .respond_with(wiremock::ResponseTemplate::new(200).set_body_json(&scoped_models))
            .mount(&parent)
            .await;
        let reviewer_response = if case == "failure" {
            wiremock::ResponseTemplate::new(200).set_body_json(json!({"models":"invalid"}))
        } else {
            wiremock::ResponseTemplate::new(200).set_body_json(ModelsResponse {
                models: vec![reviewer_model],
            })
        };
        let delay_metadata = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let delayed = delay_metadata.clone();
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::path("/models"))
            .respond_with(move |_: &wiremock::Request| {
                if delayed.load(std::sync::atomic::Ordering::SeqCst) {
                    reviewer_response.clone().set_delay(Duration::from_secs(30))
                } else {
                    reviewer_response.clone()
                }
            })
            .mount(&reviewer)
            .await;
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::path("/models"))
            .respond_with(
                wiremock::ResponseTemplate::new(200).set_body_json(ModelsResponse {
                    models: vec![classifier_model],
                }),
            )
            .mount(&classifier)
            .await;
        let observer = responses::mount_sse_once(
            &classifier,
            responses::sse(vec![
                ev_assistant_message("policy-score", if case == "high" { "high" } else { "low" }),
                ev_completed("policy-score"),
            ]),
        )
        .await;
        let config_text = format!(
            r#"
model_provider = "openai"
[features]
api_key_model_discovery = true
guardian_approval = true
[features.guardianv2]
enabled = true
model = "classifier-C"
model_provider = "classifier"
async_classifier_mode = "conversation"
classifier_instructions = "POLICY_BEGIN\n{{{{ tenant_policy_config }}}}\nPOLICY_END"
[features.guardianv2.review_scope]
computer_use_only = false
[model_providers.classifier]
name = "Classifier C"
base_url = "{}/v1"
model_catalog_url = "{}/models"
experimental_bearer_token = "classifier-key"
supports_websockets = false
[model_providers.reviewer]
name = "Reviewer B"
base_url = "{}/v1"
model_catalog_url = "{}/models"
requires_openai_auth = true
supports_websockets = false
"#,
            classifier.uri(),
            classifier.uri(),
            reviewer.uri(),
            reviewer.uri()
        );
        let parent_url = parent.uri();
        let process_auth =
            AuthManager::from_auth_for_testing(CodexAuth::from_api_key("process-key"));
        let test = test_codex()
            .with_auth_manager(process_auth)
            .with_pre_build_hook(move |home| {
                std::fs::write(home.join("config.toml"), &config_text).unwrap()
            })
            .with_config(move |config| {
                config.model_provider = ModelProviderInfo {
                    name: "Parent A".into(),
                    base_url: Some(format!("{parent_url}/v1")),
                    model_catalog_url: Some(format!("{parent_url}/models").into()),
                    experimental_bearer_token: Some("parent-key".into()),
                    ..ModelProviderInfo::create_openai_provider(None)
                };
                config.model_catalog = Some(ModelsResponse {
                    models: process_models,
                });
                config.approvals_reviewer = ApprovalsReviewer::AutoReview;
                config
                    .permissions
                    .approval_policy
                    .set(AskForApproval::OnRequest)
                    .unwrap();
                config
                    .permissions
                    .set_permission_profile(PermissionProfile::read_only())
                    .unwrap();
            })
            .build_with_auto_env(&parent)
            .await?;
        let mut config = test.config.clone();
        config.model_catalog = if independent {
            None
        } else {
            Some(scoped_models)
        };
        config.guardian_model = if no_id || case == "parent_override" {
            None
        } else {
            Some("review-R".into())
        };
        if case == "blank" {
            config.guardian_model = Some("   ".into());
        }
        config.guardian_policy_config = (case == "explicit").then(|| EXPLICIT.into());
        config
            .features
            .set_enabled(Feature::GuardianApproval, case != "off")?;
        if independent {
            let mut provider = config.model_providers["reviewer"].clone();
            if case == "owned_credential" {
                provider.requires_openai_auth = false;
                provider.experimental_bearer_token = Some("reviewer-owned-key".into());
            }
            if case == "explicit" {
                provider.requires_openai_auth = false;
                provider.auth = Some(serde_json::from_value(json!({"command":"/bin/sh",
                    "args":["-c","printf called >> \"$1\"; printf command-key","policy-test",command_marker],
                    "cwd":command_home.path()}))?);
            }
            config.guardian_model_provider = Some(("reviewer".into(), provider));
        } else {
            config.guardian_model_provider = None;
        }
        let session_auth =
            AuthManager::from_auth_for_testing(CodexAuth::from_api_key("thread-key"));
        let source_id = test.codex.startup_metadata().thread_id;
        test.codex.ensure_rollout_materialized().await;
        test.codex
            .inject_response_items(vec![responses::user_message_item(
                "Inspect README without changing it.",
            )])
            .await?;
        let rollout_path = test.codex.rollout_path().expect("legacy fixture rollout");
        test.codex.shutdown_and_wait().await?;
        test.thread_manager.remove_thread(&source_id).await;
        let resumed = test
            .thread_manager
            .resume_legacy_thread_from_rollout(
                config.clone(),
                rollout_path,
                session_auth,
                None,
                Default::default(),
            )
            .await?;
        let thread = resumed.thread;
        let manager = thread.models_manager();
        manager
            .list_models(
                codex_models_manager::manager::RefreshStrategy::OnlineIfUncached,
                config.http_client_factory(),
            )
            .await;
        let resolved_parent = manager
            .get_model_info("gpt-5.5", &config.to_models_manager_config())
            .await;
        let store = thread.thread_extension_data();
        store.insert(resolved_parent);
        if case == "no_parent" {
            store.remove::<ModelInfo>();
        }
        let mut builder = ExtensionRegistryBuilder::new();
        super::super::install(
            &mut builder,
            test.thread_manager.auth_manager(),
            Arc::downgrade(&test.thread_manager),
        );
        let registry = builder.build();
        let session = ExtensionData::new("policy-session");
        let metrics = Arc::new(RecordingMetrics::default());
        registry.thread_lifecycle_contributors()[0]
            .on_thread_start(ThreadStartInput {
                config: &config,
                session_source: &SessionSource::Exec,
                persistent_thread_state_available: false,
                environments: &[],
                mcp_resource_client: None,
                extension_metrics: Some(metrics.clone()),
                session_store: &session,
                thread_store: store,
            })
            .await;
        if case == "off" {
            let b = reviewer.received_requests().await.unwrap();
            let c = classifier.received_requests().await.unwrap();
            eprintln!(
                "T21_EVIDENCE {}",
                json!({"case":case,"b_requests":b.len(),"c_requests":c.len()})
            );
            assert!(b.is_empty() && c.is_empty());
            continue;
        }
        assert!(
            store.get::<LunaSampler>().is_some(),
            "classifier not initialized: {case}"
        );
        if case == "explicit" {
            // Resume loads the reviewer catalog, then the installed synchronous
            // reviewer prewarms its catalog and client using separate auth owners.
            // Join those three observed command starts before measuring classification.
            tokio::time::timeout(ASYNC_TEST_TIMEOUT, async {
                while std::fs::metadata(&command_marker)
                    .map(|m| m.len())
                    .unwrap_or(0)
                    < 18
                {
                    tokio::task::yield_now().await;
                }
            })
            .await?;
            pretty_assertions::assert_eq!(std::fs::read(&command_marker)?, b"calledcalledcalled");
        }
        let b_before = reviewer.received_requests().await.unwrap().len();
        let command_before = if command_marker.exists() {
            std::fs::metadata(&command_marker)?.len()
        } else {
            0
        };
        let history = thread.conversation_history_snapshot().await;
        let expiring = matches!(case, "before_expired" | "inflight_expired");
        let backend = store.get::<crate::async_scorer::conversation::ConversationBackend>();
        let slots_before = if expiring {
            reservation_slots(backend.as_ref().expect("retained conversation backend"))
        } else {
            0
        };
        delay_metadata.store(
            case == "inflight_expired",
            std::sync::atomic::Ordering::SeqCst,
        );
        let turn = ExtensionData::new("policy-turn");
        let name = ToolName::plain("read_file");
        let payload = ToolPayload::Function {
            arguments: r#"{"path":"README.md"}"#.into(),
        };
        let (entered, permission_wait) = tokio::sync::oneshot::channel();
        let (release, permission_ready) = tokio::sync::oneshot::channel();
        let observation = registry.tool_lifecycle_contributors()[0].on_tool_start(ToolStartInput {
            permissions: Box::pin(async move {
                if case == "before_expired" {
                    let _ = entered.send(());
                    let _ = permission_ready.await;
                }
                Some(Default::default())
            }),
            session_store: &session,
            thread_store: store,
            turn_store: &turn,
            turn_id: "policy-turn",
            root_turn_id: None,
            call_id: "policy-call",
            originating_item_id: None,
            tool_name: &name,
            mcp_tool: None,
            payload: &payload,
            conversation_history: history,
            source: ToolCallSource::Direct,
        });
        let progress = store.get::<GuardianV2ScoreProgress>().unwrap();
        if case == "before_expired" {
            let (_, released) = tokio::join!(observation, async {
                permission_wait.await?;
                for _ in 0..3 {
                    observe_unscored_call(&progress, store);
                }
                release.send(()).unwrap();
                Ok::<(), anyhow::Error>(())
            });
            released?;
        } else {
            drop(release);
            observation.await;
        }
        let command_after_observation = std::fs::metadata(&command_marker)
            .map(|m| m.len())
            .unwrap_or(0);
        if expiring {
            if case == "inflight_expired" {
                tokio::time::timeout(ASYNC_TEST_TIMEOUT, async {
                    while reviewer.received_requests().await.unwrap().len() == b_before
                        && observer.requests().is_empty()
                    {
                        tokio::task::yield_now().await;
                    }
                })
                .await?;
                for _ in 0..3 {
                    observe_unscored_call(&progress, store);
                }
            }
            // Complete before the catalog client's existing five-second timeout can rescue it.
            tokio::time::timeout(Duration::from_secs(1), async {
                while !metrics.classification_samples().iter().any(|m| {
                    matches!(m,
                    RecordedMetric::Counter(name, _, _) if name == CLASSIFICATION_METRIC)
                }) || reservation_slots(backend.as_ref().unwrap()) != slots_before
                {
                    tokio::task::yield_now().await;
                }
            })
            .await?;
            let b = reviewer.received_requests().await.unwrap();
            let superseded = metrics.classification_samples().iter().any(|m| {
                matches!(m,
                RecordedMetric::Counter(name, _, tags) if name == CLASSIFICATION_METRIC &&
                    tags.contains(&("outcome".into(), "superseded".into())))
            });
            let slots_after = reservation_slots(backend.as_ref().unwrap());
            let valid = superseded
                && cached_score(store).is_none()
                && observer.requests().is_empty()
                && b.len() - b_before == usize::from(case == "inflight_expired")
                && slots_after == slots_before;
            eprintln!(
                "T21_EVIDENCE {}",
                json!({"case":case,"valid":valid,"superseded":superseded,
                "startup_b_requests":b_before,"classification_b_requests":b.len()-b_before,
                "classifier_requests":observer.requests().len(),"slots_before":slots_before,"slots_after":slots_after,
                "metrics":format!("{:?}",metrics.classification_samples()),
                "b_requests":b.iter().map(|r|json!({"method":r.method.as_str(),"url":r.url.as_str(),
                    "headers":format!("{:?}",r.headers)})).collect::<Vec<_>>() })
            );
            if !valid {
                failures.push(case);
            }
            continue;
        }
        tokio::time::timeout(ASYNC_TEST_TIMEOUT, async {
            while observer.requests().is_empty() || cached_score(store).is_none() {
                tokio::task::yield_now().await;
            }
        })
        .await?;
        let command_after_score = std::fs::metadata(&command_marker)
            .map(|m| m.len())
            .unwrap_or(0);
        let requests = observer.requests();
        let body = requests[0].body_json();
        let text = body["input"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|i| i["role"] == "developer")
            .flat_map(|i| i["content"].as_array().into_iter().flatten())
            .filter_map(|c| c["text"].as_str())
            .find(|s| s.contains("POLICY_BEGIN"))
            .expect("classifier policy instructions");
        let actual = text
            .split("POLICY_BEGIN\n")
            .nth(1)
            .unwrap()
            .split("\nPOLICY_END")
            .next()
            .unwrap();
        let expected = match case {
            "explicit" => EXPLICIT,
            "scoped" | "parent_override" => SCOPED,
            "missing" | "failure" | "no_parent" => {
                ResolvedModelMessages::bundled().auto_review().policy
            }
            "no_model" | "blank" => PARENT,
            _ => REVIEWER,
        };
        let b = reviewer.received_requests().await.unwrap();
        let should_fetch = independent && !no_id && case != "explicit";
        let credential = if case == "owned_credential" {
            "Bearer reviewer-owned-key"
        } else {
            "Bearer thread-key"
        };
        let b_headers = b[b_before..]
            .iter()
            .map(|r| {
                r.headers
                    .get("authorization")
                    .and_then(|v| v.to_str().ok())
                    .map(str::to_owned)
            })
            .collect::<Vec<_>>();
        let approval = cached_approval(
            &registry,
            store,
            &json!({"tool":"read_file","id":"policy-call","path":"README.md"}).to_string(),
            None,
        )
        .await;
        if case == "independent" {
            let current = ScoreAuthorization::current(&thread, &Default::default()).await;
            let mut stale_authorization = current.clone();
            stale_authorization.local.user_message_revision += 1;
            let mut stale_context = current.clone();
            stale_context.review_context_revision += 1;
            for (authorization, expected) in [
                (current.clone(), Some(ReviewDecision::Approved)),
                (stale_authorization, None),
                (stale_context, None),
                (current, Some(ReviewDecision::Approved)),
            ] {
                seed_cached_score(&progress, store, 1, authorization);
                pretty_assertions::assert_eq!(
                    cached_approval(
                        &registry,
                        store,
                        &json!({"tool":"read_file","id":"policy-call"}).to_string(),
                        None
                    )
                    .await,
                    expected
                );
            }
        }
        let valid = actual == expected
            && (approval == Some(ReviewDecision::Approved)) == (case != "high")
            && b.len() - b_before == usize::from(should_fetch)
            && b_headers.iter().all(|h| h.as_deref() == Some(credential))
            && (if command_marker.exists() {
                std::fs::metadata(&command_marker)?.len()
            } else {
                0
            }) == command_before
            && body["model"] == "classifier-C"
            && requests[0].header("authorization").as_deref() == Some("Bearer classifier-key");
        eprintln!(
            "T21_EVIDENCE {}",
            json!({"case":case,"valid":valid,"actual_policy":actual,"expected_policy":expected,
            "startup_b_requests":b_before,"classification_b_requests":b.len()-b_before,"startup_command_bytes":command_before,
            "approval":format!("{approval:?}"),"command_after_score":command_after_score,"command_after_observation":command_after_observation,
            "body":body,"c_authorization":requests[0].header("authorization"),"b_authorization":b_headers,
            "b_requests":b.iter().map(|r|json!({"method":r.method.as_str(),"url":r.url.as_str(),"headers":format!("{:?}",r.headers),"body":String::from_utf8_lossy(&r.body)})).collect::<Vec<_>>(),
            "credential_command_bytes":if command_marker.exists() {std::fs::metadata(&command_marker)?.len()} else {0}})
        );
        if !valid {
            failures.push(case);
        }
    }
    assert!(
        failures.is_empty(),
        "policy/credential source mismatch: {failures:?}"
    );
    Ok(())
}
