use anyhow::Result;
use anyhow::bail;
use app_test_support::TestAppServer;
use app_test_support::to_response;

use app_test_support::ChatGptAuthFixture;
use app_test_support::ChatGptIdTokenClaims;
use app_test_support::DEFAULT_CLIENT_NAME;
use app_test_support::encode_id_token;
use app_test_support::write_chatgpt_auth;
use app_test_support::write_models_cache;
use chrono::Duration as ChronoDuration;
use chrono::Utc;
use codex_app_server_protocol::Account;
use codex_app_server_protocol::AccountLoginCompletedNotification;
use codex_app_server_protocol::AccountUpdatedNotification;
use codex_app_server_protocol::AuthMode;
use codex_app_server_protocol::ChatgptAuthTokensRefreshReason;
use codex_app_server_protocol::ChatgptAuthTokensRefreshResponse;
use codex_app_server_protocol::ClientInfo;
use codex_app_server_protocol::GetAccountParams;
use codex_app_server_protocol::GetAccountResponse;
use codex_app_server_protocol::GetAuthStatusParams;
use codex_app_server_protocol::GetAuthStatusResponse;
use codex_app_server_protocol::InitializeCapabilities;
use codex_app_server_protocol::JSONRPCError;
use codex_app_server_protocol::JSONRPCErrorError;
use codex_app_server_protocol::JSONRPCMessage;
use codex_app_server_protocol::JSONRPCNotification;
use codex_app_server_protocol::JSONRPCResponse;
use codex_app_server_protocol::LoginAccountResponse;
use codex_app_server_protocol::LogoutAccountResponse;
use codex_app_server_protocol::RequestId;
use codex_app_server_protocol::ServerNotification;
use codex_app_server_protocol::ServerRequest;
use codex_app_server_protocol::TurnCompletedNotification;
use codex_app_server_protocol::TurnStatus;
use codex_config::types::AuthCredentialsStoreMode;
use codex_http_client::HttpClientBuilder;
use codex_login::AuthDotJson;
use codex_login::AuthKeyringBackendKind;
use codex_login::REFRESH_TOKEN_URL_OVERRIDE_ENV_VAR;
use codex_login::auth::BedrockAccessKeysAuth;
use codex_login::auth::BedrockApiKeyAuth;
use codex_login::load_auth_dot_json;
use codex_login::login_with_api_key;
use codex_login::login_with_bedrock_api_key;
use codex_protocol::account::PlanType as AccountPlanType;
use codex_protocol::auth::AuthMode as DomainAuthMode;
use core_test_support::responses;
use pretty_assertions::assert_eq;
use serde_json::json;
use std::path::Path;
use std::time::Duration;
use tempfile::TempDir;
use test_case::test_case;
use tokio::time::timeout;
use url::Url;
use wiremock::Mock;
use wiremock::MockServer;
use wiremock::ResponseTemplate;
use wiremock::matchers::header;
use wiremock::matchers::method;
use wiremock::matchers::path;

const DEFAULT_READ_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);
const WORKSPACE_ID_ALLOWED: &str = "123e4567-e89b-42d3-a456-426614174000";
const WORKSPACE_ID_DISALLOWED: &str = "123e4567-e89b-42d3-a456-426614174002";
const WORKSPACE_ID_EMBEDDED: &str = "123e4567-e89b-42d3-a456-426614174010";
const WORKSPACE_ID_INITIAL: &str = "123e4567-e89b-42d3-a456-426614174011";
const WORKSPACE_ID_STALE: &str = "123e4567-e89b-42d3-a456-426614174014";

fn expected_workspace_routing(
    account_id: &str,
) -> Option<codex_app_server_protocol::WorkspaceRouting> {
    Some(codex_app_server_protocol::WorkspaceRouting {
        chatgpt_account_id: account_id.to_string(),
        backend_origin: "https://chatgpt.com".to_string(),
        account_routing_override: codex_app_server_protocol::AccountRoutingOverride::NoConstraint,
    })
}

#[path = "account_enterprise_tests.rs"]
mod enterprise_tests;

// Helper to create a minimal config.toml for the app server
#[derive(Default)]
struct CreateConfigTomlParams {
    forced_method: Option<String>,
    forced_workspace_id: Option<String>,
    forced_workspace_ids: Option<Vec<String>>,
    requires_openai_auth: Option<bool>,
    base_url: Option<String>,
    chatgpt_base_url: Option<String>,
    model_provider_id: Option<String>,
    extra_provider_config: Option<String>,
}

fn create_config_toml(codex_home: &Path, params: CreateConfigTomlParams) -> std::io::Result<()> {
    let config_toml = codex_home.join("config.toml");
    let base_url = params
        .base_url
        .unwrap_or_else(|| "http://127.0.0.1:0/v1".to_string());
    let forced_line = if let Some(method) = params.forced_method {
        format!("forced_login_method = \"{method}\"\n")
    } else {
        String::new()
    };
    let forced_workspace_line = if let Some(ws) = params.forced_workspace_id {
        format!("forced_chatgpt_workspace_id = \"{ws}\"\n")
    } else if let Some(workspaces) = params.forced_workspace_ids {
        let workspaces = workspaces
            .into_iter()
            .map(|workspace_id| format!("\"{workspace_id}\""))
            .collect::<Vec<_>>()
            .join(", ");
        format!("forced_chatgpt_workspace_id = [{workspaces}]\n")
    } else {
        String::new()
    };
    let requires_line = match params.requires_openai_auth {
        Some(true) => "requires_openai_auth = true\n".to_string(),
        Some(false) => String::new(),
        None => String::new(),
    };
    let chatgpt_base_url_line = params
        .chatgpt_base_url
        .map(|url| format!("chatgpt_base_url = \"{url}\"\n"))
        .unwrap_or_default();
    let model_provider_id = params
        .model_provider_id
        .unwrap_or_else(|| "mock_provider".to_string());
    let provider_section = if model_provider_id == "mock_provider" {
        format!(
            r#"[model_providers.mock_provider]
name = "Mock provider for test"
base_url = "{base_url}"
wire_api = "responses"
request_max_retries = 0
stream_max_retries = 0
{requires_line}
"#
        )
    } else {
        params.extra_provider_config.unwrap_or_default()
    };
    let contents = format!(
        r#"
model = "mock-model"
approval_policy = "never"
sandbox_mode = "danger-full-access"
{chatgpt_base_url_line}
{forced_line}
{forced_workspace_line}

model_provider = "{model_provider_id}"

[features]
shell_snapshot = false

{provider_section}
"#
    );
    std::fs::write(config_toml, contents)
}

fn read_config_toml(codex_home: &Path) -> Result<toml::Value> {
    Ok(toml::from_str(&std::fs::read_to_string(
        codex_home.join("config.toml"),
    )?)?)
}

fn load_file_auth(codex_home: &Path) -> Result<Option<AuthDotJson>> {
    Ok(load_auth_dot_json(
        codex_home,
        AuthCredentialsStoreMode::File,
        AuthKeyringBackendKind::default(),
    )?)
}

fn aws_managed_bedrock_config() -> CreateConfigTomlParams {
    CreateConfigTomlParams {
        model_provider_id: Some("amazon-bedrock".to_string()),
        extra_provider_config: Some(
            r#"[model_providers.amazon-bedrock.aws]
profile = "codex-bedrock"
region = "us-west-2"
"#
            .to_string(),
        ),
        ..Default::default()
    }
}

async fn read_account(mcp: &mut TestAppServer) -> Result<GetAccountResponse> {
    let request_id = mcp
        .send_get_account_request(GetAccountParams {
            refresh_token: false,
        })
        .await?;
    timeout(DEFAULT_READ_TIMEOUT, mcp.read_response(request_id)).await?
}

async fn assert_account_updated(
    mcp: &mut TestAppServer,
    auth_mode: Option<AuthMode>,
) -> Result<()> {
    let payload: AccountUpdatedNotification = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_notification("account/updated"),
    )
    .await??;
    assert_eq!(
        payload,
        AccountUpdatedNotification {
            auth_mode,
            plan_type: None,
        }
    );
    Ok(())
}

#[test_case(false; "api_key")]
#[test_case(true; "enterprise_cleanup_failure")]
#[tokio::test]
async fn logout_account_removes_auth_and_notifies(enterprise_cleanup_failure: bool) -> Result<()> {
    let oauth_server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/oauth/revoke"))
        .respond_with(ResponseTemplate::new(200))
        .expect(0)
        .mount(&oauth_server)
        .await;
    let codex_home = TempDir::new()?;
    create_config_toml(codex_home.path(), CreateConfigTomlParams::default())?;

    if enterprise_cleanup_failure {
        write_chatgpt_auth(
            codex_home.path(),
            ChatGptAuthFixture::new("access-token")
                .account_id(WORKSPACE_ID_INITIAL)
                .chatgpt_user_id("enterprise-user"),
            AuthCredentialsStoreMode::File,
        )?;
        let mut config = read_config_toml(codex_home.path())?;
        config.as_table_mut().expect("config table").insert(
            "mcp_enterprise_managed_auth".into(),
            toml::toml! {
                [idp]
                issuer = "https://idp.example"
                client_id = "enterprise-client"
            }
            .into(),
        );
        std::fs::write(
            codex_home.path().join("config.toml"),
            toml::to_string(&config)?,
        )?;
        // Fail credential cleanup before touching the platform keyring.
        std::fs::write(codex_home.path().join("mcp-oauth-locks"), "not a directory")?;
    } else {
        login_with_api_key(
            codex_home.path(),
            "sk-test-key",
            AuthCredentialsStoreMode::File,
            AuthKeyringBackendKind::default(),
        )?;
    }
    assert!(codex_home.path().join("auth.json").exists());

    let refresh_url = format!("{}/oauth/token", oauth_server.uri());
    let mut mcp = TestAppServer::builder()
        .with_codex_home(codex_home.path())
        .without_auto_env()
        .with_env_overrides(&[
            ("OPENAI_API_KEY", None),
            (REFRESH_TOKEN_URL_OVERRIDE_ENV_VAR, Some(&refresh_url)),
        ])
        .build_initialized_with_timeout(DEFAULT_READ_TIMEOUT)
        .await?;

    let id = mcp.send_logout_account_request().await?;
    let _ok: LogoutAccountResponse = timeout(DEFAULT_READ_TIMEOUT, mcp.read_response(id)).await??;

    let note = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_notification_message("account/updated"),
    )
    .await??;
    let parsed: ServerNotification = note.try_into()?;
    let ServerNotification::AccountUpdated(payload) = parsed else {
        bail!("unexpected notification: {parsed:?}");
    };
    assert!(
        payload.auth_mode.is_none(),
        "auth_method should be None after logout"
    );
    assert_eq!(payload.plan_type, None);

    assert!(
        !codex_home.path().join("auth.json").exists(),
        "auth.json should be deleted"
    );

    let get_id = mcp
        .send_get_account_request(GetAccountParams {
            refresh_token: false,
        })
        .await?;
    let account: GetAccountResponse =
        timeout(DEFAULT_READ_TIMEOUT, mcp.read_response(get_id)).await??;
    assert_eq!(account.account, None);
    Ok(())
}

#[tokio::test]
async fn logout_account_succeeds_when_config_reload_fails() -> Result<()> {
    let codex_home = TempDir::new()?;
    create_config_toml(codex_home.path(), CreateConfigTomlParams::default())?;
    login_with_api_key(
        codex_home.path(),
        "sk-test-key",
        AuthCredentialsStoreMode::File,
        AuthKeyringBackendKind::default(),
    )?;

    let mut mcp = TestAppServer::builder()
        .with_codex_home(codex_home.path())
        .without_auto_env()
        .with_env_overrides(&[("OPENAI_API_KEY", None)])
        .build_initialized_with_timeout(DEFAULT_READ_TIMEOUT)
        .await?;

    std::fs::write(codex_home.path().join("config.toml"), "invalid = [")?;

    let request_id = mcp.send_logout_account_request().await?;
    let response = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(request_id)),
    )
    .await??;
    assert_eq!(
        to_response::<LogoutAccountResponse>(response)?,
        LogoutAccountResponse {}
    );
    assert_eq!(load_file_auth(codex_home.path())?, None);
    assert_account_updated(&mut mcp, /*auth_mode*/ None).await?;

    Ok(())
}

#[test_case(false, false; "local_auth_before_cloud")]
#[test_case(true, false; "local_mxc_after_cloud_selection")]
#[test_case(false, true; "cloud_mxc_without_default_recovery")]
#[tokio::test]
async fn startup_enforces_requirements_at_the_correct_load_stage(
    local_mxc: bool,
    cloud_mxc: bool,
) -> Result<()> {
    let codex_home = TempDir::new()?;
    let mock_server = MockServer::start().await;
    let require_mxc = "[windows]\nrequire_mxc = true\n";
    let fetch_cloud = local_mxc || cloud_mxc;
    if fetch_cloud {
        let bundle = codex_config::test_support::CloudConfigBundleFixture::enterprise_requirement(
            if cloud_mxc { require_mxc } else { "" },
        )
        .add_enterprise_config(format!("[features]\nprefer_mxc = {local_mxc}\n"))
        .into_bundle();
        Mock::given(path("/backend-api/wham/config/bundle"))
            .respond_with(ResponseTemplate::new(200).set_body_json(bundle))
            .expect(1..)
            .mount(&mock_server)
            .await;
    }
    create_config_toml(
        codex_home.path(),
        CreateConfigTomlParams {
            chatgpt_base_url: Some(format!("{}/backend-api", mock_server.uri())),
            ..Default::default()
        },
    )?;
    std::fs::write(
        codex_home.path().join("requirements.toml"),
        if local_mxc {
            require_mxc
        } else if cloud_mxc {
            ""
        } else {
            "allowed_login_methods = [\"api\"]\n"
        },
    )?;
    write_chatgpt_auth(
        codex_home.path(),
        ChatGptAuthFixture::new("chatgpt-token")
            .plan_type("enterprise")
            .chatgpt_user_id("user-123")
            .chatgpt_account_id("account-123")
            .account_id("account-123"),
        AuthCredentialsStoreMode::File,
    )?;

    let mut mcp = TestAppServer::builder()
        .with_codex_home(codex_home.path())
        .without_auto_env()
        .build()
        .await?;

    if cfg!(windows) && fetch_cloud && (!local_mxc || !codex_sandboxing::windows_mxc_available()) {
        assert!(
            !timeout(DEFAULT_READ_TIMEOUT, mcp.wait_for_exit())
                .await??
                .success()
        );
    } else {
        timeout(DEFAULT_READ_TIMEOUT, mcp.initialize()).await??;
        if !fetch_cloud {
            assert_eq!(read_account(&mut mcp).await?.account, None);
        }
    }
    assert_eq!(
        mock_server
            .received_requests()
            .await
            .expect("recorded requests")
            .is_empty(),
        !fetch_cloud,
    );

    Ok(())
}

#[test_case(false, "pro", AccountPlanType::Pro; "without_backend")]
#[test_case(true, "pro", AccountPlanType::Pro; "configured_backend")]
#[test_case(false, "enterprise", AccountPlanType::Enterprise; "enterprise_without_backend")]
#[test_case(true, "enterprise", AccountPlanType::Enterprise; "enterprise_configured_backend")]
#[tokio::test]
async fn set_auth_token_updates_account_and_notifies(
    with_backend: bool,
    raw_plan_type: &str,
    plan_type: AccountPlanType,
) -> Result<()> {
    let codex_home = TempDir::new()?;
    let mock_server = MockServer::start().await;
    create_config_toml(
        codex_home.path(),
        CreateConfigTomlParams {
            requires_openai_auth: Some(true),
            base_url: Some(format!("{}/v1", mock_server.uri())),
            ..Default::default()
        },
    )?;
    write_models_cache(codex_home.path()).await?;

    let access_token = encode_id_token(
        &ChatGptIdTokenClaims::new()
            .email("embedded@example.com")
            .plan_type(raw_plan_type)
            .chatgpt_account_id(WORKSPACE_ID_EMBEDDED),
    )?;

    let mut builder = TestAppServer::builder()
        .with_codex_home(codex_home.path())
        .without_auto_env()
        .with_env_overrides(&[("OPENAI_API_KEY", None)]);
    if with_backend {
        builder = builder.with_mock_chatgpt_backend();
    }
    let mut mcp = builder
        .build_initialized_with_timeout(DEFAULT_READ_TIMEOUT)
        .await?;

    let set_id = mcp
        .send_chatgpt_auth_tokens_login_request(
            access_token,
            WORKSPACE_ID_EMBEDDED.to_string(),
            Some(raw_plan_type.to_string()),
        )
        .await?;
    let response: LoginAccountResponse =
        timeout(DEFAULT_READ_TIMEOUT, mcp.read_response(set_id)).await??;
    assert_eq!(response, LoginAccountResponse::ChatgptAuthTokens {});

    let completed: AccountLoginCompletedNotification = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_notification("account/login/completed"),
    )
    .await??;
    assert!(completed.success, "{:?}", completed.error);

    let note = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_notification_message("account/updated"),
    )
    .await??;
    let parsed: ServerNotification = note.try_into()?;
    let ServerNotification::AccountUpdated(payload) = parsed else {
        bail!("unexpected notification: {parsed:?}");
    };
    assert_eq!(payload.auth_mode, Some(AuthMode::ChatgptAuthTokens));
    assert_eq!(payload.plan_type, Some(plan_type.clone()));

    let get_id = mcp
        .send_get_account_request(GetAccountParams {
            refresh_token: false,
        })
        .await?;
    let account: GetAccountResponse =
        timeout(DEFAULT_READ_TIMEOUT, mcp.read_response(get_id)).await??;
    assert_eq!(
        account,
        GetAccountResponse {
            workspace_routing: if with_backend {
                expected_workspace_routing(WORKSPACE_ID_EMBEDDED)
            } else {
                None
            },
            account: Some(Account::Chatgpt {
                email: Some("embedded@example.com".to_string()),
                plan_type,
            }),
            requires_openai_auth: true,
        }
    );

    let logout_id = mcp.send_logout_account_request().await?;
    let _: LogoutAccountResponse =
        timeout(DEFAULT_READ_TIMEOUT, mcp.read_response(logout_id)).await??;

    let get_id = mcp
        .send_get_account_request(GetAccountParams {
            refresh_token: false,
        })
        .await?;
    let account: GetAccountResponse =
        timeout(DEFAULT_READ_TIMEOUT, mcp.read_response(get_id)).await??;
    assert_eq!(account.account, None);

    Ok(())
}

#[tokio::test]
async fn account_read_refresh_token_is_noop_in_external_mode() -> Result<()> {
    let codex_home = TempDir::new()?;
    create_config_toml(
        codex_home.path(),
        CreateConfigTomlParams {
            requires_openai_auth: Some(true),
            ..Default::default()
        },
    )?;
    write_models_cache(codex_home.path()).await?;

    let access_token = encode_id_token(
        &ChatGptIdTokenClaims::new()
            .email("embedded@example.com")
            .plan_type("pro")
            .chatgpt_account_id(WORKSPACE_ID_EMBEDDED),
    )?;

    let mut mcp = TestAppServer::builder()
        .with_mock_chatgpt_backend()
        .with_codex_home(codex_home.path())
        .without_auto_env()
        .with_env_overrides(&[("OPENAI_API_KEY", None)])
        .build_initialized_with_timeout(DEFAULT_READ_TIMEOUT)
        .await?;

    let set_id = mcp
        .send_chatgpt_auth_tokens_login_request(
            access_token,
            WORKSPACE_ID_EMBEDDED.to_string(),
            Some("pro".to_string()),
        )
        .await?;
    let response: LoginAccountResponse =
        timeout(DEFAULT_READ_TIMEOUT, mcp.read_response(set_id)).await??;
    assert_eq!(response, LoginAccountResponse::ChatgptAuthTokens {});
    let _updated = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_notification_message("account/updated"),
    )
    .await??;

    let get_id = mcp
        .send_get_account_request(GetAccountParams {
            refresh_token: true,
        })
        .await?;
    let account: GetAccountResponse =
        timeout(DEFAULT_READ_TIMEOUT, mcp.read_response(get_id)).await??;
    assert_eq!(
        account,
        GetAccountResponse {
            workspace_routing: expected_workspace_routing(WORKSPACE_ID_EMBEDDED),
            account: Some(Account::Chatgpt {
                email: Some("embedded@example.com".to_string()),
                plan_type: AccountPlanType::Pro,
            }),
            requires_openai_auth: true,
        }
    );

    let refresh_request = timeout(
        Duration::from_millis(250),
        mcp.read_stream_until_request_message(),
    )
    .await;
    assert!(
        refresh_request.is_err(),
        "external mode should not emit account/chatgptAuthTokens/refresh for refreshToken=true"
    );

    Ok(())
}

async fn respond_to_refresh_request(
    mcp: &mut TestAppServer,
    access_token: &str,
    chatgpt_account_id: &str,
    chatgpt_plan_type: Option<&str>,
) -> Result<()> {
    let refresh_req: ServerRequest = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_request_message(),
    )
    .await??;
    let ServerRequest::ChatgptAuthTokensRefresh { request_id, params } = refresh_req else {
        bail!("expected account/chatgptAuthTokens/refresh request, got {refresh_req:?}");
    };
    assert_eq!(params.reason, ChatgptAuthTokensRefreshReason::Unauthorized);
    let response = ChatgptAuthTokensRefreshResponse {
        access_token: access_token.to_string(),
        chatgpt_account_id: chatgpt_account_id.to_string(),
        chatgpt_plan_type: chatgpt_plan_type.map(str::to_string),
    };
    mcp.send_response(request_id, serde_json::to_value(response)?)
        .await?;
    Ok(())
}

async fn mount_disabled_attribution_settings(mock_server: &MockServer) {
    app_test_support::mount_workspace_routing(mock_server).await;
    Mock::given(method("GET"))
        .and(path("/backend-api/wham/settings/user"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "commit_attribution_enabled": false,
        })))
        .mount(mock_server)
        .await;
}

#[test_case("/v1"; "responses")]
#[test_case("/backend-api/codex"; "discovery")]
#[tokio::test]
// 401 response triggers account/chatgptAuthTokens/refresh and retries with new tokens.
async fn external_auth_refreshes_on_unauthorized(model_path: &str) -> Result<()> {
    let codex_home = TempDir::new()?;
    let mock_server = MockServer::start().await;
    let backend = MockServer::start().await;
    create_config_toml(
        codex_home.path(),
        CreateConfigTomlParams {
            model_provider_id: Some("routing".into()),
            extra_provider_config: Some(format!(
                "[model_providers.routing]\nname = \"OpenAI\"\nrequires_openai_auth = true\nbase_url = \"{}{model_path}\"\nrequest_max_retries = 0\nstream_max_retries = 0\n",
                mock_server.uri(),
            )),
            chatgpt_base_url: Some(format!("{}/backend-api", backend.uri())),
            ..Default::default()
        },
    )?;
    write_models_cache(codex_home.path()).await?;

    let success_sse = responses::sse(vec![
        responses::ev_response_created("resp-turn"),
        responses::ev_assistant_message("msg-turn", "turn ok"),
        responses::ev_completed("resp-turn"),
    ]);
    let unauthorized = ResponseTemplate::new(401).set_body_json(json!({
        "error": { "message": "unauthorized" }
    }));
    let responses_mock = responses::mount_response_sequence(
        &mock_server,
        if model_path == "/v1" {
            vec![unauthorized, responses::sse_response(success_sse)]
        } else {
            vec![responses::sse_response(success_sse)]
        },
    )
    .await;
    mount_disabled_attribution_settings(&backend).await;

    let initial_access_token = encode_id_token(
        &ChatGptIdTokenClaims::new()
            .email("initial@example.com")
            .chatgpt_user_id("refresh-user")
            .plan_type("pro")
            .chatgpt_account_id(WORKSPACE_ID_INITIAL),
    )?;
    let refreshed_workspace = WORKSPACE_ID_INITIAL;
    let refreshed_access_token = encode_id_token(
        &ChatGptIdTokenClaims::new()
            .email("refreshed@example.com")
            .chatgpt_user_id("refresh-user")
            .plan_type("pro")
            .chatgpt_account_id(refreshed_workspace),
    )?;

    let mut mcp = TestAppServer::builder()
        .with_mock_chatgpt_backend()
        .with_codex_home(codex_home.path())
        .with_env_overrides(&[("OPENAI_API_KEY", None)])
        .build_initialized_with_timeout(DEFAULT_READ_TIMEOUT)
        .await?;

    let set_id = mcp
        .send_chatgpt_auth_tokens_login_request(
            initial_access_token.clone(),
            WORKSPACE_ID_INITIAL.to_string(),
            Some("pro".to_string()),
        )
        .await?;
    let response: LoginAccountResponse =
        timeout(DEFAULT_READ_TIMEOUT, mcp.read_response(set_id)).await??;
    assert_eq!(response, LoginAccountResponse::ChatgptAuthTokens {});
    let _updated = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_notification_message("account/updated"),
    )
    .await??;

    let thread_req = mcp
        .send_thread_start_request_with_auto_env(codex_app_server_protocol::ThreadStartParams {
            model: Some("mock-model".to_string()),
            ..Default::default()
        })
        .await?;
    let thread: codex_app_server_protocol::ThreadStartResponse =
        timeout(DEFAULT_READ_TIMEOUT, mcp.read_response(thread_req)).await??;

    if model_path != "/v1" {
        // Arm the failure before invalidating the route cached during login. Background
        // discovery can otherwise cache a successful response before this mock is mounted.
        Mock::given(path("/backend-api/wham/accounts/check"))
            .and(header(
                "authorization",
                format!("Bearer {initial_access_token}"),
            ))
            .respond_with(ResponseTemplate::new(/*s*/ 401))
            .with_priority(/*p*/ 1)
            .expect(/*r*/ 1)
            .mount(&backend)
            .await;
        let config_path = codex_home.path().join("config.toml");
        std::fs::write(
            &config_path,
            std::fs::read_to_string(&config_path)?.replace("/backend-api\"", "/backend-api/\""),
        )?;
    }

    let turn_req = mcp
        .send_turn_start_request(codex_app_server_protocol::TurnStartParams {
            thread_id: thread.thread.id,
            client_user_message_id: None,
            input: vec![codex_app_server_protocol::UserInput::Text {
                text: "Hello".to_string(),
                text_elements: Vec::new(),
            }],
            ..Default::default()
        })
        .await?;
    respond_to_refresh_request(
        &mut mcp,
        &refreshed_access_token,
        refreshed_workspace,
        Some("pro"),
    )
    .await?;
    let _: codex_app_server_protocol::TurnStartResponse =
        timeout(DEFAULT_READ_TIMEOUT, mcp.read_response(turn_req)).await??;
    let turn_completed = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_notification_message("turn/completed"),
    )
    .await??;

    let completed: TurnCompletedNotification =
        serde_json::from_value(turn_completed.params.expect("turn/completed params"))?;
    assert_eq!(completed.turn.status, TurnStatus::Completed);

    let requests = responses_mock.requests();
    assert_eq!(requests.len(), if model_path == "/v1" { 2 } else { 1 });
    assert_eq!(
        requests[requests.len() - 1].header("authorization"),
        Some(format!("Bearer {refreshed_access_token}"))
    );

    Ok(())
}

#[tokio::test]
// Client returns JSON-RPC error to refresh; turn fails.
async fn external_auth_refresh_error_fails_turn() -> Result<()> {
    let codex_home = TempDir::new()?;
    let mock_server = MockServer::start().await;
    create_config_toml(
        codex_home.path(),
        CreateConfigTomlParams {
            requires_openai_auth: Some(true),
            base_url: Some(format!("{}/v1", mock_server.uri())),
            chatgpt_base_url: Some(format!("{}/backend-api", mock_server.uri())),
            ..Default::default()
        },
    )?;
    write_models_cache(codex_home.path()).await?;

    let unauthorized = ResponseTemplate::new(401).set_body_json(json!({
        "error": { "message": "unauthorized" }
    }));
    let _responses_mock =
        responses::mount_response_sequence(&mock_server, vec![unauthorized]).await;
    mount_disabled_attribution_settings(&mock_server).await;

    let initial_access_token = encode_id_token(
        &ChatGptIdTokenClaims::new()
            .email("initial@example.com")
            .plan_type("pro")
            .chatgpt_account_id(WORKSPACE_ID_INITIAL),
    )?;

    let mut mcp = TestAppServer::builder()
        .with_mock_chatgpt_backend()
        .with_codex_home(codex_home.path())
        .with_env_overrides(&[("OPENAI_API_KEY", None)])
        .build_initialized_with_timeout(DEFAULT_READ_TIMEOUT)
        .await?;

    let set_id = mcp
        .send_chatgpt_auth_tokens_login_request(
            initial_access_token,
            WORKSPACE_ID_INITIAL.to_string(),
            Some("pro".to_string()),
        )
        .await?;
    let response: LoginAccountResponse =
        timeout(DEFAULT_READ_TIMEOUT, mcp.read_response(set_id)).await??;
    assert_eq!(response, LoginAccountResponse::ChatgptAuthTokens {});
    let _updated = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_notification_message("account/updated"),
    )
    .await??;

    let thread_req = mcp
        .send_thread_start_request_with_auto_env(codex_app_server_protocol::ThreadStartParams {
            model: Some("mock-model".to_string()),
            ..Default::default()
        })
        .await?;
    let thread: codex_app_server_protocol::ThreadStartResponse =
        timeout(DEFAULT_READ_TIMEOUT, mcp.read_response(thread_req)).await??;

    let turn_req = mcp
        .send_turn_start_request(codex_app_server_protocol::TurnStartParams {
            thread_id: thread.thread.id.clone(),
            client_user_message_id: None,
            input: vec![codex_app_server_protocol::UserInput::Text {
                text: "Hello".to_string(),
                text_elements: Vec::new(),
            }],
            ..Default::default()
        })
        .await?;

    let refresh_req: ServerRequest = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_request_message(),
    )
    .await??;
    let ServerRequest::ChatgptAuthTokensRefresh { request_id, .. } = refresh_req else {
        bail!("expected account/chatgptAuthTokens/refresh request, got {refresh_req:?}");
    };

    mcp.send_error(
        request_id,
        JSONRPCErrorError {
            code: -32_000,
            message: "refresh failed".to_string(),
            data: None,
        },
    )
    .await?;

    let _: codex_app_server_protocol::TurnStartResponse =
        timeout(DEFAULT_READ_TIMEOUT, mcp.read_response(turn_req)).await??;
    let completed_notif: JSONRPCNotification = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_notification_message("turn/completed"),
    )
    .await??;
    let completed: TurnCompletedNotification = serde_json::from_value(
        completed_notif
            .params
            .expect("turn/completed params must be present"),
    )?;
    assert_eq!(completed.turn.status, TurnStatus::Failed);
    assert!(completed.turn.error.is_some());

    Ok(())
}

#[tokio::test]
// Refresh returns tokens for the wrong workspace; turn fails.
async fn external_auth_refresh_mismatched_workspace_fails_turn() -> Result<()> {
    let codex_home = TempDir::new()?;
    let mock_server = MockServer::start().await;
    create_config_toml(
        codex_home.path(),
        CreateConfigTomlParams {
            forced_workspace_id: Some(WORKSPACE_ID_ALLOWED.to_string()),
            requires_openai_auth: Some(true),
            base_url: Some(format!("{}/v1", mock_server.uri())),
            chatgpt_base_url: Some(format!("{}/backend-api", mock_server.uri())),
            ..Default::default()
        },
    )?;
    write_models_cache(codex_home.path()).await?;

    let unauthorized = ResponseTemplate::new(401).set_body_json(json!({
        "error": { "message": "unauthorized" }
    }));
    let _responses_mock =
        responses::mount_response_sequence(&mock_server, vec![unauthorized]).await;
    mount_disabled_attribution_settings(&mock_server).await;

    let initial_access_token = encode_id_token(
        &ChatGptIdTokenClaims::new()
            .email("initial@example.com")
            .plan_type("pro")
            .chatgpt_account_id(WORKSPACE_ID_ALLOWED),
    )?;
    let refreshed_access_token = encode_id_token(
        &ChatGptIdTokenClaims::new()
            .email("refreshed@example.com")
            .plan_type("pro")
            .chatgpt_account_id(WORKSPACE_ID_DISALLOWED),
    )?;

    let mut mcp = TestAppServer::builder()
        .with_mock_chatgpt_backend()
        .with_codex_home(codex_home.path())
        .with_env_overrides(&[("OPENAI_API_KEY", None)])
        .build_initialized_with_timeout(DEFAULT_READ_TIMEOUT)
        .await?;

    let set_id = mcp
        .send_chatgpt_auth_tokens_login_request(
            initial_access_token,
            WORKSPACE_ID_ALLOWED.to_string(),
            Some("pro".to_string()),
        )
        .await?;
    let response: LoginAccountResponse =
        timeout(DEFAULT_READ_TIMEOUT, mcp.read_response(set_id)).await??;
    assert_eq!(response, LoginAccountResponse::ChatgptAuthTokens {});
    let _updated = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_notification_message("account/updated"),
    )
    .await??;

    let thread_req = mcp
        .send_thread_start_request_with_auto_env(codex_app_server_protocol::ThreadStartParams {
            model: Some("mock-model".to_string()),
            ..Default::default()
        })
        .await?;
    let thread: codex_app_server_protocol::ThreadStartResponse =
        timeout(DEFAULT_READ_TIMEOUT, mcp.read_response(thread_req)).await??;

    let turn_req = mcp
        .send_turn_start_request(codex_app_server_protocol::TurnStartParams {
            thread_id: thread.thread.id.clone(),
            client_user_message_id: None,
            input: vec![codex_app_server_protocol::UserInput::Text {
                text: "Hello".to_string(),
                text_elements: Vec::new(),
            }],
            ..Default::default()
        })
        .await?;

    let refresh_req: ServerRequest = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_request_message(),
    )
    .await??;
    let ServerRequest::ChatgptAuthTokensRefresh { request_id, .. } = refresh_req else {
        bail!("expected account/chatgptAuthTokens/refresh request, got {refresh_req:?}");
    };

    mcp.send_response(
        request_id,
        serde_json::to_value(ChatgptAuthTokensRefreshResponse {
            access_token: refreshed_access_token,
            chatgpt_account_id: WORKSPACE_ID_DISALLOWED.to_string(),
            chatgpt_plan_type: Some("pro".to_string()),
        })?,
    )
    .await?;

    let _: codex_app_server_protocol::TurnStartResponse =
        timeout(DEFAULT_READ_TIMEOUT, mcp.read_response(turn_req)).await??;
    let completed_notif: JSONRPCNotification = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_notification_message("turn/completed"),
    )
    .await??;
    let completed: TurnCompletedNotification = serde_json::from_value(
        completed_notif
            .params
            .expect("turn/completed params must be present"),
    )?;
    assert_eq!(completed.turn.status, TurnStatus::Failed);
    assert!(completed.turn.error.is_some());

    Ok(())
}

#[tokio::test]
// Refresh returns a malformed access token; turn fails.
async fn external_auth_refresh_invalid_access_token_fails_turn() -> Result<()> {
    let codex_home = TempDir::new()?;
    let mock_server = MockServer::start().await;
    create_config_toml(
        codex_home.path(),
        CreateConfigTomlParams {
            requires_openai_auth: Some(true),
            base_url: Some(format!("{}/v1", mock_server.uri())),
            chatgpt_base_url: Some(format!("{}/backend-api", mock_server.uri())),
            ..Default::default()
        },
    )?;
    write_models_cache(codex_home.path()).await?;

    let unauthorized = ResponseTemplate::new(401).set_body_json(json!({
        "error": { "message": "unauthorized" }
    }));
    let _responses_mock =
        responses::mount_response_sequence(&mock_server, vec![unauthorized]).await;
    mount_disabled_attribution_settings(&mock_server).await;

    let initial_access_token = encode_id_token(
        &ChatGptIdTokenClaims::new()
            .email("initial@example.com")
            .plan_type("pro")
            .chatgpt_account_id(WORKSPACE_ID_INITIAL),
    )?;

    let mut mcp = TestAppServer::builder()
        .with_mock_chatgpt_backend()
        .with_codex_home(codex_home.path())
        .with_env_overrides(&[("OPENAI_API_KEY", None)])
        .build_initialized_with_timeout(DEFAULT_READ_TIMEOUT)
        .await?;

    let set_id = mcp
        .send_chatgpt_auth_tokens_login_request(
            initial_access_token,
            WORKSPACE_ID_INITIAL.to_string(),
            Some("pro".to_string()),
        )
        .await?;
    let response: LoginAccountResponse =
        timeout(DEFAULT_READ_TIMEOUT, mcp.read_response(set_id)).await??;
    assert_eq!(response, LoginAccountResponse::ChatgptAuthTokens {});
    let _updated = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_notification_message("account/updated"),
    )
    .await??;

    let thread_req = mcp
        .send_thread_start_request_with_auto_env(codex_app_server_protocol::ThreadStartParams {
            model: Some("mock-model".to_string()),
            ..Default::default()
        })
        .await?;
    let thread: codex_app_server_protocol::ThreadStartResponse =
        timeout(DEFAULT_READ_TIMEOUT, mcp.read_response(thread_req)).await??;

    let turn_req = mcp
        .send_turn_start_request(codex_app_server_protocol::TurnStartParams {
            thread_id: thread.thread.id.clone(),
            client_user_message_id: None,
            input: vec![codex_app_server_protocol::UserInput::Text {
                text: "Hello".to_string(),
                text_elements: Vec::new(),
            }],
            ..Default::default()
        })
        .await?;

    let refresh_req: ServerRequest = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_request_message(),
    )
    .await??;
    let ServerRequest::ChatgptAuthTokensRefresh { request_id, .. } = refresh_req else {
        bail!("expected account/chatgptAuthTokens/refresh request, got {refresh_req:?}");
    };

    mcp.send_response(
        request_id,
        serde_json::to_value(ChatgptAuthTokensRefreshResponse {
            access_token: "not-a-jwt".to_string(),
            chatgpt_account_id: WORKSPACE_ID_INITIAL.to_string(),
            chatgpt_plan_type: Some("pro".to_string()),
        })?,
    )
    .await?;

    let _: codex_app_server_protocol::TurnStartResponse =
        timeout(DEFAULT_READ_TIMEOUT, mcp.read_response(turn_req)).await??;
    let completed_notif: JSONRPCNotification = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_notification_message("turn/completed"),
    )
    .await??;
    let completed: TurnCompletedNotification = serde_json::from_value(
        completed_notif
            .params
            .expect("turn/completed params must be present"),
    )?;
    assert_eq!(completed.turn.status, TurnStatus::Failed);
    assert!(completed.turn.error.is_some());

    Ok(())
}

#[tokio::test]
async fn login_account_api_key_succeeds_and_notifies() -> Result<()> {
    let codex_home = TempDir::new()?;
    create_config_toml(codex_home.path(), CreateConfigTomlParams::default())?;

    let mut mcp = TestAppServer::builder()
        .with_codex_home(codex_home.path())
        .without_auto_env()
        .build_initialized_with_timeout(DEFAULT_READ_TIMEOUT)
        .await?;

    let req_id = mcp
        .send_login_account_api_key_request("sk-test-key")
        .await?;
    let login: LoginAccountResponse =
        timeout(DEFAULT_READ_TIMEOUT, mcp.read_response(req_id)).await??;
    assert_eq!(login, LoginAccountResponse::ApiKey {});

    let note = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_notification_message("account/login/completed"),
    )
    .await??;
    let parsed: ServerNotification = note.try_into()?;
    let ServerNotification::AccountLoginCompleted(payload) = parsed else {
        bail!("unexpected notification: {parsed:?}");
    };
    pretty_assertions::assert_eq!(payload.login_id, None);
    pretty_assertions::assert_eq!(payload.success, true);
    pretty_assertions::assert_eq!(payload.error, None);

    let note = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_notification_message("account/updated"),
    )
    .await??;
    let parsed: ServerNotification = note.try_into()?;
    let ServerNotification::AccountUpdated(payload) = parsed else {
        bail!("unexpected notification: {parsed:?}");
    };
    pretty_assertions::assert_eq!(payload.auth_mode, Some(AuthMode::ApiKey));
    pretty_assertions::assert_eq!(payload.plan_type, None);

    assert!(codex_home.path().join("auth.json").exists());
    Ok(())
}

#[test_case("amazonBedrock"; "api_key")]
#[test_case("amazonBedrockAccessKeys"; "access_keys")]
#[tokio::test]
async fn login_amazon_bedrock_replaces_primary_auth_and_persists_provider(
    credential_type: &str,
) -> Result<()> {
    let managed_access_keys = credential_type == "amazonBedrockAccessKeys";
    let codex_home = TempDir::new()?;
    create_config_toml(codex_home.path(), CreateConfigTomlParams::default())?;
    let config_path = codex_home.path().join("config.toml");
    let original_config = std::fs::read_to_string(&config_path)?;
    std::fs::write(
        &config_path,
        format!(
            "{original_config}\n[model_providers.amazon-bedrock]\n\
             http_headers = {{ X-Existing = \"preserved\" }}\n\
             [model_providers.amazon-bedrock.aws]\n\
             profile = \"stale-profile\"\n\
             region = \"us-east-1\"\n\
             auth_refresh = {{ command = \"aws\" }}\n"
        ),
    )?;
    login_with_api_key(
        codex_home.path(),
        "sk-test-key",
        AuthCredentialsStoreMode::File,
        AuthKeyringBackendKind::default(),
    )?;
    let mut mcp = TestAppServer::builder()
        .with_codex_home(codex_home.path())
        .without_auto_env()
        .with_env_overrides(&[("OPENAI_API_KEY", None)])
        .build_initialized_with_timeout(DEFAULT_READ_TIMEOUT)
        .await?;
    let mut expected_config = read_config_toml(codex_home.path())?;
    expected_config
        .as_table_mut()
        .expect("config should be a table")
        .insert(
            "model_provider".to_string(),
            toml::Value::String("amazon-bedrock".to_string()),
        );
    expected_config["model_providers"]["amazon-bedrock"]["aws"]
        .as_table_mut()
        .expect("AWS configuration should be a table")
        .remove("profile");
    if managed_access_keys {
        expected_config["model_providers"]["amazon-bedrock"]["aws"]["region"] =
            toml::Value::String("us-west-2".to_string());
    }
    let params = if managed_access_keys {
        json!({
            "type": credential_type,
            "accessKeyId": " test-id ",
            "secretAccessKey": " test-secret ",
            "sessionToken": " test-token ",
            "region": " us-west-2 ",
        })
    } else {
        json!({
            "type": credential_type,
            "apiKey": " managed-bedrock-api-key ",
            "region": " us-west-2 ",
        })
    };
    let request_id = mcp.send_login_account_request(params).await?;
    let response: JSONRPCResponse = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(request_id)),
    )
    .await??;
    assert_eq!(
        to_response::<LoginAccountResponse>(response)?,
        LoginAccountResponse::AmazonBedrock {}
    );

    assert_eq!(
        load_file_auth(codex_home.path())?,
        Some(AuthDotJson {
            auth_mode: Some(if managed_access_keys {
                DomainAuthMode::BedrockAccessKeys
            } else {
                DomainAuthMode::BedrockApiKey
            }),
            openai_api_key: None,
            tokens: None,
            last_refresh: None,
            agent_identity: None,
            personal_access_token: None,
            bedrock_api_key: (!managed_access_keys).then(|| BedrockApiKeyAuth {
                api_key: "managed-bedrock-api-key".to_string(),
                region: "us-west-2".to_string(),
            }),
            bedrock_access_keys: managed_access_keys.then(|| BedrockAccessKeysAuth {
                access_key_id: "test-id".to_string(),
                secret_access_key: "test-secret".to_string(),
                session_token: Some("test-token".to_string()),
            }),
        })
    );
    assert_eq!(read_config_toml(codex_home.path())?, expected_config);
    assert!(!codex_home.path().join(".env").exists());

    let notification = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_notification_message("account/login/completed"),
    )
    .await??;
    let ServerNotification::AccountLoginCompleted(payload) = notification.try_into()? else {
        bail!("unexpected notification")
    };
    assert_eq!(
        payload,
        AccountLoginCompletedNotification {
            login_id: None,
            success: true,
            error: None,
            onboarding_entrypoint: None,
        }
    );
    let auth_mode = if managed_access_keys {
        AuthMode::BedrockAccessKeys
    } else {
        AuthMode::BedrockApiKey
    };
    assert_account_updated(&mut mcp, Some(auth_mode)).await?;
    assert_eq!(
        read_account(&mut mcp).await?,
        GetAccountResponse {
            workspace_routing: None,
            account: Some(Account::AmazonBedrock {
                uses_codex_managed_credentials: true,
            }),
            requires_openai_auth: false,
        }
    );

    if managed_access_keys {
        let mut expected_logout_config = expected_config;
        let expected_logout_config_root = expected_logout_config
            .as_table_mut()
            .expect("config should be a table");
        expected_logout_config_root.remove("model_provider");
        expected_logout_config_root.remove("model");
        expected_logout_config["model_providers"]["amazon-bedrock"]
            .as_table_mut()
            .expect("Bedrock provider config should be a table")
            .remove("aws");

        let request_id = mcp.send_logout_account_request().await?;
        let response: LogoutAccountResponse =
            timeout(DEFAULT_READ_TIMEOUT, mcp.read_response(request_id)).await??;
        assert_eq!(response, LogoutAccountResponse {});
        assert_eq!(load_file_auth(codex_home.path())?, None);
        assert_eq!(read_config_toml(codex_home.path())?, expected_logout_config);
        assert!(!codex_home.path().join(".env").exists());
        assert_account_updated(&mut mcp, /*auth_mode*/ None).await?;
        assert_eq!(
            read_account(&mut mcp).await?,
            GetAccountResponse {
                workspace_routing: None,
                account: None,
                requires_openai_auth: true,
            }
        );
    }

    Ok(())
}

#[tokio::test]
async fn login_amazon_bedrock_rejects_non_bedrock_provider_override_without_changes() -> Result<()>
{
    let codex_home = TempDir::new()?;
    create_config_toml(codex_home.path(), CreateConfigTomlParams::default())?;
    login_with_api_key(
        codex_home.path(),
        "sk-test-key",
        AuthCredentialsStoreMode::File,
        AuthKeyringBackendKind::default(),
    )?;
    let expected_auth = load_file_auth(codex_home.path())?;

    let mut mcp = TestAppServer::builder()
        .with_codex_home(codex_home.path())
        .without_auto_env()
        .with_env_overrides(&[("OPENAI_API_KEY", None)])
        .with_args(&["-c", "model_provider=\"mock_provider\""])
        .build_initialized_with_timeout(DEFAULT_READ_TIMEOUT)
        .await?;
    let expected_config = read_config_toml(codex_home.path())?;

    let request_id = mcp
        .send_login_account_amazon_bedrock_request("managed-bedrock-api-key", "us-west-2")
        .await?;
    let error = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_error_message(RequestId::Integer(request_id)),
    )
    .await??;
    assert_eq!(
        error.error.message,
        "Amazon Bedrock login cannot select `amazon-bedrock` because session-flags sets `model_provider` to \"mock_provider\""
    );
    assert_eq!(load_file_auth(codex_home.path())?, expected_auth);
    assert_eq!(read_config_toml(codex_home.path())?, expected_config);

    let maybe_completed = timeout(
        Duration::from_millis(500),
        mcp.read_stream_until_notification_message("account/login/completed"),
    )
    .await;
    assert!(
        maybe_completed.is_err(),
        "account/login/completed should not be emitted when the provider is overridden"
    );
    let maybe_updated = timeout(
        Duration::from_millis(500),
        mcp.read_stream_until_notification_message("account/updated"),
    )
    .await;
    assert!(
        maybe_updated.is_err(),
        "account/updated should not be emitted when the provider is overridden"
    );

    Ok(())
}

#[tokio::test]
async fn login_amazon_bedrock_access_keys_rejects_overridden_aws_configuration() -> Result<()> {
    for config_override in [
        r#"model_providers.amazon-bedrock.aws.profile="other-account""#,
        r#"model_providers.amazon-bedrock.aws.region="eu-west-1""#,
    ] {
        let codex_home = TempDir::new()?;
        create_config_toml(codex_home.path(), CreateConfigTomlParams::default())?;
        login_with_api_key(
            codex_home.path(),
            "sk-test-key",
            AuthCredentialsStoreMode::File,
            AuthKeyringBackendKind::default(),
        )?;
        let expected_auth = load_file_auth(codex_home.path())?;

        let mut mcp = TestAppServer::builder()
            .with_codex_home(codex_home.path())
            .without_auto_env()
            .with_env_overrides(&[("OPENAI_API_KEY", None)])
            .with_args(&["-c", config_override])
            .build_initialized_with_timeout(DEFAULT_READ_TIMEOUT)
            .await?;

        let request_id = mcp
            .send_login_account_request(json!({
                "type": "amazonBedrockAccessKeys",
                "accessKeyId": "managed-access-key-id",
                "secretAccessKey": "managed-secret-access-key",
                "region": "us-west-2",
            }))
            .await?;
        let error = timeout(
            DEFAULT_READ_TIMEOUT,
            mcp.read_stream_until_error_message(RequestId::Integer(request_id)),
        )
        .await??;

        assert_eq!(
            error.error.message,
            "Amazon Bedrock configuration cannot take effect: Overridden by session flags"
        );
        assert_eq!(load_file_auth(codex_home.path())?, expected_auth);
    }

    Ok(())
}

#[tokio::test]
async fn login_amazon_bedrock_allows_bedrock_provider_override() -> Result<()> {
    let codex_home = TempDir::new()?;
    create_config_toml(codex_home.path(), CreateConfigTomlParams::default())?;
    let mut expected_config = read_config_toml(codex_home.path())?;
    expected_config
        .as_table_mut()
        .expect("config should be a table")
        .insert(
            "model_provider".to_string(),
            toml::Value::String("amazon-bedrock".to_string()),
        );

    let mut mcp = TestAppServer::builder()
        .with_codex_home(codex_home.path())
        .without_auto_env()
        .with_env_overrides(&[("OPENAI_API_KEY", None)])
        .with_args(&["-c", "model_provider=\"amazon-bedrock\""])
        .build_initialized_with_timeout(DEFAULT_READ_TIMEOUT)
        .await?;

    let request_id = mcp
        .send_login_account_amazon_bedrock_request("managed-bedrock-api-key", "us-west-2")
        .await?;
    let response = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(request_id)),
    )
    .await??;
    assert_eq!(
        to_response::<LoginAccountResponse>(response)?,
        LoginAccountResponse::AmazonBedrock {}
    );
    assert_eq!(
        load_file_auth(codex_home.path())?,
        Some(AuthDotJson {
            auth_mode: Some(DomainAuthMode::BedrockApiKey),
            openai_api_key: None,
            tokens: None,
            last_refresh: None,
            agent_identity: None,
            personal_access_token: None,
            bedrock_api_key: Some(BedrockApiKeyAuth {
                api_key: "managed-bedrock-api-key".to_string(),
                region: "us-west-2".to_string(),
            }),
            bedrock_access_keys: None,
        })
    );
    assert_eq!(read_config_toml(codex_home.path())?, expected_config);
    timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_notification_message("account/login/completed"),
    )
    .await??;
    assert_account_updated(&mut mcp, Some(AuthMode::BedrockApiKey)).await?;

    Ok(())
}

#[test_case("amazon-bedrock", "mock-model"; "mantle_clears_generic_model")]
#[test_case("amazon-bedrock-runtime", "global.openai.gpt-5.6-terra"; "runtime_clears_bedrock_model")]
#[tokio::test]
async fn logout_managed_bedrock_restores_default_account(
    model_provider_id: &str,
    model: &str,
) -> Result<()> {
    let codex_home = TempDir::new()?;
    create_config_toml(codex_home.path(), CreateConfigTomlParams::default())?;

    let mut mcp = TestAppServer::builder()
        .with_codex_home(codex_home.path())
        .without_auto_env()
        .with_env_overrides(&[("OPENAI_API_KEY", None)])
        .build_initialized_with_timeout(DEFAULT_READ_TIMEOUT)
        .await?;
    let request_id = mcp
        .send_login_account_amazon_bedrock_request("managed-bedrock-api-key", "us-west-2")
        .await?;
    let response = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(request_id)),
    )
    .await??;
    assert_eq!(
        to_response::<LoginAccountResponse>(response)?,
        LoginAccountResponse::AmazonBedrock {}
    );
    timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_notification_message("account/login/completed"),
    )
    .await??;
    assert_account_updated(&mut mcp, Some(AuthMode::BedrockApiKey)).await?;
    assert_eq!(
        read_account(&mut mcp).await?,
        GetAccountResponse {
            workspace_routing: None,
            account: Some(Account::AmazonBedrock {
                uses_codex_managed_credentials: true,
            }),
            requires_openai_auth: false,
        }
    );

    let config_path = codex_home.path().join("config.toml");
    let config = std::fs::read_to_string(&config_path)?
        .replace(
            "model_provider = \"amazon-bedrock\"",
            &format!("model_provider = \"{model_provider_id}\""),
        )
        .replace(
            "model = \"mock-model\"",
            &format!("model = \"{model}\"\nmodel_reasoning_effort = \"high\""),
        );
    std::fs::write(
        config_path,
        format!(
            "{config}\n[model_providers.{model_provider_id}]\nbase_url = \"https://bedrock.example.com/v1\"\n[model_providers.{model_provider_id}.aws]\nprofile = \"managed-profile\"\nregion = \"us-west-2\"\nauth_refresh = {{ command = \"aws\" }}\n"
        ),
    )?;
    let mut expected_config = read_config_toml(codex_home.path())?;
    let expected_config_root = expected_config
        .as_table_mut()
        .expect("config should be a table");
    expected_config_root.remove("model_provider");
    expected_config_root.remove("model");
    expected_config["model_providers"][model_provider_id]
        .as_table_mut()
        .expect("Bedrock provider config should be a table")
        .remove("aws");

    let request_id = mcp.send_logout_account_request().await?;
    let response = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(request_id)),
    )
    .await??;
    assert_eq!(
        to_response::<LogoutAccountResponse>(response)?,
        LogoutAccountResponse {}
    );
    assert_eq!(load_file_auth(codex_home.path())?, None);
    assert_eq!(read_config_toml(codex_home.path())?, expected_config);
    assert_account_updated(&mut mcp, /*auth_mode*/ None).await?;
    assert_eq!(
        read_account(&mut mcp).await?,
        GetAccountResponse {
            workspace_routing: None,
            account: None,
            requires_openai_auth: true,
        }
    );
    Ok(())
}

#[tokio::test]
async fn logout_aws_managed_bedrock_clears_provider_and_restores_default_account() -> Result<()> {
    for managed_bedrock_auth in [false, true] {
        let codex_home = TempDir::new()?;
        create_config_toml(codex_home.path(), aws_managed_bedrock_config())?;
        let config_path = codex_home.path().join("config.toml");
        let config = std::fs::read_to_string(&config_path)?
            .replace(
                "model = \"mock-model\"",
                "model = \"openai.gpt-5.6-sol\"\nmodel_reasoning_effort = \"high\"",
            )
            .replace(
                "[model_providers.amazon-bedrock.aws]",
                "[model_providers.amazon-bedrock]\nbase_url = \"https://bedrock.example.com/v1\"\n[model_providers.amazon-bedrock.aws]\nauth_refresh = { command = \"aws\" }",
            );
        std::fs::write(config_path, config)?;
        let dotenv_path = codex_home.path().join(".env");
        let aws_credentials_path = codex_home.path().join("aws-credentials");
        let dotenv = "AWS_ACCESS_KEY_ID=environment-id\nAWS_SECRET_ACCESS_KEY=environment-secret\n";
        let aws_credentials = "[codex-bedrock]\naws_access_key_id = profile-id\naws_secret_access_key = profile-secret\n";
        std::fs::write(&dotenv_path, dotenv)?;
        std::fs::write(&aws_credentials_path, aws_credentials)?;
        if managed_bedrock_auth {
            login_with_bedrock_api_key(
                codex_home.path(),
                "managed-bedrock-api-key",
                "us-east-1",
                AuthCredentialsStoreMode::File,
                AuthKeyringBackendKind::default(),
            )?;
        } else {
            login_with_api_key(
                codex_home.path(),
                "sk-test-key",
                AuthCredentialsStoreMode::File,
                AuthKeyringBackendKind::default(),
            )?;
        }

        let aws_credentials_env_path = aws_credentials_path.to_string_lossy();
        let mut mcp = TestAppServer::builder()
            .with_codex_home(codex_home.path())
            .without_auto_env()
            .with_env_overrides(&[
                ("OPENAI_API_KEY", None),
                ("AWS_ACCESS_KEY_ID", Some("environment-id")),
                ("AWS_SECRET_ACCESS_KEY", Some("environment-secret")),
                (
                    "AWS_SHARED_CREDENTIALS_FILE",
                    Some(aws_credentials_env_path.as_ref()),
                ),
            ])
            .build_initialized_with_timeout(DEFAULT_READ_TIMEOUT)
            .await?;
        assert_eq!(
            read_account(&mut mcp).await?,
            GetAccountResponse {
                workspace_routing: None,
                account: Some(Account::AmazonBedrock {
                    uses_codex_managed_credentials: false,
                }),
                requires_openai_auth: false,
            }
        );
        let mut expected_config = read_config_toml(codex_home.path())?;
        let expected_config_root = expected_config
            .as_table_mut()
            .expect("config should be a table");
        expected_config_root.remove("model_provider");
        expected_config_root.remove("model");
        expected_config["model_providers"]["amazon-bedrock"]
            .as_table_mut()
            .expect("Bedrock provider config should be a table")
            .remove("aws");

        let request_id = mcp.send_logout_account_request().await?;
        let response = timeout(
            DEFAULT_READ_TIMEOUT,
            mcp.read_stream_until_response_message(RequestId::Integer(request_id)),
        )
        .await??;
        assert_eq!(
            to_response::<LogoutAccountResponse>(response)?,
            LogoutAccountResponse {}
        );
        assert_eq!(load_file_auth(codex_home.path())?, None);
        assert_eq!(read_config_toml(codex_home.path())?, expected_config);
        assert_eq!(std::fs::read_to_string(dotenv_path)?, dotenv);
        assert_eq!(
            std::fs::read_to_string(aws_credentials_path)?,
            aws_credentials
        );
        assert_account_updated(&mut mcp, /*auth_mode*/ None).await?;
        assert_eq!(
            read_account(&mut mcp).await?,
            GetAccountResponse {
                workspace_routing: None,
                account: None,
                requires_openai_auth: true,
            }
        );
    }
    Ok(())
}

#[tokio::test]
async fn logout_managed_bedrock_preserves_changed_provider_without_experimental_api() -> Result<()>
{
    let codex_home = TempDir::new()?;
    create_config_toml(codex_home.path(), aws_managed_bedrock_config())?;
    login_with_bedrock_api_key(
        codex_home.path(),
        "managed-bedrock-api-key",
        "us-west-2",
        AuthCredentialsStoreMode::File,
        AuthKeyringBackendKind::default(),
    )?;

    let mut mcp = TestAppServer::builder()
        .with_codex_home(codex_home.path())
        .without_auto_env()
        .build()
        .await?;
    let initialized = mcp
        .initialize_with_capabilities(
            ClientInfo {
                name: DEFAULT_CLIENT_NAME.to_string(),
                title: None,
                version: "0.1.0".to_string(),
            },
            Some(InitializeCapabilities {
                experimental_api: false,
                ..Default::default()
            }),
        )
        .await?;
    assert!(matches!(initialized, JSONRPCMessage::Response(_)));

    create_config_toml(codex_home.path(), CreateConfigTomlParams::default())?;
    let config_path = codex_home.path().join("config.toml");
    let config = std::fs::read_to_string(&config_path)?;
    std::fs::write(
        config_path,
        format!(
            "{config}\n[model_providers.amazon-bedrock.aws]\nprofile = \"preserved\"\nregion = \"us-west-2\"\n"
        ),
    )?;
    let expected_config = read_config_toml(codex_home.path())?;

    let request_id = mcp.send_logout_account_request().await?;
    let response = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(request_id)),
    )
    .await??;
    assert_eq!(
        to_response::<LogoutAccountResponse>(response)?,
        LogoutAccountResponse {}
    );
    assert_eq!(load_file_auth(codex_home.path())?, None);
    assert_eq!(read_config_toml(codex_home.path())?, expected_config);
    assert_account_updated(&mut mcp, /*auth_mode*/ None).await?;
    assert_eq!(
        read_account(&mut mcp).await?,
        GetAccountResponse {
            workspace_routing: None,
            account: None,
            requires_openai_auth: false,
        }
    );
    Ok(())
}

#[tokio::test]
async fn managed_bedrock_login_requires_experimental_api() -> Result<()> {
    let codex_home = TempDir::new()?;
    create_config_toml(codex_home.path(), CreateConfigTomlParams::default())?;
    let mut mcp = TestAppServer::builder()
        .with_codex_home(codex_home.path())
        .without_auto_env()
        .build()
        .await?;
    let initialized = mcp
        .initialize_with_capabilities(
            ClientInfo {
                name: DEFAULT_CLIENT_NAME.to_string(),
                title: None,
                version: "0.1.0".to_string(),
            },
            Some(InitializeCapabilities {
                experimental_api: false,
                ..Default::default()
            }),
        )
        .await?;
    assert!(matches!(initialized, JSONRPCMessage::Response(_)));

    let request_id = mcp
        .send_login_account_amazon_bedrock_request("managed-bedrock-api-key", "us-west-2")
        .await?;
    let error = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_error_message(RequestId::Integer(request_id)),
    )
    .await??;
    assert_eq!(
        error.error.message,
        "account/login/start.amazonBedrock requires experimentalApi capability"
    );
    assert_eq!(load_file_auth(codex_home.path())?, None);
    Ok(())
}

#[tokio::test]
async fn login_managed_bedrock_updates_active_bedrock_account() -> Result<()> {
    let codex_home = TempDir::new()?;
    create_config_toml(codex_home.path(), CreateConfigTomlParams::default())?;

    let mut mcp = TestAppServer::builder()
        .with_codex_home(codex_home.path())
        .without_auto_env()
        .with_env_overrides(&[("OPENAI_API_KEY", None)])
        .build_initialized_with_timeout(DEFAULT_READ_TIMEOUT)
        .await?;
    let request_id = mcp
        .send_login_account_amazon_bedrock_request("managed-bedrock-api-key", "us-west-2")
        .await?;
    let response: JSONRPCResponse = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(request_id)),
    )
    .await??;
    assert_eq!(
        to_response::<LoginAccountResponse>(response)?,
        LoginAccountResponse::AmazonBedrock {}
    );
    timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_notification_message("account/login/completed"),
    )
    .await??;
    assert_account_updated(&mut mcp, Some(AuthMode::BedrockApiKey)).await?;
    assert_eq!(
        read_account(&mut mcp).await?,
        GetAccountResponse {
            workspace_routing: None,
            account: Some(Account::AmazonBedrock {
                uses_codex_managed_credentials: true,
            }),
            requires_openai_auth: false,
        }
    );

    assert!(codex_home.path().join("auth.json").exists());
    Ok(())
}

#[tokio::test]
async fn login_account_amazon_bedrock_rejects_invalid_credentials_without_changes() -> Result<()> {
    let codex_home = TempDir::new()?;
    create_config_toml(codex_home.path(), CreateConfigTomlParams::default())?;

    let mut mcp = TestAppServer::builder()
        .with_codex_home(codex_home.path())
        .without_auto_env()
        .with_env_overrides(&[("OPENAI_API_KEY", None)])
        .build_initialized_with_timeout(DEFAULT_READ_TIMEOUT)
        .await?;
    let expected_config = read_config_toml(codex_home.path())?;

    let request_id = mcp
        .send_login_account_amazon_bedrock_request("  ", "us-west-2")
        .await?;
    let error = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_error_message(RequestId::Integer(request_id)),
    )
    .await??;
    assert_eq!(
        error.error.message,
        "Amazon Bedrock API key must not be empty."
    );

    let request_id = mcp
        .send_login_account_amazon_bedrock_request("managed-bedrock-api-key", "us-west-1")
        .await?;
    let error = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_error_message(RequestId::Integer(request_id)),
    )
    .await??;
    assert_eq!(
        error.error.message,
        "Amazon Bedrock does not support region `us-west-1`"
    );

    let request_id = mcp
        .send_login_account_request(json!({
            "type": "amazonBedrockAccessKeys",
            "accessKeyId": " ",
            "secretAccessKey": "test-secret",
            "region": "us-west-2",
        }))
        .await?;
    let error = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_error_message(RequestId::Integer(request_id)),
    )
    .await??;
    assert_eq!(
        error.error.message,
        "AWS access key ID and secret access key must not be empty."
    );
    assert_eq!(load_file_auth(codex_home.path())?, None);
    assert_eq!(read_config_toml(codex_home.path())?, expected_config);

    Ok(())
}

#[tokio::test]
async fn login_account_amazon_bedrock_rejected_when_forced_chatgpt() -> Result<()> {
    let codex_home = TempDir::new()?;
    create_config_toml(
        codex_home.path(),
        CreateConfigTomlParams {
            forced_method: Some("chatgpt".to_string()),
            ..Default::default()
        },
    )?;

    let mut mcp = TestAppServer::builder()
        .with_codex_home(codex_home.path())
        .without_auto_env()
        .build_initialized_with_timeout(DEFAULT_READ_TIMEOUT)
        .await?;
    let request_id = mcp
        .send_login_account_amazon_bedrock_request("managed-bedrock-api-key", "us-west-2")
        .await?;
    let error = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_error_message(RequestId::Integer(request_id)),
    )
    .await??;

    assert_eq!(
        error.error.message,
        "Amazon Bedrock login is disabled by configuration."
    );
    assert_eq!(load_file_auth(codex_home.path())?, None);
    Ok(())
}

#[tokio::test]
async fn login_account_amazon_bedrock_rejected_with_external_chatgpt_auth() -> Result<()> {
    let codex_home = TempDir::new()?;
    create_config_toml(codex_home.path(), CreateConfigTomlParams::default())?;
    let access_token = encode_id_token(
        &ChatGptIdTokenClaims::new()
            .email("embedded@example.com")
            .plan_type("pro")
            .chatgpt_account_id(WORKSPACE_ID_EMBEDDED),
    )?;

    let mut mcp = TestAppServer::builder()
        .with_mock_chatgpt_backend()
        .with_codex_home(codex_home.path())
        .without_auto_env()
        .build_initialized_with_timeout(DEFAULT_READ_TIMEOUT)
        .await?;
    let set_id = mcp
        .send_chatgpt_auth_tokens_login_request(
            access_token,
            WORKSPACE_ID_EMBEDDED.to_string(),
            Some("pro".to_string()),
        )
        .await?;
    let set_response = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(set_id)),
    )
    .await??;
    assert_eq!(
        to_response::<LoginAccountResponse>(set_response)?,
        LoginAccountResponse::ChatgptAuthTokens {}
    );
    timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_notification_message("account/updated"),
    )
    .await??;

    let request_id = mcp
        .send_login_account_amazon_bedrock_request("managed-bedrock-api-key", "us-west-2")
        .await?;
    let error = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_error_message(RequestId::Integer(request_id)),
    )
    .await??;
    assert_eq!(
        error.error.message,
        "External auth is active. Use account/login/start (chatgptAuthTokens) to update it or account/logout to clear it."
    );
    assert_eq!(load_file_auth(codex_home.path())?, None);
    Ok(())
}

#[tokio::test]
async fn login_account_api_key_rejected_when_forced_chatgpt() -> Result<()> {
    let codex_home = TempDir::new()?;
    create_config_toml(
        codex_home.path(),
        CreateConfigTomlParams {
            forced_method: Some("chatgpt".to_string()),
            ..Default::default()
        },
    )?;

    let mut mcp = TestAppServer::builder()
        .with_mock_chatgpt_backend()
        .with_codex_home(codex_home.path())
        .without_auto_env()
        .build_initialized_with_timeout(DEFAULT_READ_TIMEOUT)
        .await?;

    let request_id = mcp
        .send_login_account_api_key_request("sk-test-key")
        .await?;
    let err: JSONRPCError = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_error_message(RequestId::Integer(request_id)),
    )
    .await??;

    assert_eq!(
        err.error.message,
        "API key login is disabled by configuration."
    );
    Ok(())
}

#[tokio::test]
async fn browser_and_device_account_acquisition_are_not_protocol_operations() -> Result<()> {
    let codex_home = TempDir::new()?;
    create_config_toml(codex_home.path(), CreateConfigTomlParams::default())?;
    let mut server = TestAppServer::builder()
        .with_mock_chatgpt_backend()
        .with_codex_home(codex_home.path())
        .without_auto_env()
        .build_initialized_with_timeout(DEFAULT_READ_TIMEOUT)
        .await?;

    for kind in ["chatgpt", "chatgptDeviceCode"] {
        let request = server
            .send_login_account_request(json!({"type": kind}))
            .await?;
        let error = timeout(
            DEFAULT_READ_TIMEOUT,
            server.read_stream_until_error_message(RequestId::Integer(request)),
        )
        .await??;
        assert!(error.error.message.contains("unknown variant"));
        assert!(error.error.message.contains(kind));
        assert!(!codex_home.path().join("auth.json").exists());
    }
    Ok(())
}

#[tokio::test]
async fn get_account_no_auth() -> Result<()> {
    let codex_home = TempDir::new()?;
    create_config_toml(
        codex_home.path(),
        CreateConfigTomlParams {
            requires_openai_auth: Some(true),
            ..Default::default()
        },
    )?;

    let mut mcp = TestAppServer::builder()
        .with_codex_home(codex_home.path())
        .without_auto_env()
        .with_env_overrides(&[("OPENAI_API_KEY", None)])
        .build_initialized_with_timeout(DEFAULT_READ_TIMEOUT)
        .await?;

    let params = GetAccountParams {
        refresh_token: false,
    };
    let request_id = mcp.send_get_account_request(params).await?;

    let account: GetAccountResponse =
        timeout(DEFAULT_READ_TIMEOUT, mcp.read_response(request_id)).await??;

    assert_eq!(account.account, None, "expected no account");
    assert_eq!(account.requires_openai_auth, true);
    Ok(())
}

#[tokio::test]
async fn get_account_with_api_key() -> Result<()> {
    let codex_home = TempDir::new()?;
    create_config_toml(
        codex_home.path(),
        CreateConfigTomlParams {
            requires_openai_auth: Some(true),
            ..Default::default()
        },
    )?;

    let mut mcp = TestAppServer::builder()
        .with_codex_home(codex_home.path())
        .without_auto_env()
        .build_initialized_with_timeout(DEFAULT_READ_TIMEOUT)
        .await?;

    let req_id = mcp
        .send_login_account_api_key_request("sk-test-key")
        .await?;
    let _login_ok: LoginAccountResponse =
        timeout(DEFAULT_READ_TIMEOUT, mcp.read_response(req_id)).await??;

    let params = GetAccountParams {
        refresh_token: false,
    };
    let request_id = mcp.send_get_account_request(params).await?;

    let received: GetAccountResponse =
        timeout(DEFAULT_READ_TIMEOUT, mcp.read_response(request_id)).await??;

    let expected = GetAccountResponse {
        workspace_routing: None,
        account: Some(Account::ApiKey {}),
        requires_openai_auth: true,
    };
    assert_eq!(received, expected);
    Ok(())
}

#[tokio::test]
async fn get_account_when_auth_not_required() -> Result<()> {
    let codex_home = TempDir::new()?;
    create_config_toml(
        codex_home.path(),
        CreateConfigTomlParams {
            requires_openai_auth: Some(false),
            ..Default::default()
        },
    )?;

    let mut mcp = TestAppServer::builder()
        .with_codex_home(codex_home.path())
        .without_auto_env()
        .build_initialized_with_timeout(DEFAULT_READ_TIMEOUT)
        .await?;

    let params = GetAccountParams {
        refresh_token: false,
    };
    let request_id = mcp.send_get_account_request(params).await?;

    let received: GetAccountResponse =
        timeout(DEFAULT_READ_TIMEOUT, mcp.read_response(request_id)).await??;

    let expected = GetAccountResponse {
        workspace_routing: None,
        account: None,
        requires_openai_auth: false,
    };
    assert_eq!(received, expected);
    Ok(())
}

#[tokio::test]
async fn get_account_with_aws_provider() -> Result<()> {
    let codex_home = TempDir::new()?;
    create_config_toml(
        codex_home.path(),
        CreateConfigTomlParams {
            model_provider_id: Some("amazon-bedrock".to_string()),
            extra_provider_config: Some(
                r#"[model_providers.amazon-bedrock.aws]
profile = "codex-bedrock"
region = "us-west-2"
"#
                .to_string(),
            ),
            ..Default::default()
        },
    )?;

    let mut mcp = TestAppServer::builder()
        .with_codex_home(codex_home.path())
        .without_auto_env()
        .build_initialized_with_timeout(DEFAULT_READ_TIMEOUT)
        .await?;

    let params = GetAccountParams {
        refresh_token: false,
    };
    let request_id = mcp.send_get_account_request(params).await?;

    let received: GetAccountResponse =
        timeout(DEFAULT_READ_TIMEOUT, mcp.read_response(request_id)).await??;

    let expected = GetAccountResponse {
        workspace_routing: None,
        account: Some(Account::AmazonBedrock {
            uses_codex_managed_credentials: false,
        }),
        requires_openai_auth: false,
    };
    assert_eq!(received, expected);
    Ok(())
}

#[tokio::test]
async fn get_account_with_user_managed_bedrock_provider() -> Result<()> {
    let codex_home = TempDir::new()?;
    create_config_toml(
        codex_home.path(),
        CreateConfigTomlParams {
            model_provider_id: Some("amazon-bedrock".to_string()),
            extra_provider_config: Some(
                r#"[model_providers.amazon-bedrock]
base_url = "https://bedrock.example.com/v1"

[model_providers.amazon-bedrock.auth]
command = "print-token"
"#
                .to_string(),
            ),
            ..Default::default()
        },
    )?;

    let mut mcp = TestAppServer::builder()
        .with_codex_home(codex_home.path())
        .without_auto_env()
        .build_initialized_with_timeout(DEFAULT_READ_TIMEOUT)
        .await?;

    assert_eq!(
        read_account(&mut mcp).await?,
        GetAccountResponse {
            workspace_routing: None,
            account: Some(Account::AmazonBedrock {
                uses_codex_managed_credentials: false,
            }),
            requires_openai_auth: false,
        }
    );
    Ok(())
}

#[tokio::test]
async fn account_reads_use_startup_config_when_config_reload_fails() -> Result<()> {
    let codex_home = TempDir::new()?;
    create_config_toml(
        codex_home.path(),
        CreateConfigTomlParams {
            model_provider_id: Some("amazon-bedrock".to_string()),
            extra_provider_config: Some(
                r#"[model_providers.amazon-bedrock.aws]
profile = "codex-bedrock"
region = "us-west-2"
"#
                .to_string(),
            ),
            ..Default::default()
        },
    )?;

    let mut mcp = TestAppServer::builder()
        .with_codex_home(codex_home.path())
        .without_auto_env()
        .build_initialized_with_timeout(DEFAULT_READ_TIMEOUT)
        .await?;

    std::fs::write(codex_home.path().join("config.toml"), "invalid = [")?;

    assert_eq!(
        read_account(&mut mcp).await?,
        GetAccountResponse {
            workspace_routing: None,
            account: Some(Account::AmazonBedrock {
                uses_codex_managed_credentials: false,
            }),
            requires_openai_auth: false,
        }
    );

    let request_id = mcp
        .send_get_auth_status_request(GetAuthStatusParams {
            include_token: Some(false),
            refresh_token: Some(false),
        })
        .await?;
    let response = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(request_id)),
    )
    .await??;
    assert_eq!(
        to_response::<GetAuthStatusResponse>(response)?,
        GetAuthStatusResponse {
            auth_method: None,
            auth_token: None,
            requires_openai_auth: Some(false),
        }
    );

    Ok(())
}

#[tokio::test]
async fn get_account_with_managed_bedrock_provider() -> Result<()> {
    let codex_home = TempDir::new()?;
    create_config_toml(
        codex_home.path(),
        CreateConfigTomlParams {
            model_provider_id: Some("amazon-bedrock".to_string()),
            ..Default::default()
        },
    )?;
    login_with_bedrock_api_key(
        codex_home.path(),
        "managed-bedrock-api-key",
        "us-west-2",
        AuthCredentialsStoreMode::File,
        AuthKeyringBackendKind::default(),
    )?;

    let mut mcp = TestAppServer::builder()
        .with_codex_home(codex_home.path())
        .without_auto_env()
        .build_initialized_with_timeout(DEFAULT_READ_TIMEOUT)
        .await?;

    let request_id = mcp
        .send_get_account_request(GetAccountParams {
            refresh_token: false,
        })
        .await?;
    let received: GetAccountResponse =
        timeout(DEFAULT_READ_TIMEOUT, mcp.read_response(request_id)).await??;

    assert_eq!(
        received,
        GetAccountResponse {
            workspace_routing: None,
            account: Some(Account::AmazonBedrock {
                uses_codex_managed_credentials: true,
            }),
            requires_openai_auth: false,
        }
    );
    Ok(())
}

#[tokio::test]
async fn get_account_with_chatgpt() -> Result<()> {
    let codex_home = TempDir::new()?;
    create_config_toml(
        codex_home.path(),
        CreateConfigTomlParams {
            requires_openai_auth: Some(true),
            ..Default::default()
        },
    )?;
    write_chatgpt_auth(
        codex_home.path(),
        ChatGptAuthFixture::new("access-chatgpt")
            .account_id("account-123")
            .email("user@example.com")
            .plan_type("pro"),
        AuthCredentialsStoreMode::File,
    )?;

    let mut mcp = TestAppServer::builder()
        .with_codex_home(codex_home.path())
        .without_auto_env()
        .with_env_overrides(&[("OPENAI_API_KEY", None)])
        .build_initialized_with_timeout(DEFAULT_READ_TIMEOUT)
        .await?;

    let params = GetAccountParams {
        refresh_token: false,
    };
    let request_id = mcp.send_get_account_request(params).await?;

    let received: GetAccountResponse =
        timeout(DEFAULT_READ_TIMEOUT, mcp.read_response(request_id)).await??;

    let expected = GetAccountResponse {
        workspace_routing: expected_workspace_routing("account-123"),
        account: Some(Account::Chatgpt {
            email: Some("user@example.com".to_string()),
            plan_type: AccountPlanType::Pro,
        }),
        requires_openai_auth: true,
    };
    assert_eq!(received, expected);
    Ok(())
}

#[test_case("promax", AccountPlanType::ProMax; "pro_max")]
#[test_case("self_serve_business_prolite", AccountPlanType::SelfServeBusinessProLite; "business_prolite")]
#[test_case("edu_plus", AccountPlanType::EduPlus; "edu_plus")]
#[test_case("edu_pro", AccountPlanType::EduPro; "edu_pro")]
#[tokio::test]
async fn get_account_with_chatgpt_plan_variants_returns_plan_type(
    plan_type: &str,
    expected_plan: AccountPlanType,
) -> Result<()> {
    let codex_home = TempDir::new()?;
    create_config_toml(
        codex_home.path(),
        CreateConfigTomlParams {
            requires_openai_auth: Some(true),
            ..Default::default()
        },
    )?;
    write_chatgpt_auth(
        codex_home.path(),
        ChatGptAuthFixture::new("access-chatgpt")
            .account_id("account-123")
            .email("user@example.com")
            .plan_type(plan_type),
        AuthCredentialsStoreMode::File,
    )?;

    let mut mcp = TestAppServer::builder()
        .with_codex_home(codex_home.path())
        .without_auto_env()
        .with_env_overrides(&[("OPENAI_API_KEY", None)])
        .build_initialized_with_timeout(DEFAULT_READ_TIMEOUT)
        .await?;

    let request_id = mcp
        .send_get_account_request(GetAccountParams {
            refresh_token: false,
        })
        .await?;
    let received: GetAccountResponse =
        timeout(DEFAULT_READ_TIMEOUT, mcp.read_response(request_id)).await??;

    assert_eq!(
        received,
        GetAccountResponse {
            workspace_routing: expected_workspace_routing("account-123"),
            account: Some(Account::Chatgpt {
                email: Some("user@example.com".to_string()),
                plan_type: expected_plan,
            }),
            requires_openai_auth: true,
        }
    );
    Ok(())
}

#[tokio::test]
async fn get_account_with_chatgpt_without_email() -> Result<()> {
    let codex_home = TempDir::new()?;
    create_config_toml(
        codex_home.path(),
        CreateConfigTomlParams {
            requires_openai_auth: Some(true),
            ..Default::default()
        },
    )?;
    write_chatgpt_auth(
        codex_home.path(),
        ChatGptAuthFixture::new("access-chatgpt")
            .account_id("account-123")
            .plan_type("pro"),
        AuthCredentialsStoreMode::File,
    )?;

    let mut mcp = TestAppServer::builder()
        .with_codex_home(codex_home.path())
        .without_auto_env()
        .with_env_overrides(&[("OPENAI_API_KEY", None)])
        .build_initialized_with_timeout(DEFAULT_READ_TIMEOUT)
        .await?;

    let request_id = mcp
        .send_get_account_request(GetAccountParams {
            refresh_token: false,
        })
        .await?;
    let received: GetAccountResponse =
        timeout(DEFAULT_READ_TIMEOUT, mcp.read_response(request_id)).await??;

    assert_eq!(
        received,
        GetAccountResponse {
            workspace_routing: expected_workspace_routing("account-123"),
            account: Some(Account::Chatgpt {
                email: None,
                plan_type: AccountPlanType::Pro,
            }),
            requires_openai_auth: true,
        }
    );
    Ok(())
}

#[tokio::test]
async fn get_account_omits_chatgpt_after_permanent_refresh_failure() -> Result<()> {
    let codex_home = TempDir::new()?;
    create_config_toml(
        codex_home.path(),
        CreateConfigTomlParams {
            requires_openai_auth: Some(true),
            ..Default::default()
        },
    )?;
    write_chatgpt_auth(
        codex_home.path(),
        ChatGptAuthFixture::new("stale-access-token")
            .refresh_token("stale-refresh-token")
            .account_id(WORKSPACE_ID_STALE)
            .email("user@example.com")
            .plan_type("pro")
            .last_refresh(Some(Utc::now() - ChronoDuration::days(9))),
        AuthCredentialsStoreMode::File,
    )?;

    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/oauth/token"))
        .respond_with(ResponseTemplate::new(401).set_body_json(serde_json::json!({
            "error": {
                "code": "refresh_token_reused"
            }
        })))
        .expect(1..=2)
        .mount(&server)
        .await;

    let refresh_url = format!("{}/oauth/token", server.uri());
    let mut mcp = TestAppServer::builder()
        .with_codex_home(codex_home.path())
        .without_auto_env()
        .with_env_overrides(&[
            ("OPENAI_API_KEY", None),
            (
                REFRESH_TOKEN_URL_OVERRIDE_ENV_VAR,
                Some(refresh_url.as_str()),
            ),
        ])
        .build_initialized_with_timeout(DEFAULT_READ_TIMEOUT)
        .await?;

    let auth_status_request_id = mcp
        .send_get_auth_status_request(GetAuthStatusParams {
            include_token: Some(true),
            refresh_token: Some(true),
        })
        .await?;
    let _: GetAuthStatusResponse = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_response(auth_status_request_id),
    )
    .await??;

    let request_id = mcp
        .send_get_account_request(GetAccountParams {
            refresh_token: false,
        })
        .await?;

    let received: GetAccountResponse =
        timeout(DEFAULT_READ_TIMEOUT, mcp.read_response(request_id)).await??;

    assert_eq!(
        received,
        GetAccountResponse {
            workspace_routing: None,
            account: None,
            requires_openai_auth: true,
        }
    );
    server.verify().await;
    Ok(())
}

#[tokio::test]
async fn get_account_with_chatgpt_missing_plan_claim_returns_unknown() -> Result<()> {
    let codex_home = TempDir::new()?;
    create_config_toml(
        codex_home.path(),
        CreateConfigTomlParams {
            requires_openai_auth: Some(true),
            ..Default::default()
        },
    )?;
    write_chatgpt_auth(
        codex_home.path(),
        ChatGptAuthFixture::new("access-chatgpt")
            .account_id("account-123")
            .email("user@example.com"),
        AuthCredentialsStoreMode::File,
    )?;

    let mut mcp = TestAppServer::builder()
        .with_codex_home(codex_home.path())
        .without_auto_env()
        .with_env_overrides(&[("OPENAI_API_KEY", None)])
        .build_initialized_with_timeout(DEFAULT_READ_TIMEOUT)
        .await?;

    let params = GetAccountParams {
        refresh_token: false,
    };
    let request_id = mcp.send_get_account_request(params).await?;

    let received: GetAccountResponse =
        timeout(DEFAULT_READ_TIMEOUT, mcp.read_response(request_id)).await??;

    let expected = GetAccountResponse {
        workspace_routing: expected_workspace_routing("account-123"),
        account: Some(Account::Chatgpt {
            email: Some("user@example.com".to_string()),
            plan_type: AccountPlanType::Unknown,
        }),
        requires_openai_auth: true,
    };
    assert_eq!(received, expected);
    Ok(())
}
