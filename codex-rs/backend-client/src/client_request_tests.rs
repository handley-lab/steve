use codex_http_client::HttpClientFactory;
use codex_http_client::OutboundProxyPolicy;
use pretty_assertions::assert_eq;

use super::*;

#[tokio::test]
async fn client_preserves_supplied_factory_and_typed_policy_denial() {
    let client = Client::new(
        "https://example.test",
        HttpClientFactory::new(OutboundProxyPolicy::RespectSystemProxy),
    );
    assert_eq!(
        client.http.outbound_proxy_policy(),
        OutboundProxyPolicy::RespectSystemProxy
    );
}



