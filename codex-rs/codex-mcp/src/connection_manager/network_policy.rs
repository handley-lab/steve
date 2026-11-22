//! Removes caller-supplied network policy metadata from MCP calls.

use super::McpConnectionSet;
use crate::MCP_NETWORK_POLICY_META_CAPABILITY;
use codex_network_proxy::NetworkMode;
use rmcp::model::ServerCapabilities;
use serde::Serialize;
use serde_json::Value;

/// Effective network restrictions delivered with an MCP call, without proxy endpoints or credentials.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NetworkPolicyMetadata {
    pub enabled: bool,
    pub mode: NetworkMode,
    pub allowed_domains: Vec<String>,
    pub denied_domains: Vec<String>,
    pub hard_deny_allowlist_misses: bool,
}

impl McpConnectionSet {
    /// Callers cannot forge the host-owned network policy key.
    pub(crate) fn network_policy_meta_for_call(
        &self,
        mut meta: Option<Value>,
    ) -> impl Fn(&ServerCapabilities) -> anyhow::Result<Option<Value>> + Send + Sync {
        if let Some(Value::Object(meta)) = meta.as_mut() {
            meta.remove(MCP_NETWORK_POLICY_META_CAPABILITY);
        }
        move |_capabilities| Ok(meta.clone())
    }
}
