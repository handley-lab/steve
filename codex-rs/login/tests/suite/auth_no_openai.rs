use base64::Engine;
use chrono::Utc;
use codex_config::types::AuthCredentialsStoreMode;
use codex_login::AuthDotJson;
use codex_login::AuthKeyringBackendKind;
use codex_login::AuthManager;
use codex_login::REFRESH_TOKEN_URL_OVERRIDE_ENV_VAR;
use codex_login::REVOKE_TOKEN_URL_OVERRIDE_ENV_VAR;
use codex_login::load_auth_dot_json;
use codex_login::login_with_api_key;
use codex_login::save_auth;
use codex_login::test_support::transport_default_auth_route_config;
use codex_login::token_data::IdTokenInfo;
use codex_login::token_data::TokenData;
use codex_protocol::auth::AuthMode;
use pretty_assertions::assert_eq;
use serde::Serialize;
use serde_json::json;
use std::ffi::OsString;
use wiremock::MockServer;

fn jwt_with_payload(payload: serde_json::Value) -> String {
    #[derive(Serialize)]
    struct Header {
        alg: &'static str,
        typ: &'static str,
    }

    let header = Header {
        alg: "none",
        typ: "JWT",
    };
    fn b64(data: &[u8]) -> String {
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(data)
    }
    format!(
        "{}.{}.{}",
        b64(&serde_json::to_vec(&header).expect("header")),
        b64(&serde_json::to_vec(&payload).expect("payload")),
        b64(b"sig")
    )
}

fn chatgpt_auth(codex_home: &std::path::Path) {
    let access_token = jwt_with_payload(json!({ "sub": "user-123" }));
    let auth = AuthDotJson {
        auth_mode: Some(AuthMode::Chatgpt),
        openai_api_key: None,
        tokens: Some(TokenData {
            id_token: IdTokenInfo {
                raw_jwt: access_token.clone(),
                ..IdTokenInfo::default()
            },
            access_token,
            refresh_token: "refresh-token".to_string(),
            account_id: Some("account-id".to_string()),
        }),
        last_refresh: Some(Utc::now()),
        agent_identity: None,
        personal_access_token: None,
        bedrock_api_key: None,
        bedrock_access_keys: None,
    };
    save_auth(
        codex_home,
        &auth,
        AuthCredentialsStoreMode::File,
        AuthKeyringBackendKind::default(),
    )
    .expect("stored ChatGPT auth");
}

struct EnvGuard {
    key: &'static str,
    original: Option<OsString>,
}

impl EnvGuard {
    fn set(key: &'static str, value: String) -> Self {
        let original = std::env::var_os(key);
        // SAFETY: this test is the only login integration test and it runs serially.
        unsafe {
            std::env::set_var(key, &value);
        }
        Self { key, original }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        // SAFETY: the guard restores the process environment before the test exits.
        unsafe {
            match &self.original {
                Some(value) => std::env::set_var(self.key, value),
                None => std::env::remove_var(self.key),
            }
        }
    }
}

#[serial_test::serial(auth_env)]
#[tokio::test]
async fn stored_credentials_load_without_building_openai_token_requests() {
    let server = MockServer::start().await;
    let _refresh_guard = EnvGuard::set(REFRESH_TOKEN_URL_OVERRIDE_ENV_VAR, server.uri());
    let _revoke_guard = EnvGuard::set(REVOKE_TOKEN_URL_OVERRIDE_ENV_VAR, server.uri());

    let api_home = tempfile::tempdir().expect("api home");
    login_with_api_key(
        api_home.path(),
        "sk-test",
        AuthCredentialsStoreMode::File,
        AuthKeyringBackendKind::default(),
    )
    .expect("api key login");
    let api_manager = AuthManager::shared(
        api_home.path().to_path_buf(),
        /*enable_codex_api_key_env*/ false,
        AuthCredentialsStoreMode::File,
        /*forced_chatgpt_workspace_id*/ None,
        /*chatgpt_base_url*/ None,
        AuthKeyringBackendKind::default(),
        transport_default_auth_route_config(),
    )
    .await;
    assert_eq!(api_manager.auth_mode(), Some(AuthMode::ApiKey));
    api_manager
        .refresh_token()
        .await
        .expect("api key refresh does not call a token endpoint");
    assert!(
        api_manager.logout_with_revoke().await.expect("api logout"),
        "stored api key should be removed"
    );
    assert_eq!(api_manager.auth().await, None);

    let chatgpt_home = tempfile::tempdir().expect("chatgpt home");
    chatgpt_auth(chatgpt_home.path());
    let chatgpt_manager = AuthManager::shared(
        chatgpt_home.path().to_path_buf(),
        /*enable_codex_api_key_env*/ false,
        AuthCredentialsStoreMode::File,
        /*forced_chatgpt_workspace_id*/ None,
        /*chatgpt_base_url*/ None,
        AuthKeyringBackendKind::default(),
        transport_default_auth_route_config(),
    )
    .await;
    assert_eq!(chatgpt_manager.auth_mode(), Some(AuthMode::Chatgpt));
    chatgpt_manager
        .refresh_token()
        .await
        .expect("stored ChatGPT auth does not call a token endpoint");
    assert!(
        chatgpt_manager
            .logout_with_revoke()
            .await
            .expect("chatgpt logout")
    );
    assert_eq!(chatgpt_manager.auth().await, None);
    assert!(
        load_auth_dot_json(
            chatgpt_home.path(),
            AuthCredentialsStoreMode::File,
            AuthKeyringBackendKind::default(),
        )
        .expect("read auth")
        .is_none()
    );

    let requests = server.received_requests().await.expect("requests");
    assert!(
        requests.is_empty(),
        "auth.openai.com token and revoke endpoints were not built: {requests:?}"
    );
}
