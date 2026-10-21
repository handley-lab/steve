//! Routing is limited to the selected ChatGPT backend and successful discovery.

use std::future::Future;
use std::io;
use std::pin::Pin;
use std::sync::Arc;

use crate::ACCOUNT_ROUTING_HEADER;
use crate::WorkspaceRoutingContext;
use codex_login::AuthManager;
use codex_login::CodexAuth;
use codex_login::WorkspaceRouting;
use codex_login::WorkspaceRoutingRequest;
use codex_login::WorkspaceRoutingResolver;
use codex_login::default_client::ClientRedirectPolicy;
use codex_model_provider_info::ModelProviderInfo;
use pretty_assertions::assert_eq;

use crate::create_model_provider;

struct Routing(WorkspaceRouting);

#[test]
fn discovered_backend_cannot_replace_configured_provider_origin() {
    for backend in [
        "https://other.example",
        "https://configured.example:8443",
        "https://api.openai.com",
    ] {
        let mut provider = ModelProviderInfo::create_openai_provider(Some(
            "https://configured.example/backend-api/codex".into(),
        ))
        .to_api_provider(None)
        .unwrap();
        let result = crate::workspace_routing::apply_workspace_routing(
            &mut provider,
            WorkspaceRouting {
                chatgpt_account_id: "account_id".into(),
                backend_origin: backend.into(),
                account_routing_override: "us_cr".into(),
            },
        );
        assert!(result.is_err());
        assert_eq!(
            provider.base_url,
            "https://configured.example/backend-api/codex"
        );
        assert!(!provider.headers.contains_key(ACCOUNT_ROUTING_HEADER));
    }
}

struct ChangedBootstrap {
    first_lookup: std::sync::atomic::AtomicBool,
    release_first: tokio::sync::Notify,
}

impl WorkspaceRoutingResolver for ChangedBootstrap {
    fn resolve(
        &self,
        request: WorkspaceRoutingRequest,
    ) -> Pin<Box<dyn Future<Output = io::Result<Option<WorkspaceRouting>>> + Send + '_>> {
        Box::pin(async move {
            if self
                .first_lookup
                .swap(false, std::sync::atomic::Ordering::Relaxed)
            {
                self.release_first.notified().await;
                Ok(Some(WorkspaceRouting {
                    chatgpt_account_id: "account_id".into(),
                    backend_origin: "https://gov.chatgpt.com".into(),
                    account_routing_override: "us_cr".into(),
                }))
            } else if request.previously_routed {
                Err(io::Error::other("bootstrap changed"))
            } else {
                Ok(None)
            }
        })
    }
}

#[tokio::test]
async fn concurrent_discovery_cannot_forget_established_routing() {
    let context = WorkspaceRoutingContext::new("https://chatgpt.com/backend-api".into());
    let auth =
        AuthManager::from_auth_for_testing(CodexAuth::create_dummy_chatgpt_auth_for_testing());
    let resolver = Arc::new(ChangedBootstrap {
        first_lookup: std::sync::atomic::AtomicBool::new(true),
        release_first: tokio::sync::Notify::new(),
    });
    auth.set_workspace_routing_resolver(Arc::downgrade(
        &(resolver.clone() as Arc<dyn WorkspaceRoutingResolver>),
    ));
    let provider = create_model_provider(
        ModelProviderInfo::create_openai_provider(Some(
            "https://gov.chatgpt.com/backend-api/codex".into(),
        )),
        Some(auth),
    );
    let mut first = provider.responses_api_provider(&context);
    let classifier_context = context.clone();
    let mut second = provider.responses_api_provider(&classifier_context);
    let early_second = std::future::poll_fn(|cx| {
        assert!(first.as_mut().poll(cx).is_pending());
        std::task::Poll::Ready(second.as_mut().poll(cx))
    })
    .await;
    resolver.release_first.notify_one();
    assert_eq!(
        first.await.unwrap().provider.base_url,
        "https://gov.chatgpt.com/backend-api/codex"
    );
    let second = match early_second {
        std::task::Poll::Ready(result) => result,
        std::task::Poll::Pending => second.await,
    };
    assert!(
        second.is_err(),
        "a concurrent lookup must retain the first lookup's routing scope"
    );
}

impl WorkspaceRoutingResolver for Routing {
    fn resolve(
        &self,
        request: WorkspaceRoutingRequest,
    ) -> Pin<Box<dyn Future<Output = io::Result<Option<WorkspaceRouting>>> + Send + '_>> {
        Box::pin(async move {
            let origin = url::Url::parse(&request.provider_base_url)
                .unwrap()
                .origin();
            let bootstrap = url::Url::parse(&request.chatgpt_base_url).unwrap().origin();
            Ok((origin == bootstrap).then(|| self.0.clone()))
        })
    }
}

#[tokio::test]
async fn workspace_routing_preserves_paths_and_excludes_other_providers() {
    let routing_context =
        WorkspaceRoutingContext::new("https://gov.chatgpt.com:8443/backend-api".into());
    let auth =
        AuthManager::from_auth_for_testing(CodexAuth::create_dummy_chatgpt_auth_for_testing());
    let resolver: Arc<dyn WorkspaceRoutingResolver> = Arc::new(Routing(WorkspaceRouting {
        chatgpt_account_id: "account_id".into(),
        backend_origin: "https://gov.chatgpt.com:8443".into(),
        account_routing_override: "us_cr".into(),
    }));
    auth.set_workspace_routing_resolver(Arc::downgrade(&resolver));
    let info = ModelProviderInfo::create_openai_provider(Some(
        "https://gov.chatgpt.com:8443/backend-api/codex".into(),
    ));
    let provider = create_model_provider(info.clone(), Some(auth.clone()));
    let routed = provider
        .responses_api_provider(&routing_context)
        .await
        .unwrap();
    assert_eq!(
        (
            routed.provider.base_url.as_str(),
            routed.provider.headers[ACCOUNT_ROUTING_HEADER]
                .to_str()
                .unwrap(),
            routed.redirect_policy,
        ),
        (
            "https://gov.chatgpt.com:8443/backend-api/codex",
            "us_cr",
            ClientRedirectPolicy::Reject
        )
    );
    let mut custom_auth = info.clone();
    custom_auth.experimental_bearer_token = Some("custom-token".into());
    let mut unrelated_path = info.clone();
    unrelated_path.base_url = Some("https://chatgpt.com/v1".into());
    for info in [custom_auth, unrelated_path] {
        let provider = create_model_provider(info, Some(auth.clone()));
        let ordinary = provider.api_provider().await.unwrap();
        let responses = provider
            .responses_api_provider(&routing_context)
            .await
            .unwrap();
        assert_eq!(
            (
                responses.provider.base_url,
                responses.provider.headers,
                responses.redirect_policy
            ),
            (
                ordinary.base_url,
                ordinary.headers,
                ClientRedirectPolicy::Reject
            )
        );
    }
    let api_auth = AuthManager::from_auth_for_testing(CodexAuth::from_api_key("api-key"));
    api_auth.set_workspace_routing_resolver(Arc::downgrade(&resolver));
    let mut api_info = info;
    api_info.base_url = Some("https://api.openai.com/v1".into());
    let api = create_model_provider(api_info, Some(api_auth))
        .responses_api_provider(&routing_context)
        .await
        .unwrap();
    assert_eq!(api.provider.base_url, "https://api.openai.com/v1");
    assert!(!api.provider.headers.contains_key(ACCOUNT_ROUTING_HEADER));
}

#[tokio::test]
async fn codex_suffix_routes_by_path_and_auth_not_by_display_name() {
    let routing_context =
        WorkspaceRoutingContext::new("https://configured.example/backend-api".into());
    let auth =
        AuthManager::from_auth_for_testing(CodexAuth::create_dummy_chatgpt_auth_for_testing());
    let resolver: Arc<dyn WorkspaceRoutingResolver> = Arc::new(Routing(WorkspaceRouting {
        chatgpt_account_id: "account_id".into(),
        backend_origin: "https://configured.example".into(),
        account_routing_override: "us".into(),
    }));
    auth.set_workspace_routing_resolver(Arc::downgrade(&resolver));
    let mut codex_path = ModelProviderInfo::create_openai_provider(Some(
        "https://configured.example/backend-api/codex".into(),
    ));
    let mut decisions = Vec::new();
    for name in ["OpenAI", "Cambridge"] {
        codex_path.name = name.to_string();
        let routed = create_model_provider(codex_path.clone(), Some(auth.clone()))
            .responses_api_provider(&routing_context)
            .await
            .unwrap();
        decisions.push((
            routed.provider.base_url,
            routed.provider.headers[ACCOUNT_ROUTING_HEADER]
                .to_str()
                .unwrap()
                .to_string(),
            routed.redirect_policy,
        ));
    }
    assert_eq!(decisions[0], decisions[1]);
    assert_eq!(
        decisions[0].0.as_str(),
        "https://configured.example/backend-api/codex"
    );
    assert_eq!(decisions[0].1, "us");

    let mut other_path = codex_path;
    other_path.name = "OpenAI".to_string();
    other_path.base_url = Some("https://configured.example/v1".into());
    let unrouted = create_model_provider(other_path, Some(auth))
        .responses_api_provider(&routing_context)
        .await
        .unwrap();
    assert_eq!(
        unrouted.provider.base_url,
        "https://configured.example/v1"
    );
    assert!(!unrouted.provider.headers.contains_key(ACCOUNT_ROUTING_HEADER));
}

#[tokio::test]
async fn missing_routing_owner_and_wrong_workspace_fail_closed() {
    let routing_context = WorkspaceRoutingContext::new("https://chatgpt.com/backend-api".into());
    let auth =
        AuthManager::from_auth_for_testing(CodexAuth::create_dummy_chatgpt_auth_for_testing());
    let resolver: Arc<dyn WorkspaceRoutingResolver> = Arc::new(Routing(WorkspaceRouting {
        chatgpt_account_id: "other-account".into(),
        backend_origin: "https://chatgpt.com".into(),
        account_routing_override: "NO_CONSTRAINT".into(),
    }));
    auth.set_workspace_routing_resolver(Arc::downgrade(&resolver));
    let provider = create_model_provider(
        ModelProviderInfo::create_openai_provider(Some(
            "https://chatgpt.com/backend-api/codex".into(),
        )),
        Some(auth),
    );
    assert!(
        provider
            .responses_api_provider(&routing_context)
            .await
            .is_err()
    );
    drop(resolver);
    assert!(
        provider
            .responses_api_provider(&routing_context)
            .await
            .is_err()
    );
}
