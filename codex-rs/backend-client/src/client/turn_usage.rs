use super::Client;
use super::RequestError;
use http::HeaderMap;
use http::Method;
use http::header::CONTENT_TYPE;
use http::header::HeaderValue;
use serde::Deserialize;
use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ApiKeyTurnCostStatus {
    Pending,
    Priced,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct ApiKeyResponseCost {
    pub response_id: String,
    pub total_usd: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct ApiKeyTurnCost {
    pub turn_id: String,
    pub status: ApiKeyTurnCostStatus,
    pub total_usd: Option<String>,
    pub event_count: Option<u64>,
    pub responses: Option<Vec<ApiKeyResponseCost>>,
    pub model: Option<String>,
    pub speed: Option<String>,
    pub reasoning_effort: Option<String>,
}

#[derive(Serialize)]
struct ApiKeyTurnCostsRequest<'a> {
    turn_ids: &'a [String],
}

#[derive(Deserialize)]
struct ApiKeyTurnCostsResponse {
    turns: Vec<ApiKeyTurnCost>,
}

impl Client {
    /// Queries an API-key turn-cost endpoint chosen by the caller, attaching
    /// the supplied provider headers in addition to this client's auth.
    pub async fn query_api_key_turn_costs_at(
        &self,
        url: &str,
        turn_ids: &[String],
        provider_headers: &HeaderMap,
    ) -> Result<Vec<ApiKeyTurnCost>, RequestError> {
        let mut headers = provider_headers.clone();
        headers.extend(self.headers());
        let request = self
            .request(Method::POST, url)
            .headers(headers)
            .header(CONTENT_TYPE, HeaderValue::from_static("application/json"))
            .json(&ApiKeyTurnCostsRequest { turn_ids });
        let (body, content_type) = self.exec_request_detailed(request, "POST", url).await?;
        let response: ApiKeyTurnCostsResponse = self
            .decode_json(url, &content_type, &body)
            .map_err(RequestError::Other)?;
        Ok(response.turns)
    }
}

#[cfg(test)]
mod tests {
    use super::Client;
    use codex_http_client::HttpClientFactory;
    use codex_http_client::OutboundProxyPolicy;
    use codex_login::CodexAuth;
    use http::HeaderMap;
    use http::HeaderValue;
    use pretty_assertions::assert_eq;
    use wiremock::Mock;
    use wiremock::MockServer;
    use wiremock::ResponseTemplate;
    use wiremock::matchers::header;
    use wiremock::matchers::method;
    use wiremock::matchers::path;

    #[tokio::test]
    async fn custom_turn_cost_queries_apply_client_auth_after_provider_headers() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/analytics/codex/turn-costs"))
            .and(header("authorization", "Bearer sk-test"))
            .and(header("chatgpt-account-id", "account-test"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "turns": []
            })))
            .expect(1)
            .mount(&server)
            .await;

        let auth = CodexAuth::from_api_key("sk-test");
        let client = Client::from_auth(
            server.uri(),
            &auth,
            HttpClientFactory::new(OutboundProxyPolicy::ReqwestDefault),
        )
        .with_chatgpt_account_id("account-test");
        let mut provider_headers = HeaderMap::new();
        provider_headers.insert(
            "authorization",
            HeaderValue::from_static("Bearer provider-override"),
        );
        provider_headers.insert(
            "chatgpt-account-id",
            HeaderValue::from_static("provider-account-override"),
        );
        let costs = client
            .query_api_key_turn_costs_at(
                &format!("{}/analytics/codex/turn-costs", server.uri()),
                &["turn-one".to_string()],
                &provider_headers,
            )
            .await
            .expect("query custom-provider turn costs");

        assert_eq!(costs, Vec::new());
    }
}
