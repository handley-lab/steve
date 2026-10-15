use codex_http_client::OutboundProxyPolicy;
use codex_http_client::RouteAwareClientPool;
use std::fmt::Debug;

pub(crate) trait HttpClientSelector: Debug + Send + Sync {
    fn outbound_proxy_policy(&self) -> OutboundProxyPolicy;
}

impl HttpClientSelector for RouteAwareClientPool {
    fn outbound_proxy_policy(&self) -> OutboundProxyPolicy {
        RouteAwareClientPool::outbound_proxy_policy(self)
    }
}
