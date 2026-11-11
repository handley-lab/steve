//! Analytics uses the explicitly configured backend without registering a runtime.

#![cfg(test)]

use super::Live;
use super::tests::sign_in;
use crate::legacy_core::config::ConfigBuilder;
use anyhow::Context;
use anyhow::Result;
use codex_backend_client::AnalyticsReport;
use codex_backend_client::AnalyticsResponse;
use codex_config::LoaderOverrides;
use codex_features::RuntimeAuthMode;
use codex_protocol::account::PlanType;
use pretty_assertions::assert_eq;
use serde_json::json;
use std::sync::Arc;
use wiremock::Mock;
use wiremock::MockServer;
use wiremock::ResponseTemplate;
use wiremock::matchers::method;
use wiremock::matchers::path;

async fn configured_client(
    base_url: &str,
    mode: RuntimeAuthMode,
) -> Result<(tempfile::TempDir, Live)> {
    let home = tempfile::tempdir()?;
    std::fs::write(
        home.path().join("config.toml"),
        "model_provider = \"openai\"\n",
    )?;
    let mut config = ConfigBuilder::default()
        .codex_home(home.path().to_path_buf())
        .loader_overrides(LoaderOverrides::without_managed_config_for_tests())
        .build()
        .await?;
    config.chatgpt_base_url = base_url.to_string();
    config.chatgpt_path_style = codex_config::AccountPathStyle::ChatgptApi;
    config.cli_auth_credentials_store_mode = codex_login::AuthCredentialsStoreMode::File;
    config.runtime_auth_mode = mode;
    sign_in(home.path(), "account-a", "user-a", "plus");
    Ok((home, Live::new(Arc::new(config), "2026-01-15".parse()?)))
}

#[tokio::test]
async fn analytics_routes_account_and_reports_without_runtime_registration() -> Result<()> {
    for mode in [RuntimeAuthMode::DualHeader, RuntimeAuthMode::AssertionOnly] {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/backend-api/wham/accounts/check"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "accounts": [{"id": "account-a", "plan_type": "pro"}],
                "account_ordering": ["account-a"],
            })))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/backend-api/wham/usage/daily-token-usage-breakdown"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"data": []})))
            .expect(1)
            .mount(&server)
            .await;
        let (home, live) =
            configured_client(&format!("{}/backend-api", server.uri()), mode).await?;
        let session = live.session().await.map_err(anyhow::Error::msg)?;
        assert_eq!(session.backend.account().id, "account-a");
        assert_eq!(
            session.backend.account().email.as_deref(),
            Some("analytics@example.test")
        );
        assert_eq!(session.backend.account().plan_type, Some(PlanType::Pro));
        let response = session
            .backend
            .request(|client| async move {
                client
                    .get_account_analytics(AnalyticsReport::Usage, "2026-01-01", "2026-01-15")
                    .await
            })
            .await?;
        assert_eq!(
            response,
            AnalyticsResponse::Usage(
                codex_backend_client::analytics_models::DailyProductSurfaceUsageResponse::default(),
            )
        );
        server.verify().await;
        let requests = server
            .received_requests()
            .await
            .context("captured requests")?;
        assert_eq!(
            requests.len(),
            2,
            "neither auth mode may bootstrap a runtime"
        );
        for request in &requests {
            assert!(request.url.path().starts_with("/backend-api/wham/"));
            assert_eq!(request.headers["chatgpt-account-id"], "account-a");
            assert!(
                request.headers["authorization"]
                    .to_str()?
                    .starts_with("Bearer ")
            );
            assert!(!request.headers.contains_key("x-openai-agent-assertion"));
        }
        // The selected user remains part of the report's privacy boundary.
        sign_in(home.path(), "account-a", "user-b", "plus");
        assert_eq!(
            session.backend.ensure_identity().await.err().as_deref(),
            Some("Account changed. Press R to refresh Analytics.")
        );
        assert_eq!(server.received_requests().await.unwrap().len(), 2);
    }
    Ok(())
}

#[tokio::test]
async fn analytics_rejected_credentials_preserve_replacement_guidance_without_registration()
-> Result<()> {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/backend-api/wham/accounts/check"))
        .respond_with(ResponseTemplate::new(401).set_body_json(json!({
            "error": {"code": "token_expired"},
        })))
        .mount(&server)
        .await;
    let (_home, live) = configured_client(
        &format!("{}/backend-api", server.uri()),
        RuntimeAuthMode::DualHeader,
    )
    .await?;
    assert_eq!(
        live.session().await.err().as_deref(),
        Some("Replace the supplied account credentials to load Analytics.")
    );
    let requests = server
        .received_requests()
        .await
        .context("captured requests")?;
    assert!(!requests.is_empty());
    assert!(
        requests
            .iter()
            .all(|request| request.url.path() == "/backend-api/wham/accounts/check"),
        "rejected credentials must not trigger registration or account acquisition"
    );
    Ok(())
}

#[tokio::test]
async fn analytics_requires_a_backend_before_reading_credentials_or_sending_requests() -> Result<()>
{
    for base_url in ["", " ", "\t\n "] {
        let (home, live) = configured_client(base_url, RuntimeAuthMode::DualHeader).await?;
        // A credential read would fail independently of the missing backend.
        std::fs::remove_file(home.path().join("auth.json"))?;
        std::fs::create_dir(home.path().join("auth.json"))?;
        assert_eq!(
            live.session().await.err().as_deref(),
            Some("Configure chatgpt_base_url to load Analytics.")
        );
    }
    Ok(())
}
