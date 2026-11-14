//! Exercise selected transport revocation with runtime auth enabled.
#![cfg(test)]

use super::*;
use crate::client::ClientRouting;
use pretty_assertions::assert_eq;

#[test_case::test_case(RuntimeAuthMode::DualHeader; "dual_header")]
#[test_case::test_case(RuntimeAuthMode::AssertionOnly; "assertion_only")]
#[tokio::test]
async fn request_owner_tracks_the_selected_explicit_key(mode: RuntimeAuthMode) {
    let server = MockServer::start().await;
    Mock::given(path("/register"))
        .respond_with(|request: &wiremock::Request| ResponseTemplate::new(200).set_body_json(json!({
            "runtime_agent_instance_id": request.headers["idempotency-key"].to_str().unwrap(),
            "agent_host_id": "host-a",
            "actor": {"kind": "platform", "user_id": "user-a", "organization_id": "org-a", "project_id": null},
        })))
        .expect(2)
        .mount(&server).await;
    let credentials = AuthManager::from_auth_for_testing(CodexAuth::from_api_key("saved-key"));
    let context = codex_model_provider::ProviderAuthContext {
        runtime_auth: Some((
            Arc::new(RuntimeAuthManager::new(
                credentials.clone(),
                AgentHostAuthorization::InstallationId {
                    installation_id: uuid::Uuid::nil(),
                },
                RuntimeRegistrationMetadata {
                    harness: None,
                    harness_version: None,
                    role: RuntimeRole::Orchestrator,
                },
                format!("{}/register", server.uri()).parse().unwrap(),
            )),
            mode,
        )),
        credentials,
    };
    let mut owners = Vec::new();
    for key in ["key-a", "key-b", "key-a"] {
        let mut info = ModelProviderInfo::create_openai_provider(/*base_url*/ None);
        info.experimental_bearer_token = Some(key.into());
        let mut client = test_model_client(SessionSource::Exec);
        Arc::get_mut(&mut client.state).unwrap().provider = context.model_provider(info);
        let owner = client.auth_owner();
        let setup = client
            .current_client_setup(ClientRouting::ConfiguredProvider)
            .await
            .unwrap();
        assert!(owner.is_some());
        assert_eq!(setup.auth_owner, owner);
        assert_eq!(setup.auth.unwrap().api_key(), Some(key));
        let mut headers = http::HeaderMap::new();
        setup.api_auth.add_auth_headers(&mut headers).unwrap();
        match mode {
            RuntimeAuthMode::DualHeader => {
                assert_eq!(
                    headers[http::header::AUTHORIZATION],
                    format!("Bearer {key}")
                );
                assert!(headers.contains_key("x-openai-agent-assertion"));
            }
            RuntimeAuthMode::AssertionOnly => {
                assert!(
                    headers[http::header::AUTHORIZATION]
                        .to_str()
                        .unwrap()
                        .starts_with("AgentAssertion ")
                );
                assert!(!headers.contains_key("x-openai-agent-assertion"));
            }
        }
        owners.push(owner);
    }
    assert_ne!(
        owners[0], owners[1],
        "different keys cannot share turn or connection ownership"
    );
    assert_eq!(
        owners[0], owners[2],
        "reselecting a key preserves its owner"
    );
    server.verify().await;
}
