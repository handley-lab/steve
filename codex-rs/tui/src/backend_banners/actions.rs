//! Map backend CTA names to existing CLI actions and desktop-equivalent browser destinations.

use super::BackendBanner;

pub(super) enum BannerAction {
    OpenUrl(String),
}

impl BackendBanner {
    pub(super) fn resolve_action(&self, action: &str) -> Option<BannerAction> {
        if action != "request_increase" {
            return None;
        }
        let destination = self.request_url.as_deref()?;
        let url = url::Url::parse(destination).ok()?;
        if !matches!(url.scheme(), "https" | "http")
            || url.host_str().is_none()
            || !url.username().is_empty()
            || url.password().is_some()
        {
            return None;
        }
        Some(BannerAction::OpenUrl(url.to_string()))
    }
}
