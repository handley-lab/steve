//! Startup boundary coverage for server defaults and connection ownership.

use super::*;
use crate::app::backend_sessions::InitialBackendSessions;
use crate::app::startup::prepare_fresh_startup_config;
use crate::app::startup::startup_model;
use crate::app::startup_bootstrap::bootstrap_server_owned_start;
use pretty_assertions::assert_eq;

const STARTUP_FIXTURE_DEFAULT_MODEL: &str = "gpt-6.1-sol";

async fn startup_provider_fixture() -> Result<crate::test_support::TestHome> {
    let home = crate::test_support::provider_test_home().await?;
    let models = [
        "gpt-5.5",
        "gpt-5.6-sol",
        "gpt-6.1-sol",
        "client-model",
        "stale-client-model",
        "server-model",
    ]
    .into_iter()
    .map(|slug| {
        let mut model = codex_models_manager::test_support::model_info(slug);
        if slug == "gpt-6.1-sol" {
            model.display_name = "GPT-6.1-Sol".into();
        }
        model
    })
    .collect();
    crate::test_support::advertise_test_models(&home, models).await;
    Ok(home)
}

fn seed_startup_provider(
    path: &Path,
    fixture: &crate::test_support::TestHome,
) -> std::io::Result<()> {
    crate::test_support::seed_test_provider_without_model(
        path,
        fixture,
        "startup-test",
        STARTUP_FIXTURE_DEFAULT_MODEL,
    )
}

fn remote_target() -> AppServerTarget {
    AppServerTarget::Remote {
        endpoint: crate::RemoteAppServerEndpoint::WebSocket {
            websocket_url: "ws://127.0.0.1:1".into(),
            auth_token: None,
        },
    }
}

pub(super) async fn run_startup_for_test(
    tui: &mut crate::tui::Tui,
    initial_backends: InitialBackendSessions,
    config: Config,
    bootstrap: Option<AppServerBootstrap>,
    selection: SessionSelection,
    app_server_target: AppServerTarget,
) -> Result<AppExitInfo> {
    App::run(
        tui,
        initial_backends,
        config.clone(),
        config.cwd.to_path_buf(),
        backend_selection::BackendPreferenceContext::new(
            &config,
            crate::backend_preference_local_cwd(
                config.cwd.as_path(),
                &app_server_target,
                &EnvironmentManager::default_for_tests(),
            ),
        ),
        Vec::new(),
        ConfigOverrides::default(),
        LoaderOverrides::default(),
        CloudConfigBundleLoader::default(),
        /*initial_prompt*/ None,
        Vec::new(),
        selection,
        codex_feedback::CodexFeedback::new(),
        /*is_first_run*/ false,
        /*should_prompt_windows_sandbox_nux_at_startup*/ false,
        app_server_target,
        /*state_db*/ None,
        Arc::new(EnvironmentManager::default_for_tests()),
        Duration::ZERO,
        bootstrap,
        /*startup_hooks_browser*/ None,
        /*daemon_startup_warning*/ None,
        /*managed_daemon*/ false,
        crate::startup_draft::tests::quiet_startup_test_pump(),
        /*managed_worktree*/ None,
        /*daemon_cli_executable*/ None,
    )
    .await
}

#[tokio::test]
async fn resume_startup_preserves_explicit_model_selector() -> Result<()> {
    let home = tempdir()?;
    let models = wiremock::MockServer::start().await;
    wiremock::Mock::given(wiremock::matchers::method("GET"))
        .and(wiremock::matchers::path("/v1/models"))
        .respond_with(
            wiremock::ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "data": [{"id": "moonshotai/Kimi-K3"}, {"id": "google/gemma-4-31b-it"}]
            })),
        )
        .mount(&models)
        .await;
    std::fs::write(
        home.path().join("config.toml"),
        format!(
            r#"
model_provider = "turing"
model = "google/gemma-4-31b-it"
[features]
api_key_model_discovery = true
[model_providers.turing]
name = "Turing"
base_url = "{0}/v1"
wire_api = "responses"
[model_providers.cambridge]
name = "Cambridge"
base_url = "{0}/v1"
wire_api = "responses"
"#,
            models.uri(),
        ),
    )?;
    let server_config = ConfigBuilder::default()
        .codex_home(home.path().to_path_buf())
        .loader_overrides(LoaderOverrides::without_managed_config_for_tests())
        .build()
        .await?;
    let thread_id = crate::tests::write_session_rollout(
        home.path(),
        "2025-01-02T10-00-00",
        "2025-01-02T10:00:00Z",
        "Saved Cambridge session",
        "cambridge",
        server_config.cwd.as_path(),
    )?;
    for (model, provider) in [
        (None, None),
        (Some("cambridge/moonshotai/Kimi-K3"), None),
        (Some("moonshotai/Kimi-K3"), Some("cambridge")),
    ] {
        let harness_overrides = ConfigOverrides {
            model: model.map(str::to_owned),
            model_provider: provider.map(str::to_owned),
            ..Default::default()
        };
        let config = ConfigBuilder::default()
            .codex_home(home.path().to_path_buf())
            .loader_overrides(LoaderOverrides::without_managed_config_for_tests())
            .harness_overrides(harness_overrides.clone())
            .build()
            .await?;
        let (mut server, requests, proxy) =
            start_recording_remote_app_server(&server_config).await?;
        let bootstrap = server.bootstrap(&config).await?;
        let backend_id = server.backend_id();
        let mut tui = crate::tui::test_support::make_test_tui()?;
        tui.pause_events();
        let mut run = Box::pin(App::run(
            &mut tui,
            crate::app::backend_sessions::InitialBackendSessions::singleton(server),
            config.clone(),
            config.cwd.to_path_buf(),
            backend_selection::BackendPreferenceContext::new(
                &config,
                crate::backend_preference_local_cwd(
                    config.cwd.as_path(),
                    &remote_target(),
                    &EnvironmentManager::default_for_tests(),
                ),
            ),
            Vec::new(),
            harness_overrides,
            LoaderOverrides::without_managed_config_for_tests(),
            CloudConfigBundleLoader::default(),
            /*initial_prompt*/ None,
            Vec::new(),
            SessionSelection::Resume(SessionTarget {
                backend_id,
                thread_id,
                path: None,
                cwd: None,
                history_mode: None,
            }),
            codex_feedback::CodexFeedback::new(),
            /*is_first_run*/ false,
            /*should_prompt_windows_sandbox_nux_at_startup*/ false,
            remote_target(),
            /*state_db*/ None,
            Arc::new(EnvironmentManager::default_for_tests()),
            Duration::ZERO,
            Some(bootstrap),
            /*startup_hooks_browser*/ None,
            /*daemon_startup_warning*/ None,
            /*managed_daemon*/ false,
            crate::startup_draft::tests::quiet_startup_test_pump(),
            /*managed_worktree*/ None,
            /*daemon_cli_executable*/ None,
        ));
        let resumes = tokio::time::timeout(Duration::from_secs(15), async {
            loop {
                let resumes = recorded_params(&requests, "thread/resume");
                if !resumes.is_empty() {
                    return Ok::<_, color_eyre::eyre::Report>(resumes);
                }
                tokio::select! {
                    result = &mut run => {
                        result?;
                        return Err(color_eyre::eyre::eyre!("startup exited before thread/resume"));
                    }
                    () = tokio::time::sleep(Duration::from_millis(20)) => {}
                }
            }
        })
        .await??;
        drop(run);
        proxy.abort();
        assert_eq!(resumes.len(), 1);
        assert_eq!(resumes[0]["model"], serde_json::json!(model));
        assert_eq!(resumes[0]["modelProvider"], serde_json::json!(provider));
    }
    Ok(())
}

#[tokio::test]
async fn agents_overview_startup_restores_saved_grouping() -> Result<()> {
    let home = startup_provider_fixture().await?;
    std::fs::write(
        home.path().join("config.toml"),
        "[tui]\nagents_overview_grouping = \"status\"\nanimations = false\n",
    )?;
    seed_startup_provider(&home.path().join("config.toml"), &home)?;
    let config = ConfigBuilder::default()
        .codex_home(home.path().to_path_buf())
        .loader_overrides(LoaderOverrides::without_managed_config_for_tests())
        .build()
        .await?;
    let (mut server, requests, proxy) = start_recording_remote_app_server(&config).await?;
    let bootstrap = server.bootstrap(&config).await?;
    let mut tui = crate::tui::test_support::make_test_tui()?;
    tui.pause_events();
    let endpoint = crate::resolve_remote_addr("ws://127.0.0.1:4500")?;
    let mut run = Box::pin(run_startup_for_test(
        &mut tui,
        InitialBackendSessions::singleton(server),
        config,
        Some(bootstrap),
        SessionSelection::AgentsOverview,
        AppServerTarget::LocalDaemon {
            allow_embedded_fallback: true,
            endpoint,
        },
    ));
    tokio::time::timeout(Duration::from_secs(/*secs*/ 15), async {
        loop {
            if !recorded_params(&requests, "thread/list").is_empty() {
                return Ok::<(), color_eyre::eyre::Report>(());
            }
            tokio::select! {
                _ = &mut run => return Ok(()),
                () = tokio::time::sleep(Duration::from_millis(/*millis*/ 20)) => {}
            }
        }
    })
    .await??;
    drop(run);
    let buffer = crate::custom_terminal::test_support::last_rendered_buffer(&tui.terminal);
    let rendered = buffer
        .content
        .chunks(buffer.area.width as usize)
        .map(|row| {
            row.iter()
                .map(ratatui::buffer::Cell::symbol)
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n");
    let grouping_header = rendered
        .lines()
        .find(|line| line.contains("Group:"))
        .expect("production startup renders Command Center grouping")
        .trim();
    insta::assert_snapshot!(grouping_header, @"Agent command center  Group: Status  g");
    proxy.abort();
    Ok(())
}

fn layered_config_read_count(requests: &RecordedRequests) -> usize {
    recorded_params(requests, "config/read")
        .iter()
        .filter(|params| params["includeLayers"] == true)
        .count()
}

#[tokio::test]
async fn cli_fork_omits_implicit_model_and_effort() -> Result<()> {
    let home = startup_provider_fixture().await?;
    std::fs::write(
        home.path().join("config.toml"),
        "model = \"gpt-5.5\"\nmodel_reasoning_effort = \"low\"\nfeatures.fast_mode = true\n",
    )?;
    seed_startup_provider(&home.path().join("config.toml"), &home)?;
    let config = ConfigBuilder::default()
        .codex_home(home.path().to_path_buf())
        .loader_overrides(LoaderOverrides::without_managed_config_for_tests())
        .build()
        .await?;
    let source = ThreadId::from_string(
        &app_test_support::create_fake_rollout(
            home.path(),
            "2026-01-01T00-00-00",
            "2026-01-01T00:00:00Z",
            "source prompt",
            Some(config.model_provider_id.as_str()),
            /*git_info*/ None,
        )
        .expect("create source rollout"),
    )?;
    let (mut server, requests, proxy) = start_recording_remote_app_server(&config).await?;
    let backend_id = server.backend_id();
    let mut bootstrap = server.bootstrap(&config).await?;
    let mut target_preset = bootstrap
        .available_models
        .iter()
        .find(|preset| preset.show_in_picker)
        .expect("visible migration target")
        .clone();
    target_preset.id = "startup-test/server-model".into();
    target_preset.model = "startup-test/server-model".into();
    bootstrap.available_models.push(target_preset);
    let client_preset = bootstrap
        .available_models
        .iter_mut()
        .find(|preset| preset.model == "startup-test/gpt-5.5")
        .expect("client model in catalog");
    client_preset.upgrade = Some(codex_protocol::openai_models::ModelUpgrade {
        id: "startup-test/server-model".into(),
        migration_config_key: "test-fork-migration".into(),
        model_link: None,
        upgrade_copy: None,
        migration_markdown: None,
        retirement_at: None,
    });
    assert!(
        crate::app::startup_prompts::should_show_model_migration_prompt(
            "startup-test/gpt-5.5",
            "startup-test/server-model",
            &Default::default(),
            &bootstrap.available_models,
        )
    );
    std::fs::write(
        home.path().join("config.toml"),
        "model = \"server-model\"\nmodel_reasoning_effort = \"high\"\n",
    )?;
    seed_startup_provider(&home.path().join("config.toml"), &home)?;
    let mut tui = crate::tui::test_support::make_test_tui()?;
    let mut run = Box::pin(run_startup_for_test(
        &mut tui,
        InitialBackendSessions::singleton(server),
        config,
        Some(bootstrap),
        SessionSelection::Fork(crate::resume_picker::SessionTarget {
            backend_id,
            path: None,
            thread_id: source,
            cwd: None,
            history_mode: None,
        }),
        AppServerTarget::Embedded,
    ));
    tokio::time::timeout(Duration::from_secs(/*secs*/ 15), async {
        loop {
            if !recorded_params(&requests, "thread/fork").is_empty() {
                return Ok::<(), color_eyre::eyre::Report>(());
            }
            tokio::select! {
                result = &mut run => {
                    result?;
                    return Err(color_eyre::eyre::eyre!("startup exited before thread/fork"));
                }
                () = tokio::time::sleep(Duration::from_millis(/*millis*/ 20)) => {}
            }
        }
    })
    .await??;
    let fork = recorded_params(&requests, "thread/fork");
    assert_eq!(fork.len(), 1);
    assert_eq!(fork[0]["model"], serde_json::Value::Null);
    assert!(fork[0]["config"].get("model_reasoning_effort").is_none());
    assert_eq!(fork[0]["serviceTier"], serde_json::Value::Null);
    assert_eq!(layered_config_read_count(&requests), 0);
    drop(run);
    proxy.abort();
    Ok(())
}

// These tests drop startup with thread/start still in flight. Its recorder
// can still be waiting for a reply after the request assertions have passed.
async fn stop_interrupted_startup_recorder(
    proxy: tokio::task::JoinHandle<Result<()>>,
) -> Result<()> {
    proxy.abort();
    match tokio::time::timeout(Duration::from_secs(15), proxy).await? {
        Err(error) if error.is_cancelled() => Ok(()),
        result => result?,
    }
}

async fn run_until_thread_start(
    server: AppServerSession,
    config: Config,
    bootstrap: AppServerBootstrap,
    requests: &RecordedRequests,
) -> Result<()> {
    let mut tui = crate::tui::test_support::make_test_tui()?;
    tui.pause_events();
    let mut run = Box::pin(run_startup_for_test(
        &mut tui,
        InitialBackendSessions::singleton(server),
        config,
        Some(bootstrap),
        SessionSelection::StartFresh,
        AppServerTarget::Embedded,
    ));
    tokio::time::timeout(Duration::from_secs(/*secs*/ 15), async {
        loop {
            if !recorded_params(requests, "thread/start").is_empty() {
                return Ok::<(), color_eyre::eyre::Report>(());
            }
            tokio::select! {
                result = &mut run => {
                    result?;
                    return Err(color_eyre::eyre::eyre!("startup exited before thread/start"));
                }
                () = tokio::time::sleep(Duration::from_millis(/*millis*/ 20)) => {}
            }
        }
    })
    .await??;
    Ok(())
}

#[tokio::test]
async fn fresh_startup_uses_server_defaults_with_explicit_and_managed_precedence() -> Result<()> {
    for (choice, managed, expected_model, expected_effort, expected_provider) in [
        ("saved", false, None, None, None),
        ("embedded_saved", false, None, None, None),
        ("typed_reviewer", false, None, None, None),
        ("kv_reviewer", false, None, None, None),
        ("cli_model", true, Some("cli-model"), None, None),
        ("kv_model", false, Some("kv-model"), None, None),
        (
            "migration",
            false,
            Some("upgraded-model"),
            Some("high"),
            Some("server-second"),
        ),
        ("cli_effort", true, None, Some("low"), None),
        ("cli_provider", false, None, None, Some("server-second")),
        ("kv_provider", false, None, None, Some("server-second")),
        (
            "profile_model",
            false,
            Some("profile-model"),
            Some("high"),
            None,
        ),
        (
            "profile_effort",
            false,
            Some("server-model"),
            Some("low"),
            None,
        ),
        ("managed", true, Some("managed-model"), Some("medium"), None),
    ]
    .into_iter()
    .chain(
        ["managed_embedded", "managed_daemon", "managed_remote"].map(|choice| {
            (
                choice,
                true,
                Some("managed-model"),
                Some("medium"),
                Some("openai"),
            )
        }),
    ) {
        let client_home = tempdir()?;
        let server_home = tempdir()?;
        let models = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::path("/v1/models"))
            .respond_with(wiremock::ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "data": (["server-model", "cli-model", "kv-model", "old-model", "upgraded-model", "profile-model", "managed-model"]
                    .map(|id| serde_json::json!({"id": id})))
            })))
            .mount(&models).await;
        std::fs::write(
            client_home.path().join("config.toml"),
            r#"
model = "client-model"
model_reasoning_effort = "low"
model_provider = "client-provider"
approval_policy = "never"
approvals_reviewer = "auto_review"
[model_providers.client-provider]
name = "Client provider"
base_url = "http://127.0.0.1:9/v1"
wire_api = "responses"
requires_openai_auth = false
[model_providers.server-second]
name = "Second provider"
base_url = "http://127.0.0.1:9/v1"
wire_api = "responses"
"#,
        )?;
        std::fs::write(
            server_home.path().join("config.toml"),
            format!(
                "{}\nmodel_provider = \"client-provider\"\n[model_providers.client-provider]\nname = \"Server\"\nbase_url = \"{}/v1\"\nwire_api = \"responses\"\n[model_providers.server-second]\nname = \"OpenAI fixture\"\nbase_url = \"{}/v1\"\nwire_api = \"responses\"\nrequires_openai_auth = false\n",
                match choice {
                    "managed" => "",
                    "kv_reviewer" => {
                        "model = \"server-model\"\napproval_policy = { granular = { sandbox_approval = false, rules = false, request_permissions = true, mcp_elicitations = false } }\n"
                    }
                    _ => {
                        "model = \"server-model\"\nmodel_reasoning_effort = \"high\"\napproval_policy = \"on-request\"\napprovals_reviewer = \"user\"\n"
                    }
                },
                models.uri(),
                models.uri()
            ),
        )?;
        if managed {
            std::fs::write(
                server_home.path().join("requirements.toml"),
                "[models.new_thread]\nmodel = \"managed-model\"\nmodel_reasoning_effort = \"medium\"\n",
            )?;
        }
        let mut harness_overrides = ConfigOverrides::default();
        let mut cli_kv_overrides = Vec::new();
        let mut loader_overrides = LoaderOverrides::without_managed_config_for_tests();
        match choice {
            "cli_model" => harness_overrides.model = Some("cli-model".to_string()),
            "kv_model" => cli_kv_overrides.extend([
                ("model".into(), "earlier-model".into()),
                ("model".into(), "kv-model".into()),
                ("approval_policy".into(), "never".into()),
            ]),
            "migration" => harness_overrides.model = Some("old-model".into()),
            "cli_provider" => harness_overrides.model_provider = Some("server-second".into()),
            "kv_provider" | "managed_embedded" | "managed_daemon" | "managed_remote" => {
                cli_kv_overrides.push(("model_provider".into(), "server-second".into()))
            }
            "cli_effort" => cli_kv_overrides.push((
                "model_reasoning_effort".to_string(),
                TomlValue::String("low".to_string()),
            )),
            "typed_reviewer" => {
                harness_overrides.approval_policy = Some(AskForApproval::OnRequest.to_core());
                harness_overrides.approvals_reviewer = Some(ApprovalsReviewer::User);
                cli_kv_overrides.push(("approval_policy".into(), "never".into()));
                cli_kv_overrides.push(("approvals_reviewer".into(), "auto_review".into()));
            }
            "kv_reviewer" => {
                cli_kv_overrides.extend([
                    ("approvals_reviewer".into(), "user".into()),
                    ("approvals_reviewer".into(), "auto_review".into()),
                ]);
                cli_kv_overrides.extend(
                    ["sandbox_approval", "rules", "mcp_elicitations"]
                        .map(|key| (format!("approval_policy.granular.{key}"), true.into())),
                );
            }
            "profile_model" | "profile_effort" => {
                let path = client_home.path().join("work.config.toml");
                std::fs::write(
                    &path,
                    if choice == "profile_model" {
                        "model = \"profile-model\"\n"
                    } else {
                        "model_reasoning_effort = \"low\"\n"
                    },
                )?;
                loader_overrides.user_config_path = Some(path.abs());
                loader_overrides.user_config_profile = Some("work".parse()?);
            }
            _ => {}
        }
        let (target, server_home) = match choice {
            "embedded_saved" => (AppServerTarget::Embedded, &client_home),
            "managed_embedded" => (AppServerTarget::Embedded, &server_home),
            "managed_daemon" => (
                AppServerTarget::LocalDaemon {
                    endpoint: crate::resolve_remote_addr("ws://127.0.0.1:4500")?,
                    allow_embedded_fallback: true,
                },
                &server_home,
            ),
            _ => (remote_target(), &server_home),
        };
        let mut launch_choices = crate::app_server_session::StartupLaunchChoices::from_launch(
            &cli_kv_overrides,
            &harness_overrides,
            &loader_overrides,
        );
        let server_owned = crate::app::startup_bootstrap::uses_server_owned_fresh_bootstrap(
            &SessionSelection::StartFresh,
            &loader_overrides,
        );
        let mut config = ConfigBuilder::default()
            .codex_home(client_home.path().to_path_buf())
            .loader_overrides(loader_overrides)
            .cli_overrides(cli_kv_overrides.clone())
            .harness_overrides(harness_overrides.clone())
            .build()
            .await?;
        let server_config = ConfigBuilder::default()
            .codex_home(server_home.path().to_path_buf())
            .loader_overrides(LoaderOverrides {
                system_requirements_path: Some(server_home.path().join("requirements.toml")),
                ..LoaderOverrides::without_managed_config_for_tests()
            })
            .build()
            .await?;
        let (mut server, requests, proxy) = start_recording_app_server_with_history(
            &server_config,
            HistoryCapabilities::Current,
            /*blocked_thread_list*/ None,
            /*failed_thread_name*/ None,
            target.thread_params_mode(),
            LoaderOverrides {
                user_config_path: Some(server_home.path().join("config.toml").abs()),
                system_requirements_path: Some(server_home.path().join("requirements.toml")),
                ..LoaderOverrides::default()
            },
        )
        .await?;
        server = server.with_remote_cwd_override(Some(server_config.cwd.to_path_buf()));
        if !server_owned {
            server.bootstrap(&config).await?;
        }
        prepare_fresh_startup_config(
            &mut config,
            &server,
            &target,
            &cli_kv_overrides,
            &harness_overrides,
            &EnvironmentManager::default_for_tests(),
        )
        .await?;
        if server_owned {
            bootstrap_server_owned_start(
                &mut server,
                &mut config,
                &mut launch_choices,
                &cli_kv_overrides,
                &harness_overrides,
            )
            .await?;
        }
        if choice == "migration" {
            let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
            apply_accepted_model_migration(
                &mut config,
                &mut launch_choices,
                &AppEventSender::new(tx),
                "old-model".into(),
                "server-second/upgraded-model".into(),
                ReasoningEffortConfig::High,
            );
        }
        let started = crate::app_server_session::start_thread_with_request_handle(
            server.request_handle(),
            &crate::local_settings::LocalSettings::from(&config),
            config,
            server.thread_params_mode(),
            server.remote_cwd_override().map(Path::to_path_buf),
            server.thread_tool_transport(),
            launch_choices,
        )
        .await?;
        if choice.starts_with("managed_") {
            assert_eq!(started.session.model, "managed-model");
            assert_eq!(
                started.session.reasoning_effort,
                Some(ReasoningEffortConfig::Medium)
            );
        }
        let starts = recorded_params(&requests, "thread/start");
        assert_eq!(starts.len(), 1, "{choice}");
        let expected_reviewer = match choice {
            "kv_reviewer" | "profile_model" | "profile_effort" => Some("auto_review"),
            "typed_reviewer" => Some("user"),
            _ => None,
        };
        assert_eq!(
            starts[0]["approvalsReviewer"],
            serde_json::json!(expected_reviewer),
            "{choice}"
        );
        let expected_policy = match choice {
            "typed_reviewer" => Some(AskForApproval::OnRequest),
            "kv_model" | "profile_model" | "profile_effort" => Some(AskForApproval::Never),
            _ => None,
        };
        assert_eq!(
            starts[0]["approvalPolicy"],
            serde_json::json!(expected_policy),
            "{choice}"
        );
        if choice == "kv_reviewer" {
            assert_eq!(
                started.session.approval_policy,
                AskForApproval::Granular {
                    sandbox_approval: true,
                    rules: true,
                    skill_approval: false,
                    request_permissions: true,
                    mcp_elicitations: true,
                }
            );
        }
        if choice == "embedded_saved" {
            assert_eq!(
                started.session.approvals_reviewer,
                ApprovalsReviewer::AutoReview
            );
        }
        assert_eq!(
            (
                starts[0]["model"].clone(),
                starts[0]["config"].get("model_reasoning_effort").cloned(),
                starts[0]["modelProvider"].clone()
            ),
            (
                serde_json::json!(expected_model),
                expected_effort.map(serde_json::Value::from),
                serde_json::json!(expected_provider),
            ),
            "{choice}"
        );
        assert_eq!(
            recorded_params(&requests, "config/read")
                .into_iter()
                .filter(|params| !params["cwd"].is_null())
                .collect::<Vec<_>>(),
            vec![
                serde_json::json!({"cwd": server_config.cwd.display().to_string(), "includeLayers": true})
            ],
        );
        if matches!(choice, "saved" | "embedded_saved") {
            let expected_turn_policy = if choice == "saved" {
                AskForApproval::OnRequest
            } else {
                AskForApproval::Never
            };
            assert_eq!(
                started.session.approvals_reviewer,
                if choice == "saved" {
                    ApprovalsReviewer::User
                } else {
                    ApprovalsReviewer::AutoReview
                }
            );
            let (mut app, _events, mut ops) = make_test_app_with_channels().await;
            app.app_server_target = target;
            app.config.permissions.approval_policy =
                codex_config::Constrained::allow_only(AskForApproval::Never.to_core());
            app.set_approvals_reviewer_in_app_and_widget(ApprovalsReviewer::AutoReview);
            app.pending_startup_thread_start = true;
            let thread_id = started.session.thread_id;
            app.handle_startup_thread_started(&mut server, Ok(started))
                .await?;
            assert_eq!(
                app.config.permissions.approval_policy.value(),
                expected_turn_policy.to_core()
            );
            app.chat_widget.insert_str("first turn");
            app.chat_widget
                .handle_key_event(KeyEvent::from(KeyCode::Enter));
            app.submit_thread_op(&mut server, thread_id, next_user_turn_op(&mut ops))
                .await?;
            let turn = &recorded_params(&requests, "turn/start")[0];
            assert!(turn["approvalsReviewer"].is_null());
            assert_eq!(
                turn["approvalPolicy"],
                serde_json::json!(expected_turn_policy)
            );
        }
        server.shutdown().await?;
        proxy.await??;
    }
    Ok(())
}

#[tokio::test]
async fn fresh_startup_reads_destination_and_cleared_model_uses_catalog() -> Result<()> {
    for (remote, override_cwd) in [(false, false), (true, false), (true, true)] {
        let client_home = startup_provider_fixture().await?;
        let server_home = startup_provider_fixture().await?;
        let destination = tempdir()?;
        let launch_cwd = tempdir()?;
        std::fs::write(
            client_home.path().join("config.toml"),
            "model = \"stale-client-model\"\nmodel_reasoning_effort = \"low\"\nmodel_reasoning_summary = \"detailed\"\nfeatures.concurrent_reasoning_summaries = true\n",
        )?;
        std::fs::write(
            server_home.path().join("config.toml"),
            "model_reasoning_effort = \"high\"\nsandbox_mode = \"read-only\"\nmodel_reasoning_summary = \"concise\"\n",
        )?;
        seed_startup_provider(&client_home.path().join("config.toml"), &client_home)?;
        seed_startup_provider(&server_home.path().join("config.toml"), &server_home)?;
        let mut config = ConfigBuilder::default()
            .codex_home(client_home.path().to_path_buf())
            .loader_overrides(LoaderOverrides::without_managed_config_for_tests())
            .harness_overrides(ConfigOverrides {
                cwd: Some(destination.path().to_path_buf()),
                ..Default::default()
            })
            .build()
            .await?;
        let mut server_config = config.clone();
        server_config.codex_home = server_home.path().to_path_buf().abs();
        server_config.sqlite = SqliteConfig::new_for_testing(server_home.path().abs());
        let mode = if remote {
            crate::app_server_session::ThreadParamsMode::Remote
        } else {
            crate::app_server_session::ThreadParamsMode::Embedded
        };
        let (mut server, requests, proxy) = start_recording_app_server_with_history(
            &server_config,
            HistoryCapabilities::Current,
            /*blocked_thread_list*/ None,
            /*failed_thread_name*/ None,
            mode,
            LoaderOverrides {
                user_config_path: Some(server_home.path().join("config.toml").abs()),
                ..LoaderOverrides::without_managed_config_for_tests()
            },
        )
        .await?;
        if override_cwd {
            server = server.with_remote_cwd_override(Some(launch_cwd.path().to_path_buf()));
        }
        if !remote {
            // Direct starts must inherit summaries even without a preceding defaults read.
            server.start_thread(&config).await?;
        }
        assert!(config.config_layer_stack.is_projectless());
        let target = if remote {
            remote_target()
        } else {
            AppServerTarget::Embedded
        };
        let bootstrap = server.bootstrap(&config).await?;
        assert_eq!(
            bootstrap.default_model.as_deref(),
            Some("stale-client-model")
        );
        let defaults_read = prepare_fresh_startup_config(
            &mut config,
            &server,
            &target,
            &[],
            &ConfigOverrides::default(),
            &EnvironmentManager::default_for_tests(),
        )
        .await?;
        assert!(!defaults_read.prompt_windows_sandbox);
        assert_eq!(config.model, None);
        let selected_model =
            startup_model(&config, &bootstrap, /*server_defaults_read*/ true)?;
        assert_ne!(selected_model, "stale-client-model");
        let started = crate::app_server_session::start_thread_with_request_handle(
            server.request_handle(),
            &crate::local_settings::LocalSettings::from(&config),
            config,
            server.thread_params_mode(),
            server.remote_cwd_override().map(Path::to_path_buf),
            server.thread_tool_transport(),
            crate::app_server_session::StartupLaunchChoices::default(),
        )
        .await?;
        assert!(
            selected_model.is_empty(),
            "server owns the cleared model choice"
        );
        assert_eq!(
            started.session.model,
            format!("startup-test/{STARTUP_FIXTURE_DEFAULT_MODEL}")
        );
        assert!(
            !started
                .session
                .permission_profile
                .file_system_sandbox_policy()
                .can_write_local_path_with_cwd(
                    started.session.cwd.as_path(),
                    started.session.cwd.as_path()
                )
        );
        let starts = recorded_params(&requests, "thread/start");
        assert_eq!(starts.len(), if remote { 1 } else { 2 });
        let latest = starts.last().unwrap();
        assert_eq!(latest["model"], serde_json::Value::Null);
        assert!(latest["config"].get("model_reasoning_effort").is_none());
        for start in &starts {
            assert!(start["config"].get("model_reasoning_summary").is_none());
            assert!(
                start["config"]["features"]
                    .get("concurrent_reasoning_summaries")
                    .is_none()
            );
        }
        let (mut app, _, _) = make_test_app_with_channels().await;
        app.chat_widget.handle_thread_session_quiet(started.session);
        if !remote {
            let project_path = destination.path().display().to_string();
            // Keep the temporary path intact until normalization, even with a long TMPDIR.
            let width = u16::try_from(80 + project_path.len())?;
            let rendered =
                render_bottom_popup(&app.chat_widget, width).replace(&project_path, "<PROJECT>");
            insta::assert_snapshot!(rendered, @r"
            › Ask Steve to do anything

              startup-test/gpt-6.1-sol high · <PROJECT>
            ");
        }
        let expected_cwd = if override_cwd {
            launch_cwd.path().display().to_string()
        } else if !remote {
            destination.path().display().to_string()
        } else {
            ".".to_string()
        };
        let reads = recorded_params(&requests, "config/read")
            .into_iter()
            .filter(|params| !params["cwd"].is_null())
            .collect::<Vec<_>>();
        assert!(!reads.is_empty());
        assert!(
            reads.iter().all(|params| params["cwd"] == expected_cwd),
            "{reads:?}"
        );
        server.shutdown().await?;
        proxy.await??;
    }
    Ok(())
}

#[tokio::test]
async fn fresh_startup_observes_config_edits_after_directory_trust() -> Result<()> {
    let home = startup_provider_fixture().await?;
    let destination = tempdir()?;
    std::fs::create_dir(destination.path().join(".git"))?;
    let config_path = home.path().join("config.toml");
    let project = serde_json::to_string(&destination.path().display().to_string())?;
    let write_tier = |tier: &str| {
        std::fs::write(
            &config_path,
            format!("service_tier = \"{tier}\"\n[projects.{project}]\ntrust_level = \"trusted\"\n"),
        )?;
        seed_startup_provider(&config_path, &home)
    };
    write_tier("priority")?;
    let config = ConfigBuilder::default()
        .codex_home(home.path().to_path_buf())
        .loader_overrides(LoaderOverrides::without_managed_config_for_tests())
        .harness_overrides(ConfigOverrides {
            cwd: Some(destination.path().to_path_buf()),
            ..Default::default()
        })
        .build()
        .await?;
    let (mut server, requests, proxy) = start_recording_app_server_with_history(
        &config,
        HistoryCapabilities::Current,
        /*blocked_thread_list*/ None,
        /*failed_thread_name*/ None,
        crate::app_server_session::ThreadParamsMode::Embedded,
        LoaderOverrides {
            user_config_path: Some(config_path.clone().abs()),
            ..LoaderOverrides::without_managed_config_for_tests()
        },
    )
    .await?;
    let bootstrap = server.bootstrap(&config).await?;
    let reads_before = layered_config_read_count(&requests);
    let trust = crate::config_update::read_remote_project_trust(
        server.request_handle(),
        config.cwd.as_path(),
        crate::config_update::ProjectTrustHost::Local,
    )
    .await?;
    assert_eq!(trust, None);
    // A different process can edit this file even when the app server is embedded.
    write_tier("flex")?;
    run_until_thread_start(server, config, bootstrap, &requests).await?;
    let starts = recorded_params(&requests, "thread/start");
    assert_eq!(
        (
            starts.len(),
            &starts[0]["serviceTier"],
            layered_config_read_count(&requests) - reads_before,
        ),
        (1, &serde_json::json!("flex"), 2),
    );
    proxy.abort();
    Ok(())
}

#[tokio::test]
async fn fresh_startup_falls_back_only_for_unsupported_config_read() -> Result<()> {
    for capabilities in [
        HistoryCapabilities::ConfigReadUnsupported(-32601),
        HistoryCapabilities::ConfigReadUnsupported(-32600),
    ] {
        let home = startup_provider_fixture().await?;
        std::fs::write(
            home.path().join("config.toml"),
            "model = \"client-model\"\nmodel_reasoning_effort = \"low\"\n",
        )?;
        seed_startup_provider(&home.path().join("config.toml"), &home)?;
        let config = ConfigBuilder::default()
            .codex_home(home.path().to_path_buf())
            .loader_overrides(LoaderOverrides::without_managed_config_for_tests())
            .build()
            .await?;
        let (mut server, requests, proxy) = start_recording_app_server_with_history(
            &config,
            capabilities,
            /*blocked_thread_list*/ None,
            /*failed_thread_name*/ None,
            crate::app_server_session::ThreadParamsMode::Embedded,
            LoaderOverrides::default(),
        )
        .await?;
        let bootstrap = server.bootstrap(&config).await?;
        run_until_thread_start(server, config, bootstrap, &requests).await?;
        let starts = recorded_params(&requests, "thread/start");
        assert_eq!(starts.len(), 1);
        assert_eq!(
            (
                &starts[0]["model"],
                &starts[0]["config"]["model_reasoning_effort"]
            ),
            (&serde_json::Value::Null, &serde_json::Value::Null)
        );
        tokio::time::timeout(Duration::from_secs(/*secs*/ 15), proxy).await???;
    }
    Ok(())
}

#[tokio::test]
async fn startup_reads_server_defaults_before_starting_thread() -> Result<()> {
    let client_home = startup_provider_fixture().await?;
    let server_home = startup_provider_fixture().await?;
    let projectless_cwd = tempdir()?;
    std::fs::write(
        client_home.path().join("config.toml"),
        "model = \"client-model\"\nmodel_reasoning_effort = \"low\"\n",
    )?;
    std::fs::write(
        server_home.path().join("config.toml"),
        "model = \"server-model\"\nmodel_reasoning_effort = \"high\"\n",
    )?;
    seed_startup_provider(&client_home.path().join("config.toml"), &client_home)?;
    seed_startup_provider(&server_home.path().join("config.toml"), &server_home)?;
    let config = ConfigBuilder::default()
        .codex_home(client_home.path().to_path_buf())
        .loader_overrides(LoaderOverrides::without_managed_config_for_tests())
        .harness_overrides(ConfigOverrides {
            cwd: Some(projectless_cwd.path().to_path_buf()),
            ..Default::default()
        })
        .build()
        .await?;
    let mut server_config = config.clone();
    server_config.codex_home = server_home.path().to_path_buf().abs();
    server_config.sqlite = SqliteConfig::new_for_testing(server_home.path().abs());
    let (mut server, requests, proxy) = start_recording_app_server_with_history(
        &server_config,
        HistoryCapabilities::Current,
        /*blocked_thread_list*/ None,
        /*failed_thread_name*/ None,
        crate::app_server_session::ThreadParamsMode::Embedded,
        LoaderOverrides {
            user_config_path: Some(server_home.path().join("config.toml").abs()),
            ..LoaderOverrides::without_managed_config_for_tests()
        },
    )
    .await?;
    let bootstrap = server.bootstrap(&config).await?;
    run_until_thread_start(server, config, bootstrap, &requests).await?;
    {
        let recorded = requests.lock().expect("request recorder lock");
        let read = recorded
            .iter()
            .position(|request| request.method == "config/read")
            .expect("startup must read server defaults");
        let start = recorded
            .iter()
            .position(|request| request.method == "thread/start")
            .expect("startup must start a thread");
        assert!(read < start);
    }
    let starts = recorded_params(&requests, "thread/start");
    assert_eq!(starts.len(), 1);
    assert!(starts[0]["approvalPolicy"]["granular"].is_object());
    assert_eq!(
        (
            &starts[0]["model"],
            &starts[0]["config"]["model_reasoning_effort"]
        ),
        (&serde_json::Value::Null, &serde_json::Value::Null),
    );
    stop_interrupted_startup_recorder(proxy).await?;
    Ok(())
}

#[tokio::test]
async fn startup_read_failure_exits_before_thread_creation() -> Result<()> {
    let (app, _, _) = make_test_app_with_channels().await;
    let home = tempdir()?;
    let mut config = app.config.clone();
    config.codex_home = home.path().to_path_buf().abs();
    config.sqlite = SqliteConfig::new_for_testing(home.path().abs());
    let (mut server, requests, proxy) = start_recording_app_server_with_history(
        &config,
        HistoryCapabilities::ConfigReadFails,
        /*blocked_thread_list*/ None,
        /*failed_thread_name*/ None,
        crate::app_server_session::ThreadParamsMode::Embedded,
        LoaderOverrides::default(),
    )
    .await?;
    let bootstrap = server.bootstrap(&config).await?;
    let mut tui = crate::tui::test_support::make_test_tui()?;
    let result = run_startup_for_test(
        &mut tui,
        InitialBackendSessions::singleton(server),
        config,
        Some(bootstrap),
        SessionSelection::StartFresh,
        AppServerTarget::Embedded,
    )
    .await;
    insta::assert_snapshot!(result.expect_err("startup read must fail").to_string(), @"config/read failed in TUI");
    assert_eq!(recorded_params(&requests, "config/read").len(), 1);
    assert!(recorded_params(&requests, "thread/start").is_empty());
    proxy.await??;
    Ok(())
}
