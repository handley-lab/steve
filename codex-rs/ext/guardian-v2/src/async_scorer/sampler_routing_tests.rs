//! Keeps classifier evidence within its owning account and thread routing binding.

use super::connect_sampler;
use super::proxy_websocket_servers;
use super::sample_request;
use super::sampler_config;
use anyhow::Result;
use codex_login::AuthManager;
use codex_login::CodexAuth;
use codex_login::ExternalAuth;
use codex_login::ExternalAuthFuture;
use codex_login::ExternalAuthRefreshContext;
use codex_login::WorkspaceRouting;
use codex_login::WorkspaceRoutingRequest;
use codex_login::WorkspaceRoutingResolver;
use codex_model_provider::ModelProviderFuture;
use codex_model_provider::create_model_provider;
use codex_model_provider_info::ModelProviderInfo;
use core_test_support::responses;
use core_test_support::responses::WebSocketConnectionConfig;
use core_test_support::responses::ev_assistant_message;
use core_test_support::responses::ev_completed;
use pretty_assertions::assert_eq;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;
use std::time::Duration;

type RoutingFuture<'a> = ModelProviderFuture<'a, std::io::Result<Option<WorkspaceRouting>>>;

struct UnavailableRouting;

impl WorkspaceRoutingResolver for UnavailableRouting {
    fn resolve(&self, _request: WorkspaceRoutingRequest) -> RoutingFuture<'_> {
        Box::pin(async { Err(std::io::Error::other("discovery unavailable")) })
    }
}

#[tokio::test]
async fn failed_routing_blocks_a_prewarmed_classifier_connection() -> Result<()> {
    core_test_support::skip_if_no_network!(Ok(()));
    let response = vec![vec![ev_assistant_message("r1", "low"), ev_completed("r1")]];
    let server = responses::start_websocket_server(vec![response.clone(), response]).await;
    let base_url = proxy_websocket_servers(&[&server, &server])
        .await?
        .replace("/v1", "/backend-api/codex");
    let auth =
        AuthManager::from_auth_for_testing(CodexAuth::create_dummy_chatgpt_auth_for_testing());
    let mut config = sampler_config(base_url.clone());
    config.provider = create_model_provider(
        ModelProviderInfo {
            requires_openai_auth: true,
            supports_websockets: true,
            ..ModelProviderInfo::create_openai_provider(Some(base_url))
        },
        Some(auth.clone()),
    );
    let sampler = connect_sampler(config).await?;
    let resolver: Arc<dyn WorkspaceRoutingResolver> = Arc::new(UnavailableRouting);
    auth.set_workspace_routing_resolver(Arc::downgrade(&resolver));
    let error = sampler.sample(sample_request("turn-1")).await.unwrap_err();
    assert!(
        error.to_string().contains("discovery unavailable"),
        "{error}"
    );
    assert!(server.connections().iter().all(Vec::is_empty));
    Ok(())
}

struct AuthAfterRouting {
    initial: CodexAuth,
    updated: CodexAuth,
    resolved: AtomicBool,
}

impl ExternalAuth for AuthAfterRouting {
    fn resolve(&self) -> ExternalAuthFuture<'_, CodexAuth> {
        Box::pin(async {
            Ok(if self.resolved.load(Ordering::SeqCst) {
                self.updated.clone()
            } else {
                self.initial.clone()
            })
        })
    }

    fn refresh(&self, _context: ExternalAuthRefreshContext) -> ExternalAuthFuture<'_, CodexAuth> {
        ExternalAuth::resolve(self)
    }
}

impl WorkspaceRoutingResolver for AuthAfterRouting {
    fn resolve(&self, _request: WorkspaceRoutingRequest) -> RoutingFuture<'_> {
        Box::pin(async {
            self.resolved.store(/*val*/ true, Ordering::SeqCst);
            Ok(None)
        })
    }
}

#[tokio::test]
async fn classification_retries_refreshes_but_never_switches_accounts() -> Result<()> {
    core_test_support::skip_if_no_network!(Ok(()));
    // Both tokens identify the same user; their signatures differ after refresh.
    let token =
        "header.eyJodHRwczovL2FwaS5vcGVuYWkuY29tL2F1dGgiOnsiY2hhdGdwdF91c2VyX2lkIjoidXNlci1hIn19";
    for account in ["workspace-a", "workspace-b"] {
        let server = responses::start_mock_server().await;
        let response = responses::mount_sse_once(
            &server,
            responses::sse(vec![
                ev_assistant_message("score", "low"),
                ev_completed("score"),
            ]),
        )
        .await;
        let home = tempfile::tempdir()?;
        let changes = Arc::new(AuthAfterRouting {
            initial: CodexAuth::from_external_chatgpt_tokens(
                &format!("{token}.initial"),
                "workspace-a",
                /*chatgpt_plan_type*/ None,
            )?,
            updated: CodexAuth::from_external_chatgpt_tokens(
                &format!("{token}.refreshed"),
                account,
                /*chatgpt_plan_type*/ None,
            )?,
            resolved: AtomicBool::new(/*v*/ false),
        });
        let auth = AuthManager::from_auth_for_testing_with_home(
            changes.initial.clone(),
            home.path().to_owned(),
        );
        auth.set_external_auth(changes.clone()).await?;
        let resolver: Arc<dyn WorkspaceRoutingResolver> = changes;
        auth.set_workspace_routing_resolver(Arc::downgrade(&resolver));
        let mut config = sampler_config(format!("{}/backend-api/codex", server.uri()));
        let mut provider = config.provider.info().clone();
        provider.requires_openai_auth = true;
        config.provider = create_model_provider(provider, Some(auth));
        let sampler = super::LunaSampler::new(config);
        let result = sampler.sample(sample_request("turn-1")).await;
        if account == "workspace-a" {
            assert_eq!(result?, "low");
            let request = response.single_request();
            assert_eq!(
                (
                    request.header("chatgpt-account-id"),
                    request.header("authorization")
                ),
                (
                    Some("workspace-a".into()),
                    Some(format!("Bearer {token}.refreshed"))
                ),
            );
        } else {
            assert!(result.unwrap_err().to_string().contains("account changed"));
            assert!(
                response.requests().is_empty(),
                "evidence must not reach the new account"
            );
        }
    }
    Ok(())
}

struct ChangedBootstrap(AtomicBool);

impl WorkspaceRoutingResolver for ChangedBootstrap {
    fn resolve(&self, request: WorkspaceRoutingRequest) -> RoutingFuture<'_> {
        Box::pin(async move {
            if request.previously_routed {
                Err(std::io::Error::other(
                    "bootstrap changed; start a new thread",
                ))
            } else if self.0.swap(/*val*/ false, Ordering::SeqCst) {
                Ok(Some(WorkspaceRouting {
                    chatgpt_account_id: "account_id".into(),
                    backend_origin: "https://provider.invalid".into(),
                    account_routing_override: "us_cr".into(),
                }))
            } else {
                Ok(None)
            }
        })
    }
}

#[tokio::test]
async fn idle_classifier_retains_the_owning_threads_workspace_binding() -> Result<()> {
    core_test_support::skip_if_no_network!(Ok(()));
    let server = responses::start_mock_server().await;
    let test = core_test_support::test_codex::test_codex()
        .with_pre_build_hook(|home| {
            std::fs::write(home.join("config.toml"), r#"model_provider = "openai""#).unwrap();
        })
        .build_with_auto_env(&server)
        .await?;
    let auth =
        AuthManager::from_auth_for_testing(CodexAuth::create_dummy_chatgpt_auth_for_testing());
    let resolver: Arc<dyn WorkspaceRoutingResolver> =
        Arc::new(ChangedBootstrap(AtomicBool::new(/*v*/ true)));
    auth.set_workspace_routing_resolver(Arc::downgrade(&resolver));
    let mut config = test.config.clone();
    config.model_provider = ModelProviderInfo {
        requires_openai_auth: true,
        supports_websockets: true,
        ..ModelProviderInfo::create_openai_provider(Some(
            "https://provider.invalid/backend-api/codex".into(),
        ))
    };
    let session_store = codex_extension_api::ExtensionData::new("session-1");
    let input = codex_extension_api::ThreadStartInput {
        config: &config,
        session_source: &codex_protocol::protocol::SessionSource::Exec,
        persistent_thread_state_available: false,
        environments: &[],
        mcp_resource_client: None,
        extension_metrics: None,
        session_store: &session_store,
        thread_store: test.codex.thread_extension_data(),
    };
    let owning_context = input
        .thread_store
        .get::<codex_model_provider::WorkspaceRoutingContext>()
        .expect("Core seeds the owning thread's routing context before extensions start");
    let mut guardian_config = crate::async_scorer::config::GuardianV2Config::resolve(&config).unwrap();
    guardian_config.local_overrides.model = Some("gpt-test".into());
    let classifier = crate::async_scorer::startup::sampler_config(
        &input,
        &guardian_config,
        auth.clone(),
        /*manager*/ None,
    )
    .await
    .expect("classifier config");
    classifier
        .provider
        .responses_api_provider(&owning_context)
        .await?;
    let sampler = super::LunaSampler::new(classifier);
    let previous_requests = server.received_requests().await.unwrap();
    let error = sampler.sample(sample_request("turn-1")).await.unwrap_err();
    assert!(error.to_string().contains("start a new thread"), "{error}");
    let observed = server.received_requests().await.unwrap();
    eprintln!("T19_EVIDENCE {}", serde_json::json!({
        "kind": "same-provider-routing-control", "error": error.to_string(),
        "before_requests": previous_requests.len(),
        "requests": observed.iter().map(|r| serde_json::json!({
            "method": r.method.as_str(), "url": r.url.as_str(),
            "body": String::from_utf8_lossy(&r.body),
        })).collect::<Vec<_>>(),
    }));
    assert_eq!(observed.len(), previous_requests.len());
    Ok(())
}

#[tokio::test]
async fn account_switch_cancels_request_opening_and_first_token_waits() -> Result<()> {
    core_test_support::skip_if_no_network!(Ok(()));
    let token = "header.eyJodHRwczovL2FwaS5vcGVuYWkuY29tL2F1dGgiOnsiY2hhdGdwdF91c2VyX2lkIjoidXNlci1hIn19.signature";
    for transport in ["http", "websocket"] {
        let http = responses::start_mock_server().await;
        let opening = responses::mount_response_once(
            &http,
            responses::sse_response(String::new()).set_delay(Duration::from_secs(/*secs*/ 60)),
        )
        .await;
        let stalled = WebSocketConnectionConfig {
            requests: vec![Vec::new()],
            response_headers: Vec::new(),
            accept_delay: None,
            close_after_requests: false,
        };
        let websocket = responses::start_websocket_server_with_headers(vec![stalled; 2]).await;
        let changes = Arc::new(AuthAfterRouting {
            initial: CodexAuth::from_external_chatgpt_tokens(
                token,
                "workspace-a",
                /*chatgpt_plan_type*/ None,
            )?,
            updated: CodexAuth::from_external_chatgpt_tokens(
                token,
                "workspace-b",
                /*chatgpt_plan_type*/ None,
            )?,
            resolved: AtomicBool::new(/*v*/ false),
        });
        let home = tempfile::tempdir()?;
        let auth = AuthManager::from_auth_for_testing_with_home(
            changes.initial.clone(),
            home.path().to_owned(),
        );
        auth.set_external_auth(changes.clone()).await?;
        let base_url = if transport == "http" {
            format!("{}/v1", http.uri())
        } else {
            proxy_websocket_servers(&[&websocket, &websocket]).await?
        };
        let mut config = sampler_config(base_url);
        let mut provider = config.provider.info().clone();
        provider.requires_openai_auth = true;
        config.provider = create_model_provider(provider, Some(auth.clone()));
        let sampler = if transport == "http" {
            super::LunaSampler::new(config)
        } else {
            connect_sampler(config).await?
        };
        let sample = sampler.sample(sample_request("turn-1"));
        tokio::pin!(sample);
        tokio::time::timeout(Duration::from_secs(/*secs*/ 10), async {
            loop {
                if !opening.requests().is_empty() || websocket.connections().iter().any(|requests| !requests.is_empty()) {
                    break Ok::<(), anyhow::Error>(());
                }
                tokio::select! {
                    result = &mut sample => anyhow::bail!("classification finished before account switch: {result:?}"),
                    () = tokio::task::yield_now() => {}
                }
            }
        }).await??;
        changes.resolved.store(/*val*/ true, Ordering::SeqCst);
        auth.set_external_auth(changes).await?;
        let error = tokio::time::timeout(Duration::from_secs(/*secs*/ 2), &mut sample)
            .await?
            .unwrap_err();
        assert!(
            error.to_string().contains("account changed"),
            "{transport}: {error}"
        );
        assert_eq!(opening.requests().len(), usize::from(transport == "http"));
        assert_eq!(
            websocket.connections().iter().map(Vec::len).sum::<usize>(),
            usize::from(transport == "websocket")
        );
    }
    Ok(())
}

struct StartupRouting {
    calls: std::sync::Mutex<Vec<(String, bool)>>,
}

impl WorkspaceRoutingResolver for StartupRouting {
    fn resolve(&self, request: WorkspaceRoutingRequest) -> RoutingFuture<'_> {
        Box::pin(async move {
            self.calls
                .lock()
                .unwrap()
                .push((request.provider_base_url.clone(), request.previously_routed));
            let bound = request
                .provider_base_url
                .starts_with("https://classifier.invalid/");
            if bound && request.previously_routed {
                return Err(std::io::Error::other(
                    "bound classifier changed after routing",
                ));
            }
            if bound
                || request
                    .provider_base_url
                    .starts_with("https://parent.invalid/")
                || request.previously_routed
            {
                Ok(Some(WorkspaceRouting {
                    chatgpt_account_id: "account_id".into(),
                    backend_origin: if bound {
                        "https://classifier.invalid".into()
                    } else {
                        "https://parent.invalid".into()
                    },
                    account_routing_override: "us_cr".into(),
                }))
            } else {
                Ok(None)
            }
        })
    }
}

#[tokio::test]
async fn independent_classifier_keeps_its_own_workspace_binding() -> Result<()> {
    let bootstrap = responses::start_mock_server().await;
    let target = responses::start_mock_server().await;
    let response = responses::mount_sse_once(
        &target,
        responses::sse(vec![
            ev_assistant_message("independent-score", "low"),
            ev_completed("independent-score"),
        ]),
    )
    .await;
    let home = tempfile::tempdir()?;
    let mut model = codex_models_manager::test_support::model_info("test-model");
    model.slug = "scope-model".into();
    std::fs::write(
        home.path().join("catalog.json"),
        serde_json::to_vec(&codex_protocol::openai_models::ModelsResponse {
            models: vec![model],
        })?,
    )?;
    std::fs::write(
        home.path().join("config.toml"),
        format!(
            r#"
model_provider = "parent"
model = "scope-model"
chatgpt_base_url = "{}/backend-api"
model_catalog_json = "catalog.json"
[features]
api_key_model_discovery = false
[features.guardianv2]
enabled = true
model = "scope-model"
model_provider = "classifier"
[model_providers.parent]
name = "Parent"
base_url = "https://parent.invalid/backend-api/codex"
requires_openai_auth = true
[model_providers.classifier]
name = "Classifier"
base_url = "{}/backend-api/codex"
requires_openai_auth = true
[model_providers.bound]
name = "Bound classifier"
base_url = "https://classifier.invalid/backend-api/codex"
requires_openai_auth = true
"#,
            bootstrap.uri(),
            target.uri()
        ),
    )?;
    let config = codex_core::config::ConfigBuilder::default()
        .codex_home(home.path().to_path_buf())
        .loader_overrides(codex_config::LoaderOverrides::without_managed_config_for_tests())
        .build()
        .await?;
    let auth =
        AuthManager::from_auth_for_testing(CodexAuth::create_dummy_chatgpt_auth_for_testing());
    let routing = Arc::new(StartupRouting {
        calls: std::sync::Mutex::new(Vec::new()),
    });
    let resolver: Arc<dyn WorkspaceRoutingResolver> = routing.clone();
    auth.set_workspace_routing_resolver(Arc::downgrade(&resolver));
    let thread_store = codex_extension_api::ExtensionData::new("routing-parent");
    thread_store.insert(config.workspace_routing_context());
    let session_store = codex_extension_api::ExtensionData::new("routing-session");
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
    let parent_context = thread_store
        .get::<codex_model_provider::WorkspaceRoutingContext>()
        .unwrap();
    let parent = create_model_provider(config.model_provider.clone(), Some(auth.clone()));
    let before = parent.responses_api_provider(&parent_context).await?;
    assert_eq!(
        before.provider.headers["x-openai-account-routing-override"],
        "us_cr"
    );
    let mut guardian = crate::async_scorer::config::GuardianV2Config::resolve(&config).unwrap();
    let selected =
        crate::async_scorer::startup::sampler_config(&input, &guardian, auth.clone(), None)
            .await
            .unwrap();
    let result = super::LunaSampler::new(selected)
        .sample(sample_request("parent-turn"))
        .await;
    let requests = response.requests();
    eprintln!(
        "T19_EVIDENCE {}",
        serde_json::json!({"kind": "independent-classifier-routing",
            "result": format!("{result:?}"), "calls": &*routing.calls.lock().unwrap(),
            "requests": requests.iter().map(|r| serde_json::json!({"body": r.body_json(), "authorization": r.header("authorization"),
                "account_id": r.header("chatgpt-account-id")})).collect::<Vec<_>>(),
        })
    );
    let independent_ok = result.as_ref().is_ok_and(|value| value == "low")
        && requests.len() == 1
        && requests[0].header("authorization").as_deref() == Some("Bearer Access Token")
        && requests[0].header("chatgpt-account-id").as_deref() == Some("account_id");
    guardian.local_overrides.model_provider = Some("bound".into());
    let bound = crate::async_scorer::startup::sampler_config(&input, &guardian, auth, None)
        .await
        .unwrap();
    let first = bound
        .provider
        .responses_api_provider(&bound.workspace_routing)
        .await;
    let second = bound
        .provider
        .responses_api_provider(&bound.workspace_routing)
        .await;
    let after = parent.responses_api_provider(&parent_context).await?;
    let calls = routing.calls.lock().unwrap().clone();
    eprintln!(
        "T19_EVIDENCE {}",
        serde_json::json!({"kind": "classifier-retained-routing",
            "first": format!("{first:?}"), "second": format!("{second:?}"), "calls": &calls,
            "parent_after": after.provider.base_url, "bootstrap_requests": bootstrap.received_requests().await.unwrap().len(),
        })
    );
    assert!(
        independent_ok,
        "independent classifier inherited the parent's routing"
    );
    assert!(first.is_ok());
    assert!(
        second
            .unwrap_err()
            .to_string()
            .contains("bound classifier changed after routing")
    );
    let own: Vec<_> = calls
        .iter()
        .filter(|(url, _)| url.starts_with("https://classifier.invalid/"))
        .map(|(_, previous)| *previous)
        .collect();
    assert_eq!(own, vec![false, true]);
    assert_eq!(calls.last().unwrap().1, true);
    assert!(bootstrap.received_requests().await.unwrap().is_empty());
    Ok(())
}
