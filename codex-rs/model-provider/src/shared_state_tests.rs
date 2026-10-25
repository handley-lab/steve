//! Tests gateway-manager sharing, credential refresh, and model catalog cache isolation.
//! Discovery and inference must preserve primary auth and reject requests when gateway auth fails.

use super::*;
use crate::create_model_provider;
use crate::test_support::seed_gateway_auth;
use codex_api::Compression;
use codex_api::ResponsesClient;
use codex_http_client::ClientRouteClass;
use codex_http_client::HttpClientFactory;
use codex_http_client::OutboundProxyPolicy;
use codex_http_client::ReqwestTransport;
use codex_login::AuthHeaders;
use codex_login::AuthManager;
use codex_login::CodexAuth;
use codex_login::default_client::ClientRedirectPolicy;
use codex_login::default_client::create_client_for_route;
use codex_model_provider_info::GatewayOAuthDelivery;
use codex_model_provider_info::ModelProviderInfo;
use codex_models_manager::manager::RefreshStrategy;
use http::HeaderMap;
use http::HeaderName;
use http::HeaderValue;
use pretty_assertions::assert_eq;
use serde_json::json;
use wiremock::Mock;
use wiremock::MockServer;
use wiremock::ResponseTemplate;
use wiremock::matchers::method;
use wiremock::matchers::path;

#[tokio::test]
async fn gateway_credentials_accompany_primary_auth_in_models_and_responses() {
    let server = MockServer::start().await;
    let home = tempfile::tempdir().unwrap();
    Mock::given(method("GET"))
        .and(path("/models"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"models": []})))
        .expect(2)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/responses"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(
                    "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"r\"}}\n\n",
                ),
        )
        .expect(2)
        .mount(&server)
        .await;
    let factory = HttpClientFactory::new(OutboundProxyPolicy::ReqwestDefault);
    for delivery in [
        GatewayOAuthDelivery::Header {
            name: "x-gateway-auth".into(),
            scheme: "Bearer".into(),
        },
        GatewayOAuthDelivery::Cookie {
            name: "gateway_session".into(),
        },
    ] {
        let config = GatewayOAuthConfig {
            authorization_url: format!("{}/authorize", server.uri()),
            token_url: format!("{}/token", server.uri()),
            client_id: "test".into(),
            resource: None,
            scopes: vec![],
            redirect_port: None,
            delivery: delivery.clone(),
        };
        let info = ModelProviderInfo {
            gateway_oauth: Some(config.clone()),
            requires_openai_auth: true,
            ..ModelProviderInfo::create_openai_provider(Some(server.uri()))
        };
        let mut primary_headers = HeaderMap::new();
        primary_headers.insert(
            "authorization",
            HeaderValue::from_static("Bearer primary-token"),
        );
        primary_headers.insert("chatgpt-account-id", HeaderValue::from_static("account"));
        let primary = AuthManager::from_auth_for_testing_with_home(
            CodexAuth::Headers(AuthHeaders::new(primary_headers)),
            home.path().to_path_buf(),
        );
        let manager = seed_gateway_auth(
            &info,
            &primary,
            json!({"access_token": "gateway-token", "refresh_token": "refresh-secret", "expires_at": i64::MAX}),
        );
        let provider = create_model_provider(info.clone(), Some(primary));
        assert!(Arc::ptr_eq(
            &provider.gateway_auth_manager().unwrap().unwrap(),
            &manager,
        ));
        let models = provider.models_manager_without_cache(/*config_model_catalog*/ None);
        models.set_api_key_model_discovery_enabled(/*enabled*/ true);
        models
            .raw_model_catalog(RefreshStrategy::Online, factory.clone())
            .await;
        let auth = provider.api_auth().await.unwrap();
        let scoped = provider.api_auth_for_scope().await.unwrap();
        let mut expected = HeaderMap::from_iter([
            (
                http::header::AUTHORIZATION,
                HeaderValue::from_static("Bearer primary-token"),
            ),
            (
                HeaderName::from_static("chatgpt-account-id"),
                HeaderValue::from_static("account"),
            ),
        ]);
        let (name, value) = match delivery {
            GatewayOAuthDelivery::Header { .. } => ("x-gateway-auth", "Bearer gateway-token"),
            GatewayOAuthDelivery::Cookie { .. } => ("cookie", "gateway_session=gateway-token"),
        };
        expected.insert(
            HeaderName::from_static(name),
            HeaderValue::from_static(value),
        );
        // WebSocket handshakes use the synchronous header interface.
        assert_eq!(auth.to_auth_headers(), expected);
        assert_eq!(auth.resolve_auth_headers().await.unwrap(), expected);
        assert_eq!(scoped.auth.to_auth_headers(), expected);
        assert!(auth.to_auth_headers()[name].is_sensitive());
        let api = info.to_api_provider(/*auth_mode*/ None).unwrap();
        let transport = ReqwestTransport::from_http_client(
            create_client_for_route(
                &factory,
                &api.url_for_path("responses"),
                ClientRouteClass::Api,
                ClientRedirectPolicy::Default,
            )
            .unwrap(),
        );
        let client = ResponsesClient::new(transport, api, auth);
        let _stream = client
            .stream(
                json!({"model": "test", "input": []}),
                HeaderMap::new(),
                Compression::None,
                /*turn_state*/ None,
            )
            .await
            .unwrap();
    }
    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 4);
    for request in requests {
        assert_eq!(request.headers["authorization"], "Bearer primary-token");
        assert!(
            request
                .headers
                .get("x-gateway-auth")
                .is_some_and(|value| value == "Bearer gateway-token")
                || request
                    .headers
                    .get("cookie")
                    .is_some_and(|value| value == "gateway_session=gateway-token")
        );
    }
}

#[tokio::test]
async fn gateway_refresh_preserves_primary_auth_and_hides_issuer_errors() {
    for succeeds in [true, false] {
        let server = MockServer::start().await;
        let home = tempfile::tempdir().unwrap();
        let primary = AuthManager::from_auth_for_testing_with_home(
            CodexAuth::from_api_key("primary-token"),
            home.path().to_path_buf(),
        );
        let info = ModelProviderInfo {
            gateway_oauth: Some(GatewayOAuthConfig {
                authorization_url: format!("{}/authorize", server.uri()),
                token_url: format!("{}/token?credential=query-secret", server.uri()),
                client_id: "client".into(),
                resource: None,
                scopes: vec![],
                redirect_port: None,
                delivery: GatewayOAuthDelivery::Header {
                    name: "x-gateway-auth".into(),
                    scheme: "Bearer".into(),
                },
            }),
            requires_openai_auth: true,
            ..ModelProviderInfo::create_openai_provider(Some(server.uri()))
        };
        let body = if succeeds {
            json!({"access_token": "rotated-token", "expires_in": 3600})
        } else {
            json!({"error": "server_error", "error_description": format!("query-secret {}", "sensitive ".repeat(4000))})
        };
        Mock::given(method("POST"))
            .and(path("/token"))
            .respond_with(
                ResponseTemplate::new(if succeeds { 200 } else { 500 }).set_body_json(body),
            )
            .expect(1)
            .mount(&server)
            .await;
        let _gateway = seed_gateway_auth(
            &info,
            &primary,
            json!({"access_token": "expired", "refresh_token": "refresh-secret", "expires_at": 0}),
        );
        let provider = create_model_provider(info, Some(primary.clone()));
        let result = provider.api_auth().await;
        if succeeds {
            assert_eq!(
                result.unwrap().to_auth_headers(),
                HeaderMap::from_iter([
                    (
                        http::header::AUTHORIZATION,
                        HeaderValue::from_static("Bearer primary-token")
                    ),
                    (
                        HeaderName::from_static("x-gateway-auth"),
                        HeaderValue::from_static("Bearer rotated-token")
                    ),
                ])
            );
        } else {
            let error = match result {
                Ok(_) => panic!("issuer failure should fail auth"),
                Err(error) => error,
            };
            assert!(error.retry_delay(/*retry_count*/ 1).is_some());
            assert_eq!(
                error.to_string(),
                "Gateway OAuth authentication failed; check the gateway configuration and credential store."
            );
        }
        assert_eq!(
            primary.auth_cached(),
            Some(CodexAuth::from_api_key("primary-token"))
        );
        let requests = server.received_requests().await.unwrap();
        assert!(!requests[0].headers.contains_key("authorization"));
        assert!(!requests[0].headers.contains_key("x-gateway-auth"));
        assert_eq!(
            std::str::from_utf8(&requests[0].body)
                .unwrap()
                .split('&')
                .collect::<std::collections::BTreeSet<_>>(),
            std::collections::BTreeSet::from([
                "grant_type=refresh_token",
                "refresh_token=refresh-secret",
                "client_id=client"
            ])
        );
    }
}

#[tokio::test]
async fn models_cache_follows_gateway_credentials_and_configuration() {
    use codex_models_manager::manager::ModelsEndpointClient;
    use codex_models_manager::manager::ModelsManager;

    let server = MockServer::start().await;
    let home = tempfile::tempdir().unwrap();
    let primary = AuthManager::from_auth_for_testing_with_home(
        CodexAuth::from_api_key("primary-token"),
        home.path().to_path_buf(),
    );
    let mut info = ModelProviderInfo {
        model_catalog_url: Some(format!("{}/models", server.uri()).into()),
        http_headers: Some(std::collections::HashMap::from([(
            codex_login::default_client::RESIDENCY_HEADER_NAME.to_string(),
            "us".into(),
        )])),
        gateway_oauth: Some(GatewayOAuthConfig {
            authorization_url: format!("{}/authorize", server.uri()),
            token_url: format!("{}/token", server.uri()),
            client_id: "client".into(),
            resource: None,
            scopes: vec![],
            redirect_port: None,
            delivery: GatewayOAuthDelivery::Header {
                name: "x-gateway-auth".into(),
                scheme: "Bearer".into(),
            },
        }),
        ..ModelProviderInfo::create_openai_provider(Some(server.uri()))
    };
    let factory = HttpClientFactory::new(OutboundProxyPolicy::ReqwestDefault);
    let mut model = codex_models_manager::test_support::model_info("test-model");
    let mut live_models: Vec<codex_models_manager::manager::SharedModelsManager> = Vec::new();
    for (resource, token) in [
        ("first", "first"),
        ("first", "replacement"),
        ("second", "second"),
    ] {
        info.gateway_oauth.as_mut().unwrap().resource =
            Some(format!("{}/{resource}", server.uri()));
        model.slug = format!("catalog-{resource}-{token}");
        let _gateway = seed_gateway_auth(
            &info,
            &primary,
            json!({"access_token": token, "refresh_token": "refresh-secret", "expires_at": i64::MAX}),
        );
        Mock::given(method("GET"))
            .and(path("/models"))
            .and(wiremock::matchers::header(
                "x-gateway-auth",
                format!("Bearer {token}"),
            ))
            .respond_with(ResponseTemplate::new(200).insert_header("etag", "stable-gateway").set_body_json(json!({"models": [&model]})))
            .expect(1)
            .mount(&server)
            .await;

        if token == "replacement" {
            let provider = create_model_provider(info.clone(), Some(primary.clone()));
            let owner = provider.gateway_auth_manager().unwrap().unwrap();
            let cache_path = home.path().join("models_cache.json");
            let before = std::fs::read(&cache_path).unwrap();
            let requests_before = server.received_requests().await.unwrap().len();
            live_models[0].set_api_key_model_discovery_enabled(false);
            live_models[0]
                .refresh_if_new_etag("fixture", "stable-gateway".into(), factory.clone())
                .await;
            let off_cache = std::fs::read(&cache_path).unwrap();
            eprintln!("T18_EVIDENCE {}", json!({"kind": "gateway-etag-off",
                "token": owner.cached_access_token(), "cache_before": String::from_utf8_lossy(&before),
                "cache_after": String::from_utf8_lossy(&off_cache),
                "request_count": server.received_requests().await.unwrap().len(),
            }));
            assert_eq!(owner.cached_access_token().as_deref(), Some("first"));
            assert_eq!(off_cache, before);
            assert_eq!(server.received_requests().await.unwrap().len(), requests_before);
            assert!(live_models[0].get_remote_models().await.is_empty());
            live_models[0].set_api_key_model_discovery_enabled(true);
            live_models[0]
                .refresh_if_new_etag("fixture", "stable-gateway".into(), factory.clone())
                .await;
            let after = live_models[0].get_remote_models().await;
            let cache_after: serde_json::Value = serde_json::from_slice(&std::fs::read(&cache_path).unwrap()).unwrap();
            eprintln!("T18_EVIDENCE {}", json!({"kind": "gateway-etag-replacement",
                "token": owner.cached_access_token(), "models": &after, "cache_after": &cache_after,
                "request_count": server.received_requests().await.unwrap().len(),
            }));
            assert_eq!(after, vec![model.clone()]);
            let cache_before: serde_json::Value = serde_json::from_slice(&before).unwrap();
            assert_ne!(cache_before["identity"], cache_after["identity"]);
            assert_eq!(server.received_requests().await.unwrap().len(), requests_before + 1);
            assert!(live_models[1].get_remote_models().await.is_empty());
        }
        // Keep earlier managers alive so a stored replacement must update the shared owner.
        for pass in 0..2 {
            let provider = create_model_provider(info.clone(), Some(primary.clone()));
            let models = provider.models_manager(
                home.path().to_path_buf(),
                /*config_model_catalog*/ None,
            );
            models.set_api_key_model_discovery_enabled(/*enabled*/ true);
            let catalog = models
                .raw_model_catalog(RefreshStrategy::OnlineIfUncached, factory.clone())
                .await;
            eprintln!("T18_EVIDENCE {}", json!({
                "kind": "gateway-disk", "resource": resource, "token": token,
                "response_body": json!({"models": [&model]}).to_string(),
                "actual_models": &catalog.models,
                "requests": server.received_requests().await.unwrap().iter().map(|request| json!({
                    "url": request.url.as_str(), "method": request.method.to_string(),
                    "headers": request.headers.iter().map(|(name, value)|
                        (name.to_string(), String::from_utf8_lossy(value.as_bytes()).into_owned())
                    ).collect::<Vec<_>>(),
                    "body": String::from_utf8_lossy(&request.body),
                })).collect::<Vec<_>>(),
            }));
            assert_eq!(catalog.models, vec![model.clone()]);
            if resource == "first" && token == "first" && pass == 0 {
                let before = server.received_requests().await.unwrap().len();
                models
                    .refresh_if_new_etag("fixture", "stable-gateway".into(), factory.clone())
                    .await;
                assert_eq!(server.received_requests().await.unwrap().len(), before);
                let cold_owner = seed_gateway_auth(&info, &primary,
                    json!({"access_token": token, "refresh_token": "refresh-secret", "expires_at": i64::MAX}));
                let endpoint = Arc::new(crate::models_endpoint::OpenAiModelsEndpoint::new(
                    info.clone(), Some(primary.clone()), Some(Ok(cold_owner)),
                ));
                assert!(endpoint.identity().is_none());
                let cold = codex_models_manager::manager::OpenAiModelsManager::new(
                    home.path().into(), endpoint.clone(), Some(primary.clone()),
                ).with_provider_catalog();
                let offline = cold.raw_model_catalog(RefreshStrategy::Offline, factory.clone()).await;
                eprintln!("T18_EVIDENCE {}", json!({"kind": "gateway-cold-offline",
                    "identity_after": endpoint.identity(), "models": &offline.models,
                    "request_count": server.received_requests().await.unwrap().len(),
                }));
                assert_eq!(offline.models, vec![model.clone()]);
                assert!(endpoint.identity().is_some());
                assert_eq!(server.received_requests().await.unwrap().len(), before);
            }
            live_models.push(models);
        }
    }
    assert_eq!(server.received_requests().await.unwrap().len(), 3);
    let mut unavailable_info = info.clone();
    unavailable_info.gateway_oauth.as_mut().unwrap().resource = Some(format!("{}/unavailable", server.uri()));
    let unavailable = seed_gateway_auth(&unavailable_info, &primary,
        json!({"access_token": "expired", "refresh_token": "unused", "expires_at": 0}));
    let endpoint = crate::models_endpoint::OpenAiModelsEndpoint::new(
        unavailable_info, Some(primary), Some(Ok(unavailable.clone())),
    );
    endpoint.prepare_catalog_identity().await.unwrap();
    assert!(unavailable.cached_access_token().is_none());
    assert!(endpoint.identity().is_none());
    assert_eq!(server.received_requests().await.unwrap().len(), 3);
}

#[tokio::test]
async fn models_observe_gateway_rotation_from_the_same_provider_instance() {
    models_observe_gateway_rotation(ProviderInstance::Same).await;
}

#[tokio::test]
async fn models_observe_gateway_rotation_from_a_separate_provider_instance() {
    models_observe_gateway_rotation(ProviderInstance::Separate).await;
}

enum ProviderInstance {
    Same,
    Separate,
}

async fn models_observe_gateway_rotation(instance: ProviderInstance) {
    let instance_name = match &instance { ProviderInstance::Same => "same", ProviderInstance::Separate => "separate" };
    let server = MockServer::start().await;
    let home = tempfile::tempdir().unwrap();
    let primary = AuthManager::from_auth_for_testing_with_home(
        CodexAuth::from_api_key("primary-token"),
        home.path().to_path_buf(),
    );
    let info = ModelProviderInfo {
        model_catalog_url: Some(format!("{}/models", server.uri()).into()),
        gateway_oauth: Some(GatewayOAuthConfig {
            authorization_url: format!("{}/authorize", server.uri()),
            token_url: format!("{}/token", server.uri()),
            client_id: "client".into(),
            resource: None,
            scopes: vec![],
            redirect_port: None,
            delivery: GatewayOAuthDelivery::Header {
                name: "x-gateway-auth".into(),
                scheme: "Bearer".into(),
            },
        }),
        requires_openai_auth: true,
        ..ModelProviderInfo::create_openai_provider(Some(server.uri()))
    };
    let gateway = seed_gateway_auth(
        &info,
        &primary,
        json!({"access_token": "first", "refresh_token": "refresh-secret", "expires_at": i64::MAX}),
    );
    let provider = create_model_provider(info.clone(), Some(primary.clone()));
    let models = provider.models_manager_without_cache(/*config_model_catalog*/ None);
    models.set_api_key_model_discovery_enabled(/*enabled*/ true);
    let requests_provider = match instance {
        ProviderInstance::Same => provider,
        ProviderInstance::Separate => create_model_provider(info, Some(primary)),
    };
    let mut first = codex_models_manager::test_support::model_info("test-model");
    first.slug = "catalog-first".into();
    let mut second = first.clone();
    second.slug = "catalog-second".into();
    for (token, model) in [("first", &first), ("second", &second)] {
        Mock::given(method("GET"))
            .and(path("/models"))
            .and(wiremock::matchers::header(
                "x-gateway-auth",
                format!("Bearer {token}"),
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"models": [model]})))
            .expect(1)
            .mount(&server)
            .await;
    }
    assert_eq!(
        requests_provider
            .api_auth()
            .await
            .unwrap()
            .to_auth_headers(),
        HeaderMap::from_iter([
            (
                http::header::AUTHORIZATION,
                HeaderValue::from_static("Bearer primary-token")
            ),
            (
                HeaderName::from_static("x-gateway-auth"),
                HeaderValue::from_static("Bearer first")
            ),
        ])
    );
    models
        .raw_model_catalog(
            RefreshStrategy::Online,
            HttpClientFactory::new(OutboundProxyPolicy::ReqwestDefault),
        )
        .await;
    assert_eq!(models.get_remote_models().await, vec![first.clone()]);
    Mock::given(method("POST"))
        .and(path("/token"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"access_token": "second", "expires_in": 3600})),
        )
        .expect(1)
        .up_to_n_times(1)
        .mount(&server)
        .await;
    gateway.refresh_access_token("first").await.unwrap();
    assert_eq!(
        requests_provider
            .api_auth()
            .await
            .unwrap()
            .to_auth_headers(),
        HeaderMap::from_iter([
            (
                http::header::AUTHORIZATION,
                HeaderValue::from_static("Bearer primary-token")
            ),
            (
                HeaderName::from_static("x-gateway-auth"),
                HeaderValue::from_static("Bearer second")
            ),
        ])
    );
    let after_rotation = models.get_remote_models().await;
    models
        .raw_model_catalog(
            RefreshStrategy::Online,
            HttpClientFactory::new(OutboundProxyPolicy::ReqwestDefault),
        )
        .await;
    let after_fetch = models.get_remote_models().await;
    eprintln!("T18_EVIDENCE {}", json!({
        "kind": "gateway-memory", "instance": instance_name,
        "first_response_body": json!({"models": [&first]}).to_string(),
        "second_response_body": json!({"models": [&second]}).to_string(),
        "after_rotation": &after_rotation, "after_fetch": &after_fetch,
        "requests": server.received_requests().await.unwrap().iter().map(|request| json!({
            "url": request.url.as_str(), "method": request.method.to_string(),
            "headers": request.headers.iter().map(|(name, value)|
                (name.to_string(), String::from_utf8_lossy(value.as_bytes()).into_owned())
            ).collect::<Vec<_>>(),
            "body": String::from_utf8_lossy(&request.body),
        })).collect::<Vec<_>>(),
    }));
    assert_eq!(after_fetch, vec![second]);
    assert!(after_rotation.is_empty(), "previous gateway catalog survived credential replacement");
}

#[tokio::test]
async fn gateway_catalog_response_is_bound_to_the_credential_sent() {
    use codex_models_manager::manager::ModelsEndpointClient;
    use codex_models_manager::manager::ModelsEndpointFuture;
    use codex_models_manager::manager::ModelsEndpointResponse;
    use codex_models_manager::manager::ModelsManager;
    use codex_protocol::error::Result as CoreResult;
    use tokio::io::AsyncBufReadExt;
    use tokio::io::AsyncWriteExt;

    #[derive(Debug)]
    struct ObservedEndpoint {
        inner: crate::models_endpoint::OpenAiModelsEndpoint,
        decoded: Arc<std::sync::atomic::AtomicBool>,
    }
    impl ModelsEndpointClient for ObservedEndpoint {
        fn identity(&self) -> Option<String> {
            self.inner.identity()
        }
        fn prepare_catalog_identity(&self) -> ModelsEndpointFuture<'_, CoreResult<()>> {
            self.inner.prepare_catalog_identity()
        }
        fn list_models<'a>(
            &'a self,
            version: &'a str,
            factory: HttpClientFactory,
        ) -> ModelsEndpointFuture<'a, CoreResult<ModelsEndpointResponse>> {
            Box::pin(async move {
                let response =
                    ModelsEndpointClient::list_models(&self.inner, version, factory).await?;
                eprintln!(
                    "T18_EVIDENCE {}",
                    json!({"kind": "gateway-race-decoded",
                    "response_identity": &response.identity, "owner_identity": self.inner.identity(),
                    "models": response.models.iter().map(|m| &m.slug).collect::<Vec<_>>() })
                );
                self.decoded
                    .store(true, std::sync::atomic::Ordering::SeqCst);
                Ok(response)
            })
        }
    }
    let token_server = MockServer::start().await;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let home = tempfile::tempdir().unwrap();
    let primary = AuthManager::from_auth_for_testing_with_home(
        CodexAuth::from_api_key("unused-primary"),
        home.path().into(),
    );
    let info = ModelProviderInfo {
        model_catalog_url: Some(format!("{base}/models").into()),
        gateway_oauth: Some(GatewayOAuthConfig {
            authorization_url: format!("{}/authorize", token_server.uri()),
            token_url: format!("{}/token", token_server.uri()),
            client_id: "client".into(),
            resource: None,
            scopes: vec![],
            redirect_port: None,
            delivery: GatewayOAuthDelivery::Cookie {
                name: "gateway_session".into(),
            },
        }),
        ..ModelProviderInfo::create_openai_provider(Some(base))
    };
    let gateway = seed_gateway_auth(
        &info,
        &primary,
        json!({"access_token": "first", "refresh_token": "refresh", "expires_at": i64::MAX}),
    );
    codex_login::GatewayLoginControl::for_runtime(&primary.runtime_config())
        .require_explicit_login();
    Mock::given(method("POST"))
        .and(path("/token"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"access_token": "second", "expires_in": 3600})),
        )
        .expect(1)
        .mount(&token_server)
        .await;
    let mut first = codex_models_manager::test_support::model_info("test-model");
    first.slug = "gateway-old".into();
    let mut second = first.clone();
    second.slug = "gateway-new".into();
    let first_body = serde_json::to_vec(&codex_protocol::openai_models::ModelsResponse {
        models: vec![first.clone()],
    })
    .unwrap();
    let second_body = serde_json::to_vec(&codex_protocol::openai_models::ModelsResponse {
        models: vec![second.clone()],
    })
    .unwrap();
    let (seen, mut received) = tokio::sync::mpsc::unbounded_channel();
    let (release, wait) = tokio::sync::oneshot::channel();
    let bodies = [
        (first_body.clone(), Some(wait)),
        (second_body.clone(), None),
    ];
    let server = tokio::spawn(async move {
        for (body, gate) in bodies {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut reader = tokio::io::BufReader::new(&mut stream);
            let mut raw = String::new();
            loop {
                let mut line = String::new();
                assert!(
                    reader.read_line(&mut line).await.unwrap() > 0,
                    "incomplete catalog request"
                );
                raw.push_str(&line);
                if line == "\r\n" {
                    break;
                }
            }
            seen.send(raw).unwrap();
            if let Some(gate) = gate {
                gate.await.unwrap();
            }
            stream.write_all(format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n", body.len()).as_bytes()).await.unwrap();
            stream.write_all(&body).await.unwrap();
            stream.shutdown().await.unwrap();
        }
    });
    let decoded = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let endpoint = Arc::new(ObservedEndpoint {
        inner: crate::models_endpoint::OpenAiModelsEndpoint::new(
            info,
            Some(primary.clone()),
            Some(Ok(gateway.clone())),
        ),
        decoded: decoded.clone(),
    });
    let models = Arc::new(
        codex_models_manager::manager::OpenAiModelsManager::new(
            home.path().into(),
            endpoint.clone(),
            Some(primary),
        )
        .with_provider_catalog(),
    );
    let factory = HttpClientFactory::new(OutboundProxyPolicy::ReqwestDefault);
    let waiting = models.clone();
    let first_factory = factory.clone();
    let fetch = tokio::spawn(async move {
        waiting
            .raw_model_catalog(RefreshStrategy::Online, first_factory)
            .await
    });
    let request_a = tokio::time::timeout(std::time::Duration::from_secs(10), received.recv())
        .await
        .unwrap()
        .unwrap();
    let started = std::time::Instant::now();
    gateway.refresh_access_token("first").await.unwrap();
    let rotation_millis = started.elapsed().as_millis();
    release.send(()).unwrap();
    let after_first = fetch.await.unwrap();
    eprintln!(
        "T18_EVIDENCE {}",
        json!({"kind": "gateway-inflight",
            "request_a": &request_a, "response_a": String::from_utf8_lossy(&first_body),
            "rotation_millis": rotation_millis, "decoded": decoded.load(std::sync::atomic::Ordering::SeqCst),
            "owner_token": gateway.cached_access_token(), "owner_identity": endpoint.identity(),
            "models": &after_first.models, "cache_exists": home.path().join("models_cache.json").exists(),
        })
    );
    assert!(
        decoded.load(std::sync::atomic::Ordering::SeqCst),
        "catalog response did not decode during the controlled race"
    );
    assert!(
        request_a
            .to_ascii_lowercase()
            .contains("cookie: gateway_session=first\r\n")
    );
    assert!(
        !request_a
            .to_ascii_lowercase()
            .contains("\r\nauthorization:")
    );
    assert!(after_first.models.is_empty());
    assert!(models.get_remote_models().await.is_empty());
    assert!(!home.path().join("models_cache.json").exists());
    let after_second = models
        .raw_model_catalog(RefreshStrategy::OnlineIfUncached, factory)
        .await;
    let request_b = tokio::time::timeout(std::time::Duration::from_secs(10), received.recv())
        .await
        .unwrap()
        .unwrap();
    eprintln!(
        "T18_EVIDENCE {}",
        json!({"kind": "gateway-after-race",
            "request_b": &request_b, "response_b": String::from_utf8_lossy(&second_body),
            "models": &after_second.models,
            "cache": std::fs::read_to_string(home.path().join("models_cache.json")).unwrap(),
        })
    );
    assert!(
        request_b
            .to_ascii_lowercase()
            .contains("cookie: gateway_session=second\r\n")
    );
    assert_eq!(after_second.models, vec![second]);
    server.await.unwrap();
}

#[tokio::test]
async fn gateway_setup_errors_are_reported_when_authentication_is_requested() {
    use codex_models_manager::manager::ModelsEndpointClient;

    let server = MockServer::start().await;
    let mut info = ModelProviderInfo {
        gateway_oauth: Some(GatewayOAuthConfig {
            authorization_url: format!("{}/authorize", server.uri()),
            token_url: format!("{}/token", server.uri()),
            client_id: "client".into(),
            resource: None,
            scopes: vec![],
            redirect_port: None,
            delivery: GatewayOAuthDelivery::Header {
                name: "x-gateway-auth".into(),
                scheme: "Bearer".into(),
            },
        }),
        ..ModelProviderInfo::create_openai_provider(Some(server.uri()))
    };
    let provider = create_model_provider(info.clone(), /*auth_manager*/ None);
    assert_eq!(
        provider.gateway_auth_manager().unwrap_err().to_string(),
        "gateway_oauth requires auth runtime configuration"
    );
    let error = provider.api_auth().await.err().unwrap();
    assert_eq!(
        error.to_string(),
        "gateway_oauth requires auth runtime configuration"
    );

    let primary = AuthManager::from_auth_for_testing(CodexAuth::from_api_key("primary"));
    let endpoint = crate::models_endpoint::OpenAiModelsEndpoint::new(
        info.clone(),
        Some(primary),
        Some(Err("failed to create provider OAuth HTTP client".into())),
    );
    let error = endpoint
        .list_models(
            "test",
            HttpClientFactory::new(OutboundProxyPolicy::ReqwestDefault),
        )
        .await
        .unwrap_err();
    assert_eq!(
        error.to_string(),
        "failed to create provider OAuth HTTP client"
    );

    info.gateway_oauth.as_mut().unwrap().client_id.clear();
    let primary = AuthManager::from_auth_for_testing(CodexAuth::from_api_key("primary"));
    let error = create_model_provider(info, Some(primary))
        .api_auth()
        .await
        .err()
        .unwrap();
    assert!(matches!(
        error.details(),
        codex_protocol::error::CodexErrorDetails::InvalidRequest(_)
    ));
    assert!(server.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn gateway_login_states_stop_request_retries() {
    let server = MockServer::start().await;
    let home = tempfile::tempdir().unwrap();
    let primary = AuthManager::from_auth_for_testing_with_home(
        CodexAuth::from_api_key("primary-token"),
        home.path().to_path_buf(),
    );
    let info = ModelProviderInfo {
        gateway_oauth: Some(GatewayOAuthConfig {
            authorization_url: format!("{}/authorize", server.uri()),
            token_url: format!("{}/token", server.uri()),
            client_id: "client".into(),
            resource: None,
            scopes: vec![],
            redirect_port: None,
            delivery: GatewayOAuthDelivery::Header {
                name: "x-gateway-auth".into(),
                scheme: "Bearer".into(),
            },
        }),
        ..ModelProviderInfo::create_openai_provider(Some(server.uri()))
    };
    let login_control = codex_login::GatewayLoginControl::for_runtime(&primary.runtime_config());
    login_control.require_explicit_login();
    let provider = create_model_provider(info, Some(primary));
    let gateway = provider.gateway_auth_manager().unwrap().unwrap();
    let error = provider.api_auth().await.err().expect("login required");
    assert_eq!(
        (error.to_string(), error.retry_delay(/*retry_count*/ 1)),
        (
            codex_login::GatewayAuthError::LoginRequired.to_string(),
            None
        ),
    );
    let cancel = tokio::sync::Notify::new();
    let (url_tx, url_rx) = tokio::sync::oneshot::channel();
    let login = gateway.login_with_browser(cancel.notified(), |_| {
        let _ = url_tx.send(());
    });
    let check = async {
        url_rx.await.unwrap();
        let error = provider.api_auth().await.err().expect("login in progress");
        assert_eq!(
            (error.to_string(), error.retry_delay(/*retry_count*/ 1)),
            (
                codex_login::GatewayAuthError::LoginInProgress.to_string(),
                None
            ),
        );
        cancel.notify_one();
    };
    let (result, ()) = tokio::join!(login, check);
    assert_eq!(result.unwrap_err().kind(), std::io::ErrorKind::Interrupted);
    assert!(server.received_requests().await.unwrap().is_empty());
}
