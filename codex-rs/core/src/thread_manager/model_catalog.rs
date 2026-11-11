//! Keep each catalog bound to its provider and auth owner while preserving host cache policy.

use crate::config::Config;
use crate::runtime_auth::RuntimeAuthSlot;
use crate::runtime_auth::runtime_for_config;
use codex_features::Feature;
use codex_features::RuntimeAuthMode;
use codex_login::AuthManager;
use codex_model_provider::ModelCatalogFactory;
use codex_model_provider::ProviderAuthContext;
use codex_model_provider_info::ModelProviderInfo;
use codex_models_manager::manager::SharedModelsManager;
use codex_protocol::openai_models::ModelsResponse;
use codex_protocol::protocol::SessionSource;
use codex_runtime_agent_instance::RuntimeAuthManager;
use std::collections::BTreeMap;
use std::sync::Arc;

/// The startup catalog and the credential/runtime owner used to construct it.
pub struct StartupModels {
    pub(super) credentials: Arc<AuthManager>,
    pub(super) runtime_auth: RuntimeAuthSlot,
    pub(super) installation_id: String,
    pub(super) catalog: ThreadModelCatalog,
}

impl StartupModels {
    /// Share the startup catalog with host discovery and prefetch callers.
    pub fn models_manager(&self) -> SharedModelsManager {
        Arc::clone(&self.catalog.manager)
    }
}

/// Construct the catalog and runtime owner together using the host's catalog factory.
/// Use `ModelCatalogSettings` for standard provider catalogs with a disk, custom, or no cache.
/// Runtime registration remains lazy; this does not register an instance or generate a key.
pub fn build_startup_models(
    config: &Config,
    credentials: Arc<AuthManager>,
    installation_id: String,
    factory: Arc<dyn ModelCatalogFactory>,
) -> StartupModels {
    let runtime_auth = RuntimeAuthSlot::default();
    let runtime = runtime_for_config(
        &runtime_auth,
        config,
        Arc::clone(&credentials),
        &installation_id,
    );
    let catalog = ThreadModelCatalog::from_config(
        config,
        factory,
        ProviderAuthContext {
            credentials: Arc::clone(&credentials),
            runtime_auth: runtime.map(|runtime| (runtime, config.runtime_auth_mode)),
        },
    );
    StartupModels {
        credentials,
        runtime_auth,
        installation_id,
        catalog,
    }
}

pub(super) struct ThreadModelCatalog {
    pub(super) manager: SharedModelsManager,
    provider: ModelProviderInfo,
    provider_id: String,
    provider_definitions: BTreeMap<String, ModelProviderInfo>,
    catalog: Option<ModelsResponse>,
    factory: Arc<dyn ModelCatalogFactory>,
    api_key_model_discovery_default: bool,
    runtime_mode: Option<RuntimeAuthMode>,
}

impl ThreadModelCatalog {
    fn from_config(
        config: &Config,
        factory: Arc<dyn ModelCatalogFactory>,
        auth: ProviderAuthContext,
    ) -> Self {
        let manager = super::build_models_manager_with_factory(
            config,
            config.model_catalog.clone(),
            factory.as_ref(),
            auth.clone(),
        );
        Self {
            manager,
            provider: config.model_provider.clone(),
            provider_id: config.model_provider_id.clone(),
            provider_definitions: super::models_manager_provider_definitions(config),
            catalog: config.model_catalog.clone(),
            factory,
            api_key_model_discovery_default: config.features.enabled(Feature::ApiKeyModelDiscovery),
            runtime_mode: auth.runtime_auth.as_ref().map(|(_, mode)| *mode),
        }
    }
    pub(super) fn new(
        provider: ModelProviderInfo,
        catalog: Option<ModelsResponse>,
        factory: Arc<dyn ModelCatalogFactory>,
        api_key_model_discovery_default: bool,
        auth: ProviderAuthContext,
    ) -> Self {
        let runtime_mode = auth.runtime_auth.as_ref().map(|(_, mode)| *mode);
        let manager = factory.build(
            auth.model_provider(provider.clone()),
            catalog.clone(),
            api_key_model_discovery_default,
        );
        Self {
            manager,
            provider_id: codex_model_provider_info::OPENAI_PROVIDER_ID.to_string(),
            provider_definitions: BTreeMap::from([(
                codex_model_provider_info::OPENAI_PROVIDER_ID.to_string(),
                provider.clone(),
            )]),
            provider,
            catalog,
            factory,
            api_key_model_discovery_default,
            runtime_mode,
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) async fn for_thread(
        &self,
        config: &Config,
        credentials: Arc<AuthManager>,
        runtime: Option<Arc<RuntimeAuthManager>>,
        same_owner: bool,
        originator: &str,
        session_source: &SessionSource,
    ) -> SharedModelsManager {
        let models = config.model_catalog.as_ref().or_else(|| {
            self.catalog
                .as_ref()
                .filter(|_| self.matches_config_provider(config))
        });
        let auth = ProviderAuthContext {
            credentials,
            runtime_auth: runtime.map(|runtime| (runtime, config.runtime_auth_mode)),
        };
        let manager = self.for_config(config, models, auth, same_owner);
        let manager = manager.for_originator(originator).unwrap_or(manager);
        if session_source.is_non_root_agent() {
            // The final provider/auth/originator catalog may be new even when the base
            // manager was reused. Preserve loaded snapshots, including unsaved updates.
            manager
                .ensure_initialized(config.http_client_factory())
                .await;
        }
        manager
    }

    pub(super) fn for_config(
        &self,
        config: &Config,
        models: Option<&ModelsResponse>,
        auth: ProviderAuthContext,
        same_owner: bool,
    ) -> SharedModelsManager {
        let api_key_model_discovery_default =
            config.features.enabled(Feature::ApiKeyModelDiscovery);
        if same_owner
            && self.runtime_mode == auth.runtime_auth.as_ref().map(|(_, mode)| *mode)
            && self.matches_config_provider(config)
            && self.catalog.as_ref() == models
            && self.api_key_model_discovery_default == api_key_model_discovery_default
        {
            return Arc::clone(&self.manager);
        }
        super::build_models_manager_with_factory(
            config,
            models.cloned(),
            self.factory.as_ref(),
            auth,
        )
    }

    fn matches_config_provider(&self, config: &Config) -> bool {
        self.provider_id == config.model_provider_id
            && self.provider_definitions == super::models_manager_provider_definitions(config)
            && self.matches_catalog_provider(&config.model_provider)
    }

    fn matches_catalog_provider(&self, provider: &ModelProviderInfo) -> bool {
        // Guardian overrides inference transport and retries without changing the catalog.
        // Keep all other provider fields, including routing and headers, in the comparison.
        self.provider
            == ModelProviderInfo {
                request_max_retries: self.provider.request_max_retries,
                stream_max_retries: self.provider.stream_max_retries,
                supports_websockets: self.provider.supports_websockets,
                ..provider.clone()
            }
    }

    pub(super) fn set_api_key_model_discovery_enabled(&self, enabled: bool) {
        self.factory.set_api_key_model_discovery_enabled(enabled);
    }
}
