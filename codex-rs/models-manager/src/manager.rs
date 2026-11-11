use super::cache::FileModelsCache;
use crate::ModelCatalogOriginator;
use crate::cache::ModelsCache;
use crate::cache::ModelsCacheEntry;
use crate::catalog::CatalogModel;
use crate::catalog::CatalogState;
use crate::catalog::ModelOrigin;
use crate::catalog::SelectedCatalog;
use crate::collaboration_mode_presets::builtin_collaboration_mode_presets;
use crate::config::ModelsManagerConfig;
use crate::model_info;
use chrono::DateTime;
use chrono::Utc;
use codex_http_client::HttpClientFactory;
use codex_login::AuthManager;
use codex_protocol::config_types::CollaborationModeMask;
use codex_protocol::error::CodexErrorDetails;
use codex_protocol::error::Result as CoreResult;
use codex_protocol::openai_models::ModelInfo;
use codex_protocol::openai_models::ModelPreset;
use codex_protocol::openai_models::ModelsResponse;
use std::borrow::Cow;
use std::collections::HashMap;
use std::fmt;
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;
use tokio::sync::MutexGuard;
use tokio::sync::RwLock;
use tokio::sync::TryLockError;
use tokio::sync::watch;
use tracing::Instrument as _;
use tracing::error;
use tracing::info;

const MODEL_CACHE_FILE: &str = "models_cache.json";
const DEFAULT_MODEL_CACHE_TTL: Duration = Duration::from_secs(300);

/// Remote endpoint used by the OpenAI-compatible model manager.
///
/// Implementations own provider-specific auth and transport details. The model
/// manager owns refresh policy, cache behavior, and catalog merging; it calls
/// this endpoint only when it decides a remote refresh should happen.
pub trait ModelsEndpointClient: fmt::Debug + Send + Sync {
    /// Return an endpoint whose requests and identity use this catalog originator.
    fn for_originator(
        &self,
        _originator: ModelCatalogOriginator,
    ) -> Option<Arc<dyn ModelsEndpointClient>> {
        None
    }

    /// Opaque identity of the current provider and credentials, without resolving auth.
    ///
    /// Must distinguish provider routing and authenticated principals within the
    /// manager and cache's caller-established scope.
    /// Return `None` when identity is unavailable; cached catalog reuse is then disabled.
    fn identity(&self) -> Option<String>;

    /// Prepare current credential identity before catalog cache comparisons.
    /// This must not fetch a catalog or start browser authorization.
    fn prepare_catalog_identity(&self) -> ModelsEndpointFuture<'_, CoreResult<()>> {
        Box::pin(std::future::ready(Ok(())))
    }

    /// Whether this custom provider can serve a Codex catalog using its gateway credentials.
    fn supports_gateway_catalog(&self) -> bool {
        false
    }

    /// Fetches the latest remote model catalog and optional ETag.
    fn list_models<'a>(
        &'a self,
        client_version: &'a str,
        http_client_factory: HttpClientFactory,
    ) -> ModelsEndpointFuture<'a, CoreResult<ModelsEndpointResponse>>;
}

/// Catalog and validation metadata captured using the credentials for one request.
#[derive(Debug)]
pub struct ModelsEndpointResponse {
    pub models: Vec<ModelInfo>,
    pub etag: Option<String>,
    /// Identity of the credentials actually used to fetch this catalog.
    /// Successful responses must always identify their provider and authentication scope.
    pub identity: String,
}

pub type ModelsEndpointFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Strategy for refreshing available models.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefreshStrategy {
    /// Use the current in-memory catalog without loading cache or fetching from the network.
    InMemory,
    /// Always fetch from the network, ignoring cache.
    Online,
    /// Only use cached data, never fetch from the network.
    Offline,
    /// Use cache if available and fresh, otherwise fetch from the network.
    OnlineIfUncached,
}

impl RefreshStrategy {
    const fn as_str(self) -> &'static str {
        match self {
            Self::InMemory => "in_memory",
            Self::Online => "online",
            Self::Offline => "offline",
            Self::OnlineIfUncached => "online_if_uncached",
        }
    }
}

impl fmt::Display for RefreshStrategy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Initialization preserves a loaded snapshot; explicit refreshes follow their requested policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CatalogLoadMode {
    Initialize,
    Refresh(RefreshStrategy),
}

type SharedModelsEndpointClient = Arc<dyn ModelsEndpointClient>;

/// A successful in-process provider response, before bundled fallback is merged in.
#[derive(Debug)]
pub struct RefreshedModelCatalog {
    identity: String,
    models: Arc<[ModelInfo]>,
}

/// Coordinates model discovery plus cached metadata on disk.
pub trait ModelsManager: fmt::Debug + Send + Sync {
    /// Fetch discovery without turning an endpoint failure into cached availability.
    fn model_catalog_result(
        &self,
        refresh_strategy: RefreshStrategy,
        http_client_factory: HttpClientFactory,
    ) -> ModelsManagerFuture<'_, CoreResult<ModelListing>>;
    /// Initialize a newly selected catalog without replacing an already-loaded snapshot.
    /// Stateful managers must check readiness for their current provider and credentials;
    /// a successful empty catalog is initialized too. Static catalogs need no work.
    fn ensure_initialized(
        &self,
        http_client_factory: HttpClientFactory,
    ) -> ModelsManagerFuture<'_, ()> {
        Box::pin(async move {
            self.list_models(RefreshStrategy::OnlineIfUncached, http_client_factory)
                .await;
        })
    }

    /// Observe successful provider refreshes and invalidations, never cache-only reads.
    fn model_catalog_updates(&self) -> Option<watch::Receiver<Option<Arc<RefreshedModelCatalog>>>> {
        None
    }

    /// Latest successful provider response, only while its credentials are still current.
    fn refreshed_model_catalog(&self) -> Option<Arc<[ModelInfo]>> {
        None
    }

    /// Refresh already-created originator catalogs using their own endpoint and credentials.
    fn refresh_originator_catalogs(
        &self,
        _http_client_factory: HttpClientFactory,
    ) -> ModelsManagerFuture<'_, ()> {
        Box::pin(std::future::ready(()))
    }

    /// Select an isolated catalog for a supported thread originator. `None` retains this manager.
    fn for_originator(&self, _originator: &str) -> Option<SharedModelsManager> {
        None
    }

    /// Supply remote model discovery policy.
    /// Static catalogs ignore this setting.
    fn set_api_key_model_discovery_enabled(&self, _enabled: bool) {}

    /// List all available models, refreshing according to the specified strategy.
    ///
    /// Returns model presets sorted by priority with catalog visibility.
    fn list_models(
        &self,
        refresh_strategy: RefreshStrategy,
        http_client_factory: HttpClientFactory,
    ) -> ModelsManagerFuture<'_, Vec<ModelPreset>> {
        Box::pin(
            async move {
                let catalog = self
                    .raw_model_catalog(refresh_strategy, http_client_factory)
                    .await;
                self.build_available_models(catalog.models)
            }
            .instrument(tracing::info_span!(
                "list_models",
                refresh_strategy = %refresh_strategy
            )),
        )
    }

    /// Project model metadata from the source-aware read, using its refresh and fallback policy.
    fn raw_model_catalog(
        &self,
        refresh_strategy: RefreshStrategy,
        http_client_factory: HttpClientFactory,
    ) -> ModelsManagerFuture<'_, ModelsResponse> {
        Box::pin(async move {
            ModelsResponse {
                models: self
                    .model_catalog_with_origins(refresh_strategy, http_client_factory)
                    .await
                    .into_iter()
                    .map(|entry| Arc::unwrap_or_clone(entry.model))
                    .collect(),
            }
        })
    }

    /// Return model metadata and its locally determined source from the same snapshot.
    /// Implement refresh, error handling, and fallback here for both catalog views.
    fn model_catalog_with_origins(
        &self,
        refresh_strategy: RefreshStrategy,
        http_client_factory: HttpClientFactory,
    ) -> ModelsManagerFuture<'_, Vec<CatalogModel>>;

    /// Best-effort refresh when the in-memory catalog belongs to different credentials.
    /// Static catalogs need no refresh. Failures follow the catalog's fallback policy.
    fn refresh_after_auth_change(
        &self,
        _http_client_factory: HttpClientFactory,
    ) -> ModelsManagerFuture<'_, ()> {
        Box::pin(std::future::ready(()))
    }

    /// Return the current in-memory remote model catalog without refreshing or loading cache state.
    fn get_remote_models(&self) -> ModelsManagerFuture<'_, Vec<ModelInfo>>;

    /// Attempt to return the current in-memory remote model catalog without blocking.
    ///
    /// Returns an error if the internal lock cannot be acquired.
    fn try_get_remote_models(&self) -> Result<Vec<ModelInfo>, TryLockError>;

    /// Return the associated auth manager.
    fn auth_manager(&self) -> Option<&AuthManager>;

    /// Build picker-ready presets from the active catalog snapshot.
    fn build_available_models(&self, mut remote_models: Vec<ModelInfo>) -> Vec<ModelPreset> {
        remote_models.sort_by_key(|model| model.priority);

        let mut presets: Vec<ModelPreset> = remote_models.into_iter().map(Into::into).collect();
        ModelPreset::mark_default_by_picker_visibility(&mut presets);

        presets
    }

    /// List collaboration mode presets.
    ///
    /// Returns a static set of presets seeded with the configured model.
    fn list_collaboration_modes(&self) -> Vec<CollaborationModeMask>;

    /// Attempt to list models without blocking, using the current cached state.
    ///
    /// Returns an error if the internal lock cannot be acquired.
    fn try_list_models(&self) -> Result<Vec<ModelPreset>, TryLockError> {
        let remote_models = self.try_get_remote_models()?;
        Ok(self.build_available_models(remote_models))
    }

    // todo(aibrahim): should be visible to core only and sent on session_configured event
    /// Get the model identifier to use, refreshing according to the specified strategy.
    ///
    /// Preserve the explicitly supplied model; an empty result leaves missing-model
    /// reporting to the caller without selecting a catalog default.
    fn get_default_model<'a>(
        &'a self,
        model: &'a Option<String>,
        allow_provider_model_fallback: bool,
        refresh_strategy: RefreshStrategy,
        _http_client_factory: HttpClientFactory,
    ) -> ModelsManagerFuture<'a, String> {
        Box::pin(
            async move { model.clone().unwrap_or_default() }.instrument(tracing::info_span!(
                "get_default_model",
                model.provided = model.is_some(),
                allow_provider_model_fallback,
                refresh_strategy = %refresh_strategy
            )),
        )
    }

    // todo(aibrahim): look if we can tighten it to pub(crate)
    /// Resolve model metadata and its source, preserving provenance through config adjustments.
    fn get_model_info<'a>(
        &'a self,
        _provider_id: &'a str,
        model: &'a str,
        config: &'a ModelsManagerConfig,
    ) -> ModelsManagerFuture<'a, CoreResult<CatalogModel>>;

    /// Refresh models if the provided ETag differs from the cached ETag.
    ///
    /// Uses `Online` strategy to fetch latest models when ETags differ.
    fn refresh_if_new_etag(
        &self,
        provider_id: &str,
        etag: String,
        http_client_factory: HttpClientFactory,
    ) -> ModelsManagerFuture<'_, ()>;
}

pub type ModelsManagerFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Shared model manager handle used across runtime services.
pub type SharedModelsManager = Arc<dyn ModelsManager>;

#[derive(Debug, Clone, Default)]
pub struct ModelListing {
    pub models: Vec<ModelInfo>,
    pub provider_errors: Vec<ModelProviderError>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct ModelProviderError {
    pub provider: String,
    pub message: String,
}

/// Explicit catalogs serialize refresh. Auth and discovery
/// settings are still evaluated when models are requested.
#[derive(Debug)]
enum CatalogSource {
    Default,
    ExplicitProvider(Mutex<()>),
}

impl CatalogSource {
    async fn refresh_lock(&self) -> Option<MutexGuard<'_, ()>> {
        match self {
            Self::Default => None,
            Self::ExplicitProvider(lock) => Some(lock.lock().await),
        }
    }
}

/// Cache policy and live feature state shared by catalogs with different request credentials.
#[derive(Clone, Debug)]
pub struct ModelCatalogSettings {
    cache: Option<Arc<dyn ModelsCache>>,
    pub api_key_model_discovery_default: bool,
    // Before the host publishes a live change, each thread uses its configured default.
    live_discovery: Arc<std::sync::Mutex<Option<bool>>>,
}

impl Default for ModelCatalogSettings {
    fn default() -> Self {
        Self {
            cache: None,
            api_key_model_discovery_default: true,
            live_discovery: Arc::new(std::sync::Mutex::new(None)),
        }
    }
}

impl ModelCatalogSettings {
    pub fn disk(codex_home: PathBuf) -> Self {
        Self::with_cache(Arc::new(FileModelsCache::new(
            codex_home.join(MODEL_CACHE_FILE),
            DEFAULT_MODEL_CACHE_TTL,
        )))
    }

    pub fn with_cache(cache: Arc<dyn ModelsCache>) -> Self {
        Self {
            cache: Some(cache),
            ..Self::default()
        }
    }

    pub fn set_api_key_model_discovery_enabled(&self, enabled: bool) {
        *self
            .live_discovery
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(enabled);
    }

    fn api_key_model_discovery_enabled(&self) -> bool {
        self.live_discovery
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .unwrap_or(self.api_key_model_discovery_default)
    }
}

/// OpenAI-compatible model manager backed by cache and the configured `/models` endpoint.
#[derive(Debug)]
pub struct OpenAiModelsManager {
    originator_managers: std::sync::Mutex<HashMap<ModelCatalogOriginator, SharedModelsManager>>,
    remote_models: RwLock<CatalogState>,
    catalog_updates: watch::Sender<Option<Arc<RefreshedModelCatalog>>>,
    settings: ModelCatalogSettings,
    endpoint_client: SharedModelsEndpointClient,
    catalog_source: CatalogSource,
    auth_manager: Option<Arc<AuthManager>>,
}

/// Static model manager backed by an authoritative in-process catalog.
#[derive(Debug)]
pub struct StaticModelsManager {
    remote_models: Vec<ModelInfo>,
    auth_manager: Option<Arc<AuthManager>>,
    origin: ModelOrigin,
}

impl OpenAiModelsManager {
    /// Construct an OpenAI-compatible remote model manager.
    pub fn new(
        codex_home: PathBuf,
        endpoint_client: Arc<dyn ModelsEndpointClient>,
        auth_manager: Option<Arc<AuthManager>>,
    ) -> Self {
        Self::with_settings(
            ModelCatalogSettings::disk(codex_home),
            endpoint_client,
            auth_manager,
        )
    }

    /// Construct an OpenAI-compatible model manager with caching disabled.
    pub fn new_without_cache(
        endpoint_client: Arc<dyn ModelsEndpointClient>,
        auth_manager: Option<Arc<AuthManager>>,
    ) -> Self {
        Self::with_settings(
            ModelCatalogSettings::default(),
            endpoint_client,
            auth_manager,
        )
    }

    /// Constructs an OpenAI-compatible model manager with a caller-provided cache.
    ///
    /// The cache is consulted by cache-aware refresh strategies. Cache misses and backend errors
    /// fall back to the models endpoint, and cache write failures do not fail model discovery.
    pub fn new_with_cache(
        cache: Arc<dyn ModelsCache>,
        endpoint_client: Arc<dyn ModelsEndpointClient>,
        auth_manager: Option<Arc<AuthManager>>,
    ) -> Self {
        Self::with_settings(
            ModelCatalogSettings::with_cache(cache),
            endpoint_client,
            auth_manager,
        )
    }

    pub fn with_settings(
        settings: ModelCatalogSettings,
        endpoint_client: Arc<dyn ModelsEndpointClient>,
        auth_manager: Option<Arc<AuthManager>>,
    ) -> Self {
        Self {
            originator_managers: std::sync::Mutex::new(HashMap::new()),
            catalog_updates: watch::Sender::new(None),
            remote_models: RwLock::new(CatalogState::bundled(ModelsCacheEntry {
                fetched_at: Utc::now(),
                etag: None,
                client_version: Some(crate::client_version_to_whole()),
                identity: endpoint_client.identity(),
                models: Vec::new(),
            })),
            settings,
            endpoint_client,
            catalog_source: CatalogSource::Default,
            auth_manager,
        }
    }

    /// Configure a newly constructed manager to use only the provider's explicit catalog.
    /// The existing auth identity must be retained.
    pub fn with_provider_catalog(mut self) -> Self {
        self.catalog_source = CatalogSource::ExplicitProvider(Mutex::new(()));
        self.remote_models.get_mut().entry.models.clear();
        self
    }
}

impl StaticModelsManager {
    /// Construct a static model manager with the source of its supplied catalog.
    pub fn new(
        auth_manager: Option<Arc<AuthManager>>,
        model_catalog: ModelsResponse,
        origin: ModelOrigin,
    ) -> Self {
        Self {
            remote_models: model_catalog.models,
            auth_manager,
            origin,
        }
    }
}

impl ModelsManager for OpenAiModelsManager {
    fn model_catalog_result(
        &self,
        refresh_strategy: RefreshStrategy,
        http_client_factory: HttpClientFactory,
    ) -> ModelsManagerFuture<'_, CoreResult<ModelListing>> {
        Box::pin(async move {
            if self.model_discovery_disabled() {
                return Err(codex_protocol::error::CodexErr::InvalidRequest(
                    "model discovery is disabled".to_string(),
                ));
            }
            self.refresh_available_models(refresh_strategy, &http_client_factory)
                .await?;
            Ok(ModelListing {
                models: self.get_remote_models().await,
                provider_errors: Vec::new(),
            })
        })
    }

    fn ensure_initialized(
        &self,
        http_client_factory: HttpClientFactory,
    ) -> ModelsManagerFuture<'_, ()> {
        Box::pin(async move {
            // Resolve lazy credentials before checking the selected catalog's identity.
            if self
                .endpoint_client
                .prepare_catalog_identity()
                .await
                .is_err()
            {
                return;
            }
            if self.catalog_is_initialized(&*self.remote_models.read().await) {
                return;
            }
            if let Err(err) = self
                .load_catalog(CatalogLoadMode::Initialize, &http_client_factory)
                .await
            {
                error!("failed to initialize model catalog: {err}");
            }
        })
    }

    fn model_catalog_updates(&self) -> Option<watch::Receiver<Option<Arc<RefreshedModelCatalog>>>> {
        Some(self.catalog_updates.subscribe())
    }

    fn refreshed_model_catalog(&self) -> Option<Arc<[ModelInfo]>> {
        let catalog = self.catalog_updates.borrow();
        let catalog = catalog.as_ref()?;
        if self.model_discovery_disabled()
            || self.endpoint_client.identity().as_ref() != Some(&catalog.identity)
        {
            return None;
        }
        Some(Arc::clone(&catalog.models))
    }

    fn refresh_originator_catalogs(
        &self,
        http_client_factory: HttpClientFactory,
    ) -> ModelsManagerFuture<'_, ()> {
        let managers: Vec<_> = self
            .originator_managers
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .values()
            .cloned()
            .collect();
        Box::pin(async move {
            for manager in managers {
                manager
                    .list_models(RefreshStrategy::Online, http_client_factory.clone())
                    .await;
            }
        })
    }

    fn model_catalog_with_origins(
        &self,
        refresh_strategy: RefreshStrategy,
        http_client_factory: HttpClientFactory,
    ) -> ModelsManagerFuture<'_, Vec<CatalogModel>> {
        Box::pin(async move {
            if refresh_strategy != RefreshStrategy::InMemory
                && let Err(err) = self
                    .refresh_available_models(refresh_strategy, &http_client_factory)
                    .await
            {
                error!("failed to refresh available models: {err}");
            }
            let state = self.remote_models.read().await;
            state.model_catalog(self.models_for_entry(&state.entry))
        })
    }

    fn for_originator(&self, originator: &str) -> Option<SharedModelsManager> {
        let originator = ModelCatalogOriginator::from_originator(originator)?;
        let mut managers = self
            .originator_managers
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(manager) = managers.get(&originator) {
            return Some(Arc::clone(manager));
        }
        let endpoint = self.endpoint_client.for_originator(originator)?;
        // A backend that cannot partition must not overwrite the default catalog.
        let mut settings = self.settings.clone();
        settings.cache = settings
            .cache
            .as_ref()
            .and_then(|cache| cache.for_originator(originator));
        let mut manager = Self::with_settings(settings, endpoint, self.auth_manager.clone());
        if matches!(self.catalog_source, CatalogSource::ExplicitProvider(_)) {
            manager = manager.with_provider_catalog();
        }
        let manager: SharedModelsManager = Arc::new(manager);
        managers.insert(originator, Arc::clone(&manager));
        Some(manager)
    }

    fn set_api_key_model_discovery_enabled(&self, enabled: bool) {
        self.settings.set_api_key_model_discovery_enabled(enabled);
    }

    fn refresh_after_auth_change(
        &self,
        http_client_factory: HttpClientFactory,
    ) -> ModelsManagerFuture<'_, ()> {
        Box::pin(async move {
            let refresh = async {
                // Resolve current credentials before comparing catalog identities.
                if self.model_discovery_disabled() {
                    return Ok(());
                }
                self.endpoint_client.prepare_catalog_identity().await?;
                let identity = self.endpoint_client.identity();
                if identity.is_some() && self.remote_models.read().await.entry.identity == identity
                {
                    return Ok(());
                }
                self.refresh_available_models(
                    RefreshStrategy::OnlineIfUncached,
                    &http_client_factory,
                )
                .await
            };
            // Include auth resolution and cache access in the best-effort deadline.
            if !matches!(
                tokio::time::timeout(Duration::from_secs(/*secs*/ 5), refresh).await,
                Ok(Ok(()))
            ) {
                tracing::warn!("model catalog refresh after auth change failed or timed out");
            }
        })
    }

    fn get_remote_models(&self) -> ModelsManagerFuture<'_, Vec<ModelInfo>> {
        Box::pin(async move {
            let entry = self.remote_models.read().await;
            self.models_for_entry(&entry.entry)
                .into_models()
                .into_owned()
        })
    }

    fn try_get_remote_models(&self) -> Result<Vec<ModelInfo>, TryLockError> {
        let entry = self.remote_models.try_read()?;
        Ok(self
            .models_for_entry(&entry.entry)
            .into_models()
            .into_owned())
    }

    fn auth_manager(&self) -> Option<&AuthManager> {
        self.auth_manager.as_deref()
    }

    #[tracing::instrument(skip_all, fields(model = model))]
    fn get_model_info<'a>(
        &'a self,
        _provider_id: &'a str,
        model: &'a str,
        config: &'a ModelsManagerConfig,
    ) -> ModelsManagerFuture<'a, CoreResult<CatalogModel>> {
        Box::pin(async move {
            let remote = {
                let entry = self.remote_models.read().await;
                let selected = self.models_for_entry(&entry.entry);
                let models = match &selected {
                    SelectedCatalog::CurrentSnapshot(models)
                    | SelectedCatalog::Fallback(models) => models,
                };
                let model = match &self.catalog_source {
                    CatalogSource::Default => find_model_from_candidates(model, models),
                    CatalogSource::ExplicitProvider(_) => models
                        .iter()
                        .find(|candidate| candidate.slug == model)
                        .cloned(),
                };
                model.map(|model| CatalogModel {
                    origin: entry.model_origin(&selected, &model.slug),
                    model: model.into(),
                })
            };
            Ok(construct_model_info(model, remote, config))
        })
    }

    fn list_collaboration_modes(&self) -> Vec<CollaborationModeMask> {
        builtin_collaboration_mode_presets()
    }

    fn refresh_if_new_etag(
        &self,
        _provider_id: &str,
        etag: String,
        http_client_factory: HttpClientFactory,
    ) -> ModelsManagerFuture<'_, ()> {
        Box::pin(OpenAiModelsManager::refresh_if_new_etag(
            self,
            etag,
            http_client_factory,
        ))
    }
}

impl OpenAiModelsManager {
    fn catalog_is_initialized(&self, catalog: &CatalogState) -> bool {
        catalog.initialized
            && catalog.entry.identity.is_some()
            && catalog.entry.identity == self.endpoint_client.identity()
    }

    /// Selects the catalog available under the current provider and auth identity.
    fn models_for_entry<'a>(&self, entry: &'a ModelsCacheEntry) -> SelectedCatalog<'a> {
        if !self.model_discovery_disabled()
            && entry.identity.is_some()
            && entry.identity == self.endpoint_client.identity()
        {
            SelectedCatalog::CurrentSnapshot(Cow::Borrowed(&entry.models))
        } else {
            SelectedCatalog::Fallback(Cow::Borrowed(&[]))
        }
    }

    async fn refresh_if_new_etag(&self, etag: String, http_client_factory: HttpClientFactory) {
        if self.model_discovery_disabled() {
            return;
        }
        let refresh_guard = self.catalog_source.refresh_lock().await;
        if let Err(err) = self.endpoint_client.prepare_catalog_identity().await {
            error!("failed to prepare model catalog identity: {err}");
            return;
        }
        let (identity, current_etag) = {
            let entry = self.remote_models.read().await;
            (entry.entry.identity.clone(), entry.entry.etag.clone())
        };
        if let Some(identity) = identity
            && Some(&identity) == self.endpoint_client.identity().as_ref()
            && current_etag.as_deref() == Some(etag.as_str())
        {
            if let Some(cache) = self.settings.cache.as_ref()
                && let Err(err) = cache
                    .refresh_ttl(&crate::client_version_to_whole(), &identity, &etag)
                    .await
            {
                error!("failed to renew cache TTL: {err}");
            }
            return;
        }
        drop(refresh_guard);
        if let Err(err) = self
            .refresh_available_models(RefreshStrategy::Online, &http_client_factory)
            .await
        {
            error!("failed to refresh available models: {err}");
        }
    }

    /// Refresh available models according to the specified strategy.
    async fn refresh_available_models(
        &self,
        refresh_strategy: RefreshStrategy,
        http_client_factory: &HttpClientFactory,
    ) -> CoreResult<()> {
        self.load_catalog(
            CatalogLoadMode::Refresh(refresh_strategy),
            http_client_factory,
        )
        .await
    }

    async fn load_catalog(
        &self,
        mode: CatalogLoadMode,
        http_client_factory: &HttpClientFactory,
    ) -> CoreResult<()> {
        let _refresh_guard = self.catalog_source.refresh_lock().await;
        if self.model_discovery_disabled() {
            return Ok(());
        }
        if mode == CatalogLoadMode::Refresh(RefreshStrategy::InMemory) {
            return Ok(());
        }
        self.endpoint_client.prepare_catalog_identity().await?;
        if mode == CatalogLoadMode::Initialize
            && self.catalog_is_initialized(&*self.remote_models.read().await)
        {
            return Ok(());
        }
        match mode {
            CatalogLoadMode::Refresh(RefreshStrategy::InMemory) => Ok(()),
            CatalogLoadMode::Refresh(RefreshStrategy::Offline) => {
                self.try_load_cache(mode).await;
                Ok(())
            }
            CatalogLoadMode::Initialize
            | CatalogLoadMode::Refresh(RefreshStrategy::OnlineIfUncached) => {
                if self.try_load_cache(mode).await {
                    return Ok(());
                }
                self.fetch_and_update_models(http_client_factory, mode)
                    .await
            }
            CatalogLoadMode::Refresh(RefreshStrategy::Online) => {
                self.fetch_and_update_models(http_client_factory, mode)
                    .await
            }
        }
    }

    async fn fetch_and_update_models(
        &self,
        http_client_factory: &HttpClientFactory,
        mode: CatalogLoadMode,
    ) -> CoreResult<()> {
        let client_version = crate::client_version_to_whole();
        let request_identity = self.endpoint_client.identity();
        let ModelsEndpointResponse {
            models,
            etag,
            identity,
        } = match self
            .endpoint_client
            .list_models(&client_version, http_client_factory.clone())
            .await
        {
            Ok(response) => response,
            Err(error) => {
                // A failed fetch is not an authoritative empty catalog. Keep the last
                // successful snapshot (and its original freshness) unless access was
                // explicitly rejected. Provider/auth identity checks still govern reuse.
                let is_provider_catalog =
                    matches!(&self.catalog_source, CatalogSource::ExplicitProvider(_));
                let access_denied = matches!(error.http_status_code_value(), Some(401 | 403));
                let credentials_rejected = matches!(
                    error.details(),
                    CodexErrorDetails::RefreshTokenFailed(_)
                        | CodexErrorDetails::GatewayLoginRequired(_)
                );

                if is_provider_catalog && (access_denied || credentials_rejected) {
                    {
                        let mut current = self.remote_models.write().await;
                        let CatalogState {
                            entry: current,
                            provider_model_ids,
                            initialized,
                        } = &mut *current;
                        if request_identity != self.endpoint_client.identity() {
                            return Err(error);
                        }
                        // Keep this failure in memory even if persistent invalidation fails.
                        current.fetched_at = DateTime::<Utc>::MIN_UTC;
                        current.models.clear();
                        provider_model_ids.clear();
                        current.etag = None;
                        current.identity.clone_from(&request_identity);
                        *initialized = true;
                        self.catalog_updates.send_replace(None);
                    }
                    if let Some(identity) = request_identity
                        && let Some(cache) = self.settings.cache.as_ref()
                    {
                        let invalidation = async {
                            if let Some(mut entry) = cache.load(&client_version).await?
                                && entry.identity.as_deref() == Some(&identity)
                                && entry.client_version.as_deref() == Some(&client_version)
                            {
                                // Preserve backend lookup keys, but force a cache miss after restart.
                                entry.fetched_at = DateTime::<Utc>::MIN_UTC;
                                entry.etag = None;
                                entry.models.clear();
                                cache.store(&entry).await?;
                            }
                            Ok::<_, crate::cache::ModelsCacheError>(())
                        };
                        if let Err(err) = invalidation.await {
                            error!("failed to invalidate models cache: {err}");
                        }
                    }
                }
                return Err(error);
            }
        };
        if Some(&identity) != self.endpoint_client.identity().as_ref()
            || (mode == CatalogLoadMode::Initialize
                && self.catalog_is_initialized(&*self.remote_models.read().await))
        {
            return Ok(());
        }
        let refreshed = Arc::new(RefreshedModelCatalog {
            identity: identity.clone(),
            models: models.clone().into(),
        });
        let entry = ModelsCacheEntry {
            fetched_at: Utc::now(),
            etag,
            client_version: Some(client_version),
            identity: Some(identity),
            models,
        };
        if let Some(cache) = self.settings.cache.as_ref()
            && let Err(err) = cache.store(&entry).await
        {
            error!("failed to write models cache: {err}");
        }
        let mut current = self.remote_models.write().await;
        if self.apply_remote_models(&mut current, entry, mode) {
            self.catalog_updates.send_replace(Some(refreshed));
        }
        Ok(())
    }

    fn model_discovery_disabled(&self) -> bool {
        !self.settings.api_key_model_discovery_enabled()
    }

    /// Publish only while the request identity still matches, including after async storage.
    fn apply_remote_models(
        &self,
        current: &mut CatalogState,
        entry: ModelsCacheEntry,
        mode: CatalogLoadMode,
    ) -> bool {
        // A refresh may finish while initialization awaits cache or network I/O.
        // Check under the write lock so initialization cannot replace its loaded snapshot.
        if entry.identity != self.endpoint_client.identity()
            || (mode == CatalogLoadMode::Initialize && self.catalog_is_initialized(current))
        {
            return false;
        }
        *current = CatalogState::provider(entry);
        true
    }

    /// Attempt to satisfy the refresh from the cache when it matches the provider and TTL.
    async fn try_load_cache(&self, mode: CatalogLoadMode) -> bool {
        let Some(cache) = self.settings.cache.as_ref() else {
            return false;
        };
        let _timer =
            codex_otel::start_global_timer("codex.remote_models.load_cache.duration_ms", &[]);
        let client_version = crate::client_version_to_whole();
        info!(client_version, "models cache: evaluating cache eligibility");
        let Some(identity) = self.endpoint_client.identity() else {
            return false;
        };
        {
            let current = self.remote_models.read().await;
            // An access rejection must not be replaced by stale cache data if invalidation failed.
            if current.entry.fetched_at == DateTime::<Utc>::MIN_UTC
                && current.entry.identity.as_ref() == Some(&identity)
            {
                return false;
            }
        }
        let cache_entry = match cache.load(&client_version).await {
            Ok(Some(cache_entry)) => cache_entry,
            Ok(None) => {
                info!("models cache: no usable cache entry");
                return false;
            }
            Err(err) => {
                error!("failed to load models cache: {err}");
                return false;
            }
        };
        if cache_entry.fetched_at == DateTime::<Utc>::MIN_UTC {
            return false;
        }
        if cache_entry.client_version.as_deref() != Some(client_version.as_str()) {
            info!(
                expected_version = client_version,
                cached_version = ?cache_entry.client_version,
                "models cache: cache version mismatch"
            );
            return false;
        }
        if cache_entry.identity.as_ref() != Some(&identity)
            || self.endpoint_client.identity().as_ref() != Some(&identity)
        {
            info!("models cache: provider or auth identity mismatch");
            return false;
        }
        let mut current = self.remote_models.write().await;
        (mode == CatalogLoadMode::Initialize && self.catalog_is_initialized(&current))
            || self.apply_remote_models(&mut current, cache_entry, mode)
    }
}

impl ModelsManager for StaticModelsManager {
    fn model_catalog_result(
        &self,
        _refresh_strategy: RefreshStrategy,
        _http_client_factory: HttpClientFactory,
    ) -> ModelsManagerFuture<'_, CoreResult<ModelListing>> {
        Box::pin(async move {
            Ok(ModelListing {
                models: self.remote_models.clone(),
                provider_errors: Vec::new(),
            })
        })
    }

    fn ensure_initialized(
        &self,
        _http_client_factory: HttpClientFactory,
    ) -> ModelsManagerFuture<'_, ()> {
        Box::pin(std::future::ready(()))
    }

    fn model_catalog_with_origins(
        &self,
        _refresh_strategy: RefreshStrategy,
        _http_client_factory: HttpClientFactory,
    ) -> ModelsManagerFuture<'_, Vec<CatalogModel>> {
        Box::pin(async move {
            self.remote_models
                .iter()
                .cloned()
                .map(|model| CatalogModel {
                    model: model.into(),
                    origin: self.origin,
                })
                .collect()
        })
    }
    #[tracing::instrument(skip_all, fields(model = model))]
    fn get_model_info<'a>(
        &'a self,
        _provider_id: &'a str,
        model: &'a str,
        config: &'a ModelsManagerConfig,
    ) -> ModelsManagerFuture<'a, CoreResult<CatalogModel>> {
        Box::pin(async move {
            Ok(construct_model_info_from_candidates(
                model,
                &self.remote_models,
                self.origin,
                config,
            ))
        })
    }

    fn get_remote_models(&self) -> ModelsManagerFuture<'_, Vec<ModelInfo>> {
        Box::pin(async { self.remote_models.clone() })
    }

    fn try_get_remote_models(&self) -> Result<Vec<ModelInfo>, TryLockError> {
        Ok(self.remote_models.clone())
    }

    fn auth_manager(&self) -> Option<&AuthManager> {
        self.auth_manager.as_deref()
    }

    fn list_collaboration_modes(&self) -> Vec<CollaborationModeMask> {
        builtin_collaboration_mode_presets()
    }

    fn refresh_if_new_etag(
        &self,
        _provider_id: &str,
        _etag: String,
        _http_client_factory: HttpClientFactory,
    ) -> ModelsManagerFuture<'_, ()> {
        Box::pin(async {})
    }
}

fn find_model_by_longest_prefix(model: &str, candidates: &[ModelInfo]) -> Option<ModelInfo> {
    let mut best: Option<ModelInfo> = None;
    for candidate in candidates {
        if !model.starts_with(&candidate.slug) {
            continue;
        }
        let is_better_match = if let Some(current) = best.as_ref() {
            candidate.slug.len() > current.slug.len()
        } else {
            true
        };
        if is_better_match {
            best = Some(candidate.clone());
        }
    }
    best
}

fn find_model_by_namespaced_suffix(model: &str, candidates: &[ModelInfo]) -> Option<ModelInfo> {
    // Retry metadata lookup for a single namespaced slug like `namespace/model-name`.
    //
    // This only strips one leading namespace segment and only when the namespace looks
    // like a simple provider id to avoid broadly matching arbitrary aliases.
    let (namespace, suffix) = model.split_once('/')?;
    if suffix.contains('/') {
        return None;
    }
    if namespace.is_empty()
        || !namespace
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        return None;
    }
    find_model_by_longest_prefix(suffix, candidates)
}

/// Selects owned model metadata without applying configuration overrides.
fn find_model_from_candidates(model: &str, candidates: &[ModelInfo]) -> Option<ModelInfo> {
    // First use the normal longest-prefix match. If that misses, allow a narrowly scoped
    // retry for namespaced slugs like `custom/gpt-5.3-codex`.
    find_model_by_longest_prefix(model, candidates)
        .or_else(|| find_model_by_namespaced_suffix(model, candidates))
}

pub(crate) fn construct_model_info_from_candidates(
    model: &str,
    candidates: &[ModelInfo],
    origin: ModelOrigin,
    config: &ModelsManagerConfig,
) -> CatalogModel {
    let remote = find_model_from_candidates(model, candidates).map(|model| CatalogModel {
        model: model.into(),
        origin,
    });
    construct_model_info(model, remote, config)
}

fn construct_model_info(
    model: &str,
    remote: Option<CatalogModel>,
    config: &ModelsManagerConfig,
) -> CatalogModel {
    let mut resolved = remote.unwrap_or_else(|| CatalogModel {
        model: model_info::model_info_from_slug(model).into(),
        origin: ModelOrigin::Synthetic,
    });
    let mut metadata = Arc::unwrap_or_clone(resolved.model);
    metadata.slug = model.to_owned();
    resolved.model = model_info::with_config_overrides(metadata, config).into();
    resolved
}

#[cfg(test)]
#[path = "manager_tests.rs"]
mod tests;
