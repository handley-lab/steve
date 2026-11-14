use super::*;
use codex_features::RuntimeAuthMode;
use codex_http_client::OutboundProxyPolicy;
use codex_runtime_agent_instance::AgentHostAuthorization;
use codex_runtime_agent_instance::RuntimeAuthManager;
use codex_runtime_agent_instance::RuntimeRegistrationMetadata;
use codex_runtime_agent_instance::RuntimeRole;
use pretty_assertions::assert_eq;
use serde_json::json;
use wiremock::Mock;
use wiremock::MockServer;
use wiremock::ResponseTemplate;
use wiremock::matchers::method;
use wiremock::matchers::path;

#[tokio::test]
async fn runtime_registration_environment_matches_inference_and_catalog() {
    for mode in [RuntimeAuthMode::DualHeader, RuntimeAuthMode::AssertionOnly] {
        for (registration_base, destination, matching_destination) in [
            (
                "https://auth.openai.com/agent/register",
                Some("https://api.openai-staging.com/v1"),
                "https://eu.api.openai.com/v1",
            ),
            (
                "https://auth.api.openai.org/agent/register",
                Some("https://api.openai.com/v1"),
                "https://api.openai-staging.com/v1",
            ),
            (
                "https://auth.api.openai.org/agent/register",
                None,
                "https://chatgpt-staging.com/backend-api/codex",
            ),
        ] {
            let home = tempfile::tempdir().unwrap();
            // Keep external network access disabled even if destination validation regresses.
            let controller = codex_http_client::NetworkPolicyController::default();
            let factory = HttpClientFactory::new(OutboundProxyPolicy::ReqwestDefault)
                .with_network_policy(controller.policy());
            let credentials = AuthManager::from_api_key(
                "ambient-key",
                codex_login::AuthRuntimeConfig {
                    codex_home: home.path().to_path_buf(),
                    auth_route_config: codex_login::AuthRouteConfig::from_http_client_factory(
                        factory.clone(),
                    ),
                },
            );
            let runtime = Arc::new(RuntimeAuthManager::new(
                Arc::clone(&credentials),
                AgentHostAuthorization::InstallationId {
                    installation_id: "00000000-0000-0000-0000-000000000000".parse().unwrap(),
                },
                RuntimeRegistrationMetadata {
                    harness: None,
                    harness_version: None,
                    role: RuntimeRole::Orchestrator,
                },
                registration_base.parse().unwrap(),
            ));
            crate::validate_runtime_environment(&runtime, Some(matching_destination)).unwrap();
            let info = ModelProviderInfo {
                experimental_bearer_token: Some("provider-key".into()),
                env_http_headers: None,
                ..ModelProviderInfo::create_openai_provider(destination.map(str::to_owned))
            };
            let provider = crate::provider::create_model_provider_with_runtime(
                info.clone(),
                Some(Arc::clone(&credentials)),
                Some((Arc::clone(&runtime), mode)),
            );
            let error = provider.request_auth().await.err().unwrap();
            assert!(matches!(
                error.details(),
                codex_protocol::error::CodexErrorDetails::InvalidRequest(_)
            ));

            let mut catalog_info = info;
            if let Some(destination) = destination {
                catalog_info.base_url = Some(matching_destination.into());
                catalog_info.model_catalog_url = Some(format!("{destination}/models").into());
            }
            let catalog = OpenAiModelsEndpoint::new(
                catalog_info,
                Some(credentials),
                /*gateway_auth_manager*/ None,
            )
            .with_runtime_auth(Some((runtime, mode)));
            let error = catalog.list_models("0.0.0", factory).await.unwrap_err();
            assert!(matches!(
                error.details(),
                codex_protocol::error::CodexErrorDetails::InvalidRequest(_)
            ));
        }
    }
}

#[tokio::test]
async fn unsupported_openai_destinations_do_not_fall_back_to_bearer_auth() {
    for mode in [RuntimeAuthMode::DualHeader, RuntimeAuthMode::AssertionOnly] {
        let server = MockServer::start().await;
        let credentials = AuthManager::from_auth_for_testing(CodexAuth::from_api_key("key-a"));
        let runtime = runtime_manager(&server, Arc::clone(&credentials));
        let factory = HttpClientFactory::new(OutboundProxyPolicy::ReqwestDefault);
        for destination in [
            "https://unknown.api.openai.com/v1",
            "http://api.openai.com/v1",
            "https://api.openai.com:8443/v1",
        ] {
            let mut info = ModelProviderInfo::create_openai_provider(Some(destination.into()));
            info.requires_openai_auth = true;
            let provider = crate::provider::create_model_provider_with_runtime(
                info.clone(),
                Some(Arc::clone(&credentials)),
                Some((Arc::clone(&runtime), mode)),
            );
            let error = provider
                .request_auth()
                .await
                .err()
                .expect("unsupported inference origin");
            assert!(
                error
                    .to_string()
                    .contains("does not support this OpenAI endpoint")
            );
            let mut catalog_info = info;
            catalog_info.base_url = Some("https://api.openai.com/v1".into());
            catalog_info.model_catalog_url = Some(format!("{destination}/models").into());
            let catalog = OpenAiModelsEndpoint::new(
                catalog_info,
                Some(Arc::clone(&credentials)),
                /*gateway_auth_manager*/ None,
            )
            .with_runtime_auth(Some((Arc::clone(&runtime), mode)));
            let error = catalog
                .list_models("0.0.0", factory.clone())
                .await
                .unwrap_err();
            assert!(
                error
                    .to_string()
                    .contains("does not support this OpenAI endpoint")
            );
        }
        // An explicitly configured other provider retains its existing bearer auth.
        let mut info = ModelProviderInfo::create_openai_provider(Some(server.uri()));
        info.requires_openai_auth = true;
        let provider = crate::provider::create_model_provider_with_runtime(
            info,
            Some(credentials),
            Some((runtime, mode)),
        );
        let resolved = provider.request_auth().await.unwrap();
        let headers = resolved
            .auth
            .apply_auth_headers(http::HeaderMap::new())
            .await
            .unwrap();
        assert_eq!(headers["authorization"], "Bearer key-a");
        assert!(!headers.contains_key("x-openai-agent-assertion"));
        assert!(server.received_requests().await.unwrap().is_empty());
    }
}

#[tokio::test]
async fn catalog_timeout_keeps_registration_alive_for_the_next_request() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/register"))
        .respond_with(ResponseTemplate::new(200).set_delay(Duration::from_secs(6)).set_body_json(json!({
            "runtime_agent_instance_id": "runtime-a",
            "agent_host_id": "host-a",
            "actor": {"kind":"platform", "user_id":"user-a", "organization_id":"org-a", "project_id":null},
        })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/models"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"models":[]})))
        .expect(1)
        .mount(&server)
        .await;
    let credentials = AuthManager::from_auth_for_testing(CodexAuth::from_api_key("key-a"));
    let runtime = runtime_manager(&server, Arc::clone(&credentials));
    let mut provider = ModelProviderInfo::create_openai_provider(Some(server.uri()));
    provider.requires_openai_auth = true;
    provider.env_http_headers = None;
    let endpoint = OpenAiModelsEndpoint::new(
        provider,
        Some(credentials),
        /*gateway_auth_manager*/ None,
    )
    .with_runtime_auth(Some((runtime, RuntimeAuthMode::DualHeader)));
    let factory = HttpClientFactory::new(OutboundProxyPolicy::ReqwestDefault);
    let result = timeout(
        Duration::from_secs(10),
        endpoint.list_models("0.0.0", factory.clone()),
    )
    .await
    .expect("catalog timeout must precede the registration client's timeout");
    assert!(matches!(
        result.unwrap_err().details(),
        codex_protocol::error::CodexErrorDetails::RequestTimeout
    ));
    endpoint.list_models("0.0.0", factory).await.unwrap();
    let requests = server.received_requests().await.unwrap();
    assert_eq!(
        requests
            .iter()
            .filter(|request| request.url.path() == "/register")
            .count(),
        1
    );
    server.verify().await;
}

#[tokio::test]
async fn runtime_catalog_rejects_redirects_without_forwarding_credentials_to_another_origin() {
    for mode in [RuntimeAuthMode::DualHeader, RuntimeAuthMode::AssertionOnly] {
        for policy in [
            OutboundProxyPolicy::ReqwestDefault,
            OutboundProxyPolicy::RespectSystemProxy,
        ] {
            let server = MockServer::start().await;
            let target = MockServer::start().await;
            Mock::given(method("POST"))
                .and(path("/register"))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                    "runtime_agent_instance_id": "runtime-a",
                    "agent_host_id": "host-a",
                    "actor": {"kind":"platform", "user_id":"user-a", "organization_id":"org-a", "project_id":null},
                })))
                .expect(1)
                .mount(&server)
                .await;
            Mock::given(method("GET"))
                .and(path("/models"))
                .respond_with(ResponseTemplate::new(307).insert_header("location", "/same-origin"))
                .expect(1)
                .mount(&server)
                .await;
            Mock::given(method("GET"))
                .and(path("/same-origin"))
                .respond_with(
                    ResponseTemplate::new(302)
                        .insert_header("location", format!("{}/models", target.uri())),
                )
                .expect(0)
                .mount(&server)
                .await;
            Mock::given(method("GET"))
                .and(path("/models"))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!({"models": []})))
                .expect(0)
                .mount(&target)
                .await;
            let credentials = AuthManager::from_auth_for_testing(CodexAuth::from_api_key("key-a"));
            let runtime = runtime_manager(&server, Arc::clone(&credentials));
            let mut info = ModelProviderInfo::create_openai_provider(Some(server.uri()));
            info.requires_openai_auth = true;
            info.env_http_headers = None;
            let endpoint = OpenAiModelsEndpoint::new(
                info,
                Some(credentials),
                /*gateway_auth_manager*/ None,
            )
            .with_runtime_auth(Some((runtime, mode)));
            let error = endpoint
                .list_models("test", HttpClientFactory::new(policy))
                .await
                .expect_err("catalog redirects must be rejected");
            assert_eq!(error.http_status_code_value(), Some(307));
            let requests = server.received_requests().await.unwrap();
            let initial = requests
                .iter()
                .find(|request| request.url.path() == "/models")
                .unwrap();
            assert!(
                !requests
                    .iter()
                    .any(|request| request.url.path() == "/same-origin")
            );
            match mode {
                RuntimeAuthMode::DualHeader => {
                    assert_eq!(initial.headers["authorization"], "Bearer key-a");
                    assert!(initial.headers.contains_key("x-openai-agent-assertion"));
                }
                RuntimeAuthMode::AssertionOnly => {
                    assert!(
                        initial.headers["authorization"]
                            .to_str()
                            .unwrap()
                            .starts_with("AgentAssertion ")
                    );
                    assert!(!initial.headers.contains_key("x-openai-agent-assertion"));
                }
            }
            let redirected = target.received_requests().await.unwrap();
            assert!(redirected.is_empty());
            server.verify().await;
            target.verify().await;
        }
    }
}

fn runtime_manager(server: &MockServer, credentials: Arc<AuthManager>) -> Arc<RuntimeAuthManager> {
    Arc::new(RuntimeAuthManager::new(
        credentials,
        AgentHostAuthorization::InstallationId {
            installation_id: "00000000-0000-0000-0000-000000000000".parse().unwrap(),
        },
        RuntimeRegistrationMetadata {
            harness: None,
            harness_version: None,
            role: RuntimeRole::Orchestrator,
        },
        format!("{}/register", server.uri()).parse().unwrap(),
    ))
}
