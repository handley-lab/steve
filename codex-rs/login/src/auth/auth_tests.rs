use super::*;
use crate::auth::storage::get_auth_file;
use codex_protocol::auth::AuthMode;

use codex_protocol::config_types::ForcedLoginMethod;
use codex_protocol::config_types::ModelProviderAuthInfo;
use pretty_assertions::assert_eq;
use serde_json::json;
use std::sync::Arc;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;
use tempfile::TempDir;
use tempfile::tempdir;

#[test]
fn logout_does_not_create_a_missing_file_store() -> std::io::Result<()> {
    let root = tempdir()?;
    let missing_home = root.path().join("missing");
    assert!(!logout(
        &missing_home,
        AuthCredentialsStoreMode::File,
        AuthKeyringBackendKind::default()
    )?);
    assert!(!missing_home.exists());
    Ok(())
}

#[test]
fn logout_removes_auth_file() -> Result<(), std::io::Error> {
    let dir = tempdir()?;
    let auth_dot_json = AuthDotJson {
        auth_mode: Some(AuthMode::ApiKey),
        openai_api_key: Some("sk-test-key".to_string()),
        bedrock_api_key: None,
        bedrock_access_keys: None,
    };
    super::save_auth(
        dir.path(),
        &auth_dot_json,
        AuthCredentialsStoreMode::File,
        AuthKeyringBackendKind::default(),
    )?;
    let auth_file = get_auth_file(dir.path());
    assert!(auth_file.exists());
    assert!(logout(
        dir.path(),
        AuthCredentialsStoreMode::File,
        AuthKeyringBackendKind::default(),
    )?);
    assert!(!auth_file.exists());
    Ok(())
}

#[tokio::test]
async fn provider_credentials_follow_the_primary_network_policy() {
    let script = ProviderAuthScript::new(&["provider-key", "rotated-provider-key"]).unwrap();
    let controller = codex_http_client::NetworkPolicyController::default();
    let policy = controller.policy();
    let factory = HttpClientFactory::new(OutboundProxyPolicy::ReqwestDefault)
        .with_network_policy(policy.clone());
    let mut primary = AuthManager::from_auth_for_testing(CodexAuth::from_api_key("login-key"));
    Arc::get_mut(&mut primary).unwrap().auth_route_config =
        AuthRouteConfig::from_http_client_factory(factory);
    assert!(controller.publish(
        policy.revision(),
        codex_http_client::DestinationPolicy::Restricted {
            allowed_hosts: ["allowed.example".to_string()].into_iter().collect(),
        },
    ));

    let provider =
        AuthManager::external_bearer_with_runtime(script.auth_config(), primary.runtime_config());
    let transport = provider.http_client_factory();
    let allowed = "https://allowed.example/".parse().unwrap();
    let blocked = "https://blocked.example/".parse().unwrap();
    let permit = transport.network_policy().acquire(&allowed).unwrap();
    assert_eq!(
        provider.auth().await.unwrap().api_key(),
        Some("provider-key"),
    );
    permit.check().unwrap();
    let first_owner = provider.auth_owner();

    provider.refresh_token().await.unwrap();
    assert_eq!(
        provider.auth_cached().unwrap().api_key(),
        Some("rotated-provider-key"),
    );
    assert!(!first_owner.is_current());
    permit.check().unwrap();
    assert!(transport.network_policy().acquire(&allowed).is_ok());
    assert!(transport.network_policy().acquire(&blocked).is_err());

    primary.set_cached_auth(Some(CodexAuth::from_api_key("another-login-key")));
    assert!(permit.check().is_err());
    assert!(transport.network_policy().acquire(&allowed).is_err());
}

#[tokio::test]
async fn external_bearer_only_auth_manager_uses_cached_provider_token() {
    let script = ProviderAuthScript::new(&["provider-token", "next-token"]).unwrap();
    let manager = AuthManager::external_bearer_only(script.auth_config());

    let first = manager
        .auth()
        .await
        .and_then(|auth| auth.api_key().map(str::to_string));
    let second = manager
        .auth()
        .await
        .and_then(|auth| auth.api_key().map(str::to_string));

    assert_eq!(first.as_deref(), Some("provider-token"));
    assert_eq!(second.as_deref(), Some("provider-token"));
    assert_eq!(manager.auth_mode(), Some(AuthMode::ApiKey));
    assert_eq!(manager.get_api_auth_mode(), Some(AuthMode::ApiKey));
}

#[tokio::test]
async fn external_bearer_only_auth_manager_disables_auto_refresh_when_interval_is_zero() {
    let script = ProviderAuthScript::new(&["provider-token", "next-token"]).unwrap();
    let mut auth_config = script.auth_config();
    auth_config.refresh_interval_ms = 0;
    let manager = AuthManager::external_bearer_only(auth_config);

    let first = manager
        .auth()
        .await
        .and_then(|auth| auth.api_key().map(str::to_string));
    let second = manager
        .auth()
        .await
        .and_then(|auth| auth.api_key().map(str::to_string));

    assert_eq!(first.as_deref(), Some("provider-token"));
    assert_eq!(second.as_deref(), Some("provider-token"));
}

#[tokio::test]
async fn external_bearer_only_auth_manager_returns_none_when_command_fails() {
    let script = ProviderAuthScript::new_failing().unwrap();
    let manager = AuthManager::external_bearer_only(script.auth_config());

    assert_eq!(manager.auth().await, None);
}

#[tokio::test]
async fn unauthorized_recovery_retries_provider_command_after_initial_failure() {
    let script = ProviderAuthScript::new(&["provider-token"]).unwrap();
    std::fs::write(script.tempdir.path().join("fail-once"), "").unwrap();
    let manager = AuthManager::external_bearer_only(script.auth_config());
    let mut recovery = manager.unauthorized_recovery();

    assert_eq!(manager.auth().await, None);
    assert_eq!(manager.auth_cached(), None);
    assert!(recovery.has_next());
    assert_eq!(recovery.unavailable_reason(), "ready");

    let result = recovery
        .next()
        .await
        .expect("external refresh should succeed");

    assert_eq!(result.auth_state_changed(), Some(true));
    assert_eq!(
        manager.auth_cached(),
        Some(CodexAuth::from_api_key("provider-token"))
    );
    assert!(!recovery.has_next());
    assert_eq!(recovery.unavailable_reason(), "recovery_exhausted");
    recovery.next().await.expect_err("recovery is bounded");
}

#[test]
fn unauthorized_recovery_without_an_external_provider_still_requires_refreshable_auth() {
    for auth in [None, Some(CodexAuth::from_api_key("static-token"))] {
        let manager = AuthManager::from_optional_auth_for_testing(auth);
        let recovery = manager.unauthorized_recovery();

        assert!(!recovery.has_next());
        assert_eq!(recovery.unavailable_reason(), "no_external_auth");
    }
}

#[tokio::test]
async fn unauthorized_recovery_uses_external_refresh_for_bearer_manager() {
    let script = ProviderAuthScript::new(&["provider-token", "refreshed-provider-token"]).unwrap();
    let mut auth_config = script.auth_config();
    auth_config.refresh_interval_ms = 0;
    let manager = AuthManager::external_bearer_only(auth_config);
    let mut recovery = manager.unauthorized_recovery();
    let initial_token = manager
        .auth()
        .await
        .and_then(|auth| auth.api_key().map(str::to_string));

    assert!(recovery.has_next());
    assert_eq!(recovery.mode_name(), "external");
    assert_eq!(recovery.step_name(), "external_refresh");

    let result = recovery
        .next()
        .await
        .expect("external refresh should succeed");

    assert_eq!(result.auth_state_changed(), Some(true));
    let refreshed_token = manager
        .auth()
        .await
        .and_then(|auth| auth.api_key().map(str::to_string));
    assert_eq!(initial_token.as_deref(), Some("provider-token"));
    assert_eq!(refreshed_token.as_deref(), Some("refreshed-provider-token"));
}

#[derive(Clone)]
struct StaticExternalAuth(CodexAuth);

impl ExternalAuth for StaticExternalAuth {
    fn resolve(&self) -> ExternalAuthFuture<'_, CodexAuth> {
        Box::pin(async { Ok(self.0.clone()) })
    }

    fn refresh(&self, _context: ExternalAuthRefreshContext) -> ExternalAuthFuture<'_, CodexAuth> {
        Box::pin(async { Ok(self.0.clone()) })
    }
}

struct FailingExternalAuth {
    auth: CodexAuth,
    resolve_count: AtomicUsize,
}

impl ExternalAuth for FailingExternalAuth {
    fn resolve(&self) -> ExternalAuthFuture<'_, CodexAuth> {
        let resolve_count = self.resolve_count.fetch_add(1, Ordering::SeqCst);
        Box::pin(async move {
            if resolve_count == 0 {
                Ok(self.auth.clone())
            } else {
                Err(std::io::Error::other("external auth failed"))
            }
        })
    }

    fn refresh(&self, _context: ExternalAuthRefreshContext) -> ExternalAuthFuture<'_, CodexAuth> {
        Box::pin(async { Err(std::io::Error::other("external auth failed")) })
    }

    fn classify_error(&self, error: std::io::Error) -> RefreshTokenError {
        RefreshTokenError::Permanent(RefreshTokenFailedError::new(
            RefreshTokenFailedReason::Other,
            error.to_string(),
        ))
    }
}

#[tokio::test]
async fn external_auth_keeps_cached_credentials_after_permanent_reload_failure() {
    let manager = AuthManager::from_auth_for_testing(CodexAuth::from_api_key("seed"));
    let auth = CodexAuth::from_api_key("configured-token");
    let external_auth = Arc::new(FailingExternalAuth {
        auth: auth.clone(),
        resolve_count: AtomicUsize::new(0),
    });
    manager
        .set_external_auth(external_auth.clone())
        .await
        .expect("external auth should install");

    assert_eq!(external_auth.resolve_count.load(Ordering::SeqCst), 1);

    assert_eq!(manager.auth().await, Some(auth.clone()));
    assert_eq!(external_auth.resolve_count.load(Ordering::SeqCst), 2);
    assert_eq!(
        manager
            .refresh_failure_for_auth(&auth)
            .expect("permanent failure should be recorded")
            .to_string(),
        "external auth failed"
    );

    assert_eq!(manager.auth().await, Some(auth));
    assert_eq!(external_auth.resolve_count.load(Ordering::SeqCst), 2);
}

struct RecoveringExternalAuth {
    initial: CodexAuth,
    recovered: CodexAuth,
    resolve_count: AtomicUsize,
}

impl ExternalAuth for RecoveringExternalAuth {
    fn resolve(&self) -> ExternalAuthFuture<'_, CodexAuth> {
        let attempt = self.resolve_count.fetch_add(1, Ordering::SeqCst);
        Box::pin(async move {
            match attempt {
                0 => Ok(self.initial.clone()),
                1 => Err(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    "external credential exchange timed out",
                )),
                _ => Ok(self.recovered.clone()),
            }
        })
    }

    fn refresh(&self, _context: ExternalAuthRefreshContext) -> ExternalAuthFuture<'_, CodexAuth> {
        self.resolve()
    }
}

#[tokio::test]
async fn external_auth_keeps_its_identity_through_a_transient_reload_failure() {
    let manager = AuthManager::from_optional_auth_for_testing(/*auth*/ None);
    let initial = CodexAuth::from_api_key("initial-token");
    let recovered = CodexAuth::from_api_key("recovered-token");
    manager
        .set_external_auth(Arc::new(RecoveringExternalAuth {
            initial: initial.clone(),
            recovered,
            resolve_count: AtomicUsize::new(0),
        }))
        .await
        .expect("external auth should install");
    let mut changes = manager.auth_change_state_receiver();
    let mut revisions = manager.auth_change_receiver();
    let initial_state = *changes.borrow_and_update();
    let initial_revision = *revisions.borrow_and_update();

    let retained = manager
        .auth()
        .await
        .expect("cached auth should be retained");
    assert_eq!(retained.api_key(), Some("initial-token"));
    assert_eq!(*changes.borrow(), initial_state);
    assert!(
        !changes
            .has_changed()
            .expect("auth state sender remains alive")
    );
    assert_eq!(*revisions.borrow(), initial_revision);
    assert!(
        !revisions
            .has_changed()
            .expect("auth revision sender remains alive")
    );
    assert!(manager.refresh_failure_for_auth(&initial).is_none());

    let recovered = manager.auth().await.expect("provider should recover");
    assert_eq!(recovered.api_key(), Some("recovered-token"));
    let cached = manager
        .auth_cached()
        .expect("recovered auth should be cached");
    assert_eq!(cached.api_key(), Some("recovered-token"));
    tokio::time::timeout(std::time::Duration::from_secs(5), changes.changed())
        .await
        .expect("recovery should notify auth state watchers")
        .expect("auth state sender remains alive");
    tokio::time::timeout(std::time::Duration::from_secs(5), revisions.changed())
        .await
        .expect("recovery should notify auth revision watchers")
        .expect("auth revision sender remains alive");
    assert!(changes.borrow().generation > initial_state.generation);
    assert!(*revisions.borrow() > initial_revision);
}

#[tokio::test]
async fn replacing_external_auth_clears_permanent_failure() {
    let manager = AuthManager::from_auth_for_testing(CodexAuth::from_api_key("seed"));
    let auth = CodexAuth::from_api_key("external-token");
    manager
        .set_external_auth(Arc::new(FailingExternalAuth {
            auth: auth.clone(),
            resolve_count: AtomicUsize::new(0),
        }))
        .await
        .expect("external auth should install");

    assert_eq!(manager.auth().await, Some(auth.clone()));
    assert!(manager.refresh_failure_for_auth(&auth).is_some());

    manager
        .set_external_auth(Arc::new(StaticExternalAuth(auth.clone())))
        .await
        .expect("replacement external auth should install");

    manager
        .refresh_token_from_authority()
        .await
        .expect("replacement external auth should refresh");
    assert_eq!(manager.auth_cached(), Some(auth));
}

#[tokio::test]
async fn runtime_external_auth_uses_provider_error_classification() {
    let manager = AuthManager::from_auth_for_testing(CodexAuth::from_api_key("seed"));
    manager
        .set_external_auth(Arc::new(FailingExternalAuth {
            auth: CodexAuth::from_api_key("runtime-token"),
            resolve_count: AtomicUsize::new(0),
        }))
        .await
        .expect("runtime auth should install");

    assert!(matches!(
        manager.refresh_token_from_authority().await,
        Err(RefreshTokenError::Permanent(_))
    ));
}

struct ProviderAuthScript {
    tempdir: TempDir,
    command: String,
    args: Vec<String>,
}

impl ProviderAuthScript {
    fn new(tokens: &[&str]) -> std::io::Result<Self> {
        let tempdir = tempfile::tempdir()?;
        let token_file = tempdir.path().join("tokens.txt");
        // `cmd.exe`'s `set /p` treats LF-only input as one line, so use CRLF on Windows.
        let token_line_ending = if cfg!(windows) { "\r\n" } else { "\n" };
        let mut token_file_contents = String::new();
        for token in tokens {
            token_file_contents.push_str(token);
            token_file_contents.push_str(token_line_ending);
        }
        std::fs::write(&token_file, token_file_contents)?;

        #[cfg(unix)]
        let (command, args) = {
            let script_path = tempdir.path().join("print-token.sh");
            codex_utils_cargo_bin::write_executable(
                &script_path,
                r#"#!/bin/sh
if [ -f fail-once ]; then
    rm fail-once
    exit 1
fi
first_line=$(sed -n '1p' tokens.txt)
printf '%s\n' "$first_line"
tail -n +2 tokens.txt > tokens.next
mv tokens.next tokens.txt
"#,
            )?;
            ("./print-token.sh".to_string(), Vec::new())
        };

        #[cfg(windows)]
        let (command, args) = {
            let script_path = tempdir.path().join("print-token.cmd");
            std::fs::write(
                &script_path,
                r#"@echo off
setlocal EnableExtensions DisableDelayedExpansion
if exist fail-once (
    del fail-once
    exit /b 1
)
set "first_line="
<tokens.txt set /p "first_line="
if not defined first_line exit /b 1
setlocal EnableDelayedExpansion
echo(!first_line!
endlocal
more +1 tokens.txt > tokens.next
move /y tokens.next tokens.txt >nul
"#,
            )?;
            (
                "cmd.exe".to_string(),
                vec![
                    "/d".to_string(),
                    "/s".to_string(),
                    "/c".to_string(),
                    ".\\print-token.cmd".to_string(),
                ],
            )
        };

        Ok(Self {
            tempdir,
            command,
            args,
        })
    }

    fn new_failing() -> std::io::Result<Self> {
        let tempdir = tempfile::tempdir()?;

        #[cfg(unix)]
        let (command, args) = {
            let script_path = tempdir.path().join("fail.sh");
            codex_utils_cargo_bin::write_executable(
                &script_path,
                r#"#!/bin/sh
exit 1
"#,
            )?;
            ("./fail.sh".to_string(), Vec::new())
        };

        #[cfg(windows)]
        let (command, args) = (
            "cmd.exe".to_string(),
            vec![
                "/d".to_string(),
                "/s".to_string(),
                "/c".to_string(),
                "exit /b 1".to_string(),
            ],
        );

        Ok(Self {
            tempdir,
            command,
            args,
        })
    }

    fn auth_config(&self) -> ModelProviderAuthInfo {
        serde_json::from_value(json!({
            "command": self.command,
            "args": self.args,
            // Process startup can be slow on loaded Windows CI workers, so leave enough slack to
            // avoid turning these auth-cache assertions into a process-launch timing test.
            "timeout_ms": 10_000,
            "refresh_interval_ms": 60000,
            "cwd": self.tempdir.path(),
        }))
        .expect("provider auth config should deserialize")
    }
}

struct TestAuthManagerConfig(AuthConfig);

impl AuthManagerConfig for TestAuthManagerConfig {
    fn codex_home(&self) -> PathBuf {
        self.0.codex_home.clone()
    }

    fn cli_auth_credentials_store_mode(&self) -> AuthCredentialsStoreMode {
        self.0.auth_credentials_store_mode
    }

    fn auth_keyring_backend_kind(&self) -> AuthKeyringBackendKind {
        self.0.keyring_backend_kind
    }

    fn forced_login_method(&self) -> Option<ForcedLoginMethod> {
        self.0.forced_login_method
    }

    fn forced_chatgpt_workspace_id(&self) -> Option<Vec<String>> {
        self.0.forced_chatgpt_workspace_id.clone()
    }

    fn managed_auth_policy(&self) -> ManagedAuthPolicy {
        self.0.managed_auth_policy.clone()
    }

    fn chatgpt_base_url(&self) -> String {
        self.0
            .chatgpt_base_url
            .clone()
            .expect("test config should include a ChatGPT base URL")
    }

    fn account_path_style(&self) -> codex_config::AccountPathStyle {
        codex_config::AccountPathStyle::CodexApi
    }

    fn auth_route_config(&self) -> AuthRouteConfig {
        self.0.auth_route_config.clone()
    }
}

fn test_auth_manager_config(codex_home: &Path) -> TestAuthManagerConfig {
    TestAuthManagerConfig(AuthConfig {
        codex_home: codex_home.to_path_buf(),
        auth_credentials_store_mode: AuthCredentialsStoreMode::File,
        keyring_backend_kind: AuthKeyringBackendKind::Direct,
        forced_login_method: Some(ForcedLoginMethod::Chatgpt),
        chatgpt_base_url: Some("https://chatgpt-staging.com/backend-api".to_string()),
        forced_chatgpt_workspace_id: Some(vec!["forced-workspace".to_string()]),
        managed_auth_policy: ManagedAuthPolicy {
            allowed_login_methods: Some(vec![ForcedLoginMethod::Chatgpt]),
            allowed_chatgpt_workspaces: Some(vec![
                "forced-workspace".to_string(),
                "managed-workspace".to_string(),
            ]),
        },
        auth_route_config: AuthRouteConfig::from_http_client_factory(HttpClientFactory::new(
            OutboundProxyPolicy::RespectSystemProxy,
        )),
    })
}

#[test]
fn auth_config_from_preserves_all_fields() {
    let codex_home = tempdir().expect("tempdir");
    let config = test_auth_manager_config(codex_home.path());

    assert_eq!(auth_config_from(&config), config.0);
}
