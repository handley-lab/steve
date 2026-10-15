//! Shared remote plugin mutations for the CLI and app-server.
//! The sync gate covers cache/backend changes and refresh scheduling. An install outcome keeps
//! its bundle protected from pruning until the caller finishes downstream setup and drops it.

use super::EffectivePluginsChangedCallback;
use super::PluginsConfigInput;
use super::PluginsManager;
use crate::remote;
use crate::remote::RemoteInstalledPluginBundleSyncError;
use crate::remote::RemotePluginCatalogError;
use crate::remote_bundle::RemotePluginBundleInstallError;
use codex_login::CodexAuth;
use codex_plugin::PluginId;
use codex_plugin::PluginTelemetryMetadata;
use std::sync::Arc;

#[cfg(test)]
#[path = "remote_mutations_tests.rs"]
mod tests;

pub struct RemotePluginInstallRequest {
    pub marketplace_name: String,
    pub remote_plugin_id: String,
    pub install_attempt_id: Option<String>,
}

pub struct RemotePluginUninstallOutcome {
    pub telemetry: PluginTelemetryMetadata,
    pub effective_plugins_changed: bool,
    /// The backend uninstall succeeded even when removing the local cache failed.
    pub cache_removal_error: Option<RemotePluginCatalogError>,
}

#[derive(Debug, thiserror::Error)]
#[error("{kind}")]
pub struct RemotePluginOperationError {
    pub plugin_id: Option<PluginId>,
    #[source]
    pub kind: Box<RemotePluginOperationErrorKind>,
}

#[derive(Debug, thiserror::Error)]
pub enum RemotePluginOperationErrorKind {
    #[error("{context}: {source}")]
    Catalog {
        context: &'static str,
        source: RemotePluginCatalogError,
    },
    #[error("install remote plugin bundle: {0}")]
    Bundle(#[source] RemotePluginBundleInstallError),
    #[error("{context}: {source}")]
    Sync {
        context: &'static str,
        source: RemoteInstalledPluginBundleSyncError,
    },
    #[error("remote plugin {0} is disabled by admin")]
    DisabledByAdmin(String),
    #[error("remote plugin {0} is not available for install")]
    NotAvailable(String),
    #[error("{0}")]
    InvalidRequest(String),
    #[error("{0}")]
    Internal(String),
}

impl PluginsManager {
    /// POST the remote plugin install.
    pub async fn install_remote_plugin(
        self: &Arc<Self>,
        config: &PluginsConfigInput,
        auth: Option<&CodexAuth>,
        request: RemotePluginInstallRequest,
        on_effective_plugins_changed: Option<EffectivePluginsChangedCallback>,
    ) -> Result<remote::RemotePluginInstallResult, RemotePluginOperationError> {
        use RemotePluginOperationErrorKind as Error;
        let unresolved = |kind| RemotePluginOperationError {
            plugin_id: None,
            kind: Box::new(kind),
        };
        let RemotePluginInstallRequest {
            marketplace_name,
            remote_plugin_id,
            install_attempt_id,
        } = request;
        if !config.plugins_enabled {
            return Err(unresolved(Error::InvalidRequest(format!(
                "remote plugin install is not enabled for marketplace {marketplace_name}"
            ))));
        }
        remote::validate_remote_plugin_id(&remote_plugin_id)
            .map_err(|err| unresolved(Error::InvalidRequest(err.message)))?;
        let service = config.remote_plugin_service_config();
        let _sync_guard = self
            .acquire_remote_installed_plugin_sync_guard()
            .await
            .map_err(|source| {
                unresolved(Error::Sync {
                    context: "failed to coordinate remote plugin install",
                    source,
                })
            })?;
        let install_result = if let Some(install_attempt_id) = install_attempt_id.as_deref() {
            remote::install_remote_plugin_with_install_attempt_id(
                &service,
                auth,
                &marketplace_name,
                &remote_plugin_id,
                install_attempt_id,
            )
            .await
        } else {
            remote::install_remote_plugin(&service, auth, &marketplace_name, &remote_plugin_id)
                .await
        }
        .map_err(|source| {
            unresolved(Error::Catalog {
                context: "install remote plugin",
                source,
            })
        })?;
        let _ = on_effective_plugins_changed;
        Ok(install_result)
    }

    /// POST the remote plugin uninstall.
    pub async fn uninstall_remote_plugin(
        self: &Arc<Self>,
        config: &PluginsConfigInput,
        auth: Option<&CodexAuth>,
        remote_plugin_id: &str,
        on_effective_plugins_changed: Option<EffectivePluginsChangedCallback>,
    ) -> Result<RemotePluginUninstallOutcome, RemotePluginOperationError> {
        use RemotePluginOperationErrorKind as Error;
        let unresolved = |kind| RemotePluginOperationError {
            plugin_id: None,
            kind: Box::new(kind),
        };
        if !config.plugins_enabled {
            return Err(unresolved(Error::InvalidRequest(
                "remote plugin uninstall is not enabled".to_string(),
            )));
        }
        remote::validate_remote_plugin_id(remote_plugin_id)
            .map_err(|err| unresolved(Error::InvalidRequest(err.message)))?;
        let service = config.remote_plugin_service_config();
        let telemetry = PluginTelemetryMetadata {
            plugin_id: None,
            remote_plugin_id: Some(remote_plugin_id.to_string()),
            capability_summary: None,
        };
        let _sync_guard = self
            .acquire_remote_installed_plugin_sync_guard()
            .await
            .map_err(|source| {
                unresolved(Error::Sync {
                    context: "failed to coordinate remote plugin uninstall",
                    source,
                })
            })?;
        remote::uninstall_remote_plugin(&service, auth, remote_plugin_id)
            .await
            .map_err(|source| {
                unresolved(Error::Catalog {
                    context: "uninstall remote plugin",
                    source,
                })
            })?;
        let effective_plugins_changed = self.clear_remote_installed_plugins_cache();
        let _ = on_effective_plugins_changed;
        Ok(RemotePluginUninstallOutcome {
            telemetry,
            effective_plugins_changed,
            cache_removal_error: None,
        })
    }
}
