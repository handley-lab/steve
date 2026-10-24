//! A live catalogue composed from the configured providers and one metadata overlay.

use crate::manager::ModelListing;
use crate::manager::ModelProviderError;
use crate::manager::ModelsManager;
use crate::manager::ModelsManagerFuture;
use crate::manager::RefreshStrategy;
use crate::manager::SharedModelsManager;
use codex_http_client::HttpClientFactory;
use codex_protocol::openai_models::ModelInfo;
use codex_protocol::openai_models::ModelPreset;
use codex_protocol::openai_models::ModelsResponse;
use std::collections::HashMap;
use tokio::sync::TryLockError;

#[derive(Debug)]
pub struct ProviderModels {
    pub id: String,
    pub name: String,
    pub default_model: Option<String>,
    pub manager: SharedModelsManager,
}

#[derive(Debug)]
pub struct MultiProviderModelsManager {
    providers: Vec<ProviderModels>,
    metadata: HashMap<String, ModelInfo>,
    default_model: String,
}

impl MultiProviderModelsManager {
    pub fn new(
        providers: Vec<ProviderModels>,
        metadata: Option<ModelsResponse>,
        default_provider: &str,
        default_model: Option<&str>,
    ) -> Self {
        let metadata = metadata
            .into_iter()
            .flat_map(|catalog| catalog.models)
            .map(|model| (model.slug.clone(), model))
            .collect();
        Self {
            providers,
            metadata,
            default_model: default_model
                .map(|model| format!("{default_provider}/{model}"))
                .unwrap_or_default(),
        }
    }

    fn enrich(&self, provider: &ProviderModels, models: Vec<ModelInfo>) -> Vec<ModelInfo> {
        models
            .into_iter()
            .filter_map(|live| {
                let slug = format!("{}/{}", provider.id, live.slug);
                let mut model = self
                    .metadata
                    .get(&slug)
                    .cloned()
                    .unwrap_or_else(|| live.clone());
                let non_chat = live.discovered_non_chat.unwrap_or(model.non_chat);
                if non_chat {
                    return None;
                }
                model.slug = slug;
                if let Some(upgrade) = &mut model.upgrade {
                    upgrade.model = format!("{}/{}", provider.id, upgrade.model);
                }
                model.non_chat = false;
                model.display_name = format!("{} / {}", provider.name, model.display_name);
                model.discovered_non_chat = live.discovered_non_chat;
                Some(model)
            })
            .collect()
    }

    async fn catalogue(
        &self,
        strategy: RefreshStrategy,
        factory: HttpClientFactory,
    ) -> (Vec<ModelInfo>, Vec<ModelProviderError>) {
        let mut models = Vec::new();
        let mut errors = Vec::new();
        for provider in &self.providers {
            match provider
                .manager
                .model_catalog_result(strategy, factory.clone())
                .await
            {
                Ok(catalog) => {
                    let available = self.enrich(provider, catalog.models);
                    if provider.default_model.as_ref().is_some_and(|default| {
                        !available
                            .iter()
                            .any(|model| model.slug == format!("{}/{default}", provider.id))
                    }) {
                        errors.push(ModelProviderError {
                            provider: provider.id.clone(),
                            message: "Configured provider default is unavailable".to_string(),
                        });
                    }
                    models.extend(available);
                }
                Err(error) => {
                    let message = match error.details() {
                        codex_protocol::error::CodexErrorDetails::InvalidRequest(message)
                            if message == "model discovery is disabled" =>
                        {
                            message.clone()
                        }
                        _ => "Model discovery failed".to_string(),
                    };
                    errors.push(ModelProviderError {
                        provider: provider.id.clone(),
                        message,
                    });
                }
            }
        }
        if !self.default_model.is_empty()
            && !models.iter().any(|model| model.slug == self.default_model)
        {
            let provider = self.default_model.split_once('/').unwrap().0;
            if !errors.iter().any(|error| error.provider == provider) {
                errors.push(ModelProviderError {
                    provider: provider.to_string(),
                    message: "Configured model is unavailable".to_string(),
                });
            }
        }
        (models, errors)
    }
}

impl ModelsManager for MultiProviderModelsManager {
    fn get_model_info<'a>(
        &'a self,
        provider_id: &'a str,
        model: &'a str,
        config: &'a crate::ModelsManagerConfig,
    ) -> ModelsManagerFuture<'a, codex_protocol::error::Result<ModelInfo>> {
        Box::pin(async move {
            let models = self.get_remote_models().await;
            let qualified = format!("{provider_id}/{model}");
            let mut info = models
                .into_iter()
                .find(|candidate| candidate.slug == qualified)
                .ok_or_else(|| {
                    codex_protocol::error::CodexErr::InvalidRequest(format!(
                        "Model `{qualified}` is unavailable for coding"
                    ))
                })?;
            info.slug = model.to_string();
            Ok(crate::model_info::with_config_overrides(info, config))
        })
    }
    fn set_api_key_model_discovery_enabled(&self, enabled: bool) {
        for provider in &self.providers {
            provider
                .manager
                .set_api_key_model_discovery_enabled(enabled);
        }
    }

    fn raw_model_catalog(
        &self,
        strategy: RefreshStrategy,
        factory: HttpClientFactory,
    ) -> ModelsManagerFuture<'_, ModelsResponse> {
        Box::pin(async move {
            ModelsResponse {
                models: self.catalogue(strategy, factory).await.0,
            }
        })
    }

    fn model_catalog_result(
        &self,
        strategy: RefreshStrategy,
        factory: HttpClientFactory,
    ) -> ModelsManagerFuture<'_, codex_protocol::error::Result<ModelListing>> {
        Box::pin(async move {
            let (models, provider_errors) = self.catalogue(strategy, factory).await;
            Ok(ModelListing {
                models,
                provider_errors,
            })
        })
    }

    fn build_available_models(&self, mut models: Vec<ModelInfo>) -> Vec<ModelPreset> {
        models.sort_by_key(|model| model.priority);
        models
            .into_iter()
            .map(|model| {
                let mut preset = ModelPreset::from(model);
                preset.is_default = preset.model == self.default_model;
                preset.is_provider_default = self.providers.iter().any(|provider| {
                    provider
                        .default_model
                        .as_ref()
                        .is_some_and(|default| preset.model == format!("{}/{default}", provider.id))
                });
                preset
            })
            .collect()
    }

    fn get_remote_models(&self) -> ModelsManagerFuture<'_, Vec<ModelInfo>> {
        Box::pin(async move {
            let mut models = Vec::new();
            for provider in &self.providers {
                models.extend(self.enrich(provider, provider.manager.get_remote_models().await));
            }
            models
        })
    }

    fn try_get_remote_models(&self) -> Result<Vec<ModelInfo>, TryLockError> {
        let mut models = Vec::new();
        for provider in &self.providers {
            models.extend(self.enrich(provider, provider.manager.try_get_remote_models()?));
        }
        Ok(models)
    }

    fn auth_manager(&self) -> Option<&codex_login::AuthManager> {
        None
    }

    fn list_collaboration_modes(&self) -> Vec<codex_protocol::config_types::CollaborationModeMask> {
        crate::collaboration_mode_presets::builtin_collaboration_mode_presets()
    }

    fn refresh_if_new_etag(
        &self,
        provider_id: &str,
        etag: String,
        factory: HttpClientFactory,
    ) -> ModelsManagerFuture<'_, ()> {
        let provider = self
            .providers
            .iter()
            .find(|provider| provider.id == provider_id)
            .expect("selected provider must belong to the session catalogue");
        provider
            .manager
            .refresh_if_new_etag(&provider.id, etag, factory)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manager::ModelsEndpointClient;
    use crate::manager::ModelsEndpointFuture;
    use crate::manager::ModelsEndpointResponse;
    use crate::manager::OpenAiModelsManager;
    use crate::manager::StaticModelsManager;
    use codex_protocol::error::CodexErr;
    use codex_protocol::error::Result as CoreResult;
    use std::sync::Arc;

    fn provider(id: &str, default_model: Option<&str>, models: Vec<ModelInfo>) -> ProviderModels {
        ProviderModels {
            id: id.to_string(),
            name: id.to_string(),
            default_model: default_model.map(str::to_string),
            manager: Arc::new(StaticModelsManager::new(None, ModelsResponse { models })),
        }
    }

    fn model(id: &str, mode: Option<&str>) -> ModelInfo {
        ModelInfo::from_discovery_id(id.to_string(), mode.map(str::to_string))
    }

    #[tokio::test]
    async fn canonical_overlay_keys_distinguish_native_ids_from_other_provider_names() {
        let mut cambridge = model("cambridge/moonshotai/Kimi-K3", None);
        cambridge.context_window = Some(100);
        cambridge.upgrade = Some(codex_protocol::openai_models::ModelInfoUpgrade {
            model: "new/native-id".to_string(),
            migration_markdown: "Use the replacement model".to_string(),
            retirement_at: None,
        });
        let mut moonshotai = model("moonshotai/Kimi-K3", None);
        moonshotai.context_window = Some(200);
        let manager = MultiProviderModelsManager::new(
            vec![
                provider("cambridge", None, vec![model("moonshotai/Kimi-K3", None)]),
                provider("moonshotai", None, vec![model("Kimi-K3", None)]),
            ],
            Some(ModelsResponse {
                models: vec![cambridge, moonshotai],
            }),
            "cambridge",
            None,
        );
        let _ = manager
            .list_models(
                RefreshStrategy::Online,
                HttpClientFactory::new(codex_http_client::OutboundProxyPolicy::ReqwestDefault),
            )
            .await;
        let models = manager.get_remote_models().await;
        assert_eq!(models[0].slug, "cambridge/moonshotai/Kimi-K3");
        assert_eq!(models[0].context_window, Some(100));
        assert_eq!(models[0].upgrade.as_ref().unwrap().model, "cambridge/new/native-id");
        assert_eq!(models[1].slug, "moonshotai/Kimi-K3");
        assert_eq!(models[1].context_window, Some(200));
        let config = crate::ModelsManagerConfig::default();
        let selected = manager
            .get_model_info("cambridge", "moonshotai/Kimi-K3", &config)
            .await
            .unwrap();
        assert_eq!(selected.slug, "moonshotai/Kimi-K3");
        assert_eq!(selected.context_window, Some(100));
        let other = manager
            .get_model_info("moonshotai", "Kimi-K3", &config)
            .await
            .unwrap();
        assert_eq!(other.slug, "Kimi-K3");
        assert_eq!(other.context_window, Some(200));
        assert!(
            manager
                .get_model_info("cambridge", "not-live", &config)
                .await
                .is_err()
        );
    }

    #[derive(Debug)]
    struct CountingEndpoint(Arc<std::sync::atomic::AtomicUsize>);

    impl ModelsEndpointClient for CountingEndpoint {
        fn identity(&self) -> Option<String> {
            Some("fixture".to_string())
        }
        fn list_models<'a>(
            &'a self,
            _version: &'a str,
            _factory: HttpClientFactory,
        ) -> ModelsEndpointFuture<'a, CoreResult<ModelsEndpointResponse>> {
            Box::pin(async move {
                let count = self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
                Ok(ModelsEndpointResponse {
                    models: vec![model(&count.to_string(), None)],
                    etag: Some(count.to_string()),
                    identity: "fixture".to_string(),
                })
            })
        }
    }

    #[tokio::test]
    async fn response_etag_refreshes_only_its_originating_provider() {
        let first = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let second = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let route = |id: &str, count| ProviderModels {
            id: id.to_string(),
            name: id.to_string(),
            default_model: None,
            manager: Arc::new(OpenAiModelsManager::new_without_cache(
                Arc::new(CountingEndpoint(count)),
                None,
            )),
        };
        let manager = MultiProviderModelsManager::new(
            vec![
                route("first", first.clone()),
                route("second", second.clone()),
            ],
            None,
            "first",
            None,
        );
        let factory =
            HttpClientFactory::new(codex_http_client::OutboundProxyPolicy::ReqwestDefault);
        manager
            .list_models(RefreshStrategy::Online, factory.clone())
            .await;
        manager
            .refresh_if_new_etag("second", "changed".to_string(), factory)
            .await;
        assert_eq!(first.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert_eq!(second.load(std::sync::atomic::Ordering::SeqCst), 2);
        let models = manager.get_remote_models().await;
        assert_eq!(models[0].slug, "first/1");
        assert_eq!(models[1].slug, "second/2");
    }

    #[tokio::test]
    async fn qualified_live_ids_and_explicit_defaults_do_not_follow_order_or_static_metadata() {
        let mut kimi = model("cambridge/moonshotai/Kimi-K3", None);
        kimi.context_window = Some(123_456);
        let manager = MultiProviderModelsManager::new(
            vec![
                provider(
                    "cambridge",
                    Some("moonshotai/Kimi-K3"),
                    vec![model("first", None), model("moonshotai/Kimi-K3", None)],
                ),
                provider(
                    "turing",
                    Some("moonshotai/Kimi-K3"),
                    vec![model("moonshotai/Kimi-K3", None)],
                ),
            ],
            Some(ModelsResponse {
                models: vec![kimi, model("cambridge/stale-heretic", None)],
            }),
            "cambridge",
            Some("moonshotai/Kimi-K3"),
        );
        let listing = manager
            .model_catalog_result(
                RefreshStrategy::Online,
                HttpClientFactory::new(codex_http_client::OutboundProxyPolicy::ReqwestDefault),
            )
            .await
            .unwrap();
        assert!(listing.provider_errors.is_empty());
        let presets = manager.build_available_models(listing.models);
        assert_eq!(presets.len(), 3);
        assert!(!presets[0].is_default);
        assert!(presets[1].is_default);
        assert!(presets[1].is_provider_default);
        assert!(presets[2].is_provider_default);
        assert!(!presets[2].is_default);
        assert_eq!(presets[1].model, "cambridge/moonshotai/Kimi-K3");
        assert_eq!(presets[2].model, "turing/moonshotai/Kimi-K3");
        let info = manager.get_remote_models().await;
        assert_eq!(info[1].context_window, Some(123_456));
        assert_eq!(info[2].context_window, None);
        assert!(!info.iter().any(|model| model.slug.contains("heretic")));
    }

    #[tokio::test]
    async fn advertised_mode_and_explicit_non_chat_metadata_are_separate_from_visibility() {
        let mut ocr = model("cambridge/ocr", None);
        ocr.non_chat = true;
        let mut chat = model("cambridge/chat", None);
        chat.non_chat = true;
        let manager = MultiProviderModelsManager::new(
            vec![provider(
                "cambridge",
                Some("decisions"),
                vec![
                    model("kimi", None),
                    model("ocr", None),
                    model("chat", Some("chat")),
                    model("decisions", Some("decisions")),
                    model("embedding", Some("embedding")),
                    model("image", Some("image_generation")),
                ],
            )],
            Some(ModelsResponse {
                models: vec![ocr, chat],
            }),
            "cambridge",
            Some("kimi"),
        );
        let listing = manager
            .model_catalog_result(
                RefreshStrategy::Online,
                HttpClientFactory::new(codex_http_client::OutboundProxyPolicy::ReqwestDefault),
            )
            .await
            .unwrap();
        assert_eq!(
            listing
                .models
                .iter()
                .map(|model| model.slug.as_str())
                .collect::<Vec<_>>(),
            vec!["cambridge/kimi", "cambridge/chat"]
        );
        assert_eq!(listing.provider_errors.len(), 1);
        assert!(listing.provider_errors[0].message.contains("default"));
    }

    #[derive(Debug)]
    struct FailingEndpoint;

    impl ModelsEndpointClient for FailingEndpoint {
        fn identity(&self) -> Option<String> {
            Some("fixture".to_string())
        }
        fn list_models<'a>(
            &'a self,
            _version: &'a str,
            _factory: HttpClientFactory,
        ) -> ModelsEndpointFuture<'a, CoreResult<ModelsEndpointResponse>> {
            Box::pin(async {
                Err(CodexErr::InvalidRequest(
                    "https://secret.invalid?key=secret-value".to_string(),
                ))
            })
        }
    }

    #[tokio::test]
    async fn provider_failure_preserves_other_choices_without_leaking_upstream_diagnostics() {
        let failed = ProviderModels {
            id: "failed".to_string(),
            name: "failed".to_string(),
            default_model: None,
            manager: Arc::new(OpenAiModelsManager::new_without_cache(
                Arc::new(FailingEndpoint),
                None,
            )),
        };
        let manager = MultiProviderModelsManager::new(
            vec![
                failed,
                provider("turing", Some("gemma"), vec![model("gemma", None)]),
            ],
            None,
            "turing",
            Some("gemma"),
        );
        let listing = manager
            .model_catalog_result(
                RefreshStrategy::Online,
                HttpClientFactory::new(codex_http_client::OutboundProxyPolicy::ReqwestDefault),
            )
            .await
            .unwrap();
        assert_eq!(listing.models.len(), 1);
        assert!(manager.build_available_models(listing.models)[0].is_default);
        assert_eq!(listing.provider_errors[0].provider, "failed");
        assert!(
            !serde_json::to_string(&listing.provider_errors)
                .unwrap()
                .contains("secret")
        );
        manager.set_api_key_model_discovery_enabled(false);
        let disabled = manager
            .model_catalog_result(
                RefreshStrategy::Online,
                HttpClientFactory::new(codex_http_client::OutboundProxyPolicy::ReqwestDefault),
            )
            .await
            .unwrap();
        assert_eq!(
            disabled.provider_errors[0].message,
            "model discovery is disabled"
        );
    }
}
