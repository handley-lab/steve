//! Tracks whether a rate-limit recovery is still pending for the current account.

#[derive(Default)]
pub(super) struct RateLimitRefreshState {
    recovery: bool,
}

impl RateLimitRefreshState {
    pub(super) fn has_pending_recovery(&self) -> bool {
        self.recovery
    }

    pub(super) fn invalidate_recovery(&mut self) {
        self.recovery = false;
    }
}
