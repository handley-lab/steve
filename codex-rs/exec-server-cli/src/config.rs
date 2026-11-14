//! Loads executor startup configuration and remote authentication without changing CLI policy.

use codex_app_server::in_process::EmbeddedNetworkPolicy;
use codex_cloud_config::managed_config_bundle_loader;
use codex_config::LoaderOverrides;
use codex_core::config::ConfigBuilder;
use codex_core::config::ConfigLoadOptions;
use codex_core::config::bootstrap_account_path_style;
use codex_core::config::bootstrap_auth_config;
use codex_core::config::find_codex_home;
use codex_core::config::load_config_toml_with_layer_stack;
use codex_login::AuthManager;
use codex_protocol::auth::AuthMode;
use codex_utils_absolute_path::AbsolutePathBuf;
use codex_utils_cli::CliConfigOverrides;
use std::sync::Arc;

const REMOTE_AUTH_MODES: &[AuthMode] = &[AuthMode::ApiKey];

pub(super) async fn load_exec_server_remote_auth_provider(
    config: &codex_core::config::Config,
    base_url: &str,
) -> anyhow::Result<codex_api::SharedAuthProvider> {
    let (_auth_manager, auth) = load_exec_server_remote_auth(
        config,
        "remote exec-server registration requires an API key; set STEVE_API_KEY",
    )
    .await?;

    if !auth.is_api_key_auth() {
        anyhow::bail!("remote exec-server registration requires API key authentication");
    }
    validate_api_key_remote_host(base_url)?;
    Ok(codex_model_provider::auth_provider_from_auth(&auth))
}

pub(super) fn validate_api_key_remote_host(base_url: &str) -> anyhow::Result<()> {
    let url = url::Url::parse(base_url)
        .map_err(|err| anyhow::anyhow!("invalid remote exec-server registration URL: {err}"))?;
    let host = url.host().ok_or_else(|| {
        anyhow::anyhow!("remote exec-server registration URL must include a host")
    })?;

    let is_loopback = match &host {
        url::Host::Domain(host) => host.eq_ignore_ascii_case("localhost"),
        url::Host::Ipv4(ip) => ip.is_loopback(),
        url::Host::Ipv6(ip) => ip.is_loopback(),
    };
    let is_allowed = match url.scheme() {
        "https" => true,
        "http" => is_loopback,
        _ => false,
    };

    if !is_allowed {
        anyhow::bail!(
            "remote exec-server API-key authentication requires HTTPS or an HTTP loopback URL"
        );
    }

    Ok(())
}

pub(super) async fn load_exec_server_config(
    root_config_overrides: &CliConfigOverrides,
    strict_config: bool,
    enable_cloud_config: bool,
) -> anyhow::Result<codex_core::config::Config> {
    let cli_kv_overrides = root_config_overrides
        .parse_overrides()
        .map_err(anyhow::Error::msg)?;
    let bootstrap_cli_overrides = cli_kv_overrides.clone();
    let mut builder = ConfigBuilder::default()
        .cli_overrides(cli_kv_overrides)
        .strict_config(strict_config);
    if enable_cloud_config {
        let codex_home = find_codex_home()?;
        let loader_overrides = LoaderOverrides::default();
        let network_policy = EmbeddedNetworkPolicy::load(&loader_overrides).await;
        let bootstrap_cwd = AbsolutePathBuf::current_dir()?;
        let bootstrap_config = load_config_toml_with_layer_stack(
            &codex_home,
            Some(&bootstrap_cwd),
            bootstrap_cli_overrides,
            ConfigLoadOptions {
                loader_overrides,
                strict_config,
                cloud_config_bundle: Default::default(),
            },
        )
        .await?;
        let path_style = bootstrap_account_path_style(&bootstrap_config);
        let bootstrap_auth_config = network_policy.bind_bootstrap_auth(
            bootstrap_auth_config(&codex_home, &bootstrap_config)?,
            path_style,
        );
        let cloud_config_bundle = managed_config_bundle_loader(
            bootstrap_auth_config.codex_home.clone(),
            bootstrap_auth_config
                .auth_route_config
                .http_client_factory()
                .clone(),
            bootstrap_config.config_layer_stack.requirements_toml(),
        );
        builder = builder.cloud_config_bundle(cloud_config_bundle);
    }
    let mut config = builder.build().await?;
    EmbeddedNetworkPolicy::default().activate(&mut config);
    Ok(config)
}

async fn load_exec_server_remote_auth(
    config: &codex_core::config::Config,
    missing_auth_error: &'static str,
) -> anyhow::Result<(Arc<AuthManager>, codex_login::CodexAuth)> {
    let auth_manager = AuthManager::shared_from_config_with_auth_modes(
        config,
        /*enable_codex_api_key_env*/ true,
        REMOTE_AUTH_MODES.to_vec(),
    )
    .await?;

    let auth = match auth_manager.auth().await {
        Some(auth) => auth,
        None => {
            auth_manager.reload().await;
            auth_manager
                .auth()
                .await
                .ok_or_else(|| anyhow::anyhow!(missing_auth_error))?
        }
    };

    Ok((auth_manager, auth))
}

#[cfg(test)]
#[path = "config_tests.rs"]
mod tests;
