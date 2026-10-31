//! Exercises workspace switching through the app-server's public account API.

use super::*;
use codex_app_server_protocol::AccountSessionsResponse;
use pretty_assertions::assert_eq;
use test_case::test_case;
use wiremock::matchers::body_json;

#[derive(Clone, Copy, PartialEq)]
enum SwitchScenario {
    Success,
    MissingWorkspace,
    DuplicateWorkspace,
    InvalidRouting,
    Unauthorized,
    RequiresSignIn,
    GovernmentWorkspace,
}

#[test_case(SwitchScenario::Success; "success_and_refresh")]
#[test_case(SwitchScenario::MissingWorkspace; "missing_workspace")]
#[test_case(SwitchScenario::DuplicateWorkspace; "duplicate_workspace")]
#[test_case(SwitchScenario::InvalidRouting; "invalid_routing")]
#[test_case(SwitchScenario::Unauthorized; "unauthorized")]
#[test_case(SwitchScenario::RequiresSignIn; "requires_sign_in")]
#[test_case(SwitchScenario::GovernmentWorkspace; "government_workspace")]
#[tokio::test]
async fn workspace_switch_validates_and_preserves_managed_login(
    scenario: SwitchScenario,
) -> Result<()> {
    let home = TempDir::new()?;
    let backend = MockServer::start().await;
    Mock::given(path("/backend-api/wham/settings/user"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"commit_attribution_enabled": false})),
        )
        .mount(&backend)
        .await;
    Mock::given(path("/backend-api/wham/config/bundle"))
        .respond_with(ResponseTemplate::new(/*s*/ 200).set_body_json(json!({})))
        .mount(&backend)
        .await;
    create_config_toml(
        home.path(),
        CreateConfigTomlParams {
            requires_openai_auth: Some(true),
            base_url: Some(format!("{}/v1", backend.uri())),
            chatgpt_base_url: Some(format!("{}/backend-api", backend.uri())),
            ..Default::default()
        },
    )?;
    write_models_cache(home.path()).await?;
    write_chatgpt_auth(
        home.path(),
        ChatGptAuthFixture::new("original-token")
            .email("person@example.com")
            .plan_type("plus")
            .chatgpt_user_id("user-123")
            .chatgpt_account_id(WORKSPACE_ID_INITIAL)
            .account_id(WORKSPACE_ID_INITIAL),
        AuthCredentialsStoreMode::File,
    )?;
    let original = load_file_auth(home.path())?.expect("managed auth exists");
    let inference = if scenario == SwitchScenario::Success {
        let command = json!({
            "cmd": "echo workspace-switch",
            "sandbox_permissions": "require_escalated",
            "justification": "test workspace switching during approval",
        });
        let pending = responses::sse(vec![
            responses::ev_function_call("pending-command", "exec_command", &command.to_string()),
            responses::ev_completed("old-workspace-turn"),
        ]);
        Some(
            responses::mount_response_sequence(
                &backend,
                vec![
                    responses::sse_response(pending)
                        .insert_header("x-codex-turn-state", "old-workspace-state"),
                    responses::sse_response(responses::sse_completed("new-workspace-turn")),
                ],
            )
            .await,
        )
    } else {
        None
    };
    let access_token = encode_id_token(
        &ChatGptIdTokenClaims::new()
            .chatgpt_user_id("user-123")
            .chatgpt_account_id(WORKSPACE_ID_SECOND_ALLOWED)
            .plan_type("business"),
    )?;
    let rejected_before_exchange = matches!(
        scenario,
        SwitchScenario::RequiresSignIn
            | SwitchScenario::GovernmentWorkspace
    );
    Mock::given(method("POST"))
        .and(path("/backend-api/accounts/switch-workspace-token"))
        .and(header("authorization", "Bearer original-token"))
        .and(body_json(
            json!({"workspace_id": WORKSPACE_ID_SECOND_ALLOWED}),
        ))
        .respond_with(
            ResponseTemplate::new(/*s*/ 200).set_body_json(json!({"access_token": access_token})),
        )
        .expect(u64::from(!rejected_before_exchange))
        .mount(&backend)
        .await;
    let source = json!({
        "id": WORKSPACE_ID_INITIAL,
        "workspace_backend_origin": "https://chatgpt.com",
        "account_routing_override": "NO_CONSTRAINT",
    });
    let mut target = json!({
        "id": WORKSPACE_ID_SECOND_ALLOWED,
        "workspace_backend_origin": "https://chatgpt.com",
        "account_routing_override": "NO_CONSTRAINT",
        "can_access_with_session": !matches!(scenario, SwitchScenario::RequiresSignIn),
        "sso_connection_name": "test-enterprise",
        "is_fedramp_compliant_workspace": scenario == SwitchScenario::GovernmentWorkspace,
    });
    Mock::given(path("/backend-api/wham/accounts/check"))
        .and(header("chatgpt-account-id", WORKSPACE_ID_INITIAL))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"accounts": [source, target]})),
        )
        .mount(&backend)
        .await;
    let target_memberships = match scenario {
        SwitchScenario::MissingWorkspace => vec![],
        SwitchScenario::DuplicateWorkspace => vec![target.clone(), target],
        SwitchScenario::InvalidRouting => {
            target["workspace_backend_origin"] = json!("invalid");
            vec![target]
        }
        _ => vec![target],
    };
    let response = if scenario == SwitchScenario::Unauthorized {
        ResponseTemplate::new(401)
    } else {
        ResponseTemplate::new(200).set_body_json(json!({"accounts": target_memberships}))
    };
    Mock::given(path("/backend-api/wham/accounts/check"))
        .and(header("chatgpt-account-id", WORKSPACE_ID_SECOND_ALLOWED))
        .respond_with(response)
        .mount(&backend)
        .await;
    let original_token = encode_id_token(
        &ChatGptIdTokenClaims::new()
            .chatgpt_user_id("user-123")
            .chatgpt_account_id(WORKSPACE_ID_INITIAL)
            .plan_type("plus"),
    )?;
    Mock::given(path("/backend-api/accounts/switch-workspace-token"))
        .and(body_json(json!({"workspace_id": WORKSPACE_ID_INITIAL})))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"access_token":original_token})),
        )
        .expect(u64::from(
            !rejected_before_exchange && scenario != SwitchScenario::Success,
        ))
        .mount(&backend)
        .await;
    let mut app = TestAppServer::builder()
        .with_codex_home(home.path())
        .with_env_overrides(&[("OPENAI_API_KEY", None)])
        .build_initialized()
        .await?;
    let id = app
        .send_request(
            "accountSession/list",
            Some(json!({
                "refreshWorkspaceMetadata": scenario == SwitchScenario::Success,
            })),
        )
        .await?;
    let listed: AccountSessionsResponse = app.read_response(id).await?;
    let session_id = listed.active_session_id.expect("active managed session");
    assert_eq!(session_id, "user-123");
    assert_eq!(listed.sessions.len(), 1);
    let session = &listed.sessions[0];
    assert_eq!(session.session_id, session_id);
    assert_eq!(session.user_id.as_deref(), Some("user-123"));
    assert_eq!(
        session.selected_workspace_account_id.as_deref(),
        Some(WORKSPACE_ID_INITIAL)
    );
    assert!(
        session
            .workspaces
            .iter()
            .any(|workspace| workspace.account_id == WORKSPACE_ID_INITIAL)
    );
    let thread_id = if scenario == SwitchScenario::Success {
        let id = app
            .send_thread_start_request_with_auto_env(codex_app_server_protocol::ThreadStartParams {
                model: Some("mock-model".into()),
                approval_policy: Some(codex_app_server_protocol::AskForApproval::OnRequest),
                approvals_reviewer: Some(codex_app_server_protocol::ApprovalsReviewer::User),
                sandbox: Some(codex_app_server_protocol::SandboxMode::WorkspaceWrite),
                ..Default::default()
            })
            .await?;
        let thread: codex_app_server_protocol::ThreadStartResponse = app.read_response(id).await?;
        let id = app
            .send_request(
                "turn/start",
                Some(json!({
                    "threadId": thread.thread.id,
                    "input": [{"type": "text", "text": "Run the command"}],
                })),
            )
            .await?;
        let _: codex_app_server_protocol::TurnStartResponse = app.read_response(id).await?;
        let request = timeout(
            DEFAULT_READ_TIMEOUT,
            app.read_stream_until_request_message(),
        )
        .await??;
        assert!(matches!(
            request,
            ServerRequest::CommandExecutionRequestApproval { .. }
        ));
        let _: AccountUpdatedNotification = app.read_notification("account/updated").await?;
        let id = app
            .send_request(
                "accountSession/switch",
                Some(json!({
                    "sessionId": "another-session", "accountId": WORKSPACE_ID_SECOND_ALLOWED,
                })),
            )
            .await?;
        let error = app
            .read_stream_until_error_message(RequestId::Integer(id))
            .await?;
        assert!(
            error
                .error
                .message
                .contains("active managed account session")
        );
        let id = app
            .send_request("thread/loaded/list", Some(json!({})))
            .await?;
        let loaded: codex_app_server_protocol::ThreadLoadedListResponse =
            app.read_response(id).await?;
        assert_eq!(loaded.data, vec![thread.thread.id.clone()]);
        assert_eq!(load_file_auth(home.path())?, Some(original.clone()));
        Some(thread.thread.id)
    } else {
        None
    };
    let id = app
        .send_request(
            "accountSession/switch",
            Some(json!({
                "sessionId": session_id, "accountId": WORKSPACE_ID_SECOND_ALLOWED,
            })),
        )
        .await?;
    if scenario != SwitchScenario::Success {
        let error = app
            .read_stream_until_error_message(RequestId::Integer(id))
            .await?;
        let expected = match scenario {
            SwitchScenario::RequiresSignIn => "requires supplied sign-in credentials",
            SwitchScenario::GovernmentWorkspace => "government workspaces",
            _ => "Workspace validation failed",
        };
        assert!(error.error.message.contains(expected), "{error:?}");
        assert_eq!(load_file_auth(home.path())?, Some(original));
        return Ok(());
    }
    let thread_id = thread_id.expect("success scenario starts a thread");
    let closed = timeout(
        DEFAULT_READ_TIMEOUT,
        app.read_stream_until_notification_message("thread/closed"),
    )
    .await??;
    assert_eq!(
        closed.params.expect("thread/closed params")["threadId"],
        thread_id.as_str()
    );
    let interrupted: TurnCompletedNotification = app.read_notification("turn/completed").await?;
    assert_eq!(interrupted.turn.status, TurnStatus::Interrupted);
    let switched: codex_app_server_protocol::AccountSessionsSwitchResponse =
        timeout(DEFAULT_READ_TIMEOUT, app.read_response(id)).await??;
    let codex_app_server_protocol::AccountSessionsSwitchResponse::Switched { sessions: switched } = switched else {
        bail!("expected workspace switch");
    };
    assert_eq!(
        switched.active_session_id.as_deref(),
        Some(session_id.as_str())
    );
    assert_eq!(
        switched.sessions[0]
            .selected_workspace_account_id
            .as_deref(),
        Some(WORKSPACE_ID_SECOND_ALLOWED)
    );
    let notification = timeout(
        DEFAULT_READ_TIMEOUT,
        app.read_stream_until_notification_message("account/updated"),
    )
    .await??;
    assert_eq!(
        serde_json::from_value::<AccountUpdatedNotification>(
            notification.params.expect("account/updated params")
        )?,
        AccountUpdatedNotification {
            auth_mode: Some(AuthMode::Chatgpt),
            plan_type: Some(AccountPlanType::Business),
        }
    );
    assert_eq!(
        read_account(&mut app).await?.account,
        Some(Account::Chatgpt {
            email: Some("person@example.com".into()),
            workspace_name: None,
            plan_type: AccountPlanType::Business,
        })
    );
    let mut expected = original;
    let tokens = expected.tokens.as_mut().expect("managed tokens exist");
    tokens.account_id = Some(WORKSPACE_ID_SECOND_ALLOWED.into());
    tokens.access_token = access_token;
    assert_eq!(load_file_auth(home.path())?, Some(expected.clone()));
    let id = app
        .send_request("account/read", Some(json!({"refreshToken": true})))
        .await?;
    let account: GetAccountResponse = app.read_response(id).await?;
    assert_eq!(
        account.account,
        Some(Account::Chatgpt {
            email: Some("person@example.com".into()),
            workspace_name: None,
            plan_type: AccountPlanType::Business
        })
    );
    let refreshed = load_file_auth(home.path())?.expect("managed auth exists");
    assert_eq!(refreshed, expected);
    let id = app
        .send_thread_resume_request(codex_app_server_protocol::ThreadResumeParams {
            thread_id: thread_id.clone(),
            exclude_turns: true,
            ..Default::default()
        })
        .await?;
    let _: codex_app_server_protocol::ThreadResumeResponse = app.read_response(id).await?;
    let id = app
        .send_request(
            "accountSession/switch",
            Some(json!({"sessionId": session_id, "accountId": WORKSPACE_ID_SECOND_ALLOWED})),
        )
        .await?;
    let unchanged: codex_app_server_protocol::AccountSessionsSwitchResponse = app.read_response(id).await?;
    let codex_app_server_protocol::AccountSessionsSwitchResponse::Switched { sessions: unchanged } = unchanged else {
        bail!("expected unchanged workspace");
    };
    assert_eq!(unchanged.active_session_id, Some(session_id));
    assert_eq!(
        unchanged.sessions[0]
            .selected_workspace_account_id
            .as_deref(),
        Some(WORKSPACE_ID_SECOND_ALLOWED)
    );
    let id = app
        .send_request("thread/loaded/list", Some(json!({})))
        .await?;
    let loaded: codex_app_server_protocol::ThreadLoadedListResponse = app.read_response(id).await?;
    assert_eq!(loaded.data, vec![thread_id]);
    let id = app
        .send_request(
            "turn/start",
            Some(json!({
                "threadId": loaded.data[0],
                "input": [{"type": "text", "text": "Continue in the selected workspace"}],
            })),
        )
        .await?;
    let _: codex_app_server_protocol::TurnStartResponse = app.read_response(id).await?;
    let completed: TurnCompletedNotification = timeout(
        DEFAULT_READ_TIMEOUT,
        app.read_notification("turn/completed"),
    )
    .await??;
    assert_eq!(completed.turn.status, TurnStatus::Completed);
    let requests = inference
        .expect("success scenario mounts inference")
        .requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(
        requests[0].header("chatgpt-account-id"),
        Some(WORKSPACE_ID_INITIAL.into())
    );
    assert_eq!(
        requests[1].header("chatgpt-account-id"),
        Some(WORKSPACE_ID_SECOND_ALLOWED.into())
    );
    assert_eq!(requests[1].header("x-codex-turn-state"), None);
    Ok(())
}

#[tokio::test]
async fn account_session_list_is_empty_without_a_managed_chatgpt_login() -> Result<()> {
    let home = TempDir::new()?;
    let mut app = TestAppServer::builder()
        .with_codex_home(home.path())
        .with_env_overrides(&[("OPENAI_API_KEY", None)])
        .build_initialized()
        .await?;
    for api_key in [None, Some("test-api-key")] {
        if let Some(api_key) = api_key {
            let id = app
                .send_request(
                    "account/login/start",
                    Some(json!({
                        "type": "apiKey", "apiKey": api_key,
                    })),
                )
                .await?;
            let _: LoginAccountResponse = app.read_response(id).await?;
        }
        let id = app
            .send_request(
                "accountSession/list",
                Some(json!({
                    "refreshWorkspaceMetadata": true,
                })),
            )
            .await?;
        let response: AccountSessionsResponse = app.read_response(id).await?;
        assert_eq!(
            response,
            AccountSessionsResponse {
                active_session_id: None,
                sessions: vec![]
            }
        );
    }
    Ok(())
}
