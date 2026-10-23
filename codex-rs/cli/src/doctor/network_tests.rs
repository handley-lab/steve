use pretty_assertions::assert_eq;
use wiremock::Mock;
use wiremock::MockServer;
use wiremock::ResponseTemplate;
use wiremock::matchers::method;

use super::super::CheckStatus;
use super::super::ProviderAuthReachabilityMode;
use super::super::provider_reachability_check;
use super::super::provider_reachability_plan_from_parts;

#[tokio::test]
async fn provider_reachability_rejects_proxy_authentication_challenges() {
    let proxy = MockServer::start().await;
    Mock::given(method("HEAD"))
        .respond_with(ResponseTemplate::new(407))
        .mount(&proxy)
        .await;
    let plan = provider_reachability_plan_from_parts(
        ProviderAuthReachabilityMode::Chatgpt,
        "openai",
        Some(&proxy.uri()),
        /*provider_query_params*/ None,
    );

    let check = provider_reachability_check(plan).await;
    assert_eq!(check.status, CheckStatus::Fail);
    assert!(
        check
            .details
            .join(" ")
            .contains("proxy authentication required (HTTP 407)")
    );
}

#[tokio::test]
async fn provider_reachability_does_not_follow_redirects() {
    let destination = MockServer::start().await;
    Mock::given(method("HEAD"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&destination)
        .await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&destination)
        .await;
    let direct_plan = provider_reachability_plan_from_parts(
        ProviderAuthReachabilityMode::NotRequired,
        "fixture",
        Some(&destination.uri()),
        None,
    );
    assert_eq!(
        provider_reachability_check(direct_plan).await.status,
        CheckStatus::Ok
    );
    let direct_requests = destination.received_requests().await.unwrap().len();
    assert_eq!(direct_requests, 1);

    let redirect = MockServer::start().await;
    for probe_method in ["HEAD", "GET"] {
        Mock::given(method(probe_method))
            .respond_with(ResponseTemplate::new(302).insert_header("Location", destination.uri()))
            .mount(&redirect)
            .await;
    }
    let redirect_plan = provider_reachability_plan_from_parts(
        ProviderAuthReachabilityMode::NotRequired,
        "fixture",
        Some(&redirect.uri()),
        None,
    );
    assert_eq!(
        provider_reachability_check(redirect_plan).await.status,
        CheckStatus::Ok
    );
    assert_eq!(
        destination.received_requests().await.unwrap().len(),
        direct_requests
    );
}

#[tokio::test]
async fn provider_reachability_http_response_needs_no_catalog_route() {
    for status in [404, 405] {
        let server = MockServer::start().await;
        Mock::given(method("HEAD"))
            .and(wiremock::matchers::path("/v1/responses"))
            .and(wiremock::matchers::query_param("api-version", "fixture"))
            .respond_with(ResponseTemplate::new(status))
            .expect(1)
            .mount(&server)
            .await;
        let query =
            std::collections::HashMap::from([("api-version".to_string(), "fixture".to_string())]);
        let plan = provider_reachability_plan_from_parts(
            ProviderAuthReachabilityMode::ApiKey,
            "configured-provider",
            Some(&format!("{}/v1", server.uri())),
            Some(&query),
        );
        assert_eq!(
            provider_reachability_check(plan).await.status,
            CheckStatus::Ok
        );
        let requests = server.received_requests().await.unwrap();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].method, "HEAD");
        assert_eq!(requests[0].url.path(), "/v1/responses");
        server.verify().await;
    }
}

#[tokio::test]
async fn provider_reachability_reports_transport_failure() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    drop(listener);
    let plan = provider_reachability_plan_from_parts(
        ProviderAuthReachabilityMode::ApiKey,
        "configured-provider",
        Some(&format!("http://{address}/v1")),
        None,
    );
    let check = provider_reachability_check(plan).await;
    assert_eq!(check.status, CheckStatus::Fail);
    assert!(check.details.iter().any(|detail| detail.contains("connect failed")));
}
