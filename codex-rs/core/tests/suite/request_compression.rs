#![cfg(not(target_os = "windows"))]

use codex_core::TurnInputRequest;
use codex_features::Feature;
use codex_login::CodexAuth;
use codex_protocol::protocol::EventMsg;
use codex_protocol::user_input::UserInput;
use core_test_support::responses::ev_completed;
use core_test_support::responses::ev_response_created;
use core_test_support::responses::mount_sse_once;
use core_test_support::responses::sse;
use core_test_support::responses::start_mock_server;
use core_test_support::skip_if_no_network;
use core_test_support::test_codex::test_codex;
use core_test_support::wait_for_event;
use pretty_assertions::assert_eq;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn request_compression_follows_explicit_feature_for_each_auth() -> anyhow::Result<()> {
    skip_if_no_network!(Ok(()));

    for backend_auth in [true, false] {
        for enabled in [Some(true), Some(false), None] {
            for (provider_id, provider_name) in [("openai", "OpenAI"), ("cambridge", "Cambridge")] {
                let server = start_mock_server().await;
                let request_log = mount_sse_once(
                    &server,
                    sse(vec![ev_response_created("resp-1"), ev_completed("resp-1")]),
                )
                .await;
                let base_url = format!("{}/backend-api/codex/v1", server.uri());
                let mut builder = test_codex()
                    .with_pre_build_hook(|home| {
                        std::fs::write(home.join("config.toml"), "model_provider = 'openai'\n")
                            .expect("explicit test provider");
                    })
                    .with_auth(if backend_auth {
                        CodexAuth::create_dummy_chatgpt_auth_for_testing()
                    } else {
                        CodexAuth::from_api_key("dummy")
                    })
                    .with_config(move |config| {
                        if let Some(enabled) = enabled {
                            if enabled {
                                config.features.enable(Feature::EnableRequestCompression)
                            } else {
                                config.features.disable(Feature::EnableRequestCompression)
                            }
                            .expect("test config should allow feature update");
                        }
                        assert_eq!(
                            config.features.enabled(Feature::EnableRequestCompression),
                            enabled.unwrap_or(false)
                        );
                        config.model_provider.base_url = Some(base_url);
                        config.model_provider.name = provider_name.to_string();
                        config.model_provider.requires_openai_auth = backend_auth;
                        config.model_provider.experimental_bearer_token =
                            (!backend_auth).then(|| "provider-key".into());
                        config.model_provider_id = provider_id.to_string();
                        config
                            .model_providers
                            .insert(provider_id.to_string(), config.model_provider.clone());
                    });
                let codex = builder.build(&server).await?.codex;
                codex
                    .start_or_steer_turn(TurnInputRequest::user_input(vec![UserInput::Text {
                        text: "capture request encoding".into(),
                        text_elements: Vec::new(),
                    }]))
                    .await?;
                wait_for_event(&codex, |ev| matches!(ev, EventMsg::TurnComplete(_))).await;

                let request = request_log.single_request();
                assert_eq!(
                    request.header("authorization").as_deref(),
                    Some(if backend_auth {
                        "Bearer Access Token"
                    } else {
                        "Bearer provider-key"
                    })
                );
                let body = if enabled == Some(true) {
                    assert_eq!(request.header("content-encoding").as_deref(), Some("zstd"));
                    zstd::stream::decode_all(std::io::Cursor::new(request.body_bytes()))?
                } else {
                    assert!(request.header("content-encoding").is_none());
                    request.body_bytes()
                };
                let json: serde_json::Value = serde_json::from_slice(&body)?;
                assert!(json.get("input").is_some());
                println!("verified backend_auth={backend_auth} enabled={enabled:?} provider={provider_id}");
            }
        }
    }
    Ok(())
}
