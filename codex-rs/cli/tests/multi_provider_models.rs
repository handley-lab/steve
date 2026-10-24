use anyhow::Result;
use codex_protocol::openai_models::ModelInfo;
use codex_protocol::openai_models::ModelsResponse;
use serde_json::Value;
use serde_json::json;
use tempfile::TempDir;
use tokio::process::Command;
use wiremock::Mock;
use wiremock::MockServer;
use wiremock::ResponseTemplate;
use wiremock::matchers::header;
use wiremock::matchers::method;
use wiremock::matchers::path;

#[tokio::test]
async fn native_models_command_combines_live_providers_and_reports_partial_failure() -> Result<()> {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/cambridge/v1/models"))
        .and(header("authorization", "Bearer fixture-key"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"data": [
            {"id":"first"}, {"id":"moonshotai/Kimi-K3"},
            {"id":"vllm-sr/d3","mode":"decisions"}, {"id":"ocr"}
        ]})))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/turing/v1/models"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"data": [
            {"id":"google/gemma"}, {"id":"moonshotai/Kimi-K3"}
        ]})))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/failed/v1/models"))
        .respond_with(ResponseTemplate::new(401).set_body_string("private-upstream-secret"))
        .expect(1)
        .mount(&server)
        .await;

    let home = TempDir::new()?;
    let cwd = TempDir::new()?;
    let mut ocr = ModelInfo::from_discovery_id("cambridge/ocr".to_string(), None);
    ocr.non_chat = true;
    let stale = ModelInfo::from_discovery_id("cambridge/stale-heretic".to_string(), None);
    std::fs::write(
        home.path().join("models.json"),
        serde_json::to_vec(&ModelsResponse {
            models: vec![ocr, stale],
        })?,
    )?;
    std::fs::write(
        home.path().join("config.toml"),
        format!(
            r#"
model_provider = "turing"
model = "google/gemma"
model_catalog_json = "models.json"
check_for_update_on_startup = false
[analytics]
enabled = false
[feedback]
enabled = false
[model_providers.cambridge]
name = "Cambridge"
base_url = "{base}/cambridge/v1"
env_key = "STEVE_FIXTURE_KEY"
default_model = "moonshotai/Kimi-K3"
[model_providers.turing]
name = "Turing"
base_url = "{base}/turing/v1"
default_model = "google/gemma"
[model_providers.failed]
name = "Failed"
base_url = "{base}/failed/v1"
"#,
            base = server.uri()
        ),
    )?;
    let output = Command::new(codex_utils_cargo_bin::cargo_bin("steve")?)
        .arg("models")
        .env("STEVE_HOME", home.path())
        .env("STEVE_FIXTURE_KEY", "fixture-key")
        .current_dir(cwd.path())
        .output()
        .await?;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let response: Value = serde_json::from_slice(&output.stdout)?;
    let rows = response["data"].as_array().unwrap();
    assert_eq!(rows.len(), 4);
    assert!(rows.iter().all(|row| row["id"] == row["model"]));
    let global = rows.iter().find(|row| row["isDefault"] == true).unwrap();
    assert_eq!(global["model"], "turing/google/gemma");
    assert_eq!(global["isProviderDefault"], true);
    let kimi = rows
        .iter()
        .find(|row| row["model"] == "cambridge/moonshotai/Kimi-K3")
        .unwrap();
    assert_eq!(kimi["isProviderDefault"], true);
    assert_eq!(kimi["isDefault"], false);
    assert_eq!(response["providerErrors"][0]["provider"], "failed");
    let text = String::from_utf8(output.stdout)?;
    for excluded in [
        "stale-heretic",
        "vllm-sr/d3",
        "cambridge/ocr",
        "fixture-key",
        "private-upstream-secret",
        "base_url",
        "STEVE_FIXTURE_KEY",
    ] {
        assert!(
            !text.contains(excluded),
            "unexpected public output: {excluded}"
        );
    }
    let requests = server.received_requests().await.unwrap();
    assert!(
        requests
            .iter()
            .filter(|request| request.url.path().starts_with("/turing/"))
            .all(|request| !request.headers.contains_key("authorization"))
    );
    Ok(())
}
