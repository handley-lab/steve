use crate::config_server::config_server_http_client_factory;
use crate::config_source::BundleSource;
use crate::service::CloudConfigBundleService;
use codex_config::CloudConfigBundleLoader;
use codex_config::ConfigRequirementsToml;
use codex_http_client::HttpClientFactory;
use codex_login::AuthConfig;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::task::AbortHandle;
use tokio::task::JoinHandle;

pub fn managed_config_auth_manager(
    auth: AuthConfig,
    requirements: &ConfigRequirementsToml,
) -> std::io::Result<Option<codex_login::OAuthSessionManager>> {
    let Some(service_config) = requirements.config_server.as_ref() else {
        return Ok(None);
    };
    let factory = config_server_http_client_factory(service_config, &auth.auth_route_config);
    crate::config_server_auth_manager(service_config, auth.codex_home, &factory)
}

/// Existing grants are left to the loader's normal cache and refresh handling.
pub async fn ensure_config_server_login(
    auth: AuthConfig,
    requirements: &ConfigRequirementsToml,
) -> std::io::Result<()> {
    let Some(manager) = managed_config_auth_manager(auth, requirements)? else {
        return Ok(());
    };
    if let Err(error) = manager.authorization_id().await {
        if error
            .get_ref()
            .and_then(|source| source.downcast_ref::<codex_login::OAuthSessionError>())
            != Some(&codex_login::OAuthSessionError::LoginRequired)
        {
            return Err(error);
        }
        manager.login().await?;
    }
    Ok(())
}

/// Loads managed configuration from the configured policy server, if any.
pub fn managed_config_bundle_loader(
    codex_home: PathBuf,
    factory: HttpClientFactory,
    requirements: &ConfigRequirementsToml,
) -> CloudConfigBundleLoader {
    let Some(service_config) = requirements.config_server.as_ref() else {
        return CloudConfigBundleLoader::default();
    };
    let factory = config_server_http_client_factory(
        service_config,
        &codex_login::AuthRouteConfig::from_http_client_factory(factory),
    );
    crate::config_server_bundle_loader(service_config.clone(), codex_home, factory)
}

/// Loads managed configuration using the stored authentication settings.
pub fn managed_config_bundle_loader_for_storage(
    auth_config: AuthConfig,
    requirements: &ConfigRequirementsToml,
) -> CloudConfigBundleLoader {
    let Some(service_config) = requirements.config_server.as_ref() else {
        return CloudConfigBundleLoader::default();
    };
    let factory = config_server_http_client_factory(service_config, &auth_config.auth_route_config);
    crate::config_server_bundle_loader(service_config.clone(), auth_config.codex_home, factory)
}

struct CloudConfigBundleLoaderLifetime<C: BundleSource> {
    service: Arc<CloudConfigBundleService<C>>,
    refresh_task: AbortHandle,
}

impl<C: BundleSource> Drop for CloudConfigBundleLoaderLifetime<C> {
    fn drop(&mut self) {
        self.refresh_task.abort();
    }
}

pub(crate) fn managed_config_bundle_loader_for_service<C>(
    service: CloudConfigBundleService<C>,
) -> (CloudConfigBundleLoader, JoinHandle<()>)
where
    C: BundleSource + 'static,
{
    let service = Arc::new(service);
    let background_service = Arc::clone(&service);
    let refresh_task = tokio::spawn(async move {
        let _ = background_service.get_latest().await;
        background_service.refresh_cache_in_background().await;
    });
    let abort_handle = refresh_task.abort_handle();
    let lifetime = Arc::new(CloudConfigBundleLoaderLifetime {
        service,
        refresh_task: abort_handle,
    });

    let snapshot_lifetime = Arc::clone(&lifetime);
    let policy = lifetime.service.policy.clone();
    let loader = CloudConfigBundleLoader::from_getter(move || {
        let lifetime = Arc::clone(&lifetime);
        async move { lifetime.service.get_latest().await }
    })
    .with_ema_policy_snapshots(policy, move || {
        let lifetime = Arc::clone(&snapshot_lifetime);
        async move { lifetime.service.get_latest_snapshot().await }
    });
    (loader, refresh_task)
}

/// Fetches directly from the configured policy server on each load, without reading or
/// writing the disk cache or starting a background refresher.
pub fn managed_config_bundle_loader_for_storage_without_cache(
    auth_config: AuthConfig,
    requirements: &ConfigRequirementsToml,
) -> CloudConfigBundleLoader {
    let Some(service_config) = requirements.config_server.as_ref() else {
        return CloudConfigBundleLoader::default();
    };
    let factory = config_server_http_client_factory(service_config, &auth_config.auth_route_config);
    crate::config_server::load_config_server_bundle(
        service_config.clone(),
        auth_config.codex_home,
        factory,
        /*persistent_cache*/ false,
    )
}
