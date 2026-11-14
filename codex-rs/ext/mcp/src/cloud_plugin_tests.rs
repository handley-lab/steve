//! Tests per-turn cloud plugin discovery, auth recovery, tool calls, and plugin policy.

use super::*;
use crate::PluginProvider;
use crate::PluginProviderError;
use crate::PluginProviderFuture;
use crate::PluginProviders;
use crate::PluginsThreadState;
use crate::install_plugin_providers;
use codex_core_plugins::ExecutorPluginProvider;
use codex_core_plugins::PluginCatalog;
use codex_core_plugins::PluginCatalogEntry;
use codex_core_plugins::PluginIdentity;
use codex_exec_server::EnvironmentManager;
use codex_extension_api::ExtensionRegistryBuilder;
use codex_extension_api::McpServerContributionContext;
use codex_extension_api::McpServerContributor;
use codex_protocol::capabilities::CapabilityRootLocation;
use codex_protocol::capabilities::SelectedCapabilityRoot;
use codex_protocol::config_types::CollaborationMode;
use codex_protocol::config_types::ModeKind;
use codex_protocol::config_types::Settings;
use codex_utils_path_uri::PathUri;
use pretty_assertions::assert_eq;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;
use std::time::Duration;
use tokio::sync::Notify;

#[tokio::test]
async fn cloud_discovery_selects_regular_task_phase() {
    struct ExistingContributor;
    impl TurnLifecycleContributor for ExistingContributor {}

    let mut builder = ExtensionRegistryBuilder::new();
    builder.turn_lifecycle_contributor(Arc::new(ExistingContributor));
    let executor = Arc::new(ExecutorPluginProvider::new(Arc::new(
        EnvironmentManager::default_for_tests(),
    )));
    install_plugin_providers(&mut builder, PluginProviders::new(executor.clone()));
    install_plugin_providers(
        &mut builder,
        PluginProviders::new(executor).with_cloud_provider(Arc::new(CloudProvider::default())),
    );
    let registry = builder.build();
    let thread = ExtensionData::new("thread");
    for plugins_enabled in [false, true] {
        thread
            .get_or_init(PluginsThreadState::default)
            .contributor_state()
            .plugins_enabled = plugins_enabled;
        let dependencies = registry
            .turn_lifecycle_contributors()
            .iter()
            .map(|contributor| contributor.requires_mcp_runtime(&thread))
            .collect::<Vec<_>>();
        assert_eq!(dependencies, vec![false, false, plugins_enabled]);
        let phases = registry
            .turn_lifecycle_contributors()
            .iter()
            .map(|contributor| contributor.turn_start_phase(&thread))
            .collect::<Vec<_>>();
        assert_eq!(
            phases,
            vec![
                TurnStartPhase::BeforeTaskRegistration,
                TurnStartPhase::BeforeTaskRegistration,
                if plugins_enabled {
                    TurnStartPhase::RegularTaskStart
                } else {
                    TurnStartPhase::BeforeTaskRegistration
                },
            ],
        );
    }
}

#[tokio::test]
async fn disabling_plugins_clears_cloud_catalog_and_skips_discovery() -> anyhow::Result<()> {
    let config_home = tempfile::tempdir()?;
    let mut config = codex_core::config::ConfigBuilder::default()
        .codex_home(config_home.path().to_path_buf())
        .fallback_cwd(Some(config_home.path().to_path_buf()))
        .build()
        .await?;
    let mode = CollaborationMode {
        mode: ModeKind::Default,
        settings: Settings {
            model: "test".to_string(),
            reasoning_effort: None,
            developer_instructions: None,
        },
    };
    let provider = Arc::new(CloudProvider::default());
    let mut builder = ExtensionRegistryBuilder::new();
    let providers = PluginProviders::new(Arc::new(ExecutorPluginProvider::new(Arc::new(
        EnvironmentManager::default_for_tests(),
    ))))
    .with_cloud_provider(provider.clone());
    install_plugin_providers(&mut builder, providers);
    let registry = builder.build();
    let session = ExtensionData::new("session");
    let thread = ExtensionData::new("thread");
    let thread_init = codex_extension_api::ExtensionDataInit::default();
    for (turn_id, plugins_enabled, reply) in [
        ("off", false, Ok(cloud_catalog())),
        ("on", true, Ok(cloud_catalog())),
        ("plugins-off", false, Ok(cloud_catalog())),
        (
            "re-enabled-failed",
            true,
            Err("discovery unavailable".to_string()),
        ),
        ("failed-plugins-off", false, Ok(cloud_catalog())),
        ("re-enabled", true, Ok(cloud_catalog())),
    ] {
        let expected_catalog = if plugins_enabled {
            reply.clone().ok()
        } else {
            None
        };
        let previous_discoveries = provider.calls.load(Ordering::SeqCst);
        provider.set_reply(reply);
        let previous = config.clone();
        config
            .features
            .set_enabled(Feature::Plugins, plugins_enabled)?;
        for contributor in registry.config_contributors() {
            contributor.on_config_changed(&session, &thread, &previous, &config);
        }
        if !plugins_enabled && let Some(state) = thread.get::<PluginsThreadState>() {
            assert_eq!(state.cloud_catalog(), None);
        }
        for contributor in registry.turn_lifecycle_contributors() {
            contributor
                .on_turn_start(TurnStartInput {
                    turn_id,
                    collaboration_mode: &mode,
                    token_usage_at_turn_start: None,
                    session_store: &session,
                    thread_store: &thread,
                    turn_store: &ExtensionData::new(turn_id),
                })
                .await;
        }
        registry.mcp_server_contributors()[0]
            .contribute(codex_extension_api::McpServerContributionContext::for_step(
                &config,
                &thread_init,
                &thread,
                "test",
                &[],
                /*executor_capability_discovery*/ None,
                /*executor_capability_discovery_v2*/ None,
            ))
            .await;
        if let Some(state) = thread.get::<PluginsThreadState>() {
            assert_eq!(state.cloud_catalog(), expected_catalog, "turn {turn_id}");
        }
        assert_eq!(
            provider.calls.load(Ordering::SeqCst),
            previous_discoveries + usize::from(plugins_enabled),
            "turn {turn_id}",
        );
    }
    Ok(())
}

#[tokio::test]
async fn account_change_clears_hosted_plugins_before_waiting_for_executor() -> anyhow::Result<()> {
    struct UnreadyExecutor;
    impl codex_exec_server::NoiseRendezvousConnectProvider for UnreadyExecutor {
        fn connect_bundle(
            &self,
            _: codex_exec_server::NoiseChannelPublicKey,
        ) -> futures::future::BoxFuture<
            '_,
            Result<
                codex_exec_server::NoiseRendezvousConnectBundle,
                codex_exec_server::ExecServerError,
            >,
        > {
            Box::pin(futures::future::pending())
        }
    }

    let home = tempfile::tempdir()?;
    let mut config = codex_core::config::ConfigBuilder::default()
        .codex_home(home.path().to_path_buf())
        .fallback_cwd(Some(home.path().to_path_buf()))
        .build()
        .await?;
    config.features.enable(Feature::Plugins)?;
    let manager = Arc::new(EnvironmentManager::default_for_tests());
    manager
        .materialize_pending_noise_environment("unready".to_string(), Arc::new(UnreadyExecutor))?;
    let contributor = PluginContributor {
        providers: PluginProviders::new(Arc::new(ExecutorPluginProvider::new(manager)))
            .with_cloud_provider(Arc::new(CloudProvider::default())),
    };
    let thread = ExtensionData::new("thread");
    let state = thread.get_or_init(PluginsThreadState::default);
    let old_auth = state
        .contributor_state()
        .prepare_cloud_generation(/*mcp_resources*/ None);
    assert!(
        state
            .contributor_state()
            .publish_cloud_catalog(old_auth.as_ref(), cloud_catalog())
    );
    let roots = [SelectedCapabilityRoot {
        id: "plugin".to_string(),
        location: CapabilityRootLocation::Environment {
            environment_id: "unready".to_string(),
            path: PathUri::parse("file:///plugins/demo")?,
        },
    }];
    let init = codex_extension_api::ExtensionDataInit::default();
    let context = McpServerContributionContext::for_step(
        &config, &init, &thread, "test", &roots, /*executor_capability_discovery*/ None,
        /*executor_capability_discovery_v2*/ None,
    )
    .with_auth_changed(/*auth_changed*/ true);
    let mut loading = std::pin::pin!(contributor.selected_plugins(context, &config.plugins));
    assert!(futures::poll!(&mut loading).is_pending());
    assert_eq!(state.cloud_catalog(), None);
    Ok(())
}

#[tokio::test(start_paused = true)]
async fn cloud_refresh_serializes_waiters_and_retries() {
    let new_catalog = PluginCatalog::default();
    for (reply, cancelled) in [
        (Ok(cloud_catalog()), false),
        (Err("discovery failed".to_string()), false),
        (Ok(cloud_catalog()), true),
    ] {
        let succeeded = reply.is_ok() && !cancelled;
        let expected_catalog = if succeeded {
            cloud_catalog()
        } else {
            new_catalog.clone()
        };
        let provider = Arc::new(CloudProvider::default());
        provider.set_reply(reply);
        let contributor = PluginContributor {
            providers: PluginProviders::new(Arc::new(ExecutorPluginProvider::new(Arc::new(
                EnvironmentManager::default_for_tests(),
            ))))
            .with_cloud_provider(provider.clone()),
        };
        let session = ExtensionData::new("session");
        let thread = ExtensionData::new("thread");
        let turn = ExtensionData::new("turn");
        let prewarm_turn = ExtensionData::new("prewarm");
        let input = CatalogRefreshInput {
            turn_id: "turn",
            session_store: &session,
            thread_store: &thread,
            turn_store: &turn,
        };
        let state = thread.get_or_init(PluginsThreadState::default);
        state.contributor_state().plugins_enabled = true;

        // A delayed prewarm waits without superseding the real turn's request.
        provider.pause_discovery.store(true, Ordering::SeqCst);
        let mut turn_start = contributor.refresh_catalog_for_step(input);
        assert!(futures::poll!(&mut turn_start).is_pending());
        let prewarm_input = CatalogRefreshInput {
            turn_id: "prewarm",
            turn_store: &prewarm_turn,
            ..input
        };
        let mut prewarm = contributor.refresh_catalog_for_step(prewarm_input);
        assert!(futures::poll!(&mut prewarm).is_pending());
        let mut cancelled_waiter = contributor.refresh_catalog_for_step(prewarm_input);
        assert!(futures::poll!(&mut cancelled_waiter).is_pending());
        drop(cancelled_waiter);
        assert_eq!(provider.calls.load(Ordering::SeqCst), 1);

        provider.set_reply(Ok(new_catalog.clone()));
        if cancelled {
            drop(turn_start);
        } else {
            provider.resume_discovery.notify_one();
            assert_eq!(turn_start.await, succeeded);
        }
        // Waiting turns share failure cooldowns; cancellation still allows immediate retry.
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(5), prewarm)
                .await
                .expect("waiting discovery should complete after its predecessor stops"),
            cancelled,
        );
        if !succeeded && !cancelled {
            assert_eq!(state.cloud_catalog(), None);
            assert_eq!(provider.calls.load(Ordering::SeqCst), 1);
            tokio::time::advance(Duration::from_secs(30)).await;
            assert!(contributor.refresh_catalog_for_step(prewarm_input).await);
        }
        assert_eq!(state.cloud_catalog(), Some(expected_catalog.clone()));
        assert!(!contributor.refresh_catalog_for_step(prewarm_input).await);
        assert_eq!(
            provider.calls.load(Ordering::SeqCst),
            1 + usize::from(!succeeded),
        );

        // Retained catalogs do not need MCP rebuilding after failed retries.
        prewarm_turn.remove::<LatestCloudPluginAuth>();
        provider.set_reply(Err("discovery failed".to_string()));
        let previous_calls = provider.calls.load(Ordering::SeqCst);
        for _ in 0..2 {
            assert!(!contributor.refresh_catalog_for_step(prewarm_input).await);
            assert_eq!(state.cloud_catalog(), Some(expected_catalog.clone()));
        }
        assert_eq!(provider.calls.load(Ordering::SeqCst), previous_calls + 1);

        // Clearing an old auth scope still requires rebuilding, even on failure.
        session.insert(McpResourceClient::new(Arc::new(
            codex_mcp::McpRuntime::empty(/*prefix_mcp_tool_names*/ false),
        )));
        assert!(contributor.refresh_catalog_for_step(prewarm_input).await);
        assert_eq!(state.cloud_catalog(), None);
        assert!(!contributor.refresh_catalog_for_step(prewarm_input).await);
        provider.set_reply(Ok(new_catalog.clone()));
        tokio::time::advance(Duration::from_secs(30)).await;
        assert!(contributor.refresh_catalog_for_step(prewarm_input).await);
    }
}

struct CloudProvider {
    reply: Mutex<Result<PluginCatalog, String>>,
    calls: AtomicUsize,
    resources: Mutex<Option<Arc<McpResourceClient>>>,
    pause_discovery: AtomicBool,
    discovery_started: Notify,
    resume_discovery: Notify,
}

impl Default for CloudProvider {
    fn default() -> Self {
        Self {
            reply: Mutex::new(Ok(PluginCatalog::default())),
            calls: AtomicUsize::new(0),
            resources: Mutex::new(None),
            pause_discovery: AtomicBool::new(false),
            discovery_started: Notify::new(),
            resume_discovery: Notify::new(),
        }
    }
}

impl CloudProvider {
    fn set_reply(&self, reply: Result<PluginCatalog, String>) {
        *self
            .reply
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = reply;
    }
}

impl PluginProvider for CloudProvider {
    fn list(&self, query: PluginListQuery) -> PluginProviderFuture<'_, PluginCatalog> {
        *self
            .resources
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = query.mcp_resources;
        self.calls.fetch_add(1, Ordering::SeqCst);
        let reply = self
            .reply
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        let pause = self.pause_discovery.swap(false, Ordering::SeqCst);
        Box::pin(async move {
            if pause {
                self.discovery_started.notify_one();
                self.resume_discovery.notified().await;
            }
            reply.map_err(PluginProviderError::Message)
        })
    }
}

fn cloud_catalog() -> PluginCatalog {
    PluginCatalog {
        entries: vec![PluginCatalogEntry {
            enabled: true,
            id: PluginIdentity::Remote {
                remote_plugin_id: "plugin_remote".to_string(),
            },
            display_name: "Cloud Demo".to_string(),
            version: "1.0.0".to_string(),
            mcp_servers: Default::default(),
            connector_ids: vec!["calendar".to_string()],
            locations: vec![codex_core_plugins::PluginSourceLocation::Cloud {
                resource_uri: "plugin://plugin_remote".to_string(),
                bundle_uri: None,
            }],
        }],
        ..Default::default()
    }
}
