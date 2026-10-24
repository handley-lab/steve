//! Renders Bedrock catalogs and exercises Astra reasoning and service-tier choices.

use super::*;
use codex_http_client::HttpClientFactory;
use codex_http_client::OutboundProxyPolicy;
use codex_model_provider::create_model_provider;
use codex_model_provider_info::ModelProviderInfo;
use codex_models_manager::manager::RefreshStrategy;

#[tokio::test]
async fn bedrock_astra_model_and_reasoning_pickers() {
    for (_name, provider_info, default_model, astra_model) in [
        (
            "mantle",
            ModelProviderInfo::create_amazon_bedrock_provider(/*aws*/ None),
            "openai.gpt-6.1-sol",
            "openai.gpt-6-astra",
        ),
        (
            "runtime",
            ModelProviderInfo::create_amazon_bedrock_runtime_provider(/*aws*/ None),
            "global.openai.gpt-6.1-sol",
            "global.openai.gpt-6-astra",
        ),
    ] {
        let mut model_info = codex_models_manager::test_support::model_info(astra_model);
        model_info.default_reasoning_level = Some(ReasoningEffortConfig::Medium);
        model_info.supported_reasoning_levels = [
            ReasoningEffortConfig::Low,
            ReasoningEffortConfig::Medium,
            ReasoningEffortConfig::High,
            ReasoningEffortConfig::XHigh,
            ReasoningEffortConfig::Max,
            ReasoningEffortConfig::Ultra,
        ]
        .into_iter()
        .map(
            |effort| codex_protocol::openai_models::ReasoningEffortPreset {
                description: format!("{effort:?}"),
                effort,
            },
        )
        .collect();
        let presets = create_model_provider(provider_info, /*auth_manager*/ None)
            .models_manager_without_cache(Some(codex_protocol::openai_models::ModelsResponse {
                models: vec![
                    codex_models_manager::test_support::model_info(default_model),
                    model_info,
                ],
            }))
            .list_models(
                RefreshStrategy::Offline,
                HttpClientFactory::new(OutboundProxyPolicy::ReqwestDefault),
            )
            .await;
        let astra = presets
            .iter()
            .find(|model| model.model == astra_model)
            .expect("Astra preset")
            .clone();
        let (mut chat, mut events, _ops) = make_chatwidget_manual(Some(default_model)).await;
        chat.thread_id = Some(ThreadId::new());
        chat.model_catalog = Arc::new(ModelCatalog::new(presets));
        chat.open_model_popup();
        let picker = render_bottom_popup(&chat, /*width*/ 100);
        assert!(picker.contains(default_model));
        assert!(picker.contains(astra_model));
        chat.handle_key_event(KeyEvent::from(KeyCode::Esc));
        chat.open_reasoning_popup(astra);
        assert!(render_bottom_popup(&chat, /*width*/ 100).contains("More reasoning"));
        chat.handle_key_event(KeyEvent::from(KeyCode::Char('5')));
        let advanced =
            std::iter::from_fn(|| events.try_recv().ok()).find_map(|event| match event {
                AppEvent::OpenAdvancedReasoningPopup { model } => Some(model),
                _ => None,
            });
        chat.open_advanced_reasoning_popup(advanced.expect("advanced reasoning popup"));
        assert!(render_bottom_popup(&chat, /*width*/ 100).contains("Advanced Reasoning"));
        chat.handle_key_event(KeyEvent::from(KeyCode::Char('2')));
        let selected =
            std::iter::from_fn(|| events.try_recv().ok()).find_map(|event| match event {
                AppEvent::AstraSelectedFromModelPicker {
                    thread_id,
                    model,
                    action: AstraModelPickerAction::ApplyAdvancedReasoning { effort },
                } => Some((thread_id, model, effort)),
                _ => None,
            });
        assert_eq!(
            selected,
            Some((
                chat.thread_id.expect("active thread"),
                astra_model.to_string(),
                ReasoningEffortConfig::Ultra,
            ))
        );
    }
}

#[tokio::test]
async fn bedrock_ultrafast_slash_command_selects_and_clears_tier() {
    for (_name, provider_info, model, standard_model) in [
        (
            "mantle",
            ModelProviderInfo::create_amazon_bedrock_provider(/*aws*/ None),
            "openai.gpt-6-astra",
            "openai.gpt-6-sol",
        ),
        (
            "runtime_global",
            ModelProviderInfo::create_amazon_bedrock_runtime_provider(/*aws*/ None),
            "global.openai.gpt-6-astra",
            "global.openai.gpt-6-sol",
        ),
        (
            "runtime_us",
            ModelProviderInfo::create_amazon_bedrock_runtime_provider(/*aws*/ None),
            "us.openai.gpt-6-astra",
            "us.openai.gpt-6-sol",
        ),
        (
            "mantle_sol61",
            ModelProviderInfo::create_amazon_bedrock_provider(/*aws*/ None),
            "openai.gpt-6.1-sol",
            "openai.gpt-6-sol",
        ),
        (
            "runtime_global_sol61",
            ModelProviderInfo::create_amazon_bedrock_runtime_provider(/*aws*/ None),
            "global.openai.gpt-6.1-sol",
            "global.openai.gpt-6-sol",
        ),
        (
            "runtime_us_sol61",
            ModelProviderInfo::create_amazon_bedrock_runtime_provider(/*aws*/ None),
            "us.openai.gpt-6.1-sol",
            "us.openai.gpt-6-sol",
        ),
    ] {
        let mut selected_model = codex_models_manager::test_support::model_info(model);
        selected_model.service_tiers = vec![codex_protocol::openai_models::ModelServiceTier {
            id: "ultrafast".into(),
            name: "Ultrafast".into(),
            description: "Configured tier".into(),
        }];
        let standard = codex_models_manager::test_support::model_info(standard_model);
        let presets = create_model_provider(provider_info.clone(), /*auth_manager*/ None)
            .models_manager_without_cache(Some(codex_protocol::openai_models::ModelsResponse {
                models: vec![selected_model, standard],
            }))
            .list_models(
                RefreshStrategy::Offline,
                HttpClientFactory::new(OutboundProxyPolicy::ReqwestDefault),
            )
            .await;
        let (mut chat, mut events, _ops) = make_chatwidget_manual(Some(model)).await;
        chat.model_catalog = Arc::new(ModelCatalog::new(presets));
        chat.set_feature_enabled(Feature::FastMode, /*enabled*/ true);
        chat.set_service_tier(/*service_tier*/ None);
        chat.bottom_pane
            .set_composer_text("/ultrafast".to_string(), Vec::new(), Vec::new());
        chat.handle_key_event(KeyEvent::from(KeyCode::Enter));
        assert_eq!(chat.current_service_tier(), Some("ultrafast"));
        assert!(std::iter::from_fn(|| events.try_recv().ok()).any(|event| {
            matches!(event, AppEvent::PersistServiceTierSelection { service_tier }
                if service_tier.as_deref() == Some("ultrafast"))
        }));

        chat.set_model(standard_model);
        assert_eq!(chat.current_service_tier(), None);
        assert!(chat.current_model_service_tier_commands().is_empty());
        chat.set_model(model);
        assert_eq!(chat.current_service_tier(), Some("ultrafast"));
        chat.bottom_pane
            .set_composer_text("/ultrafast".to_string(), Vec::new(), Vec::new());
        chat.handle_key_event(KeyEvent::from(KeyCode::Enter));
        assert_eq!(chat.current_service_tier(), Some("default"));

        let mut custom_model = codex_models_manager::test_support::model_info(model);
        custom_model.slug = model.to_string();
        custom_model.service_tiers = vec![codex_protocol::openai_models::ModelServiceTier {
            id: "ultrafast".to_string(),
            name: "Express".to_string(),
            description: "My custom Bedrock tier.".to_string(),
        }];
        custom_model.default_service_tier = Some("ultrafast".to_string());
        let presets = create_model_provider(provider_info, /*auth_manager*/ None)
            .models_manager_without_cache(Some(codex_protocol::openai_models::ModelsResponse {
                models: vec![custom_model],
            }))
            .list_models(
                RefreshStrategy::Offline,
                HttpClientFactory::new(OutboundProxyPolicy::ReqwestDefault),
            )
            .await;
        chat.model_catalog = Arc::new(ModelCatalog::new(presets));
        chat.set_service_tier(/*service_tier*/ None);
        assert_eq!(chat.current_service_tier(), Some("ultrafast"));
        chat.set_service_tier(Some("default".to_string()));
        assert_eq!(chat.current_service_tier(), Some("default"));
        chat.bottom_pane
            .set_composer_text("/express".to_string(), Vec::new(), Vec::new());
        chat.handle_key_event(KeyEvent::from(KeyCode::Enter));
        assert_eq!(chat.current_service_tier(), Some("ultrafast"));
    }
}
