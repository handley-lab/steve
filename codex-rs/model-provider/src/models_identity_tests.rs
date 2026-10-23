//! Provider and authentication dimensions that partition model catalogs.

use super::*;
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use pretty_assertions::assert_eq;

fn chatgpt_auth(email: &str, user: &str, account: &str, plan: &str, signature: &str) -> CodexAuth {
    let claims = serde_json::json!({
        "email": email,
        "https://api.openai.com/auth": {"chatgpt_user_id": user}
    });
    let payload = URL_SAFE_NO_PAD.encode(serde_json::to_vec(&claims).unwrap());
    CodexAuth::from_external_chatgpt_tokens(
        &format!("e30.{payload}.{signature}"),
        account,
        Some(plan),
    )
    .unwrap()
}

#[test]
fn cache_identity_tracks_account_email_user_plan_and_auth_mode_but_not_token_refresh() {
    let mut provider =
        ModelProviderInfo::create_openai_provider(Some("https://example.com/v1".into()));
    provider.requires_openai_auth = true;
    // The endpoint residency test temporarily sets the process default to US.
    provider.http_headers = Some(std::collections::HashMap::from([(
        codex_login::default_client::RESIDENCY_HEADER_NAME.to_string(),
        "us".into(),
    )]));
    let initial = chatgpt_auth("one@example.com", "user", "account", "team", "first");
    let key = identity(&provider, Some(&initial)).unwrap();
    let refreshed = chatgpt_auth("one@example.com", "user", "account", "team", "second");
    assert_eq!(identity(&provider, Some(&refreshed)).unwrap(), key);
    for auth in [
        chatgpt_auth("two@example.com", "user", "account", "team", "first"),
        chatgpt_auth("one@example.com", "other", "account", "team", "first"),
        chatgpt_auth("one@example.com", "user", "other", "team", "first"),
        chatgpt_auth("one@example.com", "user", "account", "plus", "first"),
        CodexAuth::from_api_key("api-key"),
    ] {
        assert_ne!(identity(&provider, Some(&auth)).unwrap(), key);
    }
    assert_ne!(identity(&provider, /*auth*/ None).unwrap(), key);
}

#[test]
fn cache_identity_tracks_provider_routing_and_effective_api_credentials() {
    let auth = CodexAuth::from_api_key("first-key");
    let mut provider =
        ModelProviderInfo::create_openai_provider(Some("https://one.example/v1".into()));
    provider.requires_openai_auth = true;
    let key = identity(&provider, Some(&auth)).unwrap();
    let other_key = CodexAuth::from_api_key("second-key");
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
}

#[test]
fn cache_identity_distinguishes_auth_requirements_and_unknown_plans() {
    let mut provider =
        ModelProviderInfo::create_openai_provider(Some("https://example.com/v1".into()));
    provider.requires_openai_auth = true;
    let first = chatgpt_auth(
        "one@example.com",
        "user",
        "account",
        "future-plan-one",
        "first",
    );
    let second = chatgpt_auth(
        "one@example.com",
        "user",
        "account",
        "future-plan-two",
        "first",
    );
    let key = identity(&provider, Some(&first)).unwrap();
    assert_ne!(key, identity(&provider, Some(&second)).unwrap());
    provider.requires_openai_auth = false;
    assert_ne!(key, identity(&provider, Some(&first)).unwrap());
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
fn cache_identity_ignores_unselected_account_snapshots() {
    let provider =
        ModelProviderInfo::create_openai_provider(Some("https://catalog.example/v1".into()));
    let key = identity(&provider, None).unwrap();
    for auth in [
        CodexAuth::from_api_key("unselected-key"),
        chatgpt_auth("one@example.com", "user", "account", "team", "first"),
        chatgpt_auth(
            "two@example.com",
            "other",
            "other-account",
            "plus",
            "second",
        ),
    ] {
        assert_eq!(identity(&provider, Some(&auth)).unwrap(), key);
    }
}

#[test]
fn cache_identity_own_bearer_ignores_unselected_account_snapshots() {
    let mut provider =
        ModelProviderInfo::create_openai_provider(Some("https://catalog.example/v1".into()));
    provider.requires_openai_auth = true;
    provider.experimental_bearer_token = Some("selected-provider-key".into());
    let key = identity(&provider, None).unwrap();
    for auth in [
        CodexAuth::from_api_key("unselected-key"),
        chatgpt_auth("one@example.com", "user", "account", "team", "first"),
        chatgpt_auth(
            "two@example.com",
            "other",
            "other-account",
            "plus",
            "second",
        ),
    ] {
        assert_eq!(identity(&provider, Some(&auth)).unwrap(), key);
    }
    provider.experimental_bearer_token = Some("changed-provider-key".into());
    assert_ne!(identity(&provider, None).unwrap(), key);
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
