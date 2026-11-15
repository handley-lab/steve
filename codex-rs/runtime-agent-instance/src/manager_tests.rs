//! Exercise shared bootstrap against HTTP and the existing external credential-provider boundary.

use super::*;
use crate::RuntimeRole;
use codex_api::AuthError;
use codex_features::RuntimeAuthMode;
use codex_http_client::DestinationPolicy;
use codex_http_client::HttpClientFactory;
use codex_http_client::NetworkPolicyController;
use codex_http_client::NetworkPolicyDenied;
use codex_http_client::OutboundProxyPolicy;
use codex_login::AuthCredentialsStoreMode;
use codex_login::AuthKeyringBackendKind;
use codex_login::AuthRouteConfig;
use codex_login::ExternalAuth;
use codex_login::ExternalAuthFuture;
use codex_login::ExternalAuthRefreshContext;
use http::HeaderMap;
use pretty_assertions::assert_eq;
use serde_json::Value;
use serde_json::json;
use std::sync::RwLock;
use std::time::Duration;
use tokio::sync::Notify;
use uuid::Uuid;
use wiremock::Mock;
use wiremock::MockServer;
use wiremock::Request;
use wiremock::ResponseTemplate;
use wiremock::matchers::body_partial_json;
use wiremock::matchers::method;

struct Credentials {
    available: std::sync::atomic::AtomicBool,
    current: RwLock<CodexAuth>,
    refreshed: CodexAuth,
}

impl ExternalAuth for Credentials {
    fn resolve(&self) -> ExternalAuthFuture<'_, CodexAuth> {
        Box::pin(async {
            if !self.available.load(std::sync::atomic::Ordering::SeqCst) {
                return Err(std::io::Error::other("credential source unavailable"));
            }
            Ok(self.current.read().unwrap().clone())
        })
    }

    fn refresh(&self, _: ExternalAuthRefreshContext) -> ExternalAuthFuture<'_, CodexAuth> {
        Box::pin(async {
            *self.current.write().unwrap() = self.refreshed.clone();
            Ok(self.refreshed.clone())
        })
    }
}

async fn setup(
    server: &MockServer,
    initial: CodexAuth,
    refreshed: CodexAuth,
) -> (Arc<RuntimeAuthManager>, Arc<Credentials>, Arc<AuthManager>) {
    let credentials = AuthManager::from_auth_for_testing(initial.clone());
    let source = Arc::new(Credentials {
        available: std::sync::atomic::AtomicBool::new(true),
        current: RwLock::new(initial),
        refreshed,
    });
    credentials.set_external_auth(source.clone()).await.unwrap();
    let manager = RuntimeAuthManager::new(
        credentials.clone(),
        AgentHostAuthorization::InstallationId {
            installation_id: Uuid::nil(),
        },
        RuntimeRegistrationMetadata {
            harness: Some("codex".into()),
            harness_version: None,
            role: RuntimeRole::OrchestratorAndExecutor,
        },
        format!("{}/agent/register", server.uri()).parse().unwrap(),
    );
    (Arc::new(manager), source, credentials)
}

fn registered(request: &Request) -> ResponseTemplate {
    let body: Value = request.body_json().unwrap();
    let actor = if body["actor_auth"]["type"] == "oauth" {
        json!({"kind": "chatgpt", "user_id": "user-a", "account_id": "workspace-a"})
    } else {
        json!({"kind": "platform", "user_id": "user-a",
            "organization_id": body["actor_auth"]["organization_id"].as_str().unwrap_or("org-a"),
            "project_id": body["actor_auth"]["project_id"].as_str().unwrap_or("project-a")})
    };
    ResponseTemplate::new(200).set_body_json(json!({
        "runtime_agent_instance_id": request.headers["idempotency-key"].to_str().unwrap(),
        "agent_host_id": "host-a", "actor": actor,
    }))
}

#[tokio::test]
async fn concurrent_requests_share_registration_and_replacement_preserves_in_flight_auth() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(503))
        .up_to_n_times(1)
        .with_priority(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .respond_with(registered)
        .mount(&server)
        .await;
    let initial = CodexAuth::from_api_key("key-a");
    let (manager, source, _) = setup(&server, initial.clone(), initial).await;
    assert!(manager.auth().await.is_err());
    let (first, second) = tokio::join!(manager.auth(), manager.auth());
    let first = first.unwrap();
    let second = second.unwrap();
    assert!(Arc::ptr_eq(first.runtime(), second.runtime()));
    *source.current.write().unwrap() = CodexAuth::from_api_key("key-b");
    let replacement = manager.auth().await.unwrap();
    assert_ne!(first.runtime().id(), replacement.runtime().id());
    assert_eq!(first.auth().get_token().unwrap(), "key-a");
    assert!(!format!("{first:?}").contains("key-a"));
    assert_eq!(replacement.auth().get_token().unwrap(), "key-b");
    assert_eq!(
        first
            .clone()
            .into_auth_provider(RuntimeAuthMode::DualHeader)
            .apply_auth_headers(HeaderMap::new())
            .await
            .unwrap()["authorization"],
        "Bearer key-a"
    );
    assert_eq!(
        replacement
            .clone()
            .into_auth_provider(RuntimeAuthMode::DualHeader)
            .apply_auth_headers(HeaderMap::new())
            .await
            .unwrap()["authorization"],
        "Bearer key-b"
    );
    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 3);
    assert_eq!(requests[0].body, requests[1].body);
    assert_eq!(
        requests[0].headers["idempotency-key"],
        requests[1].headers["idempotency-key"]
    );
    assert_ne!(
        requests[1].body_json::<Value>().unwrap()["public_key"],
        requests[2].body_json::<Value>().unwrap()["public_key"]
    );
}

#[tokio::test]
async fn projects_share_one_runtime_and_organization_changes_replace_it() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(registered)
        .expect(3)
        .mount(&server)
        .await;
    let initial = CodexAuth::from_api_key("key-a");
    let (manager, _, _) = setup(&server, initial.clone(), initial).await;
    let mut selection = PlatformResourceSelection {
        organization_id: Some("org-a".into()),
        project_id: Some("project-a".into()),
    };
    let first = manager.auth_with_selection(&selection).await.unwrap();
    for project in [Some("project-b"), None, Some("project-a")] {
        selection.project_id = project.map(str::to_owned);
        let next = manager.auth_with_selection(&selection).await.unwrap();
        assert!(Arc::ptr_eq(first.runtime(), next.runtime()));
        let captured = manager.auth_for(first.auth(), &selection).await.unwrap();
        assert!(Arc::ptr_eq(first.runtime(), captured.runtime()));
    }
    selection.organization_id = Some("org-b".into());
    let other_org = manager.auth_with_selection(&selection).await.unwrap();
    assert_ne!(first.runtime().id(), other_org.runtime().id());
    selection.organization_id = Some("org-a".into());
    let replacement = manager.auth_with_selection(&selection).await.unwrap();
    assert_ne!(first.runtime().id(), replacement.runtime().id());
    assert_ne!(other_org.runtime().id(), replacement.runtime().id());
    server.verify().await;
}

#[tokio::test]
async fn failed_registration_does_not_poison_another_project_or_reuse_its_idempotency_key() {
    for status in [403, 503] {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(body_partial_json(
                json!({"actor_auth": {"project_id": "denied"}}),
            ))
            .respond_with(ResponseTemplate::new(status))
            .with_priority(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .respond_with(registered)
            .expect(1)
            .mount(&server)
            .await;
        let initial = CodexAuth::from_api_key("key-a");
        let (manager, _, _) = setup(&server, initial.clone(), initial).await;
        let mut selection = PlatformResourceSelection {
            organization_id: Some("org-a".into()),
            project_id: Some("denied".into()),
        };
        for _ in 0..2 {
            assert!(manager.auth_with_selection(&selection).await.is_err());
        }
        let failed = server.received_requests().await.unwrap();
        assert_eq!(failed.len(), if status == 403 { 1 } else { 2 });
        if status == 503 {
            assert_eq!(failed[0].body, failed[1].body);
            assert_eq!(
                failed[0].headers["idempotency-key"],
                failed[1].headers["idempotency-key"]
            );
        }

        selection.project_id = Some("allowed".into());
        let ready = manager.auth_with_selection(&selection).await.unwrap();
        assert_ne!(
            ready.runtime().id(),
            failed[0].headers["idempotency-key"].to_str().unwrap()
        );
        selection.project_id = Some("denied".into());
        let reused = manager.auth_with_selection(&selection).await.unwrap();
        assert!(Arc::ptr_eq(ready.runtime(), reused.runtime()));
        server.verify().await;
    }
}

#[tokio::test]
async fn policy_recovery_preserves_pending_registration() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(503))
        .up_to_n_times(1)
        .with_priority(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .respond_with(registered)
        .mount(&server)
        .await;
    let controller = NetworkPolicyController::default();
    let policy = controller.policy();
    let home = tempfile::tempdir().unwrap();
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
    let auth = CodexAuth::from_api_key("key-a");
    credentials
        .set_external_auth(Arc::new(Credentials {
            available: std::sync::atomic::AtomicBool::new(true),
            current: RwLock::new(auth.clone()),
            refreshed: auth,
        }))
        .await
        .unwrap();
    assert!(controller.publish(policy.revision(), DestinationPolicy::Unrestricted));
    let manager = Arc::new(RuntimeAuthManager::new(
        credentials,
        AgentHostAuthorization::InstallationId {
            installation_id: Uuid::nil(),
        },
        RuntimeRegistrationMetadata {
            harness: Some("codex".into()),
            harness_version: None,
            role: RuntimeRole::OrchestratorAndExecutor,
        },
        format!("{}/agent/register", server.uri()).parse().unwrap(),
    ));
    assert!(matches!(
        manager.auth().await.map_err(AuthError::from),
        Err(AuthError::Transient(_))
    ));

    policy.invalidate();
    assert!(matches!(
        manager.auth().await.map_err(AuthError::from),
        Err(AuthError::Policy(NetworkPolicyDenied::Unavailable))
    ));
    assert_eq!(server.received_requests().await.unwrap().len(), 1);

    assert!(controller.publish(policy.revision(), DestinationPolicy::Unrestricted));
    let selected = manager.auth().await.unwrap();
    let headers = selected
        .clone()
        .into_auth_provider(RuntimeAuthMode::DualHeader)
        .apply_auth_headers(HeaderMap::new())
        .await
        .unwrap();
    assert_eq!(headers["authorization"], "Bearer key-a");
    assert!(headers.contains_key("x-openai-agent-assertion"));
    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0].body, requests[1].body);
    assert_eq!(
        requests[0].headers["idempotency-key"],
        requests[1].headers["idempotency-key"]
    );
    assert_eq!(
        selected.runtime().id(),
        requests[1].headers["idempotency-key"].to_str().unwrap()
    );
}

#[tokio::test]
async fn a_late_registration_cannot_publish_the_previous_actor() {
    let server = MockServer::start().await;
    let received = Arc::new(Notify::new());
    let signal = received.clone();
    Mock::given(method("POST"))
        .respond_with(move |request: &Request| {
            signal.notify_one();
            registered(request).set_delay(Duration::from_secs(1))
        })
        .mount(&server)
        .await;
    let initial = CodexAuth::from_api_key("key-a");
    let (manager, source, credentials) = setup(&server, initial.clone(), initial).await;
    let task_manager = manager.clone();
    let attempt = tokio::spawn(async move { task_manager.auth().await });
    received.notified().await;
    *source.current.write().unwrap() = CodexAuth::from_api_key("key-b");
    credentials.auth().await.unwrap();
    assert!(matches!(
        attempt.await.unwrap(),
        Err(RuntimeAuthError::Changed)
    ));
    let current = manager.auth().await.unwrap();
    assert_eq!(current.auth().get_token().unwrap(), "key-b");
    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 2);
    assert_ne!(
        requests[0].headers["idempotency-key"],
        requests[1].headers["idempotency-key"]
    );
}

#[tokio::test]
async fn registration_failure_reports_retryability() {
    for status in [403, 404, 429, 503] {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(status))
            .mount(&server)
            .await;
        let initial = CodexAuth::from_api_key("key-a");
        let (manager, _, _) = setup(&server, initial.clone(), initial).await;
        let error = AuthError::from(manager.auth().await.unwrap_err());
        assert_eq!(
            matches!(error, AuthError::Transient(_)),
            status == 429 || status == 503
        );
    }
}

#[tokio::test]
async fn explicit_keys_reuse_their_owner_without_borrowing_a_command_source() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(registered)
        .mount(&server)
        .await;
    let (root, _, _) = setup(
        &server,
        CodexAuth::from_api_key("ambient"),
        CodexAuth::from_api_key("ambient"),
    )
    .await;
    let command_credentials =
        AuthManager::from_auth_for_testing(CodexAuth::from_api_key("shared-key"));
    let command = Arc::new(Credentials {
        available: std::sync::atomic::AtomicBool::new(true),
        current: RwLock::new(CodexAuth::from_api_key("shared-key")),
        refreshed: CodexAuth::from_api_key("shared-key"),
    });
    command_credentials
        .set_external_auth(command.clone())
        .await
        .unwrap();
    let context = command_credentials.runtime_config();
    let command_runtime = root.bind(Arc::clone(&command_credentials));
    let original_command = command_runtime.auth().await.unwrap();
    command
        .available
        .store(false, std::sync::atomic::Ordering::SeqCst);

    let explicit = root
        .bind_api_key("shared-key", &context)
        .auth()
        .await
        .unwrap();
    assert_eq!(explicit.auth().api_key(), Some("shared-key"));
    assert_ne!(explicit.runtime().id(), original_command.runtime().id());
    let explicit_id = explicit.runtime().id().to_string();
    drop(explicit);
    let reused = root
        .bind_api_key("shared-key", &context)
        .auth()
        .await
        .unwrap();
    assert_eq!(reused.runtime().id(), explicit_id);
    assert_eq!(
        command_runtime.auth().await.unwrap().runtime().id(),
        original_command.runtime().id()
    );

    command
        .available
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let recovered = root.bind(command_credentials).auth().await.unwrap();
    assert_eq!(recovered.runtime().id(), original_command.runtime().id());
    *command.current.write().unwrap() = CodexAuth::from_api_key("replacement");
    assert_ne!(
        command_runtime.auth().await.unwrap().runtime().id(),
        original_command.runtime().id()
    );
    assert_eq!(
        root.bind_api_key("shared-key", &context)
            .auth()
            .await
            .unwrap()
            .runtime()
            .id(),
        explicit_id
    );
    assert_ne!(
        root.bind_api_key("other-key", &context)
            .auth()
            .await
            .unwrap()
            .runtime()
            .id(),
        explicit_id
    );
    assert_eq!(server.received_requests().await.unwrap().len(), 4);
}

#[tokio::test]
async fn explicit_key_stays_independent_when_matching_saved_login_changes() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(registered)
        .expect(2)
        .mount(&server)
        .await;
    let home = tempfile::tempdir().unwrap();
    let credentials = AuthManager::from_auth_for_testing_with_home(
        CodexAuth::from_api_key("shared-key"),
        home.path().to_path_buf(),
    );
    let manager = Arc::new(RuntimeAuthManager::new(
        credentials.clone(),
        AgentHostAuthorization::InstallationId {
            installation_id: Uuid::nil(),
        },
        RuntimeRegistrationMetadata {
            harness: None,
            harness_version: None,
            role: RuntimeRole::Orchestrator,
        },
        format!("{}/agent/register", server.uri()).parse().unwrap(),
    ));
    let explicit = manager.bind_api_key("shared-key", &credentials.runtime_config());
    let initial = explicit.auth().await.unwrap();
    codex_login::login_with_api_key(
        home.path(),
        "replacement",
        AuthCredentialsStoreMode::File,
        AuthKeyringBackendKind::Direct,
    )
    .unwrap();
    credentials.reload().await;

    let selected = explicit.auth().await.unwrap();
    assert_eq!(selected.auth().api_key(), Some("shared-key"));
    assert_eq!(selected.runtime().id(), initial.runtime().id());
    assert_eq!(
        manager.auth().await.unwrap().auth().api_key(),
        Some("replacement")
    );
    assert_eq!(
        manager
            .bind_api_key("shared-key", &credentials.runtime_config())
            .auth()
            .await
            .unwrap()
            .runtime()
            .id(),
        initial.runtime().id(),
    );
    server.verify().await;
}
