//! Account analytics rows. Nothing requests them.

use codex_backend_openapi_models::models::analytics as models;

/// The bounded set of reports used by consumer and workspace Analytics views.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum AnalyticsReport {
    Usage,
    EnterpriseTokens,
    Credits,
    WorkspaceCredits,
    EnterpriseCredits { breakdown: &'static str },
    Messages,
    Plugins { limit: u8 },
    Skills { limit: u8 },
}

/// Endpoint-specific backend contracts; normalization belongs to the consumer.
#[derive(Clone, Debug, PartialEq)]
pub enum AnalyticsResponse {
    Usage(models::DailyProductSurfaceUsageResponse),
    Credits(models::CreditUsageEventsResponse),
    EnterpriseCredits(models::CurrentUserCreditUsageResponse),
    Messages(models::DailyWorkspaceUsageCountResponse),
    Plugins(models::PluginUsageMetricsResponse),
    Skills(models::DailySkillUsageMetricsResponse),
}

