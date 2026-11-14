//! Provider and authentication dimensions that partition model catalogs.

use super::*;
use pretty_assertions::assert_eq;

#[test]
fn cache_identity_tracks_provider_routing_and_effective_api_credentials() {
    let auth = CodexAuth::from_api_key("first-key");
    let mut provider =
        ModelProviderInfo::create_openai_provider(Some("https://one.example/v1".into()));
    provider.requires_openai_auth = true;
    let key = identity(&provider, Some(&auth)).unwrap();
    let other_key = CodexAuth::from_api_key("sk-proj-fedramp-rotated-test-key");
    assert_ne!(identity(&provider, Some(&other_key)).unwrap(), key);
    let mut other_provider = provider.clone();
    other_provider.base_url = Some("https://two.example/v1".into());
    assert_ne!(identity(&other_provider, Some(&auth)).unwrap(), key);
    other_provider = provider.clone();
    other_provider.experimental_bearer_token = Some("provider-key".into());
    assert_ne!(identity(&other_provider, Some(&auth)).unwrap(), key);
    other_provider = provider;
    other_provider.http_headers = Some(std::collections::HashMap::from([(
        "openai-project".into(),
        "another-project".into(),
    )]));
    assert_ne!(identity(&other_provider, Some(&auth)).unwrap(), key);

    let mut provider =
        ModelProviderInfo::create_openai_provider(Some("https://gov.api.openai.com/v1".into()));
    // Keep identities stable while the endpoint residency test runs in parallel.
    provider.http_headers = Some(std::collections::HashMap::from([(
        codex_login::default_client::RESIDENCY_HEADER_NAME.to_string(),
        "us".into(),
    )]));
    let key = identity(&provider, Some(&auth)).unwrap();
    provider.base_url = Some("https://gov.api.openai.com/v1".into());
    assert_eq!(identity(&provider, Some(&auth)).unwrap(), key);
    provider.base_url = Some("https://api.openai.com/v1".into());
    assert_ne!(identity(&provider, Some(&auth)).unwrap(), key);
}

#[test]
fn cache_identity_tracks_catalog_url() {
    let auth = CodexAuth::from_api_key("test-key");
    let mut provider =
        ModelProviderInfo::create_openai_provider(Some("https://gateway.example/v1".into()));
    let bundled = identity(&provider, Some(&auth)).unwrap();
    provider.model_catalog_url = Some("https://gateway.example/catalog-one".into());
    let first = identity(&provider, Some(&auth)).unwrap();
    provider.model_catalog_url = Some("https://gateway.example/catalog-two".into());
    let second = identity(&provider, Some(&auth)).unwrap();
    assert_ne!(bundled, first);
    assert_ne!(first, second);
    provider.base_url = None;
    let catalog_only = identity(&provider, Some(&auth)).unwrap();
    assert_ne!(second, catalog_only);
    provider.base_url = Some("https://other.example/v1".into());
    assert_ne!(catalog_only, identity(&provider, Some(&auth)).unwrap());
}

#[test]
fn cache_identity_command_auth_keeps_opaque_credential_isolation() {
    let mut provider =
        ModelProviderInfo::create_openai_provider(Some("https://catalog.example/v1".into()));
    provider.auth = Some(codex_protocol::config_types::ModelProviderAuthInfo {
        command: "print-token".into(),
        args: Vec::new(),
        timeout_ms: std::num::NonZeroU64::new(5_000).unwrap(),
        refresh_interval_ms: 300_000,
        cwd: std::env::current_dir().unwrap().try_into().unwrap(),
    });
    let first = CodexAuth::from_api_key("first-command-token");
    let second = CodexAuth::from_api_key("second-command-token");
    assert_ne!(
        identity(&provider, Some(&first)).unwrap(),
        identity(&provider, Some(&second)).unwrap()
    );
}
