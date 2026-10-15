use super::*;
use codex_utils_absolute_path::AbsolutePathBuf;
use serde::Deserialize;
use serde::Serialize;
use std::collections::BTreeMap;
use std::io;
use std::path::Path;

mod local_paths;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum RemotePluginShareDiscoverability {
    Listed,
    Unlisted,
    Private,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RemotePluginSharePrincipalType {
    User,
    Group,
    Workspace,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct RemotePluginSharePrincipal {
    pub principal_type: RemotePluginSharePrincipalType,
    pub principal_id: String,
    pub role: RemotePluginSharePrincipalRole,
    pub name: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RemotePluginSharePrincipalRole {
    Reader,
    Editor,
    Owner,
}

pub fn load_plugin_share_remote_ids_by_local_path(
    codex_home: &Path,
) -> io::Result<BTreeMap<AbsolutePathBuf, String>> {
    let local_paths = local_paths::load_plugin_share_local_paths(codex_home)?;
    local_paths
        .into_iter()
        .map(|(remote_plugin_id, local_plugin_path)| {
            if !is_valid_remote_plugin_id(&remote_plugin_id) {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!(
                        "invalid remote plugin id in share local path mapping: {remote_plugin_id}"
                    ),
                ));
            }
            Ok((local_plugin_path, remote_plugin_id))
        })
        .collect()
}
