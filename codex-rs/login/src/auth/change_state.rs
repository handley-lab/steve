//! Tracks credential changes separately from ownership changes, before notifications coalesce.

use super::AuthManager;
use std::sync::Arc;

/// An account owner within a particular credential source, retained by in-flight work.
#[derive(Clone)]
pub struct AuthOwner {
    manager: Arc<AuthManager>,
    generation: u64,
}

impl AuthOwner {
    pub(super) fn new(manager: Arc<AuthManager>, generation: u64) -> Self {
        Self {
            manager,
            generation,
        }
    }

    /// Watch credential changes from the source that owns this work.
    pub fn changes(&self) -> tokio::sync::watch::Receiver<AuthChangeState> {
        self.manager.auth_change_state_receiver()
    }

    pub fn is_current(&self) -> bool {
        self.changes().borrow().owner_generation == self.generation
    }
}

impl PartialEq for AuthOwner {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.manager, &other.manager) && self.generation == other.generation
    }
}

impl Eq for AuthOwner {}

impl std::fmt::Debug for AuthOwner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AuthOwner")
            .field("generation", &self.generation)
            .finish_non_exhaustive()
    }
}

/// Opaque revisions local to one auth manager. Consumers must reset on reconnect.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AuthChangeState {
    /// Advances on cached credential changes and explicit invalidation of pending reads.
    pub generation: u64,
    /// Advances on login, logout, or changes of user, workspace, service account, or auth mode.
    /// Credential changes with incomplete owner identity also advance this revision.
    /// Losing login metadata alone preserves the account owner.
    pub owner_generation: u64,
    /// Advances on an underlying actor credential/source change, including a new OAuth login.
    /// Renewal within an identifiable OAuth session preserves this revision.
    pub source_generation: u64,
    /// Preserves strict login ownership for EMA, including loss of login metadata.
    pub login_generation: u64,
}

/// Whether request preparation can use the credentials it selected.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RequestAuthChange {
    Unchanged,
    CredentialsChanged,
    OwnerChanged,
}

impl AuthChangeState {
    /// Compare a completed attempt with its original owner and starting credential revision.
    pub fn compare_for_request(&self, expected_owner: u64, prepared: Self) -> RequestAuthChange {
        if self.owner_generation != expected_owner {
            RequestAuthChange::OwnerChanged
        } else if self.generation != prepared.generation {
            RequestAuthChange::CredentialsChanged
        } else {
            RequestAuthChange::Unchanged
        }
    }
}
