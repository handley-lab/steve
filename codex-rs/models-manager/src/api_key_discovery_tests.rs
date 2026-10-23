//! Model discovery opt-outs must gate network requests and cached catalog authority.

use super::*;
use pretty_assertions::assert_eq;

#[tokio::test]
async fn model_discovery_flag_controls_fetches_and_cached_catalogs() {
    let home = tempdir().unwrap();
    let models = vec![remote_model("dynamic", "Dynamic", /*priority*/ 0)];
    let endpoint = TestModelsEndpoint::new(vec![models.clone()]);
    let auth = AuthManager::from_auth_for_testing(CodexAuth::from_api_key("test-key"));
    let manager =
        OpenAiModelsManager::new(home.path().into(), endpoint.clone(), Some(auth.clone()));
    manager.set_api_key_model_discovery_enabled(/*enabled*/ false);
    let bundled = load_remote_models_from_file().unwrap();

    for strategy in [
        RefreshStrategy::Online,
        RefreshStrategy::OnlineIfUncached,
        RefreshStrategy::Offline,
    ] {
        assert_eq!(
            manager
                .raw_model_catalog(strategy, DEFAULT_HTTP_CLIENT_FACTORY)
                .await
                .models,
            bundled
        );
    }
    assert_eq!(endpoint.fetch_count(), 0);

    let manager =
        OpenAiModelsManager::new(home.path().into(), endpoint.clone(), Some(auth.clone()));
    manager.set_api_key_model_discovery_enabled(/*enabled*/ true);
    assert_eq!(
        manager
            .raw_model_catalog(RefreshStrategy::Online, DEFAULT_HTTP_CLIENT_FACTORY)
            .await
            .models,
        models
    );
    assert_eq!(endpoint.fetch_count(), 1);

    // A runtime rollback must also suppress the already-loaded catalog.
    manager.set_api_key_model_discovery_enabled(/*enabled*/ false);
    assert_eq!(
        manager
            .raw_model_catalog(RefreshStrategy::Online, DEFAULT_HTTP_CLIENT_FACTORY)
            .await
            .models,
        bundled
    );
    assert_eq!(manager.try_get_remote_models().unwrap(), bundled);
    assert_eq!(endpoint.fetch_count(), 1);

    // A new session must honor its startup flag even when a matching disk cache exists.
    for enabled in [false, true] {
        let restarted =
            OpenAiModelsManager::new(home.path().into(), endpoint.clone(), Some(auth.clone()));
        restarted.set_api_key_model_discovery_enabled(enabled);
        let expected = if enabled { &models } else { &bundled };
        for strategy in [RefreshStrategy::Offline, RefreshStrategy::OnlineIfUncached] {
            assert_eq!(
                &restarted
                    .raw_model_catalog(strategy, DEFAULT_HTTP_CLIENT_FACTORY)
                    .await
                    .models,
                expected
            );
        }
        assert_eq!(&restarted.try_get_remote_models().unwrap(), expected);
    }
    assert_eq!(endpoint.fetch_count(), 1);
}
