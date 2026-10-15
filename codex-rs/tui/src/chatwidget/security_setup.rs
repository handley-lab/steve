//! Presents the optional setup reminder without displacing account usage controls.
//! Dismissal survives banner replacement and account notifications until a different identity is verified.
use super::ChatWidget;

impl ChatWidget {
    pub(crate) fn inherit_security_setup(&mut self, previous: &mut Self) {
        // Startup and thread navigation replace the widget while the request is in flight.
        // Preserve the occurrence, including its rendered/dismissed state.
        self.security_setup_request_id = previous.security_setup_request_id;
        self.security_setup_dismissed = previous.security_setup_dismissed
            || (previous.security_setup_presented
                && previous.bottom_pane.inline_banner_lifecycle().1);
        if previous.security_setup_presented && !self.has_applicable_backend_banner() {
            self.bottom_pane
                .transfer_inline_banner_from(&mut previous.bottom_pane);
            self.security_setup_presented = true;
            previous.security_setup_presented = false;
        }
    }

    pub(crate) fn invalidate_security_setup(&mut self) {
        // AccountUpdated can announce the same account after reconnect. Hide stale content
        // and reject pending results, but retain dismissal until we verify the identity.
        self.security_setup_request_id = uuid::Uuid::new_v4();
        self.clear_security_setup_banner();
    }

    pub(super) fn clear_security_setup_banner(&mut self) {
        // Capture this notice's dismissal before the shared slot is cleared or replaced.
        // Once another banner owns the slot, its lifecycle must not affect this notice.
        if self.security_setup_presented {
            self.security_setup_dismissed |= self.bottom_pane.inline_banner_lifecycle().1;
            self.bottom_pane.set_inline_banner(/*banner*/ None);
            self.security_setup_presented = false;
        }
    }
}
