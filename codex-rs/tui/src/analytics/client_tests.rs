//! Account-scoped authentication and request identity regression coverage.

use super::*;
use crate::analytics::sections::Section;
use crate::legacy_core::config::ConfigBuilder;
use base64::Engine;
use codex_config::LoaderOverrides;
use pretty_assertions::assert_eq;
use serde_json::json;
use wiremock::Mock;
use wiremock::MockServer;
use wiremock::ResponseTemplate;
use wiremock::matchers::header;
use wiremock::matchers::method;
use wiremock::matchers::path;

pub(in crate::analytics) fn sign_in(home: &std::path::Path, account: &str, user: &str, plan: &str) {
    let claims = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(
        json!({
            "exp": 4102444800_i64, "email": "analytics@example.test",
            "https://api.openai.com/auth": {
                "chatgpt_account_id": account, "chatgpt_user_id": user, "chatgpt_plan_type": plan,
            },
        })
        .to_string(),
    );
    let token = format!("e30.{claims}.test");
    let auth = serde_json::from_value(json!({
        "auth_mode": "chatgpt", "tokens": {"id_token": token, "access_token": token,
            "refresh_token": "test-refresh", "account_id": account},
        "last_refresh": chrono::Utc::now(),
    }))
    .unwrap();
    codex_login::save_auth(
        home,
        &auth,
        codex_login::AuthCredentialsStoreMode::File,
        codex_login::AuthKeyringBackendKind::default(),
    )
    .unwrap();
}

pub(in crate::analytics) async fn live(
    server: &MockServer,
    plan: &str,
) -> (tempfile::TempDir, Live) {
    let home = tempfile::tempdir().unwrap();
    let mut config = ConfigBuilder::default()
        .codex_home(home.path().to_path_buf())
        .loader_overrides(LoaderOverrides::without_managed_config_for_tests())
        .build()
        .await
        .unwrap();
    config.chatgpt_base_url = format!("{}/backend-api", server.uri());
    config.cli_auth_credentials_store_mode = codex_login::AuthCredentialsStoreMode::File;
    sign_in(home.path(), "account-a", "user-a", plan);
    (
        home,
        Live::new(Arc::new(config)),
    )
}

async fn report_requests(server: &MockServer) -> Vec<wiremock::Request> {
    server.received_requests().await.unwrap()
}

#[tokio::test]
async fn analytics_rejects_identity_changes_during_requests() {
    let server = MockServer::start().await;
    let (home, live) = live(&server, "business").await;
    let session = live.session().await.unwrap();
    let authenticated = &session.backend;
    let result = authenticated
        .request(|_| async {
            sign_in(home.path(), "account-b", "user-a", "business");
            Ok(123)
        })
        .await;
    assert_eq!(
        result.unwrap_err().to_string(),
        "Account changed. Press R to refresh Analytics."
    );
    assert!(report_requests(&server).await.is_empty());
}

#[tokio::test]
async fn analytics_rejects_account_and_user_changes_before_requests() {
    for (account, user) in [("account-b", "user-a"), ("account-a", "user-b")] {
        let server = MockServer::start().await;
        let (home, live) = live(&server, "plus").await;
        let session = live.session().await.unwrap();
        let authenticated = &session.backend;
        sign_in(home.path(), account, user, "plus");
        let requested = std::cell::Cell::new(/*value*/ false);
        let result = authenticated
            .request(|_| async {
                requested.set(/*val*/ true);
                Ok(())
            })
            .await;
        assert!(!requested.get());
        assert_eq!(
            result.unwrap_err().to_string(),
            "Account changed. Press R to refresh Analytics."
        );
    }
}

#[tokio::test]
async fn analytics_requires_local_chatgpt_authentication() {
    for auth in [None, Some(json!({"OPENAI_API_KEY": "sk-test-only"}))] {
        let server = MockServer::start().await;
        let (home, live) = live(&server, "plus").await;
        let path = home.path().join("auth.json");
        match auth {
            Some(auth) => std::fs::write(path, serde_json::to_vec(&auth).unwrap()).unwrap(),
            None => std::fs::remove_file(path).unwrap(),
        }
        assert_eq!(
            live.session().await.err(),
            Some("Sign in locally with ChatGPT to view Analytics.".to_string())
        );
        assert!(report_requests(&server).await.is_empty());
    }
}

#[tokio::test]

#[tokio::test]
async fn unknown_plan_exposes_no_credit_breakdowns() {
    let server = MockServer::start().await;
    let (_home, live) = live(&server, "unknown").await;
    live.session().await.unwrap();
    assert!(live.credit_groups().is_empty());
}

async fn analytics_retries_session_initialization_after_sign_in() {
    let server = MockServer::start().await;
    let (home, live) = live(&server, "plus").await;
    std::fs::remove_file(home.path().join("auth.json")).unwrap();
    assert!(live.session().await.is_err());
    sign_in(home.path(), "account-a", "user-a", "plus");
    assert_eq!(
        live.session().await.unwrap().backend.account().id,
        "account-a"
    );
}

pub(in crate::analytics) async fn connected_view(
    server: &MockServer,
    plan: &str,
) -> (
    tempfile::TempDir,
    crate::AppServerSession,
    crate::analytics::AnalyticsView,
) {
    let (home, live) = live(server, plan).await;
    let config = Arc::clone(&live.config);
    let app_server = crate::start_embedded_app_server_for_picker(&config)
        .await
        .unwrap();
    let mut view =
        crate::analytics::AnalyticsView::new(crate::keymap::RuntimeKeymap::defaults().list);
    view.open(
        app_server.request_handle(),
        crate::tui::FrameRequester::test_dummy(),
        Vec::new(),
        config,
    );
    (home, app_server, view)
}

#[tokio::test]
async fn live_account_identity_is_visible_and_closing_aborts_pending_loads() {
    let server = MockServer::start().await;
    let (_home, live) = live(&server, "plus").await;
    live.session().await.unwrap();
    let mut view =
        crate::analytics::AnalyticsView::new(crate::keymap::RuntimeKeymap::defaults().list);
    view.live = Some(Arc::new(live));
    view.sections[Section::Usage].history =
        super::super::data::Load::Error("Couldn't load usage. Press R to retry.".into());
    let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(
        /*width*/ 100, /*height*/ 24,
    ))
    .unwrap();
    terminal
        .draw(|frame| view.render(frame.area(), frame.buffer_mut()))
        .unwrap();
    insta::assert_snapshot!(terminal.backend().to_string());
    let (draw_tx, _) = tokio::sync::broadcast::channel(/*capacity*/ 1);
    let (cancelled_tx, cancelled_rx) = tokio::sync::oneshot::channel::<()>();
    view.sections[Section::Usage].history = super::super::data::Load::start(
        async move {
            let _cancelled = cancelled_tx;
            std::future::pending().await
        },
        crate::tui::FrameRequester::new(draw_tx),
    );
    view.cancel_loads();
    assert!(cancelled_rx.await.is_err());
}

