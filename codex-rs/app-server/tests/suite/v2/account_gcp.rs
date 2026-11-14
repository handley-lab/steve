//! GCP account reporting follows the selected provider without resolving its credentials.

use anyhow::Result;
use app_test_support::TestAppServer;
use codex_app_server_protocol::GetAccountParams;
use pretty_assertions::assert_eq;
use serde_json::Value;
use serde_json::json;
use tempfile::TempDir;
use test_case::test_case;
use tokio::time::timeout;

const READ_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(/*secs*/ 30);

#[test_case(""; "application_default_credentials")]
#[test_case(
    r#"
[model_providers.gcp]
base_url = "https://vertex.example.com/v1"
[model_providers.gcp.auth]
command = "codex-test-gcp-command-that-does-not-exist"
"#;
    "external_command_and_endpoint"
)]
#[tokio::test]
async fn gcp_account_is_reported_without_resolving_credentials(overrides: &str) -> Result<()> {
    let codex_home = TempDir::new()?;
    std::fs::write(
        codex_home.path().join("config.toml"),
        format!("model_provider = \"gcp\"\n{overrides}"),
    )?;
    let mut app_server = TestAppServer::builder()
        .with_codex_home(codex_home.path())
        .with_env_overrides(&[("OPENAI_API_KEY", None)])
        .build_initialized()
        .await?;

    for refresh_token in [false, true] {
        let request_id = app_server
            .send_get_account_request(GetAccountParams { refresh_token })
            .await?;
        let received: Value = timeout(READ_TIMEOUT, app_server.read_response(request_id)).await??;
        assert_eq!(
            received,
            json!({"account": {"type": "gcp"}, "requiresOpenaiAuth": false})
        );
    }
    Ok(())
}
