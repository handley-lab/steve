//! Binds a transport connection to the authentication owner that established it.
//! Owner revisions invalidate queued work even before transport closure is delivered.

use codex_login::AuthChangeState;
use tokio::sync::watch;

#[derive(Clone, Debug)]
pub struct ConnectionAuth {
    changes: watch::Receiver<AuthChangeState>,
    owner_generation: u64,
}

impl ConnectionAuth {
    pub fn is_current(&self) -> bool {
        self.changes.borrow().owner_generation == self.owner_generation
            && self.changes.has_changed().is_ok()
    }
}
