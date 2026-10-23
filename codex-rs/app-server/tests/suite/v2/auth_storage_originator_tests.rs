//! Checks client attribution on exported storage metrics through the public RPC API.

use super::connection_handling_websocket::connect_websocket;
use super::connection_handling_websocket::read_response_for_id;
use super::connection_handling_websocket::send_initialize_request;
use super::connection_handling_websocket::send_request;
use super::connection_handling_websocket::spawn_websocket_server;
use anyhow::Context;
use anyhow::Result;
use pretty_assertions::assert_eq;
use serde_json::Value;
use serde_json::json;
use std::collections::BTreeMap;
use std::io::Write;
use std::path::Path;
use std::time::Duration;
use tempfile::TempDir;
use wiremock::Mock;
use wiremock::MockServer;
use wiremock::ResponseTemplate;
use wiremock::matchers::method;
use wiremock::matchers::path;

pub(super) async fn configure_collector(codex_home: &Path) -> Result<MockServer> {
    let collector = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/metrics"))
        .respond_with(ResponseTemplate::new(/*s*/ 200))
        .mount(&collector)
        .await;
    let endpoint = format!("{}/v1/metrics", collector.uri());
    let mut config = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(codex_home.join("config.toml"))?;
    writeln!(
        config,
        "\n[analytics]\nenabled = true\n[otel.metrics_exporter.otlp-http]\nendpoint = {endpoint:?}\nprotocol = \"json\""
    )?;
    Ok(collector)
}

pub(super) async fn assert_saved_originators(
    collector: &MockServer,
    expected: &[&str],
) -> Result<()> {
    let expected: BTreeMap<_, _> = expected
        .iter()
        .map(|value| (value.to_string(), 1_u64))
        .collect();
    let actual = tokio::time::timeout(Duration::from_secs(/*secs*/ 60), async {
        loop {
            let mut actual = BTreeMap::<String, u64>::new();
            for request in collector.received_requests().await.unwrap_or_default() {
                let body: Value = serde_json::from_slice(&request.body)?;
                for resource in body["resourceMetrics"].as_array().into_iter().flatten() {
                    for scope in resource["scopeMetrics"].as_array().into_iter().flatten() {
                        for metric in scope["metrics"].as_array().into_iter().flatten() {
                            if metric["name"] != "codex.auth_storage.operation" {
                                continue;
                            }
                            for point in
                                metric["sum"]["dataPoints"].as_array().into_iter().flatten()
                            {
                                let tags: BTreeMap<_, _> = point["attributes"]
                                    .as_array()
                                    .into_iter()
                                    .flatten()
                                    .filter_map(|tag| {
                                        Some((
                                            tag["key"].as_str()?,
                                            tag["value"]["stringValue"].as_str()?,
                                        ))
                                    })
                                    .collect();
                                if tags.get("credential_kind") == Some(&"codex")
                                    && tags.get("operation") == Some(&"save")
                                {
                                    let count = point["asInt"]
                                        .as_u64()
                                        .or_else(|| point["asInt"].as_str()?.parse().ok())
                                        .context("storage count must be an integer")?;
                                    *actual
                                        .entry(
                                            tags.get("originator")
                                                .unwrap_or(&"missing")
                                                .to_string(),
                                        )
                                        .or_default() += count;
                                }
                            }
                        }
                    }
                }
            }
            if actual.values().sum::<u64>() >= expected.len() as u64 {
                break Ok::<_, anyhow::Error>(actual);
            }
            tokio::time::sleep(Duration::from_millis(/*millis*/ 25)).await;
        }
    })
    .await??;
    assert_eq!(actual, expected);
    Ok(())
}

#[tokio::test]
async fn concurrent_connections_export_their_own_storage_originator() -> Result<()> {
    let codex_home = TempDir::new()?;
    std::fs::write(
        codex_home.path().join("config.toml"),
        "cli_auth_credentials_store = \"file\"\n",
    )?;
    let collector = configure_collector(codex_home.path()).await?;
    let (_process, address) = spawn_websocket_server(codex_home.path()).await?;
    let mut clients = Vec::new();
    for name in ["Codex Desktop", "codex_vscode", "private-client-name"] {
        let mut client = connect_websocket(address).await?;
        send_initialize_request(&mut client, /*id*/ 1, name).await?;
        read_response_for_id(&mut client, /*id*/ 1).await?;
        clients.push(client);
    }
    // All clients initialize before any storage request; a process-global identity fails this test.
    for client in &mut clients {
        send_request(
            client,
            "account/login/start",
            /*id*/ 2,
            Some(json!({"type": "apiKey", "apiKey": "sk-test-key"})),
        )
        .await?;
    }
    for client in &mut clients {
        let response = read_response_for_id(client, /*id*/ 2).await?;
        assert_eq!(response.result, json!({"type": "apiKey"}));
    }
    assert_saved_originators(&collector, &["codex_desktop", "codex_vscode", "other"]).await
}
