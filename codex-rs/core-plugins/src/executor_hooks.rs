//! Executor-discovered plugin hook admission and trusted MCP routing.

use codex_config::HookHandlerConfig;
use codex_config::types::PluginsConfigToml;
use codex_exec_server::DiscoveredPluginFiles;
use codex_exec_server::ExecutorCapabilityDiscoverySnapshot;
use codex_exec_server::ExecutorCapabilityDiscoveryV2Snapshot;
use codex_plugin::ExecutorPluginHookSource;
use codex_plugin::PluginId;
use codex_plugin::is_allowlisted_bundled_cleanup_hook;
use codex_plugin::manifest::PluginManifestHooks;
use codex_protocol::capabilities::CapabilityRootLocation;
use codex_utils_path_uri::PathUri;

use crate::manifest::parse_plugin_manifest_uri;

/// Returns accepted inline hook sources from executor-discovered plugin manifests.
///
/// Executor scoped hooks are best-effort because executor capabilities can become available
/// after earlier lifecycle events have passed.
///
/// Note: Executor manifests are not signed yet, so temporarily we only admit the known cleanup
/// hooks from the bundled cleanup allowlist and the remote Browser plugin.
pub fn executor_plugin_hook_sources(
    snapshot: &ExecutorCapabilityDiscoverySnapshot,
    plugins_config: &PluginsConfigToml,
) -> Vec<ExecutorPluginHookSource> {
    let plugins = snapshot.roots().iter().filter_map(|entry| {
        let CapabilityRootLocation::Environment {
            environment_id,
            path: plugin_root,
        } = &entry.selected_root.location;
        let plugin = entry.result.as_ref().ok()?.plugin.as_ref()?;
        Some((
            entry.selected_root.id.as_str(),
            environment_id.as_str(),
            plugin_root,
            plugin,
        ))
    });
    admitted_hook_sources(plugins, plugins_config)
}

/// Applies the same inline-hook admission and MCP routing policy to V2 inventories.
/// Callers must additionally restrict sources to their accepted plugin installations.
pub fn executor_plugin_hook_sources_v2(
    snapshots: &[ExecutorCapabilityDiscoveryV2Snapshot],
    plugins_config: &PluginsConfigToml,
) -> Vec<ExecutorPluginHookSource> {
    let plugins = snapshots.iter().flat_map(|snapshot| {
        snapshot.result.iter().flat_map(move |inventory| {
            inventory.plugins.iter().map(move |plugin| {
                (
                    plugin.id.as_str(),
                    snapshot.environment_id.as_str(),
                    &plugin.root,
                    &plugin.files,
                )
            })
        })
    });
    admitted_hook_sources(plugins, plugins_config)
}

fn admitted_hook_sources<'b>(
    plugins: impl Iterator<Item = (&'b str, &'b str, &'b PathUri, &'b DiscoveredPluginFiles)>,
    plugins_config: &PluginsConfigToml,
) -> Vec<ExecutorPluginHookSource> {
    let mut sources = Vec::new();
    for (plugin_id, environment_id, plugin_root, plugin) in plugins {
        if !plugins_config.allows_plugin(plugin_id) {
            continue;
        }
        let Ok(plugin_id) = PluginId::parse(plugin_id) else {
            continue;
        };
        let Ok(manifest) = parse_plugin_manifest_uri(
            plugin_root,
            &plugin.manifest.path,
            &plugin.manifest.contents,
        ) else {
            continue;
        };
        // Only inline hooks are supported for now, so skip any other source types.
        let Some(PluginManifestHooks::Inline(hook_files)) = manifest.paths.hooks else {
            continue;
        };

        for (hook_index, hook_file) in hook_files.into_iter().enumerate() {
            let manifest_relative_path = plugin
                .manifest
                .path
                .relative_path_from(plugin_root)
                .unwrap_or_else(|| plugin.manifest.path.to_string());

            sources.push(ExecutorPluginHookSource {
                plugin_id: plugin_id.clone(),
                environment_id: environment_id.to_string(),
                mcp_environment_id: None,
                mcp_metadata: None,
                plugin_root: plugin_root.clone(),
                manifest_path: plugin.manifest.path.clone(),
                source_relative_path: format!("{manifest_relative_path}#hooks[{hook_index}]"),
                hooks: hook_file.hooks,
            });
        }
    }

    // FIXME: Remove this temporary filter once executor plugin hooks can be trusted.
    sources
        .into_iter()
        .filter_map(|mut source| {
            let plugin_id = source.plugin_id.as_key();
            for (event, groups) in source.hooks.matcher_groups_mut() {
                groups.retain_mut(|group| {
                    group.hooks.retain(|handler| {
                        is_allowlisted_bundled_cleanup_hook(
                            &plugin_id,
                            event,
                            group.matcher.as_deref(),
                            handler,
                            /*app_connector_id*/ None,
                        )
                    });
                    !group.hooks.is_empty()
                });
            }
            (!source.hooks.is_empty()).then_some(source)
        })
        .filter_map(resolve_mcp_routing)
        .collect()
}

/// Resolves routing for an admitted MCP hook source.
fn resolve_mcp_routing(mut source: ExecutorPluginHookSource) -> Option<ExecutorPluginHookSource> {
    let HookHandlerConfig::McpTool { .. } = source
        .hooks
        .matcher_groups_mut()
        .into_iter()
        .find_map(|(_, groups)| groups.first())?
        .hooks
        .first()?
    else {
        return None;
    };
    Some(source)
}

#[cfg(test)]
#[path = "executor_hooks_tests.rs"]
mod tests;
