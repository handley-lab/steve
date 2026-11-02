use super::*;
use crate::collaboration_modes;
use codex_app_server_client::AppServerClient;
use codex_config::LoaderOverrides;
use codex_protocol::config_types::SERVICE_TIER_DEFAULT_REQUEST_VALUE;
use crossterm::event::KeyCode;
use pretty_assertions::assert_eq;

#[tokio::test]
async fn session_model_selection_preserves_defaults_and_updates_active_thread() -> Result<()> {
    for mode in [ModeKind::Default, ModeKind::Plan] {
        let (mut app, mut events, _ops) = make_test_app_with_channels().await;
        let models = ["gpt-5.5", "gpt-5.6-sol", "gpt-5.6-terra"]
            .into_iter()
            .map(|slug| {
                let mut model = codex_models_manager::test_support::model_info(slug);
                model.supported_reasoning_levels = [
                    ReasoningEffortConfig::Low,
                    ReasoningEffortConfig::Medium,
                    ReasoningEffortConfig::High,
                    ReasoningEffortConfig::XHigh,
                    ReasoningEffortConfig::Max,
                    ReasoningEffortConfig::Ultra,
                ]
                .into_iter()
                .map(
                    |effort| codex_protocol::openai_models::ReasoningEffortPreset {
                        effort,
                        description: "Model defaults fixture".into(),
                    },
                )
                .collect();
                model
            })
            .collect();
        crate::test_support::advertise_test_models(
            app._test_codex_home.as_ref().expect("test home"),
            models,
        )
        .await;
        app.config.model = Some("gpt-5.5".into());
        app.config.model_reasoning_effort = Some(ReasoningEffortConfig::Medium);
        app.config.plan_mode_reasoning_effort = Some(ReasoningEffortConfig::Low);
        let config_path = app.config.codex_home.join("config.toml");
        let original = "# My defaults\nmodel = 'gpt-5.5'\nmodel_reasoning_effort = 'medium'\nplan_mode_reasoning_effort = 'low'\n";
        std::fs::write(&config_path, original)?;
        crate::test_support::seed_test_provider_from_config(&config_path, &app.config)?;
        let original = std::fs::read_to_string(&config_path)?;
        let mut server = Box::pin(crate::start_embedded_app_server_for_picker(&app.config)).await?;
        let started = server.start_thread(&app.config).await?;
        let thread_id = started.session.thread_id;
        app.enqueue_primary_thread_session(started.session, started.turns)
            .await?;
        let bootstrap = server.bootstrap(&app.config).await?;
        let request = app.chat_widget.refresh_account_scoped_models();
        app.chat_widget
            .on_models_loaded(request, Ok(bootstrap.available_models));
        app.model_catalog = app.chat_widget.model_catalog();
        app.chat_widget
            .set_feature_enabled(Feature::CollaborationModes, /*enabled*/ true);
        if mode == ModeKind::Plan {
            let mask = collaboration_modes::plan_mask(app.model_catalog.as_ref()).unwrap();
            app.chat_widget.set_collaboration_mask(mask);
        }
        let mut tui = crate::tui::test_support::make_test_tui()?;
        for effort in [
            ReasoningEffortConfig::High,
            ReasoningEffortConfig::Ultra,
            ReasoningEffortConfig::Max,
        ] {
            while events.try_recv().is_ok() {}
            let command = match effort {
                ReasoningEffortConfig::High => {
                    "/model test-provider/gpt-5.6-terra --reasoning high --speed=standard".into()
                }
                ReasoningEffortConfig::Ultra => "/model --reasoning=ultra".into(),
                ReasoningEffortConfig::Max => "/model --reasoning max".into(),
                _ => unreachable!(),
            };
            app.chat_widget.apply_external_edit(command);
            app.chat_widget.handle_key_event(KeyCode::Esc.into());
            app.chat_widget.handle_key_event(KeyCode::Enter.into());
            let mut messages = Vec::new();
            while let Ok(event) = events.try_recv() {
                if let AppEvent::InsertHistoryCell(cell) = &event {
                    messages.push(lines_to_single_string(&cell.transcript_lines(/*width*/ 80)));
                }
                Box::pin(app.handle_event(&mut tui, &mut server, event)).await?;
            }
            let settings = tokio::time::timeout(Duration::from_secs(5), async {
                loop {
                    let settings = next_thread_settings_updated(&mut server, thread_id).await;
                    if crate::temporary_structured_request::native_model_id(
                        &settings.thread_settings.model,
                        &app.config.model_provider_id,
                    ) == "gpt-5.6-terra"
                        && settings.thread_settings.effort == Some(effort.clone())
                    {
                        break settings;
                    }
                }
            })
            .await?;
            assert_eq!(
                (
                    crate::temporary_structured_request::native_model_id(
                        &settings.thread_settings.model,
                        &app.config.model_provider_id
                    ),
                    settings.thread_settings.effort.clone(),
                    settings.thread_settings.service_tier.as_deref(),
                ),
                ("gpt-5.6-terra", Some(effort.clone()), Some("default"))
            );
            if mode == ModeKind::Default && effort == ReasoningEffortConfig::High {
                for command in [
                    "/model gpt-5.6-terra fast",
                    "/model gpt-5.6-terra --speeed=fast",
                    "/model test-provider/gpt-5.6-terra --reasoning=invalid",
                    "/model --speed",
                    "/model --reasoning=",
                    "/model --reasoning --speed=standard",
                    "/model --speed=fast --speed standard",
                ] {
                    app.chat_widget.apply_external_edit(command.into());
                    app.chat_widget.handle_key_event(KeyCode::Esc.into());
                    app.chat_widget.handle_key_event(KeyCode::Enter.into());
                }
                while let Ok(event) = events.try_recv() {
                    assert!(!matches!(
                        event,
                        AppEvent::SelectSessionModel { .. }
                            | AppEvent::AstraSelectedFromModelPicker { .. }
                    ));
                    if let AppEvent::InsertHistoryCell(cell) = &event {
                        messages.push(lines_to_single_string(&cell.transcript_lines(/*width*/ 80)));
                    }
                    Box::pin(app.handle_event(&mut tui, &mut server, event)).await?;
                }
                insta::assert_snapshot!("session_only_model_confirmation", messages.join("\n"));
            }
            app.enqueue_thread_notification(
                thread_id,
                ServerNotification::ThreadSettingsUpdated(settings),
            )
            .await?;
            assert_eq!(
                (
                    app.chat_widget.current_model(),
                    app.chat_widget.current_reasoning_effort()
                ),
                ("test-provider/gpt-5.6-terra", Some(effort))
            );
            assert_eq!(std::fs::read(&config_path)?, original.as_bytes());
        }
        let fresh = server.start_thread(&app.config).await?;
        assert_eq!(
            (fresh.session.model.as_str(), fresh.session.reasoning_effort),
            ("test-provider/gpt-5.5", Some(ReasoningEffortConfig::Medium))
        );
        assert_eq!(
            app.config.plan_mode_reasoning_effort,
            Some(ReasoningEffortConfig::Low)
        );
        server.shutdown().await?;
    }
    Ok(())
}

#[tokio::test]
async fn standard_speed_selection_falls_back_for_older_app_servers() -> Result<()> {
    let (mut app, mut events, mut ops) = make_test_app_with_channels().await;
    app.config.service_tier = Some("flex".into());
    let mut server = Box::pin(crate::start_embedded_app_server_for_picker(&app.config)).await?;
    let started = server.start_thread(&app.config).await?;
    app.enqueue_primary_thread_session(started.session, started.turns)
        .await?;
    app.chat_widget.set_service_tier(Some("flex".into()));
    server.thread_settings_update_supported = false;
    while events.try_recv().is_ok() {}
    while ops.try_recv().is_ok() {}

    app.select_session_model(
        &mut server,
        "gpt-5.6-terra".into(),
        Some(ReasoningEffortConfig::High),
        Some(SERVICE_TIER_DEFAULT_REQUEST_VALUE.into()),
    )
    .await;

    assert_eq!(
        app.chat_widget.current_service_tier(),
        Some(SERVICE_TIER_DEFAULT_REQUEST_VALUE)
    );
    assert_eq!(app.config.service_tier.as_deref(), Some("flex"));
    app.chat_widget.apply_external_edit("continue".into());
    app.chat_widget.handle_key_event(KeyCode::Esc.into());
    app.chat_widget.handle_key_event(KeyCode::Enter.into());
    assert_matches!(
        next_user_turn_op(&mut ops),
        Op::UserTurn { service_tier, .. }
            if service_tier == Some(Some(SERVICE_TIER_DEFAULT_REQUEST_VALUE.into()))
    );
    server.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn cloud_model_selection_requires_server_acceptance() -> Result<()> {
    for event in [
        AppEvent::SelectSessionModel {
            model: "gpt-5.6-terra".into(),
            effort: Some(ReasoningEffortConfig::High),
            service_tier: None,
        },
        AppEvent::ApplyModelSelection {
            model: "gpt-5.6-terra".into(),
            effort: Some(ReasoningEffortConfig::High),
        },
        AppEvent::ApplyAdvancedReasoning {
            model: "gpt-5.6-terra".into(),
            effort: ReasoningEffortConfig::Ultra,
        },
    ] {
        let (mut app, mut events, _ops) = make_test_app_with_channels().await;
        let thread_id = ThreadId::new();
        app.active_thread_id = Some(thread_id);
        let original_model = app.chat_widget.current_model().to_string();
        let original_effort = app.chat_widget.current_reasoning_effort();
        let (client, mut commands, _server_events) =
            codex_app_server_client::CloudClient::channel(/*capacity*/ 1);
        let mut server = AppServerSession::new(
            crate::backend_id::BackendId::new(),
            AppServerClient::Cloud(client),
            crate::app_server_session::ThreadParamsMode::Cloud,
        );
        let mut tui = crate::tui::test_support::make_test_tui()?;
        while events.try_recv().is_ok() {}

        let (result, operation) =
            tokio::join!(app.handle_event(&mut tui, &mut server, event), async {
                let command = commands.recv().await.expect("cloud settings update");
                let codex_app_server_client::CloudOperation::Request(request) = command.operation
                else {
                    panic!("expected cloud request")
                };
                let ClientRequest::ThreadSettingsUpdate { params, .. } = *request else {
                    panic!("expected thread/settings/update request")
                };
                command
                    .response
                    .send(Ok(Err(JSONRPCErrorError {
                        code: -32601,
                        data: None,
                        message: "method not found".to_string(),
                    })))
                    .expect("model selection should still await its response");
                params
            });

        result?;
        assert_eq!(operation.thread_id, thread_id.to_string());
        assert_eq!(
            (
                app.chat_widget.current_model(),
                app.chat_widget.current_reasoning_effort(),
            ),
            (original_model.as_str(), original_effort)
        );
        let messages = std::iter::from_fn(|| events.try_recv().ok())
            .filter_map(|event| match event {
                AppEvent::InsertHistoryCell(cell) => {
                    Some(lines_to_single_string(&cell.transcript_lines(/*width*/ 80)))
                }
                _ => None,
            })
            .collect::<Vec<_>>();
        insta::allow_duplicates! {
            insta::assert_snapshot!(
                messages.join("\n"),
                @"■ This cloud conversation does not support changing its model from the TUI."
            );
        }
    }
    Ok(())
}

#[tokio::test]
async fn model_default_saves_report_server_outcomes_and_target_server_profile() -> Result<()> {
    check_model_default_save_outcomes(false).await
}

#[tokio::test]
async fn advanced_reasoning_saves_supported_default_and_preserves_session_ultra() -> Result<()> {
    check_model_default_save_outcomes(true).await
}

async fn check_model_default_save_outcomes(advanced: bool) -> Result<()> {
    for outcome in ["saved", "overridden", "rejected"] {
        let (mut app, mut events, _ops) = make_test_app_with_channels().await;
        app.config.model_reasoning_effort = Some(ReasoningEffortConfig::Medium);
        let local_config_path = app.config.codex_home.join("config.toml");
        let local_config_before = std::fs::read(&local_config_path).ok();
        let server_home = tempfile::tempdir()?;
        let base_path = server_home.path().join("config.toml");
        let profile_path = server_home.path().join("work.config.toml");
        std::fs::write(&base_path, "# Base configuration stays unchanged.\n")?;
        std::fs::write(&profile_path, "")?;
        let mut loader_overrides = LoaderOverrides::without_managed_config_for_tests();
        loader_overrides.user_config_path =
            Some(AbsolutePathBuf::from_absolute_path(&profile_path)?);
        loader_overrides.user_config_profile = Some("work".parse()?);
        let mut overrides = if outcome == "overridden" {
            vec![
                ("model".into(), toml::Value::String("gpt-5.6-terra".into())),
                (
                    "model_reasoning_effort".into(),
                    toml::Value::String("low".into()),
                ),
                (
                    "plan_mode_reasoning_effort".into(),
                    toml::Value::String("low".into()),
                ),
                ("service_tier".into(), toml::Value::String("flex".into())),
            ]
        } else {
            Vec::new()
        };
        overrides.extend([
            (
                "model_provider".into(),
                app.config.model_provider_id.clone().into(),
            ),
            (
                format!("model_providers.{}", app.config.model_provider_id),
                toml::Value::try_from(&app.config.model_provider)?,
            ),
        ]);
        let config = ConfigBuilder::default()
            .codex_home(server_home.path().to_path_buf())
            .cli_overrides(overrides.clone())
            .loader_overrides(loader_overrides.clone())
            .harness_overrides(ConfigOverrides {
                cwd: Some(server_home.path().to_path_buf()),
                ..Default::default()
            })
            .build()
            .await?;
        let client = crate::start_embedded_app_server(
            codex_arg0::Arg0DispatchPaths::default(),
            config,
            overrides,
            loader_overrides,
            /*strict_config*/ false,
            codex_config::CloudConfigBundleLoader::default(),
            codex_feedback::CodexFeedback::new(),
            /*log_db*/ None,
            /*state_db*/ None,
            Arc::new(codex_exec_server::EnvironmentManager::default_for_tests()),
            Default::default(),
        )
        .await?;
        let mut server = AppServerSession::new(
            crate::backend_id::BackendId::new(),
            AppServerClient::InProcess(client),
            crate::app_server_session::ThreadParamsMode::Remote,
        );
        let advanced_thread = if advanced {
            let mut model = codex_models_manager::test_support::model_info("gpt-5.5");
            model.default_reasoning_level = Some(ReasoningEffortConfig::Medium);
            model.supported_reasoning_levels = [
                ReasoningEffortConfig::Medium,
                ReasoningEffortConfig::High,
                ReasoningEffortConfig::Ultra,
            ]
            .into_iter()
            .map(
                |effort| codex_protocol::openai_models::ReasoningEffortPreset {
                    effort,
                    description: "Advanced default fixture".into(),
                },
            )
            .collect();
            crate::test_support::advertise_test_models(
                app._test_codex_home.as_ref().expect("test home"),
                vec![
                    codex_models_manager::test_support::model_info("gpt-5.6-sol"),
                    codex_models_manager::test_support::model_info("gpt-5.6-terra"),
                    model,
                ],
            )
            .await;
            let started = server.start_thread(&app.config).await?;
            let id = started.session.thread_id;
            let initial_model = started.session.model.clone();
            app.enqueue_primary_thread_session(started.session, started.turns)
                .await?;
            let bootstrap = server.bootstrap(&app.config).await?;
            let request = app.chat_widget.refresh_account_scoped_models();
            app.chat_widget
                .on_models_loaded(request, Ok(bootstrap.available_models));
            app.model_catalog = app.chat_widget.model_catalog();
            app.config.model_reasoning_effort = Some(ReasoningEffortConfig::Medium);
            Some((id, initial_model))
        } else {
            None
        };
        if outcome == "rejected" {
            std::fs::write(&profile_path, "[broken")?;
        }
        while events.try_recv().is_ok() {}
        let mut tui = crate::tui::test_support::make_test_tui()?;
        for event in [
            AppEvent::PersistModelSelection {
                model: format!("{}/gpt-5.5", app.config.model_provider_id),
                effort: Some(ReasoningEffortConfig::High),
            },
            AppEvent::PersistPlanModeReasoningEffort(Some(ReasoningEffortConfig::High)),
            AppEvent::PersistServiceTierSelection {
                service_tier: Some(ServiceTier::Fast.request_value().into()),
            },
        ] {
            Box::pin(app.handle_event(&mut tui, &mut server, event)).await?;
        }
        if let Some((thread_id, _initial_model)) = advanced_thread.as_ref() {
            let selected = format!("{}/gpt-5.5", app.config.model_provider_id);
            Box::pin(app.handle_event(
                &mut tui,
                &mut server,
                AppEvent::ApplyAdvancedReasoning {
                    model: selected.clone(),
                    effort: ReasoningEffortConfig::Ultra,
                },
            ))
            .await?;

            let updated = tokio::time::timeout(Duration::from_secs(5), async {
                loop {
                    let updated = next_thread_settings_updated(&mut server, *thread_id).await;
                    if crate::temporary_structured_request::native_model_id(
                        &updated.thread_settings.model,
                        &app.config.model_provider_id,
                    ) == "gpt-5.5"
                        && updated.thread_settings.effort == Some(ReasoningEffortConfig::Ultra)
                    {
                        break updated;
                    }
                }
            })
            .await?;
            app.enqueue_thread_notification(
                *thread_id,
                ServerNotification::ThreadSettingsUpdated(updated),
            )
            .await?;
            assert_eq!(
                (
                    app.chat_widget.current_model(),
                    app.chat_widget.current_reasoning_effort()
                ),
                (selected.as_str(), Some(ReasoningEffortConfig::Ultra))
            );
            assert_eq!(
                server
                    .thread_read(*thread_id, false)
                    .await?
                    .model
                    .as_deref(),
                Some(selected.as_str())
            );
        }
        let mut service_tier_error = None;
        let messages = std::iter::from_fn(|| events.try_recv().ok())
            .filter_map(|event| match event {
                AppEvent::InsertHistoryCell(cell) => {
                    if outcome == "rejected" {
                        let text = lines_to_single_string(&cell.transcript_lines(/*width*/ 200));
                        if text.contains("Failed to save default service tier") {
                            service_tier_error = Some(
                                text.replace(&profile_path.display().to_string(), "<config-path>"),
                            );
                        }
                    }
                    Some(lines_to_single_string(&cell.transcript_lines(/*width*/ 80)))
                }
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert_eq!(
            messages.matches("higher-priority").count(),
            if outcome == "overridden" {
                if advanced { 4 } else { 3 }
            } else {
                0
            }
        );
        assert_eq!(
            messages.matches("Failed to save").count(),
            if outcome == "rejected" {
                if advanced { 4 } else { 3 }
            } else {
                0
            }
        );
        if outcome == "overridden" && !advanced {
            insta::assert_snapshot!("overridden_model_defaults", messages);
        }
        let persisted = std::fs::read_to_string(&profile_path)?;
        if outcome == "rejected" {
            let service_tier_error =
                service_tier_error.expect("expected a service-tier save error");
            if !advanced {
                insta::assert_snapshot!("service_tier_default_save_error", service_tier_error);
            } else {
                assert!(service_tier_error.contains("Failed to save default service tier"));
            }
            assert_eq!(persisted, "[broken");
            assert!(!messages.contains("Model changed"));
            assert!(!messages.contains("Service tier set"));
        } else {
            let mut expected = toml::toml! {
                model_provider = "test-provider"
                model = "gpt-5.5"
                model_reasoning_effort = "high"
                plan_mode_reasoning_effort = "high"
                service_tier = "fast"
            };
            expected.insert(
                "model_reasoning_effort".into(),
                toml::Value::String(if advanced { "medium" } else { "high" }.into()),
            );
            assert_eq!(
                toml::from_str::<toml::Value>(&persisted)?,
                toml::Value::Table(expected)
            );
        }
        assert_eq!(std::fs::read(&local_config_path).ok(), local_config_before);
        assert_eq!(
            std::fs::read_to_string(&base_path)?,
            "# Base configuration stays unchanged.\n"
        );
        if !advanced && outcome != "rejected" {
            let mut model = codex_models_manager::test_support::model_info("gpt-5.5");
            model.supported_reasoning_levels =
                [ReasoningEffortConfig::High, ReasoningEffortConfig::Ultra]
                    .into_iter()
                    .map(
                        |effort| codex_protocol::openai_models::ReasoningEffortPreset {
                            effort,
                            description: "Session reasoning fixture".into(),
                        },
                    )
                    .collect();
            crate::test_support::advertise_test_models(
                app._test_codex_home.as_ref().expect("test home"),
                vec![
                    codex_models_manager::test_support::model_info("gpt-5.6-sol"),
                    codex_models_manager::test_support::model_info("gpt-5.6-terra"),
                    model,
                ],
            )
            .await;
            let started = crate::app_server_session::start_thread_with_request_handle(
                server.request_handle(), &app.local_settings, app.config.clone(),
                server.thread_params_mode(), None, server.thread_tool_transport(),
                crate::app_server_session::StartupLaunchChoices::default(),
            ).await?;
            if outcome == "overridden" {
                assert_eq!(
                    crate::temporary_structured_request::native_model_id(
                        &started.session.model,
                        &app.config.model_provider_id
                    ),
                    "gpt-5.6-terra"
                );
                assert_eq!(
                    started.session.reasoning_effort,
                    Some(ReasoningEffortConfig::Low)
                );
            }
            let thread_id = started.session.thread_id;
            app.enqueue_primary_thread_session(started.session, started.turns)
                .await?;
            let bootstrap = server.bootstrap(&app.config).await?;
            let request_id = app.chat_widget.refresh_account_scoped_models();
            app.chat_widget
                .on_models_loaded(request_id, Ok(bootstrap.available_models));
            app.model_catalog = app.chat_widget.model_catalog();
            let before_session = (
                std::fs::read(&profile_path)?,
                std::fs::read(&base_path)?,
                std::fs::read(&local_config_path)?,
            );
            let selected = format!("{}/gpt-5.5", app.config.model_provider_id);
            Box::pin(app.handle_event(
                &mut tui,
                &mut server,
                AppEvent::SelectSessionModel {
                    model: selected.clone(),
                    effort: Some(ReasoningEffortConfig::Ultra),
                    service_tier: None,
                },
            ))
            .await?;
            let updated = tokio::time::timeout(Duration::from_secs(5), async {
                loop {
                    let updated = next_thread_settings_updated(&mut server, thread_id).await;
                    if crate::temporary_structured_request::native_model_id(
                        &updated.thread_settings.model,
                        &app.config.model_provider_id,
                    ) == "gpt-5.5"
                        && updated.thread_settings.effort == Some(ReasoningEffortConfig::Ultra)
                    {
                        break updated;
                    }
                }
            })
            .await?;
            app.enqueue_thread_notification(
                thread_id,
                ServerNotification::ThreadSettingsUpdated(updated),
            )
            .await?;
            assert_eq!(
                (
                    app.chat_widget.current_model(),
                    app.chat_widget.current_reasoning_effort()
                ),
                (selected.as_str(), Some(ReasoningEffortConfig::Ultra))
            );
            assert_eq!(
                (
                    std::fs::read(&profile_path)?,
                    std::fs::read(&base_path)?,
                    std::fs::read(&local_config_path)?
                ),
                before_session
            );
        }
        server.shutdown().await?;
    }
    Ok(())
}
