//! Origin resolution, strict discovery validation, and background read eligibility.

use super::*;
use pretty_assertions::assert_eq;
use test_case::test_case;

#[test_case(CloudConfigBundleLoadErrorCode::RequestFailed, "cloud config: request failed"; "request")]
#[test_case(CloudConfigBundleLoadErrorCode::Auth, "cloud config: authentication failed"; "refresh")]
fn cloud_failures_preserve_policy_denials_but_redact_other_messages(
    code: CloudConfigBundleLoadErrorCode,
    redacted: &str,
) {
    let denied = NetworkPolicyDenied::Destination.to_string();
    for (message, expected) in [
        (denied.clone(), denied),
        (
            "destination denied by application network policy: sk-test-private-value".into(),
            redacted.into(),
        ),
    ] {
        let error = io::Error::other(CloudConfigBundleLoadError::new(
            code, /*status_code*/ None, message,
        ));
        assert_eq!(workspace_config_load_reason(&error), expected);
    }
}

#[test_case(None; "signed_out")]
#[test_case(Some("dummy"); "api_key")]
#[tokio::test]
async fn background_routing_skips_config_until_chatgpt_sign_in(
    api_key: Option<&str>,
) -> anyhow::Result<()> {
    use app_test_support::ChatGptAuthFixture;
    use app_test_support::write_chatgpt_auth;
    use codex_config::CloudConfigBundleLoader;
    use codex_config::LoaderOverrides;
    use codex_config::types::AuthCredentialsStoreMode;
    use codex_exec_server::EnvironmentManager;
    use codex_login::AuthKeyringBackendKind;
    use std::sync::atomic::AtomicUsize;
    use std::sync::atomic::Ordering;

    let home = tempfile::tempdir()?;
    let backend = wiremock::MockServer::start().await;
    let loads = Arc::new(AtomicUsize::new(/*v*/ 0));
    let calls = Arc::clone(&loads);
    let loader = CloudConfigBundleLoader::from_getter(move || {
        calls.fetch_add(/*val*/ 1, Ordering::SeqCst);
        async { Ok(None) }
    });
    let manager = ConfigManager::new_for_tests(
        home.path().to_path_buf(),
        vec![(
            "chatgpt_base_url".into(),
            format!("{}/backend-api", backend.uri()).into(),
        )],
        LoaderOverrides::without_managed_config_for_tests(),
        loader,
    );
    let config = Arc::new(
        manager
            .load_latest_config(Some(home.path().to_path_buf()))
            .await?,
    );
    if let Some(api_key) = api_key {
        codex_login::login_with_api_key(
            home.path(),
            api_key,
            AuthCredentialsStoreMode::File,
            AuthKeyringBackendKind::default(),
        )?;
    }
    let auth_manager = Arc::new(
        AuthManager::new(
            home.path().to_path_buf(),
            /*enable_codex_api_key_env*/ false,
            AuthCredentialsStoreMode::File,
            /*forced_chatgpt_workspace_id*/ None,
            Some(config.chatgpt_base_url.clone()),
            AuthKeyringBackendKind::default(),
            config.auth_config().auth_route_config,
        )
        .await,
    );
    let threads = Arc::new(
        codex_core::test_support::thread_manager_with_models_provider_and_home(
            CodexAuth::from_api_key("dummy"),
            config.model_provider.clone(),
            config.codex_home.to_path_buf(),
            Arc::new(EnvironmentManager::default_for_tests()),
        ),
    );
    let (sender, _receiver) = tokio::sync::mpsc::channel(16);
    let outgoing = Arc::new(OutgoingMessageSender::new(
        sender,
        codex_analytics::AnalyticsEventsClient::disabled(),
    ));
    let processor = AccountRequestProcessor::new(
        Arc::clone(&auth_manager),
        threads,
        outgoing,
        config,
        manager,
    );
    let initial_loads = loads.load(Ordering::SeqCst);
    for _ in 0..3 {
        assert!(
            processor
                .read_account_for_background_workspace_routing()
                .await?
                .is_none()
        );
    }
    assert_eq!(loads.load(Ordering::SeqCst), initial_loads);

    processor.read_account(/*request*/ None).await?;
    assert!(loads.load(Ordering::SeqCst) > initial_loads);

    write_chatgpt_auth(
        home.path(),
        ChatGptAuthFixture::new("access-chatgpt").account_id("account-123"),
        AuthCredentialsStoreMode::File,
    )?;
    assert!(auth_manager.reload().await);
    let initial_loads = loads.load(Ordering::SeqCst);
    processor
        .read_account_for_background_workspace_routing()
        .await?
        .expect("chatgpt sign-in reads the local account");
    assert!(loads.load(Ordering::SeqCst) > initial_loads);

    let config_path = home.path().join("config.toml");
    std::fs::write(&config_path, "invalid = [ 'sk-test-private-value',")?;
    let Err(error) = processor
        .read_account_for_background_workspace_routing()
        .await
    else {
        panic!("invalid config must prevent workspace routing");
    };
    let message = error.to_string();
    assert!(
        message.starts_with(&format!(
            "failed to load workspace requirements: invalid configuration at {}:1:",
            config_path.display(),
        )),
        "{message}"
    );
    assert!(!message.contains("sk-test-private-value"), "{message}");
    Ok(())
}

