//! Selected-workspace account reads through the public account API.

use anyhow::Result;
use app_test_support::ChatGptAuthFixture;
use app_test_support::TestAppServer;
use app_test_support::write_chatgpt_auth;
use codex_app_server_protocol::GetAccountParams;
use codex_app_server_protocol::LoginAccountResponse;
use codex_config::types::AuthCredentialsStoreMode;
use pretty_assertions::assert_eq;
use serde_json::Value;
use serde_json::json;
use std::time::Duration;
use tempfile::TempDir;
use tokio::time::timeout;
use wiremock::Mock;
use wiremock::MockServer;
use wiremock::ResponseTemplate;
use wiremock::matchers::method;
use wiremock::matchers::path;

const READ_TIMEOUT: Duration = Duration::from_secs(/*secs*/ 60);

async fn config(home: &TempDir, backend: &MockServer) -> Result<()> {
    std::fs::write(
        home.path().join("config.toml"),
        format!(
            "chatgpt_base_url = '{}/backend-api/'\ncli_auth_credentials_store = 'file'\n",
            backend.uri()
        ),
    )?;
    // Routing uses a plain HTTP fixture; restricted policies require HTTPS/WSS.
    Mock::given(method("GET")).and(path("/backend-api/wham/config/bundle"))
        .respond_with(|request: &wiremock::Request| {
            let account_id = request.headers.get("chatgpt-account-id").expect("workspace header")
                .to_str().expect("workspace header text");
            ResponseTemplate::new(200).set_body_json(json!({"requirements_toml": {"enterprise_managed": [{
                "id": "workspace-policy", "name": "Workspace policy",
                "contents": format!("[application.network]\nenabled = false\n[application.network.domains]\n'{account_id}.example' = 'allow'"),
            }]}}))
        }).mount(backend).await;
    Ok(())
}

async fn start(home: &TempDir) -> Result<TestAppServer> {
    TestAppServer::builder()
        .with_codex_home(home.path())
        .with_env_overrides(&[("OPENAI_API_KEY", None)])
        .build_initialized_with_timeout(READ_TIMEOUT)
        .await
}

async fn read(server: &mut TestAppServer) -> Result<Value> {
    let request = server
        .send_get_account_request(GetAccountParams {
            refresh_token: false,
        })
        .await?;
    timeout(READ_TIMEOUT, server.read_response(request)).await?
}

#[tokio::test]
async fn saved_chatgpt_login_without_selected_workspace_preserves_account() -> Result<()> {
    let backend = MockServer::start().await;
    let home = TempDir::new()?;
    config(&home, &backend).await?;
    write_chatgpt_auth(
        home.path(),
        ChatGptAuthFixture::new("token").plan_type("pro"),
        AuthCredentialsStoreMode::File,
    )?;
    let mut server = start(&home).await?;
    assert_eq!(
        read(&mut server).await?,
        json!({"account": {"type": "chatgpt", "email": null, "planType": "pro"},
            "requiresOpenaiAuth": true, "workspaceRouting": null})
    );
    Ok(())
}

#[tokio::test]
async fn api_only_login_does_not_discover_chatgpt_routing() -> Result<()> {
    let backend = MockServer::start().await;
    let home = TempDir::new()?;
    config(&home, &backend).await?;
    std::fs::write(
        home.path().join("requirements.toml"),
        "allowed_login_methods = ['api']",
    )?;
    let mut server = start(&home).await?;
    let request = server.send_login_account_api_key_request("sk-test").await?;
    let _: LoginAccountResponse = timeout(READ_TIMEOUT, server.read_response(request)).await??;
    assert_eq!(
        read(&mut server).await?,
        json!({"account": {"type": "apiKey"}, "requiresOpenaiAuth": true, "workspaceRouting": null})
    );
    Ok(())
}
