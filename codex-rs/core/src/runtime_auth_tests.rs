//! Verifies the supported destinations and environment boundaries for Realtime auth.

#![cfg(test)]

use super::*;
use codex_model_provider::validate_runtime_environment;
use codex_runtime_agent_instance::AgentHostAuthorization;
use codex_runtime_agent_instance::RuntimeRegistrationMetadata;
use codex_runtime_agent_instance::RuntimeRole;
use pretty_assertions::assert_eq;

#[test]
fn realtime_auth_accepts_only_secure_first_party_destinations() {
    for url in [
        "https://api.openai.com/v1",
        "https://api.openai.org/v1",
        "wss://api.openai.com/v1",
        "wss://chatgpt.com/backend-api",
    ] {
        assert!(is_openai_realtime_url(url).unwrap(), "{url}");
    }
    for url in [
        "ws://api.openai.com/v1",
        "http://api.openai.com/v1",
        "wss://unknown.openai.com/v1",
        "wss://api.openai.com:8443/v1",
        "wss://user:secret@api.openai.com/v1",
    ] {
        assert!(is_openai_realtime_url(url).is_err(), "{url}");
    }
    assert!(!is_openai_realtime_url("wss://voice.example.com").unwrap());
}

#[test]
fn realtime_rejects_a_different_registration_environment() {
    for (registration_url, expected_staging) in [
        ("https://auth.openai.com/register", false),
        ("https://auth.api.openai.org/register", true),
    ] {
        let runtime = RuntimeAuthManager::new(
            AuthManager::from_auth_for_testing(codex_login::CodexAuth::from_api_key("test-key")),
            AgentHostAuthorization::InstallationId {
                installation_id: uuid::Uuid::nil(),
            },
            RuntimeRegistrationMetadata {
                harness: None,
                harness_version: None,
                role: RuntimeRole::Orchestrator,
            },
            registration_url.parse().expect("explicit registration URL"),
        );
        for (destination, staging) in [
            ("wss://api.openai.com/v1", false),
            ("https://chatgpt.com/backend-api", false),
            ("wss://api.openai-staging.com/v1", true),
            ("https://api.openai.org/v1", true),
            ("https://workspace.chatgpt-staging.com/backend-api", true),
            ("https://chatgpt-staging.com/backend-api", true),
        ] {
            assert_eq!(
                validate_runtime_environment(&runtime, Some(destination)).is_ok(),
                staging == expected_staging,
                "{registration_url} -> {destination}",
            );
        }
    }
}
