//! Thread usage totals carried by analytics rows. Nothing requests them.

use serde::Deserialize;

/// Backend usage grouped by model, reasoning effort, and response speed.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub struct ThreadUsageBreakdownGroup {
    pub model: Option<String>,
    pub reasoning_effort: Option<String>,
    pub speed: Option<String>,
    pub estimated_usage_credits_micros: i64,
    pub native_usage_usd_micros: Option<i64>,
    pub net_new_input_tokens: Option<i64>,
    pub cached_input_tokens: Option<i64>,
    pub input_tokens: Option<i64>,
    pub output_tokens: Option<i64>,
    pub total_tokens: Option<i64>,
}

/// Backend usage totals expressed in integer millionths of their respective units.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub struct ThreadUsage {
    pub thread_id: String,
    pub estimated_usage_credits_micros: i64,
    pub estimated_usage_usd_micros: Option<i64>,
    pub native_usage_usd_micros: Option<i64>,
    #[serde(default)]
    pub groups: Vec<ThreadUsageBreakdownGroup>,
}
