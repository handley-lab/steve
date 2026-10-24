//! Resolves the classifier's own model budget and transport configuration.
//! Parent-model context overrides do not change the classifier allowance.

use std::sync::Arc;

use codex_core::ThreadManager;
use codex_core::config::Config;
use codex_extension_api::ThreadOriginator;
use codex_extension_api::ThreadStartInput;
use codex_features::Feature;
use codex_login::AuthManager;
use codex_model_provider::create_model_provider;

use super::config::GuardianV2Config;
use super::decisions::DecisionsError;
use super::decisions::DecisionsSampler;
use super::sampler::LunaSamplerConfig;

// Decisions setup failures never affect the baseline sampler.
pub(super) async fn decisions_sampler(
    input: &ThreadStartInput<'_, Config>,
    guardian_config: &GuardianV2Config,
    auth_manager: Arc<AuthManager>,
) -> Result<DecisionsSampler, DecisionsError> {
    let provider = guardian_config
        .local_overrides
        .comparison_model_provider
        .as_ref()
        .and_then(|provider_id| input.config.model_providers.get(provider_id));
    let model = guardian_config
        .local_overrides
        .comparison_model
        .as_deref()
        .filter(|model| !model.trim().is_empty());
    let result = match (provider, model) {
        (Some(provider), Some(model)) => {
            DecisionsSampler::new(
                input.config.http_client_factory(),
                create_model_provider(provider.clone(), Some(auth_manager)),
                model.to_owned(),
            )
            .await
        }
        _ => Err(DecisionsError::Configuration),
    };
    if let Err(error) = result {
        if let Some(metrics) = input.extension_metrics.as_deref() {
            metrics.counter(
                "codex.guardian_v2.decisions_comparison.setup_failure",
                /*inc*/ 1,
                &[("reason", super::metrics::decisions_failure_reason(error))],
            );
        }
    }
    result
}

pub(super) async fn sampler_config(
    input: &ThreadStartInput<'_, Config>,
    guardian_config: &GuardianV2Config,
    auth_manager: Arc<AuthManager>,
    manager: Option<Arc<ThreadManager>>,
) -> Result<LunaSamplerConfig, String> {
    let mut config = input.config.clone();
    let model = guardian_config
        .local_overrides
        .model
        .as_deref()
        .filter(|model| !model.trim().is_empty())
        .ok_or("Set features.guardianv2.model before enabling the classifier")?;
    let (provider_id, model) = config
        .resolve_model_selection(
            model,
            guardian_config.local_overrides.model_provider.as_deref(),
        )
        .map_err(|error| error.to_string())?;
    let provider = config.model_providers[&provider_id].clone();
    let independent_provider =
        provider_id != config.model_provider_id || provider != config.model_provider;
    config.model_provider_id = provider_id;
    config.model_provider = provider;
    let models_manager = if independent_provider {
        let models_manager = codex_core::build_models_manager(&config, Arc::clone(&auth_manager));
        models_manager
            .list_models(
                codex_models_manager::manager::RefreshStrategy::OnlineIfUncached,
                config.http_client_factory(),
            )
            .await;
        Some(models_manager)
    } else {
        input
            .thread_store
            .get::<codex_models_manager::manager::SharedModelsManager>()
            .map(|manager| manager.as_ref().clone())
            .or_else(|| manager.map(|manager| manager.get_models_manager()))
    };
    let luna_model = if let Some(models_manager) = models_manager {
        let mut model_config = config.to_models_manager_config();
        model_config.model_context_window = None;
        Some(
            models_manager
                .get_model_info(&config.model_provider_id, &model, &model_config)
                .await
                .map_err(|error| error.to_string())?,
        )
    } else {
        None
    };
    let max_input_tokens =
        luna_model
            .as_ref()
            .map_or(codex_guardian_context::DEFAULT_MAX_INPUT_TOKENS, |model| {
                codex_guardian_context::effective_input_token_limit(
                    model, /*configured_window*/ None,
                )
            });
    Ok(LunaSamplerConfig {
        model: model.to_owned(),
        api_key_cyber_access_programs: config.features.enabled(Feature::ApiKeyCyberAccessPrograms),
        content_item_kinds: config.features.enabled(Feature::ContentItemKinds),
        workspace_routing: if independent_provider {
            config.workspace_routing_context()
        } else {
            input
                .thread_store
                .get_or_init(|| input.config.workspace_routing_context())
                .as_ref()
                .clone()
        },
        provider: create_model_provider(config.model_provider, Some(auth_manager)),
        http_client_factory: input.config.http_client_factory(),
        session_id: input.session_store.level_id().to_string(),
        thread_id: input.thread_store.level_id().to_string(),
        originator: input
            .thread_store
            .get::<ThreadOriginator>()
            .map(|originator| originator.0.clone()),
        service_tier: input.config.service_tier.clone(),
        model_info: luna_model,
        max_input_tokens,
        metrics: input.extension_metrics.clone(),
    })
}
