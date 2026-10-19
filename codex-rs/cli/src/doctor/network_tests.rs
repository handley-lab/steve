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
        "OpenAI",
        /*provider_base_url*/ None,
        /*provider_query_params*/ None,
        /*is_amazon_bedrock*/ false,
        &format!("{}/backend-api/", proxy.uri()),
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
        "Fixture",
        Some(&destination.uri()),
        None,
        false,
        "",
    );
    assert_eq!(
        provider_reachability_check(direct_plan).await.status,
        CheckStatus::Ok
    );
    let direct_requests = destination.received_requests().await.unwrap().len();
    assert_eq!(direct_requests, 2);

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
        "Fixture",
        Some(&redirect.uri()),
        None,
        false,
        "",
    );
    assert_eq!(
        provider_reachability_check(redirect_plan).await.status,
        CheckStatus::Warning
    );
    assert_eq!(
        destination.received_requests().await.unwrap().len(),
        direct_requests
    );
}
