use super::backend_banner_fallback_tests::fallback_response;
use super::*;
use crate::chatwidget::UserMessage;
use crate::chatwidget::tests::helpers::set_fast_mode_test_catalog_for_models;
use pretty_assertions::assert_eq;

#[tokio::test]
async fn backend_banner_fallback_applies_before_initial_and_queued_startup_prompts() -> Result<()> {
    check_startup_fallback(false).await
}

#[tokio::test]
async fn startup_catalog_failure_holds_prompts_until_a_fresh_model_reply() -> Result<()> {
    check_startup_fallback(true).await
}

async fn check_startup_fallback(fail_catalog: bool) -> Result<()> {
    let (mut app, mut events, _ops) = make_test_app_with_channels().await;
    super::backend_banner_fallback_tests::advertise_recovery_models(&app).await;
    let mut config = app.config.clone();
    config.model = Some("gpt-5.5".into());
    config.fallback_model = Some("gpt-5.6-terra".into());
    app.config.fallback_model = config.fallback_model.clone();
    let mut tui = crate::tui::test_support::make_test_tui()?;
    let init = app.chatwidget_init_for_forked_or_resumed_thread(
        &mut tui,
        config.clone(),
        Some(UserMessage::from("initial prompt")),
    );
    app.replace_chat_widget(ChatWidget::new_with_app_event(init));
    set_chatgpt_auth(&mut app.chat_widget);
    set_fast_mode_test_catalog_for_models(&mut app.chat_widget, "gpt-5.5", "gpt-5.6-terra");
    app.model_catalog = app.chat_widget.model_catalog();
    app.pending_startup_thread_start = true;
    app.chat_widget.set_model("gpt-5.5");
    app.chat_widget
        .set_queue_submissions_until_session_configured(/*queue*/ true);
    app.chat_widget
        .apply_external_edit("queued follow-up".into());
    app.chat_widget
        .handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    let (mut server, requests, proxy) =
        session_lifecycle_requests::start_recording_app_server(&config, None, None).await?;
    let generation = app.rate_limit_hard_stop_generation;
    app.handle_event(
        &mut tui,
        &mut server,
        AppEvent::RateLimitsLoaded {
            request_id: 1,
            origin: RateLimitRefreshOrigin::StartupPrefetch {
                reset_hint_request_id: 0,
            },
            hard_stop_generation: generation,
            result: Ok(fallback_response()),
        },
    )
    .await?;
    assert!(requests.lock().unwrap().is_empty());
    assert!(
        !render_bottom_popup(&app.chat_widget, /*width*/ 72)
            .contains("Selected model usage exhausted")
    );
    let started = server.start_thread(&config).await?;
    let thread_id = started.session.thread_id;
    requests.lock().unwrap().clear();
    while events.try_recv().is_ok() {}
    let recovered_models = app
        .model_catalog
        .try_list_models()?
        .into_iter()
        .map(|mut model| {
            model.model = format!("{}/{}", app.config.model_provider_id, model.model);
            model
        })
        .collect();
    if fail_catalog {
        // Start normally, including service-tier validation; fail only the later
        // startup recovery read, after the real task has been created.
        let (mut unavailable, failed_proxy) = failing_catalog_session(&config, thread_id).await?;
        app.handle_startup_thread_started(&mut unavailable, Ok(started))
            .await?;
        unavailable.shutdown().await?;
        failed_proxy.await??;
    } else {
        app.handle_startup_thread_started(&mut server, Ok(started))
            .await?;
    }
    assert_eq!(app.active_thread_id, Some(thread_id));
    if fail_catalog {
        let held_events = std::iter::from_fn(|| events.try_recv().ok()).collect::<Vec<_>>();
        assert!(
            !held_events
                .iter()
                .any(|event| matches!(event, AppEvent::CodexOp(AppCommand::UserTurn { .. })))
        );
        assert!(held_events.iter().any(|event| matches!(event, AppEvent::InsertHistoryCell(cell)
            if lines_to_single_string(&cell.display_lines(80)).contains("Couldn't load models for startup recovery"))));
        assert_eq!(
            app.chat_widget.queued_user_message_texts(),
            vec!["queued follow-up"]
        );
        let request_id = held_events
            .into_iter()
            .find_map(|event| match event {
                AppEvent::FetchModels { request_id }
                    if app.chat_widget.model_popup_request_is_current(request_id) =>
                {
                    Some(request_id)
                }
                _ => None,
            })
            .expect("thread-scoped catalog retry remains current");
        app.handle_event(
            &mut tui,
            &mut server,
            AppEvent::ModelsLoaded {
                request_id,
                result: Ok(recovered_models),
                provider_errors: vec![],
            },
        )
        .await?;
    }
    let submissions = std::iter::from_fn(|| events.try_recv().ok())
        .filter_map(|event| {
            if let AppEvent::CodexOp(AppCommand::UserTurn { model, items, .. }) = event {
                Some((model, items))
            } else {
                None
            }
        })
        .collect::<Vec<_>>();
    assert_eq!(
        submissions,
        vec![(
            "test-provider/gpt-5.6-terra".into(),
            vec![UserInput::Text {
                text: "initial prompt".into(),
                text_elements: Vec::new(),
            }]
        )]
    );
    assert_eq!(
        app.chat_widget.queued_user_message_texts(),
        vec!["queued follow-up"]
    );
    assert_eq!(
        requests
            .lock()
            .unwrap()
            .iter()
            .map(|request| request.method.as_str())
            .collect::<Vec<_>>(),
        if fail_catalog {
            vec!["thread/settings/update"]
        } else {
            vec!["model/list", "thread/settings/update"]
        }
    );
    server.shutdown().await?;
    proxy.await??;
    Ok(())
}

async fn failing_catalog_session(
    config: &Config,
    thread_id: ThreadId,
) -> Result<(AppServerSession, tokio::task::JoinHandle<Result<()>>)> {
    use futures::{SinkExt, StreamExt};
    use tokio_tungstenite::tungstenite::Message;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let websocket_url = format!("ws://{}", listener.local_addr()?);
    let codex_home = config.codex_home.display().to_string();
    let proxy = tokio::spawn(async move {
        let (stream, _) = listener.accept().await?;
        let mut socket = tokio_tungstenite::accept_async(stream).await?;
        while let Some(Ok(Message::Text(text))) = socket.next().await {
            let request: serde_json::Value = serde_json::from_str(&text)?;
            if request["id"].is_null() {
                continue;
            }
            let response = if request["method"] == "initialize" {
                serde_json::json!({"id": request["id"], "result": {
                    "userAgent": "startup-catalog-failure-test", "codexHome": codex_home,
                }})
            } else {
                assert_eq!(request["method"], "model/list");
                assert_eq!(request["params"]["threadId"], thread_id.to_string());
                assert_eq!(request["params"]["includeHidden"], true);
                serde_json::json!({"id": request["id"], "error": {
                    "code": -32603, "message": "catalog temporarily unavailable",
                }})
            };
            socket
                .send(Message::Text(response.to_string().into()))
                .await?;
        }
        Ok(())
    });
    let client = crate::connect_remote_app_server(crate::RemoteAppServerEndpoint::WebSocket {
        websocket_url,
        auth_token: None,
    })
    .await?;
    Ok((
        AppServerSession::new(
            crate::backend_id::BackendId::new(),
            client,
            crate::app_server_session::ThreadParamsMode::Embedded,
        ),
        proxy,
    ))
}
