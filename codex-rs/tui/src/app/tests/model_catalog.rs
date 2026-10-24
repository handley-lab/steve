use super::*;
use assert_matches::assert_matches;
use codex_config::types::ModelAvailabilityNuxConfig;
use codex_protocol::config_types::ServiceTier;
use codex_protocol::openai_models::ModelAvailabilityNux;
use pretty_assertions::assert_eq;
use tokio::sync::mpsc::unbounded_channel;

fn all_model_presets() -> Vec<ModelPreset> {
    crate::test_support::TEST_MODEL_PRESETS.clone()
}

fn model_presets(slugs: &[&str]) -> Vec<ModelPreset> {
    slugs
        .iter()
        .map(|slug| ModelPreset::from(codex_models_manager::test_support::model_info(slug)))
        .collect()
}

#[tokio::test]
async fn model_picker_refresh_updates_app_catalog_from_app_server() -> Result<()> {
    let (mut app, mut rx, _op_rx) = make_test_app_with_channels().await;
    let mut model = codex_models_manager::test_support::model_info("gpt-5.5");
    model.service_tiers = vec![codex_protocol::openai_models::ModelServiceTier {
        id: ServiceTier::Fast.request_value().to_string(),
        name: "Fast".to_string(),
        description: "Test fast tier".to_string(),
    }];
    let catalog = codex_protocol::openai_models::ModelsResponse {
        models: vec![model],
    };
    let catalog_path = app.config.codex_home.as_path().join("picker-models.json");
    std::fs::write(&catalog_path, serde_json::to_vec(&catalog)?)?;
    std::fs::write(
        app.config.codex_home.join("config.toml"),
        format!(
            "model_provider = \"openai\"\nmodel = \"gpt-5.5\"\nmodel_catalog_json = {catalog_path:?}\n"
        ),
    )?;
    app.config.model_catalog = Some(catalog);
    let mut app_server = start_config_write_test_app_server(&app).await?;
    let mut tui = crate::tui::test_support::make_test_tui()?;
    let fast = Some(ServiceTier::Fast.request_value());
    let thread_id = app_server
        .start_thread(&app.config)
        .await?
        .session
        .thread_id;
    let mut session = test_thread_session(thread_id, test_path_buf("/tmp/project"));
    session.model = "gpt-5.5".to_string();
    app.primary_thread_id = Some(thread_id);
    app.active_thread_id = Some(thread_id);
    app.primary_session_configured = Some(session.clone());
    app.thread_event_channels.insert(
        thread_id,
        ThreadEventChannel::new_with_session(/*capacity*/ 4, session.clone(), Vec::new()),
    );
    app.chat_widget.handle_thread_session(session.clone());
    app.chat_widget
        .set_feature_enabled(Feature::FastMode, /*enabled*/ true);
    app.chat_widget.open_model_popup();
    let request_id = std::iter::from_fn(|| rx.try_recv().ok())
        .find_map(|event| match event {
            AppEvent::FetchModels { request_id }
                if app.chat_widget.model_popup_request_is_current(request_id) =>
            {
                Some(request_id)
            }
            _ => None,
        })
        .expect("initial model request");
    let mut hidden = all_model_presets();
    for preset in &mut hidden {
        preset.show_in_picker = false;
    }
    Box::pin(app.handle_event(
        &mut tui,
        &mut app_server,
        AppEvent::ModelsLoaded {
            request_id,
            result: Ok(hidden),
        },
    ))
    .await?;
    app.chat_widget.open_model_popup();
    assert!(app.chat_widget.no_modal_or_popup_active());
    let request = std::iter::from_fn(|| rx.try_recv().ok())
        .find(|event| {
            matches!(event, AppEvent::FetchModels { request_id }
            if app.chat_widget.model_popup_request_is_current(*request_id))
        })
        .expect("catalog request");

    Box::pin(app.handle_event(&mut tui, &mut app_server, request)).await?;
    let mut loaded = tokio::time::timeout(Duration::from_secs(/*secs*/ 10), async {
        loop {
            if let Some(event @ AppEvent::ModelsLoaded { .. }) = rx.recv().await {
                break event;
            }
        }
    })
    .await?;
    let AppEvent::ModelsLoaded {
        result: Ok(models), ..
    } = &mut loaded
    else {
        panic!("model/list should return models");
    };
    assert!(!models.is_empty());
    for model in models.iter_mut() {
        model.is_default = model.model == "gpt-5.5";
        model.default_service_tier = model
            .is_default
            .then(|| ServiceTier::Fast.request_value().to_string());
    }
    let expected = models.clone();
    Box::pin(app.handle_event(&mut tui, &mut app_server, loaded)).await?;
    assert!(app.chat_widget.no_modal_or_popup_active());
    app.chat_widget.open_model_popup();
    assert!(!app.chat_widget.no_modal_or_popup_active());

    session.service_tier = fast.map(str::to_string);
    let channel = &app.thread_event_channels[&thread_id];
    let cached = channel.store.lock().await.session.clone();
    assert_eq!(
        (&app.primary_session_configured, &cached),
        (&Some(session.clone()), &Some(session.clone()))
    );
    assert_eq!(app.chat_widget.model_catalog().try_list_models().unwrap(), expected);
    app.chat_widget.handle_thread_session(test_thread_session(
        ThreadId::new(),
        test_path_buf("/tmp/other"),
    ));
    app.chat_widget.handle_thread_session(cached.unwrap());
    assert_eq!(app.chat_widget.current_service_tier(), fast);
    let stale = AppEvent::ModelsLoaded {
        request_id: uuid::Uuid::new_v4(),
        result: Ok(all_model_presets()),
    };
    Box::pin(app.handle_event(&mut tui, &mut app_server, stale)).await?;
    assert_eq!(
        (
            app.model_catalog.try_list_models().unwrap(),
            app.chat_widget.model_catalog().try_list_models().unwrap(),
        ),
        (expected, Vec::new())
    );
    app_server.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn lifecycle_service_tier_uses_the_destination_catalog() -> Result<()> {
    use codex_app_server_protocol::ClientRequest;
    use codex_app_server_protocol::ModelListParams;
    use codex_app_server_protocol::ModelListResponse;
    use codex_app_server_protocol::RequestId;
    use codex_app_server_protocol::ServerNotification;
    use codex_protocol::openai_models::ModelServiceTier;
    use core_test_support::responses;

    let mut traces = Vec::new();
    let mut failures = Vec::new();
    for (process_tier, destination_tier) in [("priority", "ultrafast"), ("ultrafast", "priority")] {
        let a = responses::start_mock_server().await;
        let b = responses::start_mock_server().await;
        let home = tempdir()?;
        let mut model = codex_models_manager::test_support::model_info("test-model");
        model.slug = "gpt-5.5".to_string();
        model.service_tiers = ["priority", "ultrafast"]
            .into_iter()
            .map(|id| ModelServiceTier {
                id: id.to_string(),
                name: id.to_string(),
                description: id.to_string(),
            })
            .collect();
        let mut config_texts = Vec::new();
        for (label, host, tier) in [
            ("a", a.uri(), process_tier),
            ("b", b.uri(), destination_tier),
        ] {
            model.default_service_tier = Some(tier.to_string());
            let catalog = home.path().join(format!("{label}.json"));
            std::fs::write(
                &catalog,
                serde_json::to_vec(&codex_protocol::openai_models::ModelsResponse {
                    models: vec![model.clone()],
                })?,
            )?;
            config_texts.push(format!(
                r#"model_provider = "tier"
model = "gpt-5.5"
model_catalog_json = {catalog:?}
[features]
fast_mode = true
ultrafast_mode = true
guardian_approval = false
api_key_model_discovery = false
shell_snapshot = false
cli_daybreak = false
[model_providers.tier]
name = "Selected service"
base_url = "{host}/v1"
experimental_bearer_token = "local-tier-control"
request_max_retries = 0
stream_max_retries = 0
"#
            ));
        }
        std::fs::write(home.path().join("config.toml"), &config_texts[0])?;
        let config_a = ConfigBuilder::default()
            .codex_home(home.path().to_path_buf())
            .loader_overrides(LoaderOverrides::without_managed_config_for_tests())
            .build()
            .await?;
        let (mut server, requests, proxy) =
            session_lifecycle_requests::start_recording_app_server_with_history(
                &config_a,
                session_lifecycle_requests::HistoryCapabilities::CurrentWithNotifications,
                None,
                None,
                crate::app_server_session::ThreadParamsMode::Embedded,
                LoaderOverrides::without_managed_config_for_tests(),
            )
            .await?;
        let process_models = server.bootstrap(&config_a).await?.available_models;
        let parent = server.start_thread(&config_a).await?;
        let parent_capture = responses::mount_sse_once(
            &a,
            responses::sse(vec![
                responses::ev_response_created("tier-parent"),
                responses::ev_completed("tier-parent"),
            ]),
        )
        .await;
        server
            .turn_start(
                parent.session.thread_id,
                uuid::Uuid::new_v4().to_string(),
                vec![codex_app_server_protocol::UserInput::Text {
                    text: "materialize the source".to_string(),
                    text_elements: Vec::new(),
                }],
                config_a.cwd.to_path_buf(),
                None,
                None,
                crate::app_server_session::TurnPermissionsOverride::Preserve,
                &config_a.workspace_roots,
                parent.session.model.clone(),
                None,
                None,
                None,
                None,
                None,
                None,
            )
            .await?;
        tokio::time::timeout(Duration::from_secs(30), async {
            while let Some(event) = server.next_event().await {
                if let codex_app_server_client::AppServerEvent::ServerNotification(notification) =
                    event
                    && let ServerNotification::TurnCompleted(completed) = *notification
                {
                    assert_eq!(
                        completed.turn.status,
                        codex_app_server_protocol::TurnStatus::Completed
                    );
                    return;
                }
            }
            panic!("server disconnected before source completion");
        })
        .await?;
        let parent_body = parent_capture.single_request().body_json();
        assert_eq!(parent_body["service_tier"], serde_json::json!(process_tier));
        traces.push(
            serde_json::json!({"stage": "source control", "configs": config_texts,
            "rpc": *requests.lock().unwrap(), "inference": parent_body}),
        );
        if let Some(path) = std::env::var_os("STEVE_TEST_TIER_SCOPE_CAPTURE") {
            std::fs::write(path, serde_json::to_vec_pretty(&traces)?)?;
        }
        std::fs::write(home.path().join("config.toml"), &config_texts[1])?;
        let config_b = ConfigBuilder::default()
            .codex_home(home.path().to_path_buf())
            .loader_overrides(LoaderOverrides::without_managed_config_for_tests())
            .build()
            .await?;
        let fork = server
            .fork_thread_at(
                &crate::local_settings::LocalSettings::from(&config_b),
                config_b.clone(),
                parent.session.thread_id,
                None,
                None,
                crate::app_server_session::ForkGoalContinuation::StartIfIdle,
                None,
            )
            .await?;
        let mut bodies = Vec::new();
        for loaded in [false, true] {
            let (mut chat, _sender, mut events, mut commands) =
                crate::chatwidget::tests::make_chatwidget_manual_with_sender().await;
            chat.set_feature_enabled(Feature::FastMode, true);
            chat.set_feature_enabled(Feature::UltrafastMode, true);
            chat.set_feature_enabled(Feature::CliDaybreak, false);
            chat.set_service_tier(None);
            chat.handle_thread_session(fork.session.clone());
            assert!(chat.model_catalog().try_list_models()?.is_empty());
            assert!(!chat.on_models_loaded(uuid::Uuid::new_v4(), Ok(process_models.clone())));
            let current_id = std::iter::from_fn(|| events.try_recv().ok())
                .filter_map(|event| {
                    if let AppEvent::FetchModels { request_id } = event {
                        Some(request_id)
                    } else {
                        None
                    }
                })
                .last()
                .expect("destination catalog request");
            if loaded {
                let response: ModelListResponse = server
                    .request_handle()
                    .request_typed(ClientRequest::ModelList {
                        request_id: RequestId::String(uuid::Uuid::new_v4().to_string()),
                        params: ModelListParams {
                            thread_id: Some(fork.session.thread_id.to_string()),
                            include_hidden: Some(true),
                            ..Default::default()
                        },
                    })
                    .await?;
                assert!(
                    chat.on_models_loaded(
                        current_id,
                        Ok(response
                            .data
                            .into_iter()
                            .map(crate::app_server_session::model_preset_from_api_model)
                            .collect())
                    )
                );
            }
            chat.handle_paste("check the tier".to_string());
            chat.handle_key_event(crossterm::event::KeyCode::Enter.into());
            let dispatched = std::iter::from_fn(|| commands.try_recv().ok()).collect::<Vec<_>>();
            let diagnostic = format!(
                "loaded={loaded}, commands={dispatched:?}, popup={}",
                crate::chatwidget::tests::helpers::render_bottom_popup(&chat, 100)
            );
            traces
                .push(serde_json::json!({"stage": "widget submission", "diagnostic": diagnostic}));
            if let Some(path) = std::env::var_os("STEVE_TEST_TIER_SCOPE_CAPTURE") {
                std::fs::write(path, serde_json::to_vec_pretty(&traces)?)?;
            }
            let service_tier = dispatched
                .into_iter()
                .find_map(|command| {
                    if let AppCommand::UserTurn { service_tier, .. } = command {
                        Some(service_tier)
                    } else {
                        None
                    }
                })
                .unwrap_or_else(|| panic!("ordinary widget turn: {diagnostic}"));
            assert_eq!(
                service_tier,
                loaded.then(|| Some(destination_tier.to_string()))
            );
            let capture = responses::mount_sse_once(
                &b,
                responses::sse(vec![
                    responses::ev_response_created("tier-response"),
                    responses::ev_completed("tier-response"),
                ]),
            )
            .await;
            server
                .turn_start(
                    fork.session.thread_id,
                    uuid::Uuid::new_v4().to_string(),
                    vec![codex_app_server_protocol::UserInput::Text {
                        text: "check the tier".to_string(),
                        text_elements: Vec::new(),
                    }],
                    config_b.cwd.to_path_buf(),
                    None,
                    None,
                    crate::app_server_session::TurnPermissionsOverride::Preserve,
                    &config_b.workspace_roots,
                    fork.session.model.clone(),
                    None,
                    None,
                    service_tier,
                    None,
                    None,
                    None,
                )
                .await?;
            tokio::time::timeout(Duration::from_secs(30), async {
                while let Some(event) = server.next_event().await {
                    if let codex_app_server_client::AppServerEvent::ServerNotification(notification) = event
                        && let ServerNotification::TurnCompleted(completed) = *notification {
                        assert_eq!(completed.turn.status, codex_app_server_protocol::TurnStatus::Completed);
                        return;
                    }
                }
                panic!("server disconnected before tier turn completed");
            }).await?;
            let body = capture.single_request().body_json();
            if body["service_tier"] != serde_json::json!(destination_tier) {
                failures.push(format!("process={process_tier}, destination={destination_tier}, loaded={loaded}, actual={}", body["service_tier"]));
            }
            bodies.push(body);
        }
        traces.push(serde_json::json!({"configs": config_texts, "process_tier": process_tier,
            "destination_tier": destination_tier, "rpc": *requests.lock().unwrap(), "inference": bodies}));
        if let Some(path) = std::env::var_os("STEVE_TEST_TIER_SCOPE_CAPTURE") {
            std::fs::write(path, serde_json::to_vec_pretty(&traces)?)?;
        }
        server.shutdown().await?;
        proxy.await??;
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
    Ok(())
}

#[tokio::test]
async fn lifecycle_service_tier_respects_choices_and_reports_partial_creation() -> Result<()> {
    use codex_protocol::openai_models::ModelServiceTier;
    use session_lifecycle_requests::HistoryCapabilities;

    let mut traces = Vec::new();
    for (case, capability) in [
        ("models-error", HistoryCapabilities::ScopedModelsFail),
        (
            "settings-error",
            HistoryCapabilities::ThreadSettingsUnsupported,
        ),
        ("explicit-flex", HistoryCapabilities::ScopedModelsFail),
        ("opt-out", HistoryCapabilities::ScopedModelsFail),
        ("feature-off", HistoryCapabilities::ScopedModelsFail),
        ("destination-tier", HistoryCapabilities::ScopedModelsFail),
        ("unlisted-model", HistoryCapabilities::Current),
    ] {
        let endpoint = core_test_support::responses::start_mock_server().await;
        let home = tempdir()?;
        let mut catalog = codex_protocol::openai_models::ModelsResponse {
            models: vec![codex_models_manager::test_support::model_info("gpt-5.5")],
        };
        catalog.models.truncate(1);
        catalog.models[0].slug = "gpt-5.5".to_string();
        catalog.models[0].service_tiers = vec![ModelServiceTier {
            id: "priority".into(),
            name: "Priority".into(),
            description: "Configured tier".into(),
        }];
        catalog.models[0].default_service_tier = Some("priority".to_string());
        let catalog_path = home.path().join("models.json");
        std::fs::write(&catalog_path, serde_json::to_vec(&catalog)?)?;
        let destination_tier = if case == "destination-tier" {
            "service_tier = \"priority\""
        } else {
            ""
        };
        let config_text = format!(
            r#"model_provider = "tier"
model = "gpt-5.5"
model_catalog_json = {catalog_path:?}
{destination_tier}
[features]
fast_mode = true
ultrafast_mode = true
guardian_approval = false
api_key_model_discovery = false
shell_snapshot = false
[model_providers.tier]
name = "Selected service"
base_url = "{}/v1"
experimental_bearer_token = "local-tier-control"
"#,
            endpoint.uri()
        );
        std::fs::write(home.path().join("config.toml"), &config_text)?;
        let mut config = ConfigBuilder::default()
            .codex_home(home.path().to_path_buf())
            .loader_overrides(LoaderOverrides::without_managed_config_for_tests())
            .build()
            .await?;
        match case {
            "explicit-flex" => config.service_tier = Some("flex".into()),
            "opt-out" => config.notices.fast_default_opt_out = Some(true),
            "feature-off" => {
                config.features.set_enabled(Feature::FastMode, false)?;
                config.features.set_enabled(Feature::UltrafastMode, false)?;
            }
            "destination-tier" => config.service_tier = None,
            "unlisted-model" => config.model = Some("unlisted-model".into()),
            "models-error" | "settings-error" => {}
            _ => unreachable!("enumerated fixture"),
        }
        let (mut server, requests, proxy) =
            session_lifecycle_requests::start_recording_app_server_with_history(
                &config,
                capability,
                None,
                None,
                crate::app_server_session::ThreadParamsMode::Embedded,
                LoaderOverrides::without_managed_config_for_tests(),
            )
            .await?;
        let result = server.start_thread(&config).await;
        let scoped = requests
            .lock()
            .unwrap()
            .iter()
            .find(|request| {
                request.method == "model/list"
                    && request
                        .params
                        .as_ref()
                        .is_some_and(|params| params["threadId"].is_string())
            })
            .and_then(|request| request.params.as_ref())
            .map(|params| params["threadId"].as_str().unwrap().to_string());
        let (thread_id, outcome) = match case {
            "models-error" | "settings-error" => {
                let error = result.err().expect("post-creation failure");
                let id = scoped.clone().expect("scoped destination request");
                let message = format!("{error:#}");
                assert!(
                    message.contains(&id) && message.contains("was created"),
                    "{message}"
                );
                assert!(
                    message.contains(if case == "models-error" {
                        "model/list unavailable"
                    } else {
                        "does not support thread/settings/update"
                    }),
                    "{message}"
                );
                (id, serde_json::json!({"error": message}))
            }
            _ => {
                let started = result?;
                let expected = match case {
                    "explicit-flex" => Some("flex"),
                    "opt-out" => Some("default"),
                    "destination-tier" => Some("priority"),
                    _ => None,
                };
                assert_eq!(started.session.service_tier.as_deref(), expected, "{case}");
                assert_eq!(scoped.is_some(), case == "unlisted-model", "{case}");
                assert!(
                    !requests
                        .lock()
                        .unwrap()
                        .iter()
                        .any(|request| request.method == "thread/settings/update")
                );
                (
                    started.session.thread_id.to_string(),
                    serde_json::json!({"tier": started.session.service_tier}),
                )
            }
        };
        let loaded = server.thread_loaded_list(Default::default()).await?;
        assert!(
            loaded.data.contains(&thread_id),
            "created thread remains discoverable"
        );
        assert!(endpoint.received_requests().await.unwrap().is_empty());
        traces.push(serde_json::json!({"case": case, "config": config_text,
            "outcome": outcome, "thread_id": thread_id, "loaded": loaded, "rpc": *requests.lock().unwrap()}));
        if let Some(path) = std::env::var_os("STEVE_TEST_TIER_ERROR_CAPTURE") {
            std::fs::write(path, serde_json::to_vec_pretty(&traces)?)?;
        }
        server.shutdown().await?;
        proxy.await??;
    }
    Ok(())
}

fn model_presets_with_test_upgrades() -> Vec<ModelPreset> {
    let mut presets = all_model_presets();
    presets[0].display_name = "GPT-5.5".to_string();
    presets[0].description = "Legacy coding model.".to_string();
    let mut preset = presets
        .iter()
        .find(|preset| preset.model == "gpt-5.5")
        .expect("current model template")
        .clone();
    // Catalog-provided migration metadata is independent of the retired model's prompts.
    preset.id = "gpt-5.2".to_string();
    preset.model = "gpt-5.2".to_string();
    preset.upgrade = Some(ModelUpgrade {
        id: "gpt-5.5".to_string(),
        migration_config_key: "hide_test_migration_prompt".to_string(),
        model_link: None,
        upgrade_copy: None,
        migration_markdown: None,
        retirement_at: None,
    });
    presets[0].upgrade = Some(ModelUpgrade {
        id: "gpt-6-sol".to_string(),
        migration_config_key: "test-current-upgrade".to_string(),
        model_link: None,
        upgrade_copy: None,
        migration_markdown: None,
        retirement_at: None,
    });
    presets.extend(model_presets(&["gpt-6-sol"]));
    presets.push(preset);
    presets
}

fn model_availability_nux_config(shown_count: &[(&str, u32)]) -> ModelAvailabilityNuxConfig {
    ModelAvailabilityNuxConfig {
        shown_count: shown_count
            .iter()
            .map(|(model, count)| ((*model).to_string(), *count))
            .collect(),
    }
}

fn model_migration_copy_to_plain_text(copy: &crate::model_migration::ModelMigrationCopy) -> String {
    if let Some(markdown) = copy.markdown.as_ref() {
        return markdown.clone();
    }
    let mut s = String::new();
    for span in &copy.heading {
        s.push_str(&span.content);
    }
    s.push('\n');
    s.push('\n');
    for line in &copy.content {
        for span in &line.spans {
            s.push_str(&span.content);
        }
        s.push('\n');
    }
    s
}

#[tokio::test]
async fn model_migration_prompt_only_shows_for_deprecated_models() {
    let seen = BTreeMap::new();
    let presets = model_presets_with_test_upgrades();
    assert!(should_show_model_migration_prompt(
        "gpt-5.2", "gpt-5.5", &seen, &presets
    ));
    assert!(should_show_model_migration_prompt(
        "gpt-5.4",
        "gpt-6-sol",
        &seen,
        &presets
    ));
    assert!(!should_show_model_migration_prompt(
        "gpt-5.4", "gpt-5.4", &seen, &presets
    ));
}

#[test]
fn retired_model_migration_requires_catalog_metadata() {
    for (model, target) in [
        ("gpt-5.4", "gpt-6-sol"),
        ("gpt-5.4-mini", "gpt-6-luna"),
        ("openai.gpt-5.4", "openai.gpt-6-sol"),
    ] {
        let mut presets = all_model_presets();
        assert!(!presets.iter().any(|preset| preset.model == model));
        presets[0].upgrade = Some(codex_protocol::openai_models::ModelUpgrade {
            id: target.to_string(),
            migration_config_key: "unrelated-model".to_string(),
            model_link: None,
            upgrade_copy: None,
            migration_markdown: None,
            retirement_at: None,
        });
        assert_eq!(model_upgrade_for_migration(model, &presets), None);
    }
}

#[test]
fn retired_model_migration_respects_catalog_metadata() {
    for model in ["gpt-5.4-mini", "gpt-5.4", "openai.gpt-5.4"] {
        let mut presets = model_presets_with_test_upgrades();
        let current = presets
            .iter_mut()
            .find(|preset| preset.model == "gpt-5.2")
            .expect("test preset");
        current.id = model.to_string();
        current.model = model.to_string();
        let expected = current.upgrade.clone();
        assert_eq!(
            model_upgrade_for_migration(model, &presets),
            expected
        );

        presets
            .iter_mut()
            .find(|preset| preset.model == model)
            .expect("catalog preset")
            .upgrade = None;
        assert_eq!(model_upgrade_for_migration(model, &presets), None);
    }
}

#[test]
fn select_model_availability_nux_picks_only_eligible_model() {
    let mut presets = model_presets(&["gpt-5.6-terra", "gpt-5.5"]);
    presets.iter_mut().for_each(|preset| {
        preset.availability_nux = None;
    });
    let target = presets
        .iter_mut()
        .find(|preset| preset.model == "gpt-5.6-terra")
        .expect("target preset present");
    target.availability_nux = Some(ModelAvailabilityNux {
        message: "gpt-5.6-terra is available".to_string(),
    });

    let selected = select_model_availability_nux(&presets, &model_availability_nux_config(&[]));

    assert_eq!(
        selected,
        Some(StartupTooltipOverride {
            model_slug: "gpt-5.6-terra".to_string(),
            message: "gpt-5.6-terra is available".to_string(),
        })
    );
}

#[test]
fn select_model_availability_nux_does_not_fall_back_to_older_announcement() {
    let mut presets = model_presets(&["gpt-5.6-terra", "gpt-5.5"]);
    presets.iter_mut().for_each(|preset| {
        preset.availability_nux = None;
    });
    let gpt_5 = presets
        .iter_mut()
        .find(|preset| preset.model == "gpt-5.6-terra")
        .expect("gpt-5.6-terra preset present");
    gpt_5.availability_nux = Some(ModelAvailabilityNux {
        message: "gpt-5.6-terra is available".to_string(),
    });
    let gpt_5_2 = presets
        .iter_mut()
        .find(|preset| preset.model == "gpt-5.5")
        .expect("gpt-5.5 preset present");
    gpt_5_2.availability_nux = Some(ModelAvailabilityNux {
        message: "gpt-5.5 is available".to_string(),
    });

    let selected = select_model_availability_nux(
        &presets,
        &model_availability_nux_config(&[("gpt-5.6-terra", MODEL_AVAILABILITY_NUX_MAX_SHOW_COUNT)]),
    );

    assert_eq!(selected, None);
}

#[test]
fn select_model_availability_nux_uses_existing_model_order_as_priority() {
    let mut presets = model_presets(&["gpt-5.6-terra", "gpt-5.5"]);
    presets.iter_mut().for_each(|preset| {
        preset.availability_nux = None;
    });
    let first = presets
        .iter_mut()
        .find(|preset| preset.model == "gpt-5.5")
        .expect("gpt-5.5 preset present");
    first.availability_nux = Some(ModelAvailabilityNux {
        message: "first".to_string(),
    });
    let second = presets
        .iter_mut()
        .find(|preset| preset.model == "gpt-5.6-terra")
        .expect("gpt-5.6-terra preset present");
    second.availability_nux = Some(ModelAvailabilityNux {
        message: "second".to_string(),
    });

    let selected = select_model_availability_nux(&presets, &model_availability_nux_config(&[]));

    assert_eq!(
        selected,
        Some(StartupTooltipOverride {
            model_slug: "gpt-5.6-terra".to_string(),
            message: "second".to_string(),
        })
    );
}

#[test]
fn select_model_availability_nux_returns_none_when_all_models_are_exhausted() {
    let mut presets = model_presets(&["gpt-5.6-terra", "gpt-5.5"]);
    presets.iter_mut().for_each(|preset| {
        preset.availability_nux = None;
    });
    let target = presets
        .iter_mut()
        .find(|preset| preset.model == "gpt-5.6-terra")
        .expect("target preset present");
    target.availability_nux = Some(ModelAvailabilityNux {
        message: "gpt-5.6-terra is available".to_string(),
    });

    let selected = select_model_availability_nux(
        &presets,
        &model_availability_nux_config(&[("gpt-5.6-terra", MODEL_AVAILABILITY_NUX_MAX_SHOW_COUNT)]),
    );

    assert_eq!(selected, None);
}

#[tokio::test]
async fn prepare_startup_tooltip_override_persists_model_availability_nux_count() {
    let codex_home = tempdir().expect("temp steve home");
    std::fs::write(
        codex_home.path().join("config.toml"),
        "model_provider = 'openai'\n",
    )
    .expect("explicit test provider");
    let config = ConfigBuilder::default()
        .codex_home(codex_home.path().to_path_buf())
        .build()
        .await
        .expect("config");
    let mut presets = model_presets(&["gpt-5.6-terra", "gpt-5.5"]);
    presets.iter_mut().for_each(|preset| {
        preset.availability_nux = None;
    });
    let target = presets
        .iter_mut()
        .find(|preset| preset.model == "gpt-5.6-terra")
        .expect("target preset present");
    target.availability_nux = Some(ModelAvailabilityNux {
        message: "gpt-5.6-terra is available".to_string(),
    });

    let mut local_settings = crate::local_settings::LocalSettings::from(&config);
    let tooltip = prepare_startup_tooltip_override(
        &mut local_settings,
        &presets,
        /*is_first_run*/ false,
    )
    .await;

    assert_eq!(tooltip.as_deref(), Some("gpt-5.6-terra is available"));
    assert_eq!(
        local_settings.tui.model_availability_nux.shown_count,
        HashMap::from([("gpt-5.6-terra".to_string(), 1)])
    );

    let reloaded = ConfigBuilder::default()
        .codex_home(codex_home.path().to_path_buf())
        .build()
        .await
        .expect("reloaded config");
    assert_eq!(
        reloaded.model_availability_nux.shown_count,
        HashMap::from([("gpt-5.6-terra".to_string(), 1)])
    );
}

#[tokio::test]
async fn accepted_model_migration_persists_target_default_reasoning_effort() -> Result<()> {
    let mut migration_copies = Vec::new();
    for (model, replacement, provider_id) in [
        ("gpt-5.4", "gpt-6-sol", "openai"),
        ("gpt-5.4-mini", "gpt-6-luna", "openai"),
        ("gpt-5.5", "gpt-6-sol", "openai"),
        ("gpt-5.6-luna", "gpt-6-luna", "openai"),
        ("gpt-5.6-terra", "gpt-6-sol", "openai"),
        ("gpt-5.6-sol", "gpt-6-sol", "openai"),
        ("openai.gpt-5.4", "openai.gpt-6-sol", "amazon-bedrock"),
    ] {
        let mut presets = model_presets(&[replacement]);
        presets[0].default_reasoning_effort = ReasoningEffortConfig::Medium;
        let mut retired = presets[0].clone();
        retired.id = model.to_string();
        retired.model = model.to_string();
        retired.show_in_picker = false;
        retired.upgrade = Some(ModelUpgrade {
            id: replacement.to_string(),
            migration_config_key: model.to_string(),
            model_link: None,
            upgrade_copy: None,
            migration_markdown: Some(format!("Catalog recommends {replacement} for {model}.")),
            retirement_at: None,
        });
        presets.push(retired);
        let codex_home = tempdir()?;
        std::fs::write(
            codex_home.path().join("config.toml"),
            format!(
                "model = \"{model}\"\nmodel_reasoning_effort = \"xhigh\"\nmodel_provider = \"{provider_id}\"\n"
            ),
        )?;
        let mut config = ConfigBuilder::default()
            .codex_home(codex_home.path().to_path_buf())
            .build()
            .await?;
        let current_model = config.model.clone().expect("saved model");
        let provider = config.model_provider.clone();
        let upgrade = model_upgrade_for_migration(&current_model, &presets)
            .expect("catalog selection should have an upgrade");
        assert_eq!(upgrade.id, replacement);
        assert!(should_show_model_migration_prompt(
            &current_model,
            &upgrade.id,
            &BTreeMap::new(),
            &presets,
        ));
        let target =
            target_preset_for_upgrade(&presets, &upgrade.id).expect("available replacement");
        let target_effort = target.default_reasoning_effort.clone();
        let can_opt_out = presets.iter().any(|preset| preset.model == current_model);
        let copy = migration_copy_for_models(
            &current_model,
            &upgrade.id,
            upgrade.model_link,
            upgrade.upgrade_copy,
            upgrade.migration_markdown,
            target.display_name.clone(),
            Some(target.description.clone()),
            can_opt_out,
        );
        migration_copies.push(model_migration_copy_to_plain_text(&copy));

        let (tx_raw, mut rx) = unbounded_channel();
        let app_event_tx = AppEventSender::new(tx_raw);
        apply_accepted_model_migration(
            &mut config,
            &mut crate::app_server_session::StartupLaunchChoices::default(),
            &app_event_tx,
            current_model,
            format!("{provider_id}/{}", upgrade.id),
            target_effort.clone(),
        );

        assert_eq!(
            (
                config.model.as_deref(),
                config.model_reasoning_effort.as_ref(),
                config.model_provider_id.as_str(),
                &config.model_provider,
            ),
            (
                Some(replacement),
                Some(&target_effort),
                provider_id,
                &provider
            )
        );

        let acknowledged = rx.try_recv().expect("acknowledged event");
        assert_matches!(
            acknowledged,
            AppEvent::PersistModelMigrationPromptAcknowledged { from_model, to_model }
                if from_model == model && to_model == format!("{provider_id}/{replacement}")
        );

        let update_model = rx.try_recv().expect("update model event");
        assert_matches!(
            update_model,
            AppEvent::UpdateModel(updated_model) if updated_model == format!("{provider_id}/{replacement}")
        );

        let update_effort = rx.try_recv().expect("update effort event");
        assert_matches!(
            update_effort,
            AppEvent::UpdateReasoningEffort(Some(effort)) if effort == target_effort
        );

        let persist_selection = rx.try_recv().expect("persist model selection event");
        assert_matches!(
            persist_selection,
            AppEvent::PersistModelSelection { model: selected_model, effort }
                if selected_model == format!("{provider_id}/{replacement}") && effort == Some(target_effort)
        );
    }
    assert_eq!(
        migration_copies,
        [
            ("gpt-5.4", "gpt-6-sol"),
            ("gpt-5.4-mini", "gpt-6-luna"),
            ("gpt-5.5", "gpt-6-sol"),
            ("gpt-5.6-luna", "gpt-6-luna"),
            ("gpt-5.6-terra", "gpt-6-sol"),
            ("gpt-5.6-sol", "gpt-6-sol"),
            ("openai.gpt-5.4", "openai.gpt-6-sol"),
        ]
        .map(|(model, replacement)| format!("Catalog recommends {replacement} for {model}."))
    );
    Ok(())
}

#[tokio::test]
async fn model_migration_prompt_respects_hide_flag_and_self_target() {
    for (model, previous_target, target) in [
        ("gpt-5.4-mini", "gpt-5.6-luna", "gpt-6-luna"),
        ("gpt-5.4", "gpt-5.6-terra", "gpt-6-sol"),
        ("openai.gpt-5.4", "openai.gpt-5.6-terra", "openai.gpt-6-sol"),
    ] {
        let mut presets = model_presets(&[target]);
        let mut retired = presets
            .iter()
            .find(|preset| preset.model == target)
            .expect("migration target")
            .clone();
        retired.id = model.to_string();
        retired.model = model.to_string();
        retired.show_in_picker = false;
        retired.upgrade = Some(codex_protocol::openai_models::ModelUpgrade {
            id: target.to_string(),
            migration_config_key: model.to_string(),
            model_link: None,
            upgrade_copy: None,
            migration_markdown: None,
            retirement_at: None,
        });
        presets.push(retired);
        // A previously acknowledged intermediate upgrade must not hide the new target.
        let mut seen = BTreeMap::from([(model.to_string(), previous_target.to_string())]);
        assert!(should_show_model_migration_prompt(
            model,
            target,
            &seen,
            &presets
        ));
        seen.insert(model.to_string(), target.to_string());
        assert!(!should_show_model_migration_prompt(
            model,
            target,
            &seen,
            &presets
        ));
        assert!(!should_show_model_migration_prompt(
            target,
            target,
            &seen,
            &presets
        ));
    }
}

#[tokio::test]
async fn model_migration_prompt_skips_when_target_missing_or_hidden() {
    for (model, target_model) in [
        ("gpt-5.4-mini", "gpt-6-luna"),
        ("gpt-5.4", "gpt-6-sol"),
        ("openai.gpt-5.4", "openai.gpt-6-sol"),
    ] {
        let mut available = model_presets(&[target_model]);
        let mut source = model_presets(&[model]).remove(0);
        source.upgrade = Some(ModelUpgrade {
            id: target_model.to_string(),
            migration_config_key: model.to_string(),
            model_link: None,
            upgrade_copy: None,
            migration_markdown: None,
            retirement_at: None,
        });
        available.push(source);
        assert!(should_show_model_migration_prompt(
            model,
            target_model,
            &BTreeMap::new(),
            &available,
        ));
        let target = available
            .iter_mut()
            .find(|preset| preset.model == target_model)
            .expect("target preset present");
        target.show_in_picker = false;
        assert!(!should_show_model_migration_prompt(
            model,
            target_model,
            &BTreeMap::new(),
            &available,
        ));
        assert!(target_preset_for_upgrade(&available, target_model).is_none());

        available.retain(|preset| preset.model != target_model);
        assert!(!should_show_model_migration_prompt(
            model,
            target_model,
            &BTreeMap::new(),
            &available,
        ));
        assert!(target_preset_for_upgrade(&available, target_model).is_none());
    }
}

#[tokio::test]
async fn model_migration_prompt_shows_for_hidden_model() {
    let codex_home = tempdir().expect("temp steve home");
    std::fs::write(codex_home.path().join("config.toml"), "model_provider = 'openai'\n")
        .expect("explicit test provider");
    let config = ConfigBuilder::default()
        .codex_home(codex_home.path().to_path_buf())
        .build()
        .await
        .expect("config");

    let mut available_models = model_presets_with_test_upgrades();
    let current = available_models
        .iter_mut()
        .find(|preset| preset.model == "gpt-5.2")
        .expect("gpt-5.2 preset present");
    current.show_in_picker = false;
    let current = current.clone();
    assert!(
        !current.show_in_picker,
        "expected gpt-5.2 to be hidden from picker for this test"
    );

    let upgrade = current.upgrade.as_ref().expect("upgrade configured");
    available_models
        .iter_mut()
        .find(|preset| preset.model == upgrade.id)
        .expect("upgrade target present")
        .show_in_picker = true;
    assert!(
        should_show_model_migration_prompt(
            &current.model,
            &upgrade.id,
            &config.notices.model_migrations,
            &available_models,
        ),
        "expected migration prompt to be eligible for hidden model"
    );

    let target =
        target_preset_for_upgrade(&available_models, &upgrade.id).expect("upgrade target present");
    let target_description = (!target.description.is_empty()).then(|| target.description.clone());
    let can_opt_out = true;
    let copy = migration_copy_for_models(
        &current.model,
        &upgrade.id,
        upgrade.model_link.clone(),
        upgrade.upgrade_copy.clone(),
        upgrade.migration_markdown.clone(),
        target.display_name.clone(),
        target_description,
        can_opt_out,
    );

    assert_snapshot!(
        "model_migration_prompt_shows_for_hidden_model",
        model_migration_copy_to_plain_text(&copy)
    );
}


#[tokio::test]
async fn model_scope_attach_invalidates_the_app_projection() -> Result<()> {
    for replay in [false, true] {
        let (mut app, _events, _ops) = make_test_app_with_channels().await;
        let modes = app.chat_widget.model_catalog().collaboration_modes.clone();
        assert!(!app.model_catalog.models.is_empty());
        // Clearing the widget alone does not clear the separately retained app catalog.
        app.chat_widget.handle_thread_session(test_thread_session(
            ThreadId::new(),
            app.config.cwd.to_path_buf(),
        ));
        assert!(app.chat_widget.model_catalog().models.is_empty());
        assert!(!app.model_catalog.models.is_empty());
        let session = test_thread_session(ThreadId::new(), app.config.cwd.to_path_buf());
        if replay {
            app.replay_thread_snapshot(
                ThreadEventSnapshot {
                    delegated_turns: Vec::new(),
                    session: Some(session),
                    turns: Vec::new(),
                    events: Vec::new(),
                    active_reasoning_item: None,
                    input_state: None,
                },
                false,
            );
        } else {
            app.enqueue_primary_thread_session(session, Vec::new())
                .await?;
        }
        assert!(app.model_catalog.models.is_empty(), "replay={replay}");
        assert!(Arc::ptr_eq(
            &app.model_catalog,
            &app.chat_widget.model_catalog()
        ));
        assert_eq!(app.model_catalog.collaboration_modes, modes);
    }
    Ok(())
}
