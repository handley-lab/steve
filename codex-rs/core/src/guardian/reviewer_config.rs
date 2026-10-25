//! Resolves reviewer models and adds policy context and live network state to reviewer configuration.
//! Both prewarming and reviews finish this setup before context preparation and reuse checks.

use std::sync::Arc;

use codex_features::Feature;
use codex_guardian_reviewer::guardian_output_contract_prompt;
use codex_prompts::GuardianPolicyInstructions;
use codex_prompts::ResolvedModelMessages;
use codex_protocol::models::BaseInstructionsProvenance;

use crate::config::Config;
use crate::config::NetworkProxySpec;
use crate::context::ContextualUserFragment;
use crate::context::GuardianConversationHistory;

/// Adds the captured model, policy prompt and live network rules before reuse selection.
pub fn build_guardian_review_session_config(
    mut guardian_config: Config,
    live_network_config: Option<codex_network_proxy::NetworkProxyConfig>,
    active_model: &str,
    reasoning_effort: Option<codex_protocol::openai_models::ReasoningEffort>,
    reasoning_summary: codex_protocol::config_types::ReasoningSummary,
    personality: Option<codex_protocol::config_types::Personality>,
    model_messages: ResolvedModelMessages<'_>,
) -> anyhow::Result<Config> {
    let (provider_id, native_model) =
        guardian_config.resolve_model_selection(active_model, None)?;
    guardian_config.model_provider_id = provider_id;
    guardian_config.model_provider =
        guardian_config.model_providers[&guardian_config.model_provider_id].clone();
    guardian_config.model = Some(native_model);
    guardian_config.model_reasoning_effort = reasoning_effort;
    guardian_config.model_reasoning_summary = Some(reasoning_summary);
    guardian_config.personality = personality;
    let auto_review = model_messages.auto_review();
    let tenant_policy_config = guardian_config.resolve_guardian_policy(model_messages);
    let extra_policy = guardian_config
        .guardian_extra_policy
        .as_deref()
        .unwrap_or_default();
    let policy_template = guardian_config
        .guardian_policy_template
        .as_deref()
        .unwrap_or(auto_review.policy_template);
    let mut instructions = GuardianPolicyInstructions::new(
        guardian_config.guardian_transcript_mode,
        tenant_policy_config,
        extra_policy,
        policy_template,
        guardian_output_contract_prompt(),
    )
    .render();
    if guardian_config
        .features
        .enabled(Feature::GuardianConversationHistoryTools)
        && guardian_config.features.enabled(Feature::Apps)
    {
        instructions.push('\n');
        instructions.push_str(
            &GuardianConversationHistory {
                prompt: guardian_config
                    .guardian_conversation_history_prompt
                    .as_deref(),
            }
            .render(),
        );
    }
    guardian_config.base_instructions = Some(instructions);
    guardian_config.base_instructions_provenance = Some(BaseInstructionsProvenance::Custom);
    if let Some(live_network_config) = live_network_config
        && guardian_config.permissions.network.is_some()
    {
        let network_constraints = guardian_config
            .config_layer_stack
            .requirements()
            .network
            .as_ref()
            .map(|network| network.value.clone());
        guardian_config.permissions.network = Some(NetworkProxySpec::from_config_and_constraints(
            live_network_config,
            network_constraints,
            guardian_config.permissions.permission_profile(),
        )?);
    }
    Ok(guardian_config)
}

/// Resolves the same catalog-backed reviewer for approval and checkpoint migration.
pub(crate) async fn resolve_review_model(
    session: &crate::session::session::Session,
    context: &super::GuardianReviewContext,
) -> anyhow::Result<(
    codex_guardian_reviewer::ReviewModel,
    Arc<codex_protocol::openai_models::ModelInfo>,
    Option<codex_models_manager::manager::SharedModelsManager>,
)> {
    let turn = context.turn();
    let mut config = turn.config.as_ref().clone();
    let review_model_id = config
        .guardian_model
        .clone()
        .or_else(|| {
            config
                .model_catalog
                .as_ref()
                .and(context.model_info.auto_review_model_override.clone())
        })
        .filter(|model| !model.trim().is_empty())
        .ok_or_else(|| anyhow::anyhow!("Set auto_review.model before enabling automatic review"))?;
    let (provider_id, native_model) = config.resolve_model_selection(
        &review_model_id,
        config
            .guardian_model_provider
            .as_ref()
            .map(|(id, _)| id.as_str()),
    )?;
    let independent_provider = provider_id != config.model_provider_id;
    config.model_provider_id = provider_id;
    config.model_provider = config.model_providers[&config.model_provider_id].clone();
    let review_model_id = format!("{}/{}", config.model_provider_id, native_model);
    if independent_provider || native_model != context.model_info.slug {
        config.model_context_window = None;
        config.model_auto_compact_token_limit = None;
    }
    let (models_manager, refresh) = if independent_provider {
        (
            crate::build_models_manager(&config, Arc::clone(&session.services.auth_manager)),
            codex_models_manager::manager::RefreshStrategy::OnlineIfUncached,
        )
    } else {
        (
            Arc::clone(&session.services.models_manager),
            codex_models_manager::manager::RefreshStrategy::Offline,
        )
    };
    let available_models = models_manager
        .list_models(refresh, config.http_client_factory())
        .await;
    let review_model = codex_guardian_reviewer::select_review_model(
        &context.model_info,
        context.reasoning_effort.as_ref(),
        &review_model_id,
        &available_models,
    );
    let guardian_model_info = Arc::new(
        models_manager
            .get_model_info(
                &config.model_provider_id,
                &native_model,
                &config.to_models_manager_config(),
            )
            .await?,
    );
    Ok((
        review_model,
        guardian_model_info,
        independent_provider.then_some(models_manager),
    ))
}
