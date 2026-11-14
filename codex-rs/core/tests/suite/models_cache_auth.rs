//! Host model selection must follow the thread catalog and the provider fallback setting.

use anyhow::Result;
use codex_login::CodexAuth;
use codex_protocol::openai_models::ModelsResponse;
use codex_protocol::protocol::SessionSource;
use codex_protocol::protocol::SubAgentSource;
use codex_protocol::protocol::ThreadSettingsOverrides;
use core_test_support::test_codex::test_codex;
use pretty_assertions::assert_eq;
use test_case::test_case;
use wiremock::Mock;
use wiremock::MockServer;
use wiremock::ResponseTemplate;
use wiremock::matchers::header;
use wiremock::matchers::method;
use wiremock::matchers::path;

#[test_case(SessionSource::VSCode; "root")]
#[test_case(SessionSource::SubAgent(SubAgentSource::Review); "child")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn host_model_info_uses_the_thread_catalog(source: SessionSource) -> Result<()> {
    let slug = "provider-default";
    let server = MockServer::start().await;
    let test = test_codex()
        .with_auth(CodexAuth::from_api_key("catalog-test"))
        .with_model_info_override(slug, |model| {
            model.comp_hash = Some("runtime-revision".into());
        })
        .build_with_auto_env(&server)
        .await?;

    let mut options = test.start_thread_options().await;
    let catalog = options
        .config
        .model_catalog
        .take()
        .expect("runtime catalog");
    let mut model = catalog
        .models
        .into_iter()
        .find(|m| m.slug == slug)
        .expect("runtime catalog contains the selected model");
    model.comp_hash = Some("thread-revision".into());
    let mut next_model = model.clone();
    next_model.slug = "after-update".into();
    model.priority = i32::MIN;
    Mock::given(method("GET"))
        .and(path("/provider/models"))
        .and(header("authorization", "Bearer catalog-test"))
        .and(header("originator", "codex_work_cca"))
        .respond_with(ResponseTemplate::new(200).set_body_json(ModelsResponse {
            models: vec![model, next_model],
        }))
        .mount(&server)
        .await;

    options.config.model_provider.base_url = Some(format!("{}/provider", server.uri()));
    options.config.model = None;
    options.config.model_context_window = Some(32_000);
    options.metrics_service_name = Some("codex_work_cca".into());
    options.session_source = Some(source);
    let selected = test.thread_manager.resolve_startup_model(&options).await?;
    assert_eq!(selected.slug, slug);
    assert_eq!(selected.comp_hash.as_deref(), Some("thread-revision"));
    assert_eq!(selected.context_window, Some(32_000));

    let thread = test.thread_manager.start_thread(options).await?.thread;
    assert_eq!(thread.selected_model_info().await, selected);
    thread
        .update_thread_settings(ThreadSettingsOverrides {
            model: Some("after-update".into()),
            ..Default::default()
        })
        .await?;
    let next = thread.selected_model_info().await;
    assert_eq!(next.slug, "after-update");
    assert_eq!(next.comp_hash.as_deref(), Some("thread-revision"));
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn host_model_info_honors_provider_fallback() -> Result<()> {
    let server = MockServer::start().await;
    let catalog = codex_protocol::openai_models::ModelsResponse {
        models: vec![codex_models_manager::test_support::model_info("gpt-5.5")],
    };
    let test = test_codex()
        .with_config(move |config| config.model_catalog = Some(catalog))
        .build_with_auto_env(&server)
        .await?;

    for (allow_fallback, expected) in [(false, "unavailable-model"), (true, "gpt-5.5")] {
        let mut options = test.start_thread_options().await;
        options.config.model = Some("unavailable-model".into());
        options.allow_provider_model_fallback = allow_fallback;
        let selected = test.thread_manager.resolve_startup_model(&options).await?;
        assert_eq!(selected.slug, expected);
        let thread = test.thread_manager.start_thread(options).await?.thread;
        assert_eq!(thread.selected_model_info().await, selected);
    }
    Ok(())
}
