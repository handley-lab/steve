use serde::Deserialize;

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

