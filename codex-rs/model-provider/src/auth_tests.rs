//! Unit tests for the auth module.

use codex_login::auth::BedrockApiKeyAuth;
use codex_model_provider_info::WireApi;
use codex_model_provider_info::create_oss_provider_with_base_url;
use codex_protocol::config_types::ModelProviderAuthInfo;
use codex_protocol::error::CodexErrorDetails;
use http::HeaderValue;
use http::header::AUTHORIZATION;
use pretty_assertions::assert_eq;
use std::num::NonZeroU64;

use super::*;

#[test]
fn unauthenticated_auth_provider_adds_no_headers() {
    let provider =
        create_oss_provider_with_base_url("http://localhost:11434/v1", WireApi::Responses);
    let auth = resolve_provider_auth(/*auth*/ None, &provider).expect("auth should resolve");

    assert!(auth.to_auth_headers().unwrap().is_empty());
}

#[test]
fn custom_provider_does_not_inherit_ambient_bedrock_auth() {
    let provider =
        create_oss_provider_with_base_url("http://localhost:11434/v1", WireApi::Responses);
    let ambient_auth = CodexAuth::BedrockApiKey(BedrockApiKeyAuth {
        api_key: "bedrock-api-key-test".to_string(),
        region: "us-east-1".to_string(),
    });

    let auth = resolve_provider_auth(Some(&ambient_auth), &provider).expect("auth should resolve");

    assert!(auth.to_auth_headers().unwrap().is_empty());
}

#[test]
fn custom_provider_uses_explicit_bearer_instead_of_ambient_auth() {
    let mut provider =
        create_oss_provider_with_base_url("http://localhost:11434/v1", WireApi::Responses);
    provider.experimental_bearer_token = Some("provider-token".into());
    let ambient_auth = CodexAuth::BedrockApiKey(BedrockApiKeyAuth {
        api_key: "bedrock-api-key-test".to_string(),
        region: "us-east-1".to_string(),
    });

    let headers = resolve_provider_auth(Some(&ambient_auth), &provider)
        .expect("auth should resolve")
        .to_auth_headers()
        .unwrap();

    assert_eq!(
        headers.get(AUTHORIZATION),
        Some(&HeaderValue::from_static("Bearer provider-token"))
    );
    assert_eq!(headers.len(), 1);
}

#[test]
fn api_key_fedramp_routing_follows_the_selected_provider_credential() {
    let project_key = "sk-proj-fedramp-synthetic-test-key";
    let service_account_key = "sk-svcacct-fedramp-synthetic-test-key";
    let legacy_key = "sk-proj-synthetic-test-key";

    for (ambient_key, provider_key, expected_fedramp) in [
        (project_key, None, true),
        (service_account_key, None, true),
        (legacy_key, None, false),
        (project_key, Some(legacy_key), false),
        (legacy_key, Some(project_key), true),
        (legacy_key, Some(service_account_key), true),
    ] {
        let mut provider = ModelProviderInfo::create_openai_provider(/*base_url*/ None);
        provider.requires_openai_auth = true;
        provider.experimental_bearer_token = provider_key.map(Into::into);
        let ambient_auth = CodexAuth::from_api_key(ambient_key);

        let headers = resolve_provider_auth(Some(&ambient_auth), &provider)
            .expect("auth should resolve")
            .to_auth_headers()
            .unwrap();

        let key = provider_key.unwrap_or(ambient_key);
        let mut expected_headers = HeaderMap::from_iter([(
            AUTHORIZATION,
            HeaderValue::from_str(&format!("Bearer {key}")).unwrap(),
        )]);
        if expected_fedramp {
            expected_headers.insert("X-OpenAI-Fedramp", HeaderValue::from_static("true"));
        }
        assert_eq!(headers, expected_headers);
    }
}

#[test]
fn custom_provider_uses_command_resolved_auth() {
    let mut provider =
        create_oss_provider_with_base_url("http://localhost:11434/v1", WireApi::Responses);
    provider.auth = Some(ModelProviderAuthInfo {
        command: "print-token".to_string(),
        args: Vec::new(),
        timeout_ms: NonZeroU64::new(5_000).expect("timeout should be non-zero"),
        refresh_interval_ms: 300_000,
        cwd: std::env::current_dir()
            .expect("current directory should be available")
            .try_into()
            .expect("current directory should be absolute"),
    });
    let command_auth = CodexAuth::from_api_key("command-token");

    let headers = resolve_provider_auth(Some(&command_auth), &provider)
        .expect("auth should resolve")
        .to_auth_headers()
        .unwrap();

    assert_eq!(
        headers.get(AUTHORIZATION),
        Some(&HeaderValue::from_static("Bearer command-token"))
    );
}

#[test]
fn openai_provider_rejects_bedrock_api_key_auth() {
    let mut provider = ModelProviderInfo::create_openai_provider(/*base_url*/ None);
    provider.requires_openai_auth = true;
    let auth = CodexAuth::BedrockApiKey(BedrockApiKeyAuth {
        api_key: "bedrock-api-key-test".to_string(),
        region: "us-east-1".to_string(),
    });

    match resolve_provider_auth(Some(&auth), &provider) {
        Err(err) => match err.details() {
            CodexErrorDetails::UnsupportedOperation(message) => {
                assert_eq!(message, BEDROCK_API_KEY_UNSUPPORTED_MESSAGE);
            }
            details => panic!("unexpected auth error: {details:?}"),
        },
        Ok(_) => panic!("Bedrock API key auth should be rejected"),
    }
}
