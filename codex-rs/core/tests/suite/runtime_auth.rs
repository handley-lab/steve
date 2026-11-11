use anyhow::Result;
use codex_features::RuntimeAuthMode;
use codex_login::AuthManager;
use codex_login::CodexAuth;
use core_test_support::test_codex::test_codex;
use wiremock::Mock;
use wiremock::ResponseTemplate;
use wiremock::matchers::method;
use wiremock::matchers::path;

#[tokio::test]
async fn configured_backend_does_not_register_runtime_automatically() -> Result<()> {
    let server = core_test_support::responses::start_mock_server().await;
    Mock::given(method("POST"))
        .and(path("/register"))
        .respond_with(ResponseTemplate::new(500))
        .expect(0)
        .mount(&server)
        .await;
    let auth = AuthManager::from_auth_for_testing(CodexAuth::from_api_key("configured-key"));
    let backend_url = server.uri();
    let test = test_codex()
        .with_pre_build_hook(|home| {
            std::fs::write(
                home.join("config.toml"),
                "model_provider = \"openai\"\nmodel = \"gpt-5.5\"\n",
            )
            .expect("write explicit provider configuration");
        })
        .with_models_manager(std::sync::Arc::new(
            codex_models_manager::manager::StaticModelsManager::new(
                None,
                codex_protocol::openai_models::ModelsResponse {
                    models: vec![codex_models_manager::test_support::model_info("gpt-5.5")],
                },
                codex_models_manager::ModelOrigin::ExplicitConfig,
            ),
        ))
        .with_auth_manager(auth.clone())
        .with_config(move |config| {
            config.chatgpt_base_url = backend_url.clone();
            config.runtime_auth_mode = RuntimeAuthMode::AssertionOnly;
        })
        .build(&server)
        .await?;
    assert!(
        codex_core::build_runtime_auth_manager(&test.config, auth, &uuid::Uuid::nil().to_string())
            .is_none()
    );
    assert!(test.thread_manager.runtime_auth_manager().is_none());
    test.codex.shutdown_and_wait().await?;
    server.verify().await;
    Ok(())
}
