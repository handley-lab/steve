//! Local Analytics account session. Nothing requests account analytics.

use super::models::AccountAnalyticsGrouping as Grouping;
use crate::legacy_core::config::Config;
use codex_backend_client::AnalyticsSession;
use std::sync::Arc;
use tokio::sync::OnceCell;

pub(super) struct Live {
    pub(super) identity_invalidated: std::sync::atomic::AtomicBool,
    config: Arc<Config>,
    session: OnceCell<Session>,
    token_models: std::sync::RwLock<Vec<String>>,
    attributed_usage: std::sync::atomic::AtomicBool,
}

pub(super) struct Session {
    pub(super) backend: AnalyticsSession,
    credit_groups: Vec<usize>,
}

impl Live {
    pub(super) fn new(config: Arc<Config>) -> Self {
        Self {
            identity_invalidated: std::sync::atomic::AtomicBool::new(/*v*/ false),
            config,
            session: OnceCell::new(),
            token_models: std::sync::RwLock::new(Vec::new()),
            attributed_usage: std::sync::atomic::AtomicBool::new(/*v*/ true),
        }
    }

    pub(super) fn account_label(&self) -> Option<String> {
        let session = self.session.get()?;
        session.backend.account().email.clone()
    }

    pub(super) fn credit_groups(&self) -> &[usize] {
        self.session
            .get()
            .map_or(&[0], |session| &session.credit_groups)
    }

    pub(super) fn attributed_usage(&self) -> bool {
        self.attributed_usage
            .load(std::sync::atomic::Ordering::Relaxed)
    }

    pub(super) fn token_models(&self) -> Vec<String> {
        self.token_models
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    pub(super) async fn session(&self) -> Result<&Session, String> {
        self.session
            .get_or_try_init(|| async {
                let session = AnalyticsSession::from_config(self.config.as_ref()).await?;
                let credit_groups = Grouping::credit_groupings(session.account().plan_type)
                    .iter()
                    .filter_map(|group| {
                        super::data::GROUPINGS
                            .iter()
                            .position(|candidate| candidate == group)
                    })
                    .collect();
                Ok(Session {
                    credit_groups,
                    backend: session,
                })
            })
            .await
    }
}

#[cfg(test)]
#[path = "client_tests.rs"]
pub(super) mod tests;

#[cfg(test)]
#[path = "connection_tests.rs"]
mod connection_tests;
