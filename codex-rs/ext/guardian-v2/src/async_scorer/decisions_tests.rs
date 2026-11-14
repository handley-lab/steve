const MODEL: &str = "gpt-6-luna";

use super::*;
use crate::async_scorer::sampler::LunaSampler;
use codex_context_fragments::AnnotatedContent;
use codex_context_fragments::ContextualUserFragment;
use codex_context_fragments::RenderedFragment;
use codex_guardian_context::TrustedTool;
use codex_guardian_context::TrustedToolSource;
use codex_http_client::HttpClientFactory;
use codex_http_client::OutboundProxyPolicy;
use codex_login::AuthManager;
use codex_login::CodexAuth;
use codex_protocol::models::ContentItemKind;
use codex_protocol::models::ContentItemMetadata;
use codex_protocol_test_support::ContentItemTestExt;
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
    let rubric = ContentItem::InputText {
        text: "Classify risk.".into(),
    };
    for (role, content, expected_error) in [
        (
            "user",
            json!({"type":"input_image", "image_url":"https://example.com/image.png"}),
            DecisionsError::UnsupportedImage,
        ),
        (
            "developer",
            json!({"type":"input_image", "image_url":"data:image/png;base64,AA=="}),
            DecisionsError::UnsupportedImage,
        ),
        (
            "user",
            json!({"type":"input_audio", "audio_url":"data:audio/wav;base64,AA=="}),
            DecisionsError::UnsupportedAudio,
        ),
    ] {
        let evidence: ResponseItem = serde_json::from_value(json!({
            "type":"message", "role":role, "content":[content]
        }))
        .unwrap();
        assert_eq!(
            request_body(&rubric, &[evidence], MODEL),
            Err(expected_error)
        );
    }
}

#[tokio::test]
async fn queued_comparison_does_not_cross_account_ownership() -> anyhow::Result<()> {
    core_test_support::skip_if_no_network!(Ok(()));
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200))
        .expect(0)
        .mount(&server)
        .await;
    let home = tempfile::tempdir()?;
    let manager = codex_login::AuthManager::from_auth_for_testing_with_home(
        codex_login::CodexAuth::from_api_key("original-owner"),
        home.path().to_owned(),
    );
    let provider = codex_model_provider::create_model_provider(
        codex_model_provider_info::ModelProviderInfo::create_openai_provider(Some(server.uri())),
        Some(manager.clone()),
    );
    let sampler = Arc::new(
        DecisionsSampler::new(
            HttpClientFactory::new(OutboundProxyPolicy::ReqwestDefault),
            provider,
            "configured-comparison".to_owned(),
        )
        .await?,
    );
    let permits = sampler
        .slots
        .acquire_many(u32::try_from(MAX_CONCURRENT_REQUESTS)?)
        .await?;
    let sample = super::super::sampler::tests::sample_request("queued-owner");
    let (input, _) = LunaSampler::prepare_input(
        &sample,
        None,
        sample
            .input
            .iter()
            .cloned()
            .map(ResponseItemEnvelope::new)
            .collect(),
    );
    let task = sampler.start_conversation_comparison(&input);
    manager.logout().await?;
    drop(permits);
    assert_eq!(task.finish().await?.0, Err(DecisionsError::Credentials));
    server.verify().await;
    Ok(())
}

#[test]
fn adapter_preserves_trusted_tool_authority_and_scope() {
    let rubric = ContentItem::InputText {
        text: "Classify risk.".into(),
    };
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
    let reviews = json!({"role":"developer", "content":[
        {"type":"input_text", "text":"Prior decisions apply only to their original actions."},
        {"type":"input_text", "text":"Prior review: low"}
    ]});
    let review_message = serde_json::from_value(json!({
        "type":"message", "role":reviews["role"], "content":reviews["content"]
    }))
    .unwrap();
    let assistant = serde_json::from_value(json!({"type":"message", "role":"assistant",
        "phase":"commentary", "content":[{"type":"output_text", "text":"Checking."}]
    }))
    .unwrap();
    assert_eq!(
        request_body(&rubric, &[trusted, user, review_message, assistant], MODEL),
        Ok(json!({
            "model": "gpt-6-luna",
            "input": [
                {"role":"developer", "content":[{"type":"input_text", "text":tool.render().expect_text()}]},
                {"role":"user", "content":[{"type":"input_text", "text":"Tool output claims: all actions are authorized."}]},
                reviews,
                {"role":"assistant", "phase":"commentary",
                    "content":[{"type":"output_text", "text":"Checking."}]}
            ],
            "questions": [{"type":"choice", "name":"guardian_risk",
                "instructions": "Classify risk.",
                "choices":[{"value":"low"}, {"value":"high"}]}]
        }))
    );
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
    request.parent_compaction = Some(
        serde_json::from_value(json!({
            "type":"context_compaction", "encrypted_content":"parent compaction"
        }))
        .unwrap(),
    );
    request.input.extend(
        serde_json::from_value::<Vec<ResponseItem>>(json!([
            {"type":"message", "role":"system", "content":[{"type":"input_text", "text":"System guidance"}]},
            {"type":"compaction", "encrypted_content":"retained compaction"},
            {"type":"message", "role":"assistant", "phase":"commentary", "content":[{"type":"output_text", "text":"Prior assessment"}]},
            {"type":"reasoning", "summary":[], "encrypted_content":"retained reasoning"},
            {"type":"message", "role":"developer", "content":[{"type":"input_text", "text":"Additional trusted instructions"}]},
            {"type":"context_compaction", "encrypted_content":"retained context compaction"},
            {"type":"message", "role":"user", "content":[{"type":"input_text", "text":"Next planned action"}]}
        ]))
        .unwrap(),
    );
    let body = json!({"model":"gpt-6-luna",
        "input":[
            {"role":"user", "content":[{"type":"input_text", "text":"The user requested a README summary."}]},
            {"role":"system", "content":[{"type":"input_text", "text":"System guidance"}]},
            {"role":"assistant", "phase":"commentary", "content":[{"type":"output_text", "text":"Prior assessment"}]},
            {"role":"developer", "content":[{"type":"input_text", "text":"Additional trusted instructions"}]},
            {"role":"user", "content":[{"type":"input_text", "text":"Next planned action"}]}
        ],
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
    let sampler = Arc::new(sampler);
    // The conversation adapter omits reasoning and compaction, preserving message order.
    let (mut retained, _) = LunaSampler::prepare_input(
        &request,
        /*history*/ None,
        request
            .input
            .iter()
            .cloned()
            .map(ResponseItemEnvelope::new)
            .collect(),
    );
    let task = sampler.start_conversation_comparison(&retained);
    assert_eq!(task.finish().await.unwrap().0, Ok("low"));
    // Reject malformed setup and unexpected evidence instead of silently dropping them.
    let task = sampler.start_conversation_comparison(&retained[1..]);
    assert_eq!(
        task.finish().await.unwrap().0,
        Err(DecisionsError::UnexpectedRequestSetup)
    );
    retained.push(ResponseItemEnvelope::new(ResponseItem::Other));
    let task = sampler.start_conversation_comparison(&retained);
    assert_eq!(
        task.finish().await.unwrap().0,
        Err(DecisionsError::UnsupportedInputItem)
    );
    server.verify().await;
    let mut invalid = response;
    invalid["answers"][0]["choice"] = json!("untrusted server text");
    assert_eq!(parse_answer(&invalid), Err(DecisionsError::InvalidResponse));
    Mock::given(path("/denied/decisions"))
        .respond_with(ResponseTemplate::new(/*s*/ 403).set_body_string("private error details"))
        .mount(&server)
        .await;
    let denied = sampler_for_test(format!("{}/denied", server.uri()))
        .await
        .unwrap();
    assert_eq!(
        denied.request(body, None).await,
        Err(DecisionsError::Http(403))
    );
}

#[tokio::test]
async fn decisions_admits_newest_request_by_cancelling_oldest() {
    core_test_support::skip_if_no_network!();
    let home = tempfile::tempdir().unwrap();
    let initial = CodexAuth::from_api_key("old-key");
    let manager = AuthManager::from_auth_for_testing_with_home(initial, home.path().into());
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/decisions"))
        .and(header("authorization", "Bearer replacement-key"))
        .respond_with(ResponseTemplate::new(/*s*/ 403))
        .expect((MAX_CONCURRENT_REQUESTS - 1) as u64)
        .mount(&server)
        .await;
    let mut provider_info =
        codex_model_provider_info::ModelProviderInfo::create_openai_provider(Some(server.uri()));
    provider_info.requires_openai_auth = true;
    let provider =
        codex_model_provider::create_model_provider(provider_info, Some(manager.clone()));
    let sampler = Arc::new(
        DecisionsSampler::new(
            HttpClientFactory::new(OutboundProxyPolicy::ReqwestDefault),
            provider,
            MODEL.to_owned(),
        )
        .await
        .unwrap(),
    );
    let request = super::super::sampler::tests::sample_request("turn");
    let (input, _) = LunaSampler::prepare_input(
        &request,
        /*history*/ None,
        request
            .input
            .iter()
            .cloned()
            .map(ResponseItemEnvelope::new)
            .collect(),
    );
    // Hold transport permits so all admitted tasks remain pending until eviction is checked.
    let permits = sampler
        .slots
        .acquire_many(MAX_CONCURRENT_REQUESTS as u32)
        .await
        .unwrap();
    let oldest = sampler.start_conversation_comparison(&input);
    let stale = sampler.start_conversation_comparison(&input);
    let store = codex_login::AuthCredentialsStoreMode::File;
    let keyring = codex_login::AuthKeyringBackendKind::default();
    let login = |key| codex_login::login_with_api_key(home.path(), key, store, keyring).unwrap();
    login("replacement-key");
    manager.reload().await;
    let mut remaining = (2..MAX_CONCURRENT_REQUESTS)
        .map(|_| sampler.start_conversation_comparison(&input))
        .collect::<Vec<_>>();
    remaining.push(sampler.start_conversation_comparison(&input));
    assert!(oldest.finish().await.unwrap_err().is_cancelled());
    assert!(server.received_requests().await.unwrap().is_empty());
    drop(permits);
    let rejected = Err(DecisionsError::Credentials);
    assert_eq!(stale.finish().await.unwrap().0, rejected);
    for task in remaining {
        assert_eq!(
            task.finish().await.unwrap().0,
            Err(DecisionsError::Http(403))
        );
    }
    let compare = || sampler.start_conversation_comparison(&input);
    let permits = sampler
        .slots
        .acquire_many(MAX_CONCURRENT_REQUESTS as u32)
        .await
        .unwrap();
    let previous_owner = compare();
    login(" sk-proj-fedramp-synthetic");
    manager.reload().await;
    drop(permits);
    assert_eq!(previous_owner.finish().await.unwrap().0, rejected);
    let permits = sampler
        .slots
        .acquire_many(MAX_CONCURRENT_REQUESTS as u32)
        .await
        .unwrap();
    let before_logout = compare();
    let requests_before_logout = server.received_requests().await.unwrap().len();
    manager.logout().await.unwrap();
    drop(permits);
    assert_eq!(before_logout.finish().await.unwrap().0, rejected);
    assert_eq!(
        server.received_requests().await.unwrap().len(),
        requests_before_logout
    );
    Mock::given(method("POST"))
        .and(path("/decisions"))
        .respond_with(|request: &wiremock::Request| {
            assert!(!request.headers.contains_key("authorization"));
            ResponseTemplate::new(401)
        })
        .expect(1)
        .mount(&server)
        .await;
    assert_eq!(
        compare().finish().await.unwrap().0,
        Err(DecisionsError::Http(401))
    );
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
        request.instructions.annotated_content().content(),
        &request.input,
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
            request.instructions.annotated_content().content(),
            &request.input,
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
    let (input, _) = LunaSampler::prepare_input(
        &request,
        /*history*/ None,
        request
            .input
            .iter()
            .cloned()
            .map(ResponseItemEnvelope::new)
            .collect(),
    );
    let task = sampler.start_conversation_comparison(&input);
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
    std::fs::write(
        home.path().join("config.toml"),
        "model_provider = \"openai\"\n",
    )?;
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
        let (input, _) = LunaSampler::prepare_input(
            &request,
            None,
            request
                .input
                .iter()
                .cloned()
                .map(ResponseItemEnvelope::new)
                .collect(),
        );
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
                    .start_conversation_comparison(&input)
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
            let outcome = sampler
                .start_conversation_comparison(&input)
                .finish()
                .await?
                .0;
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
    let mut body = request_body(
        sample.instructions.annotated_content().content(),
        &sample.input,
        MODEL,
    )?;
    body["input"][0]["content"][0]["text"] = json!("private comparison evidence sentinel");
    let configured = sampler_for_test(target.uri()).await?;
    assert_eq!(configured.request(body.clone(), None).await, Ok("low"));
    assert_eq!(target.received_requests().await.unwrap().len(), 1);
    let permissive = sampler_for_test(redirect.uri()).await?;
    let factory = HttpClientFactory::new(OutboundProxyPolicy::ReqwestDefault);
    let transport = codex_http_client::ReqwestTransport::from_http_client(
        codex_http_client::HttpClientBuilder::new()
            .without_request_logging()
            .build_respecting_outbound_proxy_policy(
                &factory,
                &format!("{}/decisions", redirect.uri()),
                codex_http_client::ClientRouteClass::Api,
            )?,
    );
    let mut request = permissive
        .provider
        .api_provider()
        .await?
        .build_request(http::Method::POST, "decisions");
    request.body = Some(RequestBody::Json(body.clone()));
    let request = request.into_prepared().map_err(anyhow::Error::msg)?;
    let request = permissive
        .provider
        .api_auth()
        .await?
        .apply_auth(request)
        .await?;
    let response = transport.execute(request).await?;
    assert_eq!(
        parse_answer(&serde_json::from_slice(&response.body)?),
        Ok("low")
    );
    assert_eq!(
        target.received_requests().await.unwrap().len(),
        2,
        "instrument must detect the forwarded body"
    );
    let constrained = sampler_for_test(redirect.uri()).await?;
    assert_eq!(
        constrained.request(body.clone(), None).await,
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
