//! Exercise Cloud credential isolation through the CLI using a supplied API key.

use anyhow::Result;
use predicates::str::contains;
use serde_json::json;
use tempfile::TempDir;
use wiremock::Mock;
use wiremock::MockServer;
use wiremock::ResponseTemplate;
use wiremock::matchers::header;
use wiremock::matchers::path;

#[tokio::test]
async fn cloud_list_only_allows_configured_credential_destinations() -> Result<()> {
    let configured = MockServer::start().await;
    let other = MockServer::start().await;
    let codex_home = TempDir::new()?;
    std::fs::write(
        codex_home.path().join("config.toml"),
        format!(
            "cli_auth_credentials_store = 'file'\nchatgpt_base_url = {:?}\n",
            configured.uri()
        ),
    )?;
    codex_login::login_with_api_key(
        codex_home.path(),
        "synthetic-cloud-api-key",
        codex_login::AuthCredentialsStoreMode::File,
        codex_login::AuthKeyringBackendKind::Direct,
    )?;
    Mock::given(path("/api/codex/tasks/list"))
        .and(header("authorization", "Bearer synthetic-cloud-api-key"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"items": []})))
        .expect(1)
        .mount(&configured)
        .await;

    let command = || -> Result<assert_cmd::Command> {
        let mut command = assert_cmd::Command::new(codex_utils_cargo_bin::cargo_bin("steve")?);
        command
            .current_dir(codex_home.path())
            .env("STEVE_HOME", codex_home.path())
            .env_remove("STEVE_API_KEY")
            .env_remove("STEVE_ACCESS_TOKEN")
            .env_remove("OPENAI_API_KEY")
            .env_remove("STEVE_CLOUD_TASKS_MODE")
            .env_remove("STEVE_CLOUD_TASKS_BASE_URL")
            .env("NO_PROXY", "127.0.0.1,localhost")
            .env("no_proxy", "127.0.0.1,localhost")
            .timeout(std::time::Duration::from_secs(15));
        Ok(command)
    };
    command()?
        .env("STEVE_CLOUD_TASKS_BASE_URL", other.uri())
        .args(["cloud", "list", "--limit", "1", "--json"])
        .assert()
        .failure()
        .stderr(contains("must retain the configured backend origin"));
    assert!(other.received_requests().await.unwrap().is_empty());
    assert!(configured.received_requests().await.unwrap().is_empty());

    // Positive control: configuration alone selects the local service and sends the key.
    command()?
        .args(["cloud", "list", "--limit", "1", "--json"])
        .assert()
        .success();
    configured.verify().await;
    assert!(other.received_requests().await.unwrap().is_empty());
    Ok(())
}
