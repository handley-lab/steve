//! Resolves refreshable MCP inputs without rebuilding unrelated session configuration.
//! Existing sessions refresh ordinary MCP state without gaining enterprise authority.
//! Validate the retained backend after applying the refreshable requirements.

use crate::config::Config;
use crate::config::ManagedFeatures;
use crate::config::constrain_mcp_servers;
use crate::config::resolve_tool_suggest_config_from_layer_stack;
use codex_features::Feature;
use codex_features::FeatureConfigSource;
use codex_features::FeatureOverrides;
use codex_features::Features;
use codex_features::FeaturesToml;
use serde::Deserialize;

#[cfg(test)]
#[path = "runtime_refresh_tests.rs"]
mod tests;

#[derive(Clone, Copy)]
pub(crate) enum RuntimeConfigRefresh {
    User,
    Mcp,
    UserFiles,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct McpRefreshToml {
    features: Option<FeaturesToml>,
    plugins: codex_config::types::PluginsConfigToml,
}

impl Config {
    pub(crate) fn resolve_runtime_refresh(
        &self,
        incoming: &Self,
        scope: RuntimeConfigRefresh,
    ) -> std::io::Result<Self> {
        let mut layers = match scope {
            RuntimeConfigRefresh::User | RuntimeConfigRefresh::UserFiles => self
                .config_layer_stack
                .with_user_layer_from(&incoming.config_layer_stack),
            RuntimeConfigRefresh::Mcp => Self::layer_stack_preserving_session(
                &self.config_layer_stack,
                &incoming.config_layer_stack,
            )?
            .with_user_layer_from(&self.config_layer_stack),
        };
        if matches!(scope, RuntimeConfigRefresh::Mcp) {
            let plugins = incoming
                .config_layer_stack
                .all_layers_low_to_high()
                .filter(|layer| {
                    layer.name == codex_config::ConfigLayerSource::SessionFlags
                        && !layer.is_disabled()
                })
                .filter_map(|layer| layer.config.get("plugins").cloned())
                .reduce(|mut plugins, overlay| {
                    codex_config::merge_toml_values(&mut plugins, &overlay);
                    plugins
                });
            if let Some(plugins) = plugins {
                layers = layers.with_session_plugins(plugins)?;
            }
        }
        // User refreshes retain the session's managed configuration.
        layers.ensure_config_server_applied()?;
        let cfg: McpRefreshToml = layers
            .effective_config()
            .try_into()
            .map_err(|err| std::io::Error::new(std::io::ErrorKind::InvalidData, err))?;
        let mut configured_features = Features::from_sources(
            FeatureConfigSource {
                features: cfg.features.as_ref(),
                ..Default::default()
            },
            FeatureConfigSource::default(),
            FeatureOverrides::default(),
        );
        // These operational controls accept host defaults beneath explicit layer values.
        // Enterprise opt-in always comes from trusted layers instead.
        for feature in [
            Feature::McpConnectionSharing,
            Feature::McpLazyStdioStartup,
            Feature::SecretAuthStorage,
            Feature::McpOAuthRefreshCoordination,
        ] {
            if matches!(scope, RuntimeConfigRefresh::UserFiles) {
                let explicitly_configured = cfg.features.as_ref().is_some_and(|features| {
                    features
                        .entries()
                        .keys()
                        .any(|key| codex_features::feature_for_key(key) == Some(feature))
                });
                if !explicitly_configured
                    && let Some(enabled) = self.runtime_feature_defaults.get(&feature)
                {
                    configured_features.set_enabled(feature, *enabled);
                }
            } else {
                // A publication wait may have allowed a newer host/user override.
                configured_features.set_enabled(feature, incoming.features.enabled(feature));
            }
        }
        let features = ManagedFeatures::from_configured(
            configured_features,
            layers.requirements().feature_requirements.clone(),
        )?;
        let servers = incoming.mcp_servers.get().clone();
        let mut config = self.clone();
        if !matches!(scope, RuntimeConfigRefresh::UserFiles) {
            config.runtime_feature_defaults = incoming.runtime_feature_defaults.clone();
            config.mcp_optional_startup_grace = incoming.mcp_optional_startup_grace;
            config.mcp_oauth_credentials_store_mode = incoming.mcp_oauth_credentials_store_mode;
        }
        config
            .features
            .refresh_mcp_features(&features)
            .map_err(std::io::Error::other)?;
        config.mcp_servers =
            constrain_mcp_servers(servers, layers.requirements().mcp_servers.as_ref())
                .map_err(std::io::Error::other)?;
        if matches!(scope, RuntimeConfigRefresh::User) {
            config.active_project = incoming.active_project.clone();
        }
        if !matches!(scope, RuntimeConfigRefresh::Mcp) {
            config.tool_suggest = resolve_tool_suggest_config_from_layer_stack(&layers);
        }
        config.config_layer_stack = layers;
        config.plugins = cfg.plugins;
        config.validate_windows_mxc_requirement()?;
        Ok(config)
    }
}
