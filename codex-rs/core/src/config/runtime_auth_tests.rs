//! Runtime injection is explicit and cannot be enabled by a removed feature.

#![cfg(test)]

use super::*;
use codex_login::CodexAuth;
use pretty_assertions::assert_eq;

#[tokio::test]
async fn runtime_auth_mode_does_not_enable_automatic_registration() -> anyhow::Result<()> {
    let mut config = super::test_config().await;
    config.runtime_auth_mode = RuntimeAuthMode::AssertionOnly;
    let auth =
        codex_login::AuthManager::from_auth_for_testing(CodexAuth::from_api_key("configured-key"));
    assert!(
        crate::runtime_auth::build_runtime_auth_manager(&config, auth, "test-installation")
            .is_none()
    );
    assert!(codex_features::feature_for_key("runtime_agent_instances").is_none());
    Ok(())
}

#[test]
fn removed_runtime_feature_has_no_typed_requirements() -> anyhow::Result<()> {
    let requirement: codex_config::ConfigRequirementsToml =
        toml::from_str("[features]\nruntime_agent_instances = false")?;
    assert_eq!(
        requirement
            .feature_requirements
            .unwrap()
            .entries
            .get("runtime_agent_instances"),
        Some(&false)
    );
    assert!(
        toml::from_str::<codex_config::ConfigRequirementsToml>(
            "[features.runtime_agent_instances]\nmode = 'assertion_only'"
        )
        .is_err()
    );
    Ok(())
}
