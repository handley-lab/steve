//! Captured Cloud clients retain explicit credential ownership and destinations.

#![cfg(test)]

use base64::Engine;
use chrono::Utc;
use codex_cloud_tasks_client::PathStyle;
use codex_http_client::ClientRouteClass;
use codex_http_client::HttpClientFactory;
use codex_http_client::OutboundProxyPolicy;
use codex_http_client::RouteAwareClientPool;
use codex_login::AuthManager;
use codex_login::CodexAuth;
use codex_login::ExternalAuth;
use codex_model_provider::ClientAuth;
use codex_model_provider::auth_provider_from_auth_manager;
use codex_utils_cli::CliConfigOverrides;
use pretty_assertions::assert_eq;
use serde_json::json;
use std::sync::Arc;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;
use wiremock::Mock;
use wiremock::MockServer;
use wiremock::ResponseTemplate;
use wiremock::matchers::header;
use wiremock::matchers::method;
use wiremock::matchers::path;

fn access_token(session: &str) -> String {
    let claims = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(
        json!({
            "exp": Utc::now().timestamp() + 3_600,
            "iss": "https://auth.example.test/",
            "sub": "user-a",
            "session_id": session,
            "https://api.openai.com/auth": {
                "chatgpt_account_id": "account-a",
                "chatgpt_user_id": "user-a",
                "chatgpt_plan_type": "plus",
            },
        })
        .to_string(),
    );
    format!("header.{claims}.signature")
}

struct RefreshingOwner {
    initial: CodexAuth,
    refreshed: CodexAuth,
    refreshes: AtomicUsize,
}

impl ExternalAuth for RefreshingOwner {
    fn resolve(&self) -> codex_login::ExternalAuthFuture<'_, CodexAuth> {
        Box::pin(async {
            Ok(if self.refreshes.load(Ordering::SeqCst) == 0 {
                self.initial.clone()
            } else {
                self.refreshed.clone()
            })
        })
    }

    fn refresh(
        &self,
        _context: codex_login::ExternalAuthRefreshContext,
    ) -> codex_login::ExternalAuthFuture<'_, CodexAuth> {
        Box::pin(async {
            self.refreshes.fetch_add(1, Ordering::SeqCst);
            self.resolve().await
        })
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn captured_environment_client_refreshes_with_explicit_external_owner() -> anyhow::Result<()>
{
    let server = MockServer::start().await;
    let initial_token = access_token("startup");
    let refreshed_token = access_token("refreshed");
    let external = Arc::new(RefreshingOwner {
        initial: CodexAuth::from_external_chatgpt_tokens(&initial_token, "account-a", None)?,
        refreshed: CodexAuth::from_external_chatgpt_tokens(&refreshed_token, "account-a", None)?,
        refreshes: AtomicUsize::new(0),
    });
    let home = tempfile::tempdir()?;
    let manager = AuthManager::from_auth_for_testing_with_home(
        external.initial.clone(),
        home.path().to_owned(),
    );
    manager.set_external_auth(external.clone()).await?;
    let (mut auth, factory) = ClientAuth::for_auth(
        &external.initial,
        None,
        HttpClientFactory::new(OutboundProxyPolicy::ReqwestDefault),
        &Default::default(),
    )
    .await?;
    auth.set_provider(auth_provider_from_auth_manager(
        manager.clone(),
        &external.initial,
    ))?;
    let environment_http = crate::env_detect::EnvironmentClient {
        http: RouteAwareClientPool::new_without_redirects_or_request_logging(
            factory,
            ClientRouteClass::Api,
        ),
        auth: Some(auth),
    };
    Mock::given(method("GET"))
        .and(path("/configured/api/codex/environments"))
        .and(header("authorization", format!("Bearer {initial_token}")))
        .respond_with(ResponseTemplate::new(401))
        .expect(0)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/configured/api/codex/environments"))
        .and(header("authorization", format!("Bearer {refreshed_token}")))
        .and(header("chatgpt-account-id", "account-a"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            {"id": "env-a", "label": "Environment A"},
        ])))
        .expect(2)
        .mount(&server)
        .await;
    manager.refresh_token().await?;
    for _ in 0..2 {
        let environments = crate::env_detect::list_environments(
            &environment_http,
            &format!("{}/configured", server.uri()),
            PathStyle::CodexApi,
            &crate::util::build_chatgpt_headers(),
        )
        .await?;
        assert_eq!(environments.len(), 1);
        assert_eq!(environments[0].id, "env-a");
    }
    assert_eq!(external.refreshes.load(Ordering::SeqCst), 1);
    server.verify().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cloud_list_preserves_explicit_agent_identity_without_registration() -> anyhow::Result<()> {
    const CHILD: &str = "STEVE_CLOUD_EXPLICIT_IDENTITY_TEST";
    const TEST: &str =
        "backend_auth_tests::cloud_list_preserves_explicit_agent_identity_without_registration";
    if std::env::var_os(CHILD).is_some() {
        let context =
            super::init_backend("cloud-programmatic-test", &CliConfigOverrides::default()).await?;
        let page = context.backend.list_tasks(None, Some(10), None).await?;
        assert!(page.tasks.is_empty());
        let environments = crate::env_detect::list_environments(
            &context.environment_http,
            &context.base_url,
            context.path_style,
            &crate::util::build_chatgpt_headers(),
        )
        .await?;
        assert_eq!(environments.len(), 1);
        assert_eq!(environments[0].id, "env-a");
        return Ok(());
    }
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/configured/wham/tasks/list"))
        .and(header("chatgpt-account-id", "account-a"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"items": [], "cursor": null})),
        )
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/configured/wham/environments"))
        .and(header("chatgpt-account-id", "account-a"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            {"id": "env-a", "label": "Environment A"},
        ])))
        .expect(1)
        .mount(&server)
        .await;
    let home = tempfile::tempdir()?;
    let key = codex_agent_identity::generate_agent_key_material()?;
    std::fs::write(
        home.path().join("auth.json"),
        json!({
            "auth_mode": "agentIdentity",
            "agent_identity": {
                "agent_runtime_id": "existing-agent",
                "agent_private_key": key.private_key_pkcs8_base64,
                "account_id": "account-a",
                "chatgpt_user_id": "user-a",
                "plan_type": "plus",
                "chatgpt_account_is_fedramp": false,
                "task_id": "existing-task",
            },
        })
        .to_string(),
    )?;
    std::fs::write(
        home.path().join("config.toml"),
        format!(
            "model_provider = 'openai'\nchatgpt_base_url = {:?}\nchatgpt_path_style = 'chatgpt_api'\ncli_auth_credentials_store = 'file'\n",
            format!("{}/configured", server.uri()),
        ),
    )?;
    let mut command = std::process::Command::new(std::env::current_exe()?);
    command
        .args(["--exact", TEST, "--nocapture"])
        .current_dir(home.path())
        .env(CHILD, "1")
        .env("STEVE_HOME", home.path())
        .env_remove("STEVE_CLOUD_TASKS_BASE_URL")
        .env_remove("STEVE_CLOUD_TASKS_MODE")
        .env_remove("STEVE_ACCESS_TOKEN")
        .env_remove("STEVE_API_KEY")
        .env_remove("OPENAI_API_KEY");
    let output = tokio::task::spawn_blocking(move || command.output()).await??;
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("1 passed"));
    let requests = server.received_requests().await.unwrap();
    assert_eq!(
        requests.len(),
        2,
        "only configured task and environment requests are admitted"
    );
    for request in requests {
        let assertion = request.headers["authorization"]
            .to_str()?
            .strip_prefix("AgentAssertion ")
            .unwrap();
        let envelope: serde_json::Value = serde_json::from_slice(
            &base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(assertion)?,
        )?;
        assert_eq!(envelope["agent_runtime_id"], "existing-agent");
        assert_eq!(envelope["task_id"], "existing-task");
        assert!(!request.headers.contains_key("x-openai-agent-assertion"));
    }
    server.verify().await;
    Ok(())
}
