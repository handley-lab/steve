//! Ordinary refresh resolution and enterprise revocation without Session construction.

use super::*;
use crate::config::ConfigBuilder;
use codex_config::CONFIG_TOML_FILE;
use codex_config::ConfigLayerSource;
use codex_config::test_support::CloudConfigBundleFixture;
use pretty_assertions::assert_eq;

async fn base_config() -> (tempfile::TempDir, Config) {
    let home = tempfile::tempdir().unwrap();
    let config = ConfigBuilder::without_managed_config_for_tests()
        .codex_home(home.path().to_path_buf())
        .fallback_cwd(Some(home.path().to_path_buf()))
        .build()
        .await
        .unwrap();
    (home, config)
}

async fn layered_config(
    base: &Config,
    user: &str,
    managed: &str,
    requirements: &str,
    session: &str,
) -> Config {
    std::fs::write(base.codex_home.join(CONFIG_TOML_FILE), user).unwrap();
    let overrides: toml::Table = toml::from_str(session).unwrap();
    ConfigBuilder::without_managed_config_for_tests()
        .codex_home(base.codex_home.to_path_buf())
        .fallback_cwd(Some(base.cwd.to_path_buf()))
        .cli_overrides(overrides.into_iter().collect())
        .cloud_config_bundle(
            CloudConfigBundleFixture::enterprise_config(managed)
                .add_enterprise_requirement(format!("[features]\n{requirements}"))
                .into_loader(),
        )
        .build()
        .await
        .unwrap()
}

#[tokio::test]
async fn plugin_refresh_resolves_only_the_final_layer_combination() {
    let (_home, base) = base_config().await;
    let snapshot = |user: &str, project: &str| {
        let mut config = base.clone();
        config.config_layer_stack = codex_config::ConfigLayerStack::new(
            vec![
                codex_config::ConfigLayerEntry::new(
                    ConfigLayerSource::User {
                        file: base.codex_home.join(CONFIG_TOML_FILE),
                        profile: None,
                    },
                    toml::from_str(user).unwrap(),
                ),
                codex_config::ConfigLayerEntry::new(
                    ConfigLayerSource::Project {
                        dot_codex_folder: base.codex_home.join(".steve"),
                    },
                    toml::from_str(project).unwrap(),
                ),
            ],
            Default::default(),
            Default::default(),
        )
        .unwrap();
        config.plugins = config
            .config_layer_stack
            .effective_config()
            .try_into::<codex_config::config_toml::ConfigToml>()
            .unwrap()
            .plugins;
        config
    };
    let current = snapshot("[plugins._default]\nenabled = false", "");
    let incoming = snapshot(
        "[plugins._default]\nenabled = false\n[plugins.\"example@marketplace\"]\nenabled = 'invalid'",
        "[plugins.\"example@marketplace\"]\nenabled = false",
    );
    for (current, incoming, scope) in [
        (&current, &incoming, RuntimeConfigRefresh::User),
        (&current, &incoming, RuntimeConfigRefresh::UserFiles),
        (&incoming, &current, RuntimeConfigRefresh::Mcp),
    ] {
        assert!(current.resolve_runtime_refresh(incoming, scope).is_err());
        assert!(!current.plugins.allows_plugin("example@marketplace"));
    }
    let corrected = snapshot("[plugins._default]\nenabled = true", "");
    let refreshed = current
        .resolve_runtime_refresh(&corrected, RuntimeConfigRefresh::User)
        .unwrap();
    assert_eq!(refreshed.plugins, corrected.plugins);
    assert!(refreshed.plugins.allows_plugin("example@marketplace"));
}

#[tokio::test]
async fn ordinary_transport_transitions_use_the_resolved_incoming_map() {
    let (_home, base) = base_config().await;
    let command = "[mcp_servers.ordinary]\ncommand = 'ordinary-server'";
    let url = "[mcp_servers.ordinary]\nurl = 'https://ordinary.example/mcp'";
    for scope in [RuntimeConfigRefresh::User, RuntimeConfigRefresh::Mcp] {
        let (current, incoming) = match scope {
            RuntimeConfigRefresh::User => (
                layered_config(&base, "", "", "", url).await,
                layered_config(&base, command, "", "", "").await,
            ),
            RuntimeConfigRefresh::Mcp => (
                layered_config(&base, command, "", "", "").await,
                layered_config(&base, "", url, "", "").await,
            ),
            RuntimeConfigRefresh::UserFiles => unreachable!(),
        };
        let refreshed = current.resolve_runtime_refresh(&incoming, scope).unwrap();
        assert_eq!(refreshed.mcp_servers.get(), incoming.mcp_servers.get());
    }
}

#[tokio::test]
async fn mcp_refresh_preserves_apps_until_new_session() {
    let (_home, base) = base_config().await;
    let current = layered_config(&base, "[features]\napps = true", "", "", "").await;
    let next = layered_config(&base, "[features]\napps = true", "", "apps = false", "").await;
    let refreshed = current
        .resolve_runtime_refresh(&next, RuntimeConfigRefresh::Mcp)
        .unwrap();
    assert!(refreshed.features.enabled(Feature::Apps));
    let reloaded = refreshed
        .resolve_runtime_refresh(&next, RuntimeConfigRefresh::User)
        .unwrap();
    assert!(reloaded.features.enabled(Feature::Apps));
    assert!(!next.features.enabled(Feature::Apps));
}

/// Explicit plugin updates replace only their session override, retaining other thread settings.
#[tokio::test]
async fn plugin_override_refresh_preserves_session_and_restores_inheritance() {
    let (_home, base) = base_config().await;
    let mut current = layered_config(
        &base,
        "[plugins.browser]\nenabled = false",
        "",
        "",
        "[apps.calendar]\nenabled = false\n[plugins.browser]\nenabled = true",
    )
    .await;
    let incoming = layered_config(
        &base,
        "[plugins.browser]\nenabled = false",
        "",
        "",
        "[apps.calendar]\nenabled = true",
    )
    .await;
    let original_apps = current.config_layer_stack.effective_config()["apps"].clone();
    let layer_count = current.config_layer_stack.all_layers_low_to_high().count();
    let unchanged = current
        .resolve_runtime_refresh(&incoming, RuntimeConfigRefresh::Mcp)
        .unwrap();
    assert_eq!(unchanged.plugins, current.plugins);
    for (override_text, expected) in [
        ("[browser]\nenabled = false", false),
        ("[browser]\nenabled = true", true),
        ("", false),
    ] {
        let mut incoming = incoming.clone();
        incoming.config_layer_stack = incoming
            .config_layer_stack
            .with_session_plugins(toml::from_str(override_text).unwrap())
            .unwrap();
        current = current
            .resolve_runtime_refresh(&incoming, RuntimeConfigRefresh::Mcp)
            .unwrap();
        assert_eq!(current.plugins.plugins["browser"].enabled, Some(expected));
        assert_eq!(
            current.config_layer_stack.effective_config()["apps"],
            original_apps
        );
        assert_eq!(
            current.config_layer_stack.all_layers_low_to_high().count(),
            layer_count
        );
    }
}
