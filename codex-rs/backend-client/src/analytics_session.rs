//! Local analytics authentication and account/user identity checks.

use codex_login::AuthManager;
use codex_login::AuthManagerConfig;
use codex_login::CodexAuth;
use codex_protocol::account::PlanType;
use std::sync::Arc;

/// Non-secret account metadata associated with an analytics session.
#[derive(Clone, Debug)]
pub struct AnalyticsAccount {
    pub id: String,
    pub email: Option<String>,
    pub plan_type: Option<PlanType>,
}

/// Local ChatGPT account identity captured for Analytics.
pub struct AnalyticsSession {
    auth_manager: Arc<AuthManager>,
    auth: CodexAuth,
    account: AnalyticsAccount,
}

impl AnalyticsSession {
    /// Load local ChatGPT credentials.
    pub async fn from_config(config: &impl AuthManagerConfig) -> Result<Self, String> {
        let auth_manager =
            AuthManager::shared_from_config(config, /*enable_codex_api_key_env*/ false)
                .await
                .map_err(|_| {
                    "Couldn't load local sign-in. Sign in with ChatGPT and retry.".to_string()
                })?;
        let auth = auth_manager
            .auth()
            .await
            .filter(CodexAuth::is_chatgpt_auth)
            .ok_or("Sign in locally with ChatGPT to view Analytics.")?;
        let (Some(id), Some(_)) = (auth.get_account_id(), auth.get_chatgpt_user_id()) else {
            return Err("Analytics requires a ChatGPT account and user identity.".into());
        };
        let account = AnalyticsAccount {
            id,
            email: auth.get_account_email(),
            plan_type: None,
        };
        let session = Self {
            auth_manager,
            auth,
            account,
        };
        session.ensure_identity().await?;
        Ok(session)
    }

    /// Return the account metadata captured when this session was opened.
    pub fn account(&self) -> &AnalyticsAccount {
        &self.account
    }
    /// Reject responses or cached data after a local account or user change.
    pub async fn ensure_identity(&self) -> Result<(), String> {
        self.auth_manager.reload().await;
        let current = self.auth_manager.auth().await;
        if current.is_none_or(|auth| {
            auth.get_account_id() != self.auth.get_account_id()
                || auth.get_chatgpt_user_id() != self.auth.get_chatgpt_user_id()
        }) {
            return Err("Account changed. Press R to refresh Analytics.".into());
        }
        Ok(())
    }
}
