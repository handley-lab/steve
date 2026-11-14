use codex_config::types::AuthCredentialsStoreMode;
use codex_protocol::auth::AuthMode;
use pretty_assertions::assert_eq;
use serial_test::serial;
use tempfile::tempdir;

use super::*;
use crate::auth::AuthKeyringBackendKind;
use crate::auth::AuthManager;
use crate::auth::CodexAuth;
use crate::auth::storage::AuthStorageBackend;
use crate::auth::storage::FileAuthStorage;

fn api_key_auth() -> AuthDotJson {
    AuthDotJson {
        auth_mode: Some(AuthMode::ApiKey),
        openai_api_key: Some("sk-test-key".to_string()),
        bedrock_api_key: None,
        bedrock_access_keys: None,
    }
}

fn bedrock_auth() -> BedrockApiKeyAuth {
    BedrockApiKeyAuth {
        api_key: "bedrock-api-key-test".to_string(),
        region: "us-east-1".to_string(),
    }
}

#[test]
fn bedrock_api_key_debug_redacts_secret() {
    let auth = bedrock_auth();

    assert_eq!(
        format!("{auth:?}"),
        r#"BedrockApiKeyAuth { api_key: "<redacted>", region: "us-east-1" }"#
    );
    assert_eq!(
        format!("{:?}", CodexAuth::BedrockApiKey(auth)),
        r#"BedrockApiKey(BedrockApiKeyAuth { api_key: "<redacted>", region: "us-east-1" })"#
    );
}

#[tokio::test]
#[serial(codex_auth_env)]
async fn logout_removes_bedrock_auth() -> anyhow::Result<()> {
    let codex_home = tempdir()?;
    let storage = FileAuthStorage::new(codex_home.path().to_path_buf());
    login_with_bedrock_api_key(
        codex_home.path(),
        "bedrock-api-key-test",
        "us-east-1",
        AuthCredentialsStoreMode::File,
        AuthKeyringBackendKind::default(),
    )?;
    let auth_manager = AuthManager::new(
        codex_home.path().to_path_buf(),
        /*enable_codex_api_key_env*/ false,
        AuthCredentialsStoreMode::File,
        /*forced_chatgpt_workspace_id*/ None,
        /*chatgpt_base_url*/ None,
        AuthKeyringBackendKind::default(),
        crate::test_support::transport_default_auth_route_config(),
    )
    .await;

    assert!(auth_manager.logout().await?);

    assert_eq!(storage.load()?, None);
    assert_eq!(auth_manager.auth_cached(), None);
    Ok(())
}

#[tokio::test]
async fn login_with_api_key_clears_bedrock_api_key() -> anyhow::Result<()> {
    let codex_home = tempdir()?;
    let storage = FileAuthStorage::new(codex_home.path().to_path_buf());
    login_with_bedrock_api_key(
        codex_home.path(),
        "bedrock-api-key-test",
        "us-east-1",
        AuthCredentialsStoreMode::File,
        AuthKeyringBackendKind::default(),
    )?;

    crate::auth::login_with_api_key(
        codex_home.path(),
        "sk-test-key",
        AuthCredentialsStoreMode::File,
        AuthKeyringBackendKind::default(),
    )?;

    assert_eq!(storage.load()?, Some(api_key_auth()));
    Ok(())
}
