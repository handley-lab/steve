//! Backend banner payload and CTA mapping.
//!
//! The backend owns eligibility, copy, actions, and ordered model fallback instructions.

use codex_protocol::account::PlanType;
use serde::Deserialize;

mod actions;
mod render;

pub(crate) const LUNA_RESERVE_BANNER: &str = "luna_reserve";
pub(crate) const LUNA_RESERVE_RECOVERY_VIEW_ID: &str = "luna-reserve-recovery";

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub(crate) struct BackendBanner {
    pub(crate) banner_type: String,
    pub(crate) title: String,
    pub(crate) description: String,
    pub(crate) ctas: Vec<BackendBannerCta>,
    pub(crate) reset_at: Option<i64>,
    pub(crate) model_slug: Option<String>,
    pub(crate) blocked_model_slug: Option<String>,
    #[serde(default)]
    pub(crate) fallback_model_slugs: Vec<String>,
    #[serde(default)]
    pub(crate) presentation: BannerPresentation,
    request_url: Option<String>,
    #[serde(skip)]
    pub(crate) account_id: String,
    #[serde(skip)]
    pub(crate) plan_type: Option<PlanType>,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum BannerPresentation {
    #[default]
    Inline,
    Dismissible,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub(crate) struct BackendBannerCta {
    action: String,
    label: String,
}

