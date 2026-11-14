//! Verify the backend client resolves a fresh runtime proof for each HTTP request.

use super::*;
use codex_features::RuntimeAuthMode;
use codex_http_client::DestinationPolicy;
use codex_http_client::NetworkPolicyController;
use codex_http_client::NetworkPolicyDenied;
use codex_http_client::OutboundProxyPolicy;
use codex_login::AuthCredentialsStoreMode;
use codex_login::AuthKeyringBackendKind;
use codex_login::AuthManager;
use codex_login::AuthRouteConfig;
use codex_protocol::error::CodexErr;
use codex_protocol::error::CodexErrorDetails;
use codex_runtime_agent_instance::AgentHostAuthorization;
use codex_runtime_agent_instance::RuntimeAuthManager;
use codex_runtime_agent_instance::RuntimeRegistrationMetadata;
use codex_runtime_agent_instance::RuntimeRole;
use pretty_assertions::assert_eq;
use std::sync::Arc;
use uuid::Uuid;
use wiremock::Mock;
use wiremock::MockServer;
use wiremock::ResponseTemplate;
use wiremock::matchers::method;
use wiremock::matchers::path;

fn runtime_provider(
    server: &MockServer,
    credentials: Arc<AuthManager>,
    mode: RuntimeAuthMode,
) -> (Arc<RuntimeAuthManager>, RuntimeAuthMode) {
    (
        Arc::new(RuntimeAuthManager::new(
            credentials,
            AgentHostAuthorization::InstallationId {
                installation_id: Uuid::nil(),
            },
            RuntimeRegistrationMetadata {
                harness: Some("codex".into()),
                harness_version: None,
                role: RuntimeRole::Orchestrator,
            },
            format!("{}/register", server.uri()).parse().unwrap(),
        )),
        mode,
    )
}

#[tokio::test]
async fn client_registers_in_requested_organization_when_default_is_denied() {
    for mode in [RuntimeAuthMode::DualHeader, RuntimeAuthMode::AssertionOnly] {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/register"))
            .respond_with(|request: &wiremock::Request| {
                let body: serde_json::Value = request.body_json().unwrap();
                if body["actor_auth"]["organization_id"] != "org-b" {
                    return ResponseTemplate::new(403);
                }
                ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "runtime_agent_instance_id": "runtime-org-b",
                    "agent_host_id": "host-a",
                    "actor": {
                        "kind": "platform",
                        "user_id": "user-a",
                        "organization_id": "org-b",
                        "project_id": null,
                    },
                }))
            })
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/turn-costs"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({"turns": []})),
            )
            .expect(1)
            .mount(&server)
            .await;
        let auth = CodexAuth::from_api_key("key-a");
        let runtime = runtime_provider(
            &server,
            AuthManager::from_auth_for_testing(auth.clone()),
            mode,
        );
        let headers = HeaderMap::from_iter([(
            HeaderName::from_static("openai-organization"),
            HeaderValue::from_static("org-b"),
        )]);
        for name in ["openai-organization", "openai-project"] {
            let mut invalid_headers = headers.clone();
            invalid_headers.insert(
                HeaderName::from_static(name),
                HeaderValue::from_bytes(b"\xff")
                    .expect("non-ASCII selector is a valid header value"),
            );
            assert!(
                Client::from_auth_with_selected_runtime(
                    server.uri(),
                    &auth,
                    HttpClientFactory::new(OutboundProxyPolicy::ReqwestDefault),
                    Some(&runtime),
                    &invalid_headers,
                    Client::new,
                )
                .await
                .is_err(),
                "invalid selectors must not fall back to the default resource"
            );
        }
        let client = Client::from_auth_with_selected_runtime(
            server.uri(),
            &auth,
            HttpClientFactory::new(OutboundProxyPolicy::ReqwestDefault),
            Some(&runtime),
            &headers,
            Client::new,
        )
        .await
        .unwrap();
        client
            .query_api_key_turn_costs_at(&format!("{}/turn-costs", server.uri()), &[], &headers)
            .await
            .unwrap();
        let requests = server.received_requests().await.unwrap();
        assert_eq!(requests.len(), 2);
        assert_eq!(requests[1].url.path(), "/turn-costs");
        assert_eq!(requests[1].headers["openai-organization"], "org-b");
        let authorization = requests[1].headers["authorization"].to_str().unwrap();
        match mode {
            RuntimeAuthMode::DualHeader => {
                assert_eq!(authorization, "Bearer key-a");
                assert!(requests[1].headers.contains_key("x-openai-agent-assertion"));
            }
            RuntimeAuthMode::AssertionOnly => {
                assert!(authorization.starts_with("AgentAssertion "));
                assert!(!requests[1].headers.contains_key("x-openai-agent-assertion"));
            }
        }
        server.verify().await;
    }
}

#[tokio::test]
async fn unsupported_openai_backend_rejects_runtime_auth_before_registration() {
    for mode in [RuntimeAuthMode::DualHeader, RuntimeAuthMode::AssertionOnly] {
        let server = MockServer::start().await;
        let auth = CodexAuth::from_api_key("key-a");
        let runtime = runtime_provider(
            &server,
            AuthManager::from_auth_for_testing(auth.clone()),
            mode,
        );
        let result = Client::from_auth_with_runtime(
            "https://future.openai.com/backend-api",
            &auth,
            HttpClientFactory::new(OutboundProxyPolicy::ReqwestDefault),
            Some(&runtime),
            &HeaderMap::new(),
        )
        .await;
        let Err(error) = result else {
            panic!("unsupported OpenAI backend accepted runtime auth");
        };
        assert!(matches!(
            error.downcast_ref::<CodexErr>().map(CodexErr::details),
            Some(CodexErrorDetails::UnsupportedOperation(_))
        ));
        assert!(server.received_requests().await.unwrap().is_empty());
    }
}

#[tokio::test]
async fn custom_backend_keeps_its_credential_without_registering_a_runtime() {
    for mode in [RuntimeAuthMode::DualHeader, RuntimeAuthMode::AssertionOnly] {
        let server = MockServer::start().await;
        Mock::given(path("/turn-costs"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({"turns": []})),
            )
            .expect(1)
            .mount(&server)
            .await;
        let auth = CodexAuth::from_api_key("custom-backend-key");
        let runtime = runtime_provider(
            &server,
            AuthManager::from_auth_for_testing(auth.clone()),
            mode,
        );
        let client = Client::from_auth_with_runtime(
            server.uri(),
            &auth,
            HttpClientFactory::new(OutboundProxyPolicy::ReqwestDefault),
            Some(&runtime),
            &HeaderMap::new(),
        )
        .await
        .unwrap();
        client
            .query_api_key_turn_costs_at(
                &format!("{}/turn-costs", server.uri()),
                &[],
                &HeaderMap::new(),
            )
            .await
            .unwrap();
        let requests = server.received_requests().await.unwrap();
        assert_eq!(requests.len(), 1);
        assert_eq!(
            requests[0].headers["authorization"],
            "Bearer custom-backend-key"
        );
        assert!(!requests[0].headers.contains_key("x-openai-agent-assertion"));
        server.verify().await;
    }
}

#[tokio::test]
async fn reusable_runtime_client_keeps_the_selected_accounts_transport_policy() {
    let server = MockServer::start().await;
    Mock::given(path("/register"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "runtime_agent_instance_id": "runtime-policy", "agent_host_id": "host-a",
            "actor": {"kind": "platform", "user_id": "user-a", "organization_id": "org-a", "project_id": null},
        })))
        .expect(1).mount(&server).await;
    Mock::given(path("/turn-costs"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"turns": []})))
        .expect(2)
        .mount(&server)
        .await;
    let controller = NetworkPolicyController::default();
    let policy = controller.policy();
    let home = tempfile::tempdir().unwrap();
    codex_login::login_with_api_key(
        home.path(),
        "key-a",
        AuthCredentialsStoreMode::Ephemeral,
        AuthKeyringBackendKind::Direct,
    )
    .unwrap();
    let credentials = Arc::new(
        AuthManager::new(
            home.path().to_path_buf(),
            /*enable_codex_api_key_env*/ false,
            AuthCredentialsStoreMode::Ephemeral,
            /*forced_chatgpt_workspace_id*/ None,
            /*chatgpt_base_url*/ None,
            AuthKeyringBackendKind::Direct,
            AuthRouteConfig::from_http_client_factory(
                HttpClientFactory::new(OutboundProxyPolicy::ReqwestDefault)
                    .with_network_policy(policy.clone()),
            ),
        )
        .await,
    );
    assert!(controller.publish(policy.revision(), DestinationPolicy::Unrestricted));
    let auth = credentials.auth().await.unwrap();
    let runtime = runtime_provider(&server, credentials, RuntimeAuthMode::AssertionOnly);
    let factory = HttpClientFactory::new(OutboundProxyPolicy::ReqwestDefault);
    let client = Client::from_auth_with_selected_runtime(
        server.uri(),
        &auth,
        factory.clone(),
        Some(&runtime),
        &HeaderMap::new(),
        Client::new,
    )
    .await
    .unwrap();
    let url = format!("{}/turn-costs", server.uri());
    client
        .query_api_key_turn_costs_at(&url, &[], &HeaderMap::new())
        .await
        .unwrap();

    policy.invalidate();
    assert!(controller.publish(policy.revision(), DestinationPolicy::Unrestricted));
    let error = client
        .query_api_key_turn_costs_at(&url, &[], &HeaderMap::new())
        .await
        .unwrap_err();
    assert!(matches!(
        error,
        RequestError::Policy(NetworkPolicyDenied::Revoked)
    ));

    let fresh = Client::from_auth_with_selected_runtime(
        server.uri(),
        &auth,
        factory,
        Some(&runtime),
        &HeaderMap::new(),
        Client::new,
    )
    .await
    .unwrap();
    fresh
        .query_api_key_turn_costs_at(&url, &[], &HeaderMap::new())
        .await
        .unwrap();

    policy.invalidate();
    assert!(controller.publish(
        policy.revision(),
        DestinationPolicy::Restricted {
            allowed_hosts: Default::default()
        }
    ));
    let selectors = HeaderMap::from_iter([(
        HeaderName::from_static("openai-organization"),
        HeaderValue::from_static("org-b"),
    )]);
    let error = fresh
        .query_api_key_turn_costs_at(&url, &[], &selectors)
        .await
        .unwrap_err();
    assert!(
        matches!(
            error,
            RequestError::Policy(NetworkPolicyDenied::Destination)
        ),
        "{error}"
    );
    server.verify().await;
}
