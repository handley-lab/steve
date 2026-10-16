//! Seven-day allowance snapshots. Nothing requests them.

use serde::Deserialize;

#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct PlanLimitHistory {
    pub data_as_of: Option<String>,
    pub coverage_start: Option<String>,
    pub coverage_complete: bool,
    #[serde(default = "approximate_by_default")]
    pub approximate: bool,
    pub boundary_tolerance_seconds: Option<u32>,
    pub periods: Vec<PlanLimitPeriod>,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct PlanLimitPeriod {
    pub id: String,
    pub window_minutes: u32,
    pub plan_type: String,
    pub starts_at: String,
    pub ends_at: String,
    pub accounting_complete: bool,
    /// Hundredths of a percent of this period's historical allowance; null is unknown.
    pub used_basis_points: Option<f64>,
    pub breakdowns: Option<Vec<PlanLimitBreakdown>>,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PlanLimitDimension {
    ThreadSource,
    TurnTrigger,
    Model,
    Surface,
    /// Additive server dimensions must not discard the supported breakdowns.
    #[serde(other)]
    Unknown,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct PlanLimitBreakdown {
    pub dimension: PlanLimitDimension,
    pub rows: Vec<PlanLimitValue>,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct PlanLimitValue {
    pub key: String,
    pub basis_points: f64,
}

fn approximate_by_default() -> bool {
    true
}

