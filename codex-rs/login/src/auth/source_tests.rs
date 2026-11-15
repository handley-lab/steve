//! Runtime reuse follows the credential source, not just its owner or token bytes.

use super::*;
use pretty_assertions::assert_eq;

#[test]
fn api_key_replacement_and_logout_retire_the_previous_source() {
    let key_a = CodexAuth::from_api_key("key-a");
    let key_b = CodexAuth::from_api_key("key-b");
    let manager = AuthManager::from_auth_for_testing(key_a.clone());
    let first = manager.auth_change_state_for(&key_a).unwrap();
    manager.set_cached_auth(Some(key_a.clone()));
    assert_eq!(first, manager.auth_change_state_for(&key_a).unwrap());
    manager.set_cached_auth(Some(key_b.clone()));
    assert!(manager.auth_change_state_for(&key_a).is_none());
    let second = manager.auth_change_state_for(&key_b).unwrap();
    manager.set_cached_auth(/*new_auth*/ None);
    assert!(manager.auth_change_state_for(&key_b).is_none());
    manager.set_cached_auth(Some(key_b.clone()));
    assert_ne!(
        second.source_generation,
        manager
            .auth_change_state_for(&key_b)
            .unwrap()
            .source_generation
    );
}

#[tokio::test]
async fn explicit_api_key_never_reloads_an_ambient_credential() -> anyhow::Result<()> {
    let home = tempfile::tempdir()?;
    login_with_api_key(
        home.path(),
        "ambient-key",
        AuthCredentialsStoreMode::File,
        AuthKeyringBackendKind::Direct,
    )?;
    let runtime = AuthRuntimeConfig {
        codex_home: home.path().to_path_buf(),
        auth_route_config: crate::test_support::transport_default_auth_route_config(),
    };
    let manager = AuthManager::from_api_key("selected-key", runtime.clone());
    for _ in 0..2 {
        manager.reload().await;
        assert_eq!(
            manager.auth().await.expect("selected key").api_key(),
            Some("selected-key")
        );
        assert_eq!(manager.runtime_config(), runtime);
    }
    let stored = create_auth_storage(
        home.path().to_path_buf(),
        AuthCredentialsStoreMode::File,
        AuthKeyringBackendKind::Direct,
    )
    .load()?
    .expect("ambient login remains stored");
    assert_eq!(stored.openai_api_key.as_deref(), Some("ambient-key"));
    Ok(())
}
