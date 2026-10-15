//! Remote plugin operation errors.

use crate::remote::RemoteInstalledPluginBundleSyncError;
use crate::remote::RemotePluginCatalogError;
use codex_plugin::PluginId;

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
