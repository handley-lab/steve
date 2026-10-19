use std::sync::Arc;

use codex_core::config::Config;
use codex_extension_api::ConfigContributor;
use codex_extension_api::ExtensionData;
use codex_extension_api::ExtensionFuture;
use codex_extension_api::ExtensionRegistryBuilder;
use codex_extension_api::ThreadLifecycleContributor;
use codex_extension_api::ThreadOriginator;
use codex_extension_api::ThreadStartInput;
use codex_extension_api::ToolCall;
use codex_extension_api::ToolContributor;
use codex_extension_api::ToolExecutor;
use codex_features::Feature;
use codex_http_client::HttpClientFactory;
use codex_login::AuthManager;
use codex_model_provider::create_model_provider;
use codex_model_provider_info::ModelProviderInfo;
use codex_utils_absolute_path::AbsolutePathBuf;

use crate::backend::CodexImagesBackend;
use crate::tool::ImageGenerationTool;

#[derive(Clone)]
struct ImageGenerationExtension {
    auth_manager: Arc<AuthManager>,
    resolve_save_root: Arc<SaveRootResolver>,
}

type SaveRootResolver = dyn Fn(&Config) -> Option<AbsolutePathBuf> + Send + Sync;

#[derive(Clone)]
struct ImageGenerationExtensionConfig {
    available: bool,
    http_client_factory: HttpClientFactory,
    provider: ModelProviderInfo,
    save_root: Option<AbsolutePathBuf>,
}

impl ImageGenerationExtensionConfig {
    /// Resolves the image provider and save root for a thread.
    fn from_config(config: &Config, resolve_save_root: &SaveRootResolver) -> Self {
        Self {
            available: config.features.get().enabled(Feature::ImageGeneration),
            http_client_factory: config.http_client_factory(),
            provider: config.model_provider.clone(),
            save_root: resolve_save_root(config),
        }
    }
}

impl ThreadLifecycleContributor<Config> for ImageGenerationExtension {
    /// Seeds image-generation configuration when a thread begins.
    fn on_thread_start<'a>(
        &'a self,
        input: ThreadStartInput<'a, Config>,
    ) -> ExtensionFuture<'a, ()> {
        Box::pin(async move {
            input
                .thread_store
                .insert(ImageGenerationExtensionConfig::from_config(
                    input.config,
                    self.resolve_save_root.as_ref(),
                ));
        })
    }
}

impl ConfigContributor<Config> for ImageGenerationExtension {
    /// Refreshes image-generation configuration after thread configuration changes.
    fn on_config_changed(
        &self,
        _session_store: &ExtensionData,
        thread_store: &ExtensionData,
        _previous_config: &Config,
        new_config: &Config,
    ) {
        thread_store.insert(ImageGenerationExtensionConfig::from_config(
            new_config,
            self.resolve_save_root.as_ref(),
        ));
    }
}

impl ToolContributor for ImageGenerationExtension {
    /// Creates the image-generation tool exposed by this installed extension.
    fn tools(
        &self,
        _session_store: &ExtensionData,
        thread_store: &ExtensionData,
    ) -> Vec<Arc<dyn for<'call> ToolExecutor<ToolCall<'call>>>> {
        let Some(config) = thread_store.get::<ImageGenerationExtensionConfig>() else {
            return Vec::new();
        };
        if !config.available {
            return Vec::new();
        }

        vec![Arc::new(ImageGenerationTool::new(
            CodexImagesBackend::new(
                create_model_provider(config.provider.clone(), Some(self.auth_manager.clone())),
                config.http_client_factory.clone(),
                thread_store
                    .get::<ThreadOriginator>()
                    .map(|originator| originator.0.clone()),
            ),
            config.save_root.clone(),
            thread_store.level_id().to_string(),
        ))]
    }
}

/// Installs the standalone image-generation extension contributors.
pub fn install(
    registry: &mut ExtensionRegistryBuilder<Config>,
    auth_manager: Arc<AuthManager>,
    resolve_save_root: impl Fn(&Config) -> Option<AbsolutePathBuf> + Send + Sync + 'static,
) {
    let extension = Arc::new(ImageGenerationExtension {
        auth_manager,
        resolve_save_root: Arc::new(resolve_save_root),
    });
    registry.thread_lifecycle_contributor(extension.clone());
    registry.config_contributor(extension.clone());
    registry.tool_contributor(extension);
}

#[cfg(test)]
mod tests {
    use super::*;
    use codex_config::LoaderOverrides;
    use codex_core::config::ConfigBuilder;
    use codex_extension_api::ExtensionData;
    use codex_login::CodexAuth;
    use codex_protocol::protocol::SessionSource;
    use tempfile::TempDir;

    #[tokio::test]
    async fn configured_image_providers_follow_feature_enablement()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut observations = Vec::new();
        for custom in [true, false] {
            let home = TempDir::new()?;
            let mut builder = ExtensionRegistryBuilder::<Config>::new();
            install(
                &mut builder,
                AuthManager::from_auth_for_testing(CodexAuth::from_api_key("synthetic-image-key")),
                |_| None,
            );
            let registry = builder.build();
            let session_store = ExtensionData::new("image-session");
            let thread_store = ExtensionData::new("image-thread");
            let mut previous_config = None;
            for enabled in [false, true, false] {
                std::fs::write(
                    home.path().join("config.toml"),
                    format!(
                        "openai_base_url = 'http://127.0.0.1:1/v1'\n[features]\nimage_generation = {enabled}\n"
                    ),
                )?;
                let mut config = ConfigBuilder::default()
                    .loader_overrides(LoaderOverrides::without_managed_config_for_tests())
                    .codex_home(home.path().to_path_buf())
                    .build()
                    .await?;
                if custom {
                    config.model_provider.name = "Local images".into();
                    config.model_provider.requires_openai_auth = false;
                    config.model_provider.experimental_bearer_token =
                        Some("synthetic-image-key".into());
                }
                if let Some(previous) = previous_config.as_ref() {
                    for contributor in registry.config_contributors() {
                        contributor.on_config_changed(
                            &session_store,
                            &thread_store,
                            previous,
                            &config,
                        );
                    }
                } else {
                    for contributor in registry.thread_lifecycle_contributors() {
                        contributor
                            .on_thread_start(ThreadStartInput {
                                config: &config,
                                session_source: &SessionSource::Cli,
                                persistent_thread_state_available: true,
                                environments: &[],
                                mcp_resource_client: None,
                                extension_metrics: None,
                                session_store: &session_store,
                                thread_store: &thread_store,
                            })
                            .await;
                    }
                }
                let count: usize = registry
                    .tool_contributors()
                    .iter()
                    .map(|contributor| contributor.tools(&session_store, &thread_store).len())
                    .sum();
                observations.push((custom, enabled, count));
                previous_config = Some(config);
            }
        }
        eprintln!("image provider activation: {observations:?}");
        assert!(
            observations
                .iter()
                .all(|(_, enabled, count)| *count == usize::from(*enabled)),
            "{observations:?}"
        );
        Ok(())
    }
}
