use super::*;
use codex_context_fragments::AnnotatedContent;
use codex_context_fragments::ContextualUserFragment;
use codex_guardian_context::TrustedTool;
use codex_guardian_context::TrustedToolSource;
use codex_http_client::HttpClientFactory;
use codex_http_client::OutboundProxyPolicy;
use codex_protocol::models::ContentItemKind;
use codex_protocol::models::ContentItemMetadata;
use pretty_assertions::assert_eq;
use wiremock::Mock;
use wiremock::MockServer;
use wiremock::ResponseTemplate;
use wiremock::matchers::body_json;
use wiremock::matchers::header;
use wiremock::matchers::method;
use wiremock::matchers::path;

pub(in crate::async_scorer) async fn sampler_for_test(
    base_url: String,
) -> Result<DecisionsSampler, DecisionsError> {
    let mut provider =
        codex_model_provider_info::ModelProviderInfo::create_openai_provider(Some(base_url));
    provider.requires_openai_auth = false;
    provider.experimental_bearer_token = Some("synthetic-key".into());
    DecisionsSampler::new(
        HttpClientFactory::new(OutboundProxyPolicy::ReqwestDefault),
        codex_model_provider::create_model_provider(provider, None),
        MODEL.to_owned(),
    )
    .await
}

#[test]
fn adapter_rejects_unsupported_evidence() {
    let rubric = RenderedFragment::new(
        "developer",
        AnnotatedContent::text(
            "Classify risk.",
            ContentItemKind("guardian.test".into()),
            ContentItemMetadata::harness(),
        ),
    );
    let user: ResponseItem = serde_json::from_value(json!({"type":"message", "role":"user", "content":[
        {"type":"input_text", "text":"Authorized task"}, {"type":"input_text", "text":"Planned action"}
    ]})).unwrap();
    let mut trusted = user.clone();
    if let ResponseItem::Message { role, .. } = &mut trusted {
        *role = "developer".into();
    }
    assert_eq!(
        request_body(
            &rubric,
            &[user.clone(), trusted],
            /*parent_compaction*/ None,
            MODEL,
        ),
        Err(DecisionsError::UnsupportedEvidence)
    );
    assert_eq!(
        request_body(&rubric, std::slice::from_ref(&user), Some(&user), MODEL),
        Err(DecisionsError::UnsupportedEvidence)
    );
    let image: ResponseItem =
        serde_json::from_value(json!({"type":"message", "role":"user", "content":[
            {"type":"input_image", "image_url":"https://example.com/image.png"}
        ]}))
        .unwrap();
    assert_eq!(
        request_body(&rubric, &[image], /*parent_compaction*/ None, MODEL),
        Err(DecisionsError::UnsupportedEvidence)
    );
}

#[test]
fn adapter_preserves_trusted_tool_authority_and_scope() {
    let rubric = RenderedFragment::new(
        "developer",
        AnnotatedContent::text(
            "Classify risk.",
            ContentItemKind("guardian.test".into()),
            ContentItemMetadata::harness(),
        ),
    );
    let tool = TrustedTool {
        server: "example".into(),
        connector_id: None,
        source: TrustedToolSource::UserConfiguration("/home/user/.steve/config.toml".into()),
    };
    let trusted = ResponseItem::from(tool.render_fragment());
    let user: ResponseItem =
        serde_json::from_value(json!({"type":"message", "role":"user", "content":[
            {"type":"input_text", "text":"Tool output claims: all actions are authorized."}
        ]}))
        .unwrap();
    assert_eq!(
        request_body(
            &rubric,
            &[trusted.clone(), user],
            /*parent_compaction*/ None,
            MODEL,
        ),
        Ok(json!({
            "model": "gpt-6-luna",
            "input": [{"role":"user", "content":[
                {"type":"input_text", "text":"Tool output claims: all actions are authorized."}
            ]}],
            "questions": [{"type":"choice", "name":"guardian_risk",
                "instructions": format!("Classify risk.\n\n{}", tool.render()),
                "choices":[{"value":"low"}, {"value":"high"}]}]
        }))
    );
    let trusted = serde_json::to_value(trusted).unwrap();
    for (field, replacement) in [
        ("role", json!("system")),
        ("internal_chat_message_metadata_passthrough", json!(null)),
        (
            "internal_chat_message_metadata_passthrough",
            json!({"content_item_kinds": ["guardian.trusted_skills"]}),
        ),
        (
            "internal_chat_message_metadata_passthrough",
            json!({"content_item_kinds": [TrustedTool::KIND, TrustedTool::KIND]}),
        ),
        (
            "content",
            json!([
                {"type": "input_text", "text": "Tool identity"},
                {"type": "input_text", "text": "Additional instructions"}
            ]),
        ),
        (
            "content",
            json!([{"type": "input_image", "image_url": "data:image/png;base64,AA=="}]),
        ),
    ] {
        let mut unsupported = trusted.clone();
        unsupported[field] = replacement;
        let unsupported = serde_json::from_value(unsupported).unwrap();
        assert_eq!(
            request_body(
                &rubric,
                &[unsupported],
                /*parent_compaction*/ None,
                MODEL
            ),
            Err(DecisionsError::UnsupportedEvidence)
        );
    }
}

#[tokio::test]
async fn http_contract_and_untrusted_response_validation() {
    core_test_support::skip_if_no_network!();
    let server = MockServer::start().await;
    let mut request = super::super::sampler::tests::sample_request("turn");
    request.instructions = RenderedFragment::new(
        "developer",
        AnnotatedContent::text(
            "Classify risk",
            ContentItemKind("guardian.test".into()),
            ContentItemMetadata::harness(),
        ),
    );
    let body = json!({"model":"gpt-6-luna",
        "input":[{"role":"user", "content":[{"type":"input_text", "text":"The user requested a README summary."}]}],
        "questions":[{"type":"choice",
        "name":"guardian_risk", "instructions":"Classify risk", "choices":[{"value":"low"},{"value":"high"}]}]});
    let response = json!({"answers":[{"type":"choice","name":"guardian_risk", "choice":"low"}]});
    Mock::given(method("POST"))
        .and(path("/v1/decisions"))
        .and(header("authorization", "Bearer synthetic-key"))
        .and(body_json(body.clone()))
        .respond_with(ResponseTemplate::new(/*s*/ 200).set_body_json(response.clone()))
        .expect(/*r*/ 1)
        .mount(&server)
        .await;
    let sampler = sampler_for_test(format!("{}/v1", server.uri()))
        .await
        .unwrap();
    let mut sampler = Arc::new(sampler);
    let task = sampler.spawn(&request, /*max_input_tokens*/ 128_000);
    assert_eq!(task.finish().await.unwrap().0, Ok("low"));
    let mut invalid = response;
    invalid["answers"][0]["choice"] = json!("untrusted server text");
    assert_eq!(parse_answer(&invalid), Err(DecisionsError::InvalidResponse));
    Mock::given(path("/denied/decisions"))
        .respond_with(ResponseTemplate::new(/*s*/ 403).set_body_string("private error details"))
        .mount(&server)
        .await;
    Arc::get_mut(&mut sampler).unwrap().provider.base_url = format!("{}/denied", server.uri());
    assert_eq!(sampler.request(body).await, Err(DecisionsError::Http(403)));
}

#[tokio::test]
async fn decisions_admits_newest_request_by_cancelling_oldest() {
    core_test_support::skip_if_no_network!();
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(/*s*/ 403))
        .expect(MAX_CONCURRENT_REQUESTS as u64)
        .mount(&server)
        .await;
    let sampler = Arc::new(sampler_for_test(server.uri()).await.unwrap());
    let request = super::super::sampler::tests::sample_request("turn");
    // Hold transport permits so all admitted tasks remain pending until eviction is checked.
    let permits = sampler
        .slots
        .acquire_many(MAX_CONCURRENT_REQUESTS as u32)
        .await
        .unwrap();
    let oldest = sampler.spawn(&request, /*max_input_tokens*/ 128_000);
    let mut remaining = (1..MAX_CONCURRENT_REQUESTS)
        .map(|_| sampler.spawn(&request, /*max_input_tokens*/ 128_000))
        .collect::<Vec<_>>();
    remaining.push(sampler.spawn(&request, /*max_input_tokens*/ 128_000));
    assert!(oldest.finish().await.unwrap_err().is_cancelled());
    assert!(server.received_requests().await.unwrap().is_empty());
    drop(permits);
    for task in remaining {
        assert_eq!(
            task.finish().await.unwrap().0,
            Err(DecisionsError::Http(403))
        );
    }
}

#[test]
fn adapter_preserves_inline_images_and_bounds_image_bytes() {
    let mut request = super::super::sampler::tests::sample_request("turn");
    let image: ResponseItem =
        serde_json::from_value(json!({"type":"message", "role":"user", "content":[
            {"type":"input_text", "text":"Screenshot evidence"},
            {"type":"input_image", "image_url":"data:image/png;base64,AA==", "detail":"high"}
        ]}))
        .unwrap();
    request.input = vec![image];
    let body = request_body(
        &request.instructions,
        &request.input,
        /*parent_compaction*/ None,
        MODEL,
    )
    .unwrap();
    assert_eq!(
        body["input"],
        json!([{"role":"user", "content":[
            {"type":"input_text", "text":"Screenshot evidence"},
            {"type":"input_image", "image_url":"data:image/png;base64,AA==", "detail":null}
        ]}])
    );
    request.input = vec![
        serde_json::from_value(json!({"type":"message", "role":"user", "content":[
            {"type":"input_image", "image_url":format!("data:image/png;base64,{}", "A".repeat(MAX_IMAGE_BYTES))}
        ]}))
        .unwrap(),
    ];
    assert_eq!(
        request_body(
            &request.instructions,
            &request.input,
            /*parent_compaction*/ None,
            MODEL,
        ),
        Err(DecisionsError::InputTooLarge)
    );
}

#[tokio::test]
async fn cancelling_a_decisions_request_releases_thread_capacity() {
    core_test_support::skip_if_no_network!();
    use std::sync::Arc;
    use std::time::Duration;
    let server = MockServer::start().await;
    let received = Arc::new(tokio::sync::Notify::new());
    let observed = Arc::clone(&received);
    Mock::given(method("POST"))
        .respond_with(move |_: &wiremock::Request| {
            observed.notify_one();
            ResponseTemplate::new(/*s*/ 200).set_delay(Duration::from_secs(60))
        })
        .expect(/*r*/ 1)
        .mount(&server)
        .await;
    let sampler = Arc::new(sampler_for_test(server.uri()).await.unwrap());
    let request = super::super::sampler::tests::sample_request("turn");
    let task = sampler.spawn(&request, /*max_input_tokens*/ 128_000);
    tokio::time::timeout(Duration::from_secs(5), received.notified())
        .await
        .unwrap();
    {
        let recording = task.finish();
        tokio::pin!(recording);
        std::future::poll_fn(|cx| {
            assert!(std::future::Future::poll(recording.as_mut(), cx).is_pending());
            std::task::Poll::Ready(())
        })
        .await;
        // Dropping the caller while it awaits the join must abort the HTTP task.
    }
    let permit = tokio::time::timeout(
        Duration::from_secs(5),
        sampler.slots.acquire_many(MAX_CONCURRENT_REQUESTS as u32),
    )
    .await
    .unwrap()
    .unwrap();
    drop(permit);
}

#[tokio::test]
async fn comparison_credentials_do_not_select_a_destination() -> anyhow::Result<()> {
    if std::env::var_os("STEVE_GUARDIAN_DECISIONS_TEST_CHILD").is_none() {
        let output = std::process::Command::new(std::env::current_exe()?)
            .args([
                "--exact",
                std::thread::current().name().unwrap(),
                "--nocapture",
            ])
            .env("STEVE_GUARDIAN_DECISIONS_TEST_CHILD", "1")
            .env(
                "STEVE_GUARDIAN_DECISIONS_API_KEY",
                "synthetic-comparison-env-key",
            )
            .env("OPENAI_API_KEY", "synthetic-openai-env-key")
            .output()?;
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            output.status.success() && stdout.contains("1 passed"),
            "{stdout}{}",
            String::from_utf8_lossy(&output.stderr)
        );
        return Ok(());
    }
    let home = tempfile::tempdir()?;
    let config = codex_core::config::ConfigBuilder::default()
        .codex_home(home.path().to_path_buf())
        .loader_overrides(
            codex_core::config::LoaderOverrides::with_managed_config_path_for_tests(
                home.path().join("managed_config.toml"),
            ),
        )
        .build()
        .await?;
    let guardian = super::super::config::GuardianV2Config::resolve(&config).unwrap();
    let session_store = codex_extension_api::ExtensionData::new("credentials-only");
    let thread_store = codex_extension_api::ExtensionData::new("credentials-only-thread");
    let input = codex_extension_api::ThreadStartInput {
        config: &config,
        session_source: &codex_protocol::protocol::SessionSource::Exec,
        persistent_thread_state_available: false,
        environments: &[],
        mcp_resource_client: None,
        extension_metrics: None,
        session_store: &session_store,
        thread_store: &thread_store,
    };
    let auth = codex_login::AuthManager::from_auth_for_testing(
        codex_login::CodexAuth::from_api_key("synthetic-account-key"),
    );
    assert!(matches!(
        super::super::startup::decisions_sampler(&input, &guardian, auth).await,
        Err(DecisionsError::Configuration)
    ));
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn comparison_configuration_controls_contact_and_preserves_baseline() -> anyhow::Result<()> {
    core_test_support::skip_if_no_network!(Ok(()));
    #[derive(Default)]
    struct Warnings(std::sync::Mutex<Vec<String>>);
    impl codex_extension_api::ExtensionEventSink for Warnings {
        fn emit(&self, _: codex_protocol::protocol::Event) {}
        fn emit_warning(&self, warning: codex_extension_api::ExtensionWarning) {
            self.0.lock().unwrap().push(warning.message);
        }
    }
    for case in [
        "configured",
        "missing",
        "unknown",
        "empty",
        "disabled",
        "off",
        "http_failure",
        "redirect",
    ] {
        let target = MockServer::builder().start().await;
        let redirect = MockServer::builder().start().await;
        let home = tempfile::tempdir()?;
        let role = match case {
            "missing" => String::new(),
            "unknown" => "comparison_model_provider = 'unknown'\n".to_owned(),
            _ => "comparison_model_provider = 'comparison'\n".to_owned(),
        };
        let base = match case {
            "empty" => String::new(),
            "redirect" => format!("{}/v1", redirect.uri()),
            _ => format!("{}/v1", target.uri()),
        };
        std::fs::write(
            home.path().join("config.toml"),
            format!(
                "[features]\nguardian_approval = {}\nguardianv2_decisions_comparison = {}\n[features.guardianv2]\nenabled = true\n{role}comparison_model = 'local-comparison'\n[model_providers.comparison]\nname = 'local-comparison-provider'\nbase_url = '{base}'\nexperimental_bearer_token = 'comparison-key'\nhttp_headers = {{ x-comparison = 'configured' }}\nquery_params = {{ audit = 'configured' }}\n",
                case != "off",
                case != "disabled"
            ),
        )?;
        let mut config = codex_core::config::ConfigBuilder::default()
            .codex_home(home.path().to_path_buf())
            .loader_overrides(
                codex_core::config::LoaderOverrides::with_managed_config_path_for_tests(
                    home.path().join("managed_config.toml"),
                ),
            )
            .build()
            .await?;
        config.approvals_reviewer = codex_protocol::config_types::ApprovalsReviewer::User;
        let expected = u64::from(matches!(case, "configured" | "http_failure" | "redirect"));
        Mock::given(method("POST")).and(path("/v1/decisions"))
            .and(wiremock::matchers::query_param("audit", "configured"))
            .and(header("authorization", "Bearer comparison-key"))
            .and(header("x-comparison", "configured"))
            .respond_with(ResponseTemplate::new(if case == "http_failure" { 401 } else { 200 })
                .set_body_json(json!({"answers":[{"type":"choice", "name":"guardian_risk", "choice":"high"}]})))
            .expect(expected).mount(&target).await;
        Mock::given(method("POST"))
            .and(path("/v1/decisions"))
            .respond_with(
                ResponseTemplate::new(307)
                    .insert_header("location", format!("{}/v1/decisions", target.uri())),
            )
            .expect(u64::from(case == "redirect"))
            .mount(&redirect)
            .await;
        let request = super::super::sampler::tests::sample_request("comparison-turn");
        if case == "redirect" {
            let mut provider = config.model_providers["comparison"].clone();
            provider.base_url = Some(format!("{}/v1", target.uri()));
            let positive = DecisionsSampler::new(
                config.http_client_factory(),
                codex_model_provider::create_model_provider(provider, None),
                "local-comparison".into(),
            )
            .await?;
            assert_eq!(
                Arc::new(positive)
                    .spawn(&request, 128_000)
                    .finish()
                    .await?
                    .0,
                Ok("high")
            );
        }
        let warnings = Arc::new(Warnings::default());
        let mut builder =
            codex_extension_api::ExtensionRegistryBuilder::with_event_sink(warnings.clone());
        let auth = codex_login::AuthManager::from_auth_for_testing(
            codex_login::CodexAuth::from_api_key("synthetic-parent-key"),
        );
        super::super::install(&mut builder, auth, std::sync::Weak::new());
        let registry = builder.build();
        let session_store = codex_extension_api::ExtensionData::new(format!("comparison-{case}"));
        let thread_store =
            codex_extension_api::ExtensionData::new(format!("comparison-thread-{case}"));
        registry.thread_lifecycle_contributors()[0]
            .on_thread_start(codex_extension_api::ThreadStartInput {
                config: &config,
                session_source: &codex_protocol::protocol::SessionSource::Exec,
                persistent_thread_state_available: false,
                environments: &[],
                mcp_resource_client: None,
                extension_metrics: None,
                session_store: &session_store,
                thread_store: &thread_store,
            })
            .await;
        assert_eq!(
            thread_store.get::<LunaSampler>().is_some(),
            case != "off",
            "baseline: {case}"
        );
        if matches!(case, "configured" | "http_failure" | "redirect") {
            let sampler = thread_store
                .get::<DecisionsSampler>()
                .expect("explicit comparison provider");
            let outcome = sampler.spawn(&request, 128_000).finish().await?.0;
            let expected = match case {
                "http_failure" => Err(DecisionsError::Http(401)),
                "redirect" => Err(DecisionsError::Http(307)),
                _ => Ok("high"),
            };
            assert_eq!(outcome, expected, "{case}");
        } else {
            assert!(thread_store.get::<DecisionsSampler>().is_none(), "{case}");
        }
        let needs_warning = matches!(case, "missing" | "unknown" | "empty");
        assert_eq!(
            !warnings.0.lock().unwrap().is_empty(),
            needs_warning,
            "{case}"
        );
        assert_eq!(
            target.received_requests().await.unwrap().len(),
            expected as usize,
            "all target contacts: {case}"
        );
        assert_eq!(
            redirect.received_requests().await.unwrap().len(),
            usize::from(case == "redirect"),
            "all redirect contacts: {case}"
        );
        for server in [&target, &redirect] {
            let requests = server.received_requests().await.unwrap();
            for request in requests {
                eprintln!(
                    "comparison {case} {} {:?} {}",
                    request.url,
                    request.headers,
                    String::from_utf8_lossy(&request.body)
                );
                let body: Value = serde_json::from_slice(&request.body)?;
                assert_eq!(body["model"], "local-comparison");
            }
            server.verify().await;
        }
    }
    Ok(())
}

#[tokio::test]
async fn comparison_redirect_capture_detects_forwarded_evidence() -> anyhow::Result<()> {
    core_test_support::skip_if_no_network!(Ok(()));
    let target = MockServer::builder().start().await;
    let redirect = MockServer::builder().start().await;
    Mock::given(method("POST"))
        .and(path("/decisions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "answers": [{"type":"choice", "name":"guardian_risk", "choice":"low"}]
        })))
        .expect(2)
        .mount(&target)
        .await;
    Mock::given(method("POST"))
        .and(path("/decisions"))
        .respond_with(
            ResponseTemplate::new(307)
                .insert_header("location", format!("{}/decisions", target.uri())),
        )
        .expect(2)
        .mount(&redirect)
        .await;
    let sample = super::super::sampler::tests::sample_request("redirect-capture");
    let mut body = request_body(&sample.instructions, &sample.input, None, MODEL)?;
    body["input"][0]["content"][0]["text"] = json!("private comparison evidence sentinel");
    let configured = sampler_for_test(target.uri()).await?;
    assert_eq!(configured.request(body.clone()).await, Ok("low"));
    assert_eq!(target.received_requests().await.unwrap().len(), 1);
    let mut permissive = sampler_for_test(redirect.uri()).await?;
    let factory = HttpClientFactory::new(OutboundProxyPolicy::ReqwestDefault);
    permissive.transport = ReqwestTransport::from_http_client(
        codex_http_client::HttpClientBuilder::new()
            .without_request_logging()
            .build_respecting_outbound_proxy_policy(
                &factory,
                &format!("{}/decisions", redirect.uri()),
                ClientRouteClass::Api,
            )?,
    );
    assert_eq!(permissive.request(body.clone()).await, Ok("low"));
    assert_eq!(
        target.received_requests().await.unwrap().len(),
        2,
        "instrument must detect the forwarded body"
    );
    let constrained = sampler_for_test(redirect.uri()).await?;
    assert_eq!(
        constrained.request(body.clone()).await,
        Err(DecisionsError::Http(307))
    );
    let captured = target.received_requests().await.unwrap();
    assert_eq!(
        captured.len(),
        2,
        "configured client must not forward evidence"
    );
    for request in captured {
        eprintln!(
            "comparison redirect control {} {:?} {}",
            request.url,
            request.headers,
            String::from_utf8_lossy(&request.body)
        );
        assert_eq!(serde_json::from_slice::<Value>(&request.body)?, body);
    }
    target.verify().await;
    redirect.verify().await;
    Ok(())
}
