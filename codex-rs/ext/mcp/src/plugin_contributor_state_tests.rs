//! Tests thread-owned discovery caching, installation ordering, and contribution projection.

use super::*;
use codex_exec_server::CapabilityTextFile;
use codex_exec_server::DiscoverV2CapabilitiesResponse;
use codex_exec_server::DiscoveredPluginFiles;
use codex_exec_server::ExecutorPlugin;
use codex_utils_path_uri::PathUri;
use pretty_assertions::assert_eq;
use std::sync::Arc;

fn executor_snapshot(
    environment_id: &str,
) -> anyhow::Result<ExecutorCapabilityDiscoveryV2Snapshot> {
    let root = PathUri::parse("file:///plugins/example/1.0.0")?;
    Ok(ExecutorCapabilityDiscoveryV2Snapshot {
        environment_id: environment_id.to_string(),
        result: Ok(Arc::new(DiscoverV2CapabilitiesResponse {
            plugins: vec![ExecutorPlugin {
                id: "example@marketplace".to_string(),
                display_name: "Example".to_string(),
                remote_plugin_id: Some("plugin_a".to_string()),
                version: "1.0.0".to_string(),
                files: DiscoveredPluginFiles {
                    manifest: CapabilityTextFile {
                        path: root.join(".codex-plugin/plugin.json")?,
                        contents: r#"{"name":"example"}"#.to_string(),
                    },
                    mcp_config: None,
                    apps_config: None,
                },
                root,
                skills: Vec::new(),
            }],
            ..Default::default()
        })),
    })
}

#[test]
fn executor_refresh_preserves_order_and_removes_stale_entries() -> anyhow::Result<()> {
    let mut state = PluginContributorState::default();
    // Reuse equal snapshots, including equal responses allocated by a later discovery.
    let b = executor_snapshot("executor-b")?;
    let c = executor_snapshot("executor-c")?;
    state.update_executor_catalog(&[b.clone(), c.clone()]);
    state.update_executor_catalog(&[executor_snapshot("executor-b")?, c.clone()]);
    assert!(Arc::ptr_eq(
        state.executor_snapshots[0].result.as_ref().unwrap(),
        b.result.as_ref().unwrap(),
    ));

    // Keep B ahead of C even when discovery reverses them and adds an earlier-named A.
    let a = executor_snapshot("executor-a")?;
    let mut updated_b = b;
    Arc::make_mut(updated_b.result.as_mut().unwrap()).plugins[0].display_name =
        "Updated".to_string();
    state.update_executor_catalog(&[a.clone(), c.clone(), updated_b.clone()]);
    assert_eq!(
        state.executor_catalog.as_ref().unwrap(),
        &catalog_from_snapshots(&[updated_b.clone(), c.clone(), a.clone()]),
    );
    // Removing B promotes C; B returning later appends it rather than displacing C.
    state.update_executor_catalog(&[a.clone(), c.clone()]);
    state.update_executor_catalog(&[updated_b.clone(), a.clone(), c.clone()]);
    assert_eq!(
        state.executor_catalog.as_ref().unwrap(),
        &catalog_from_snapshots(&[c.clone(), a.clone(), updated_b]),
    );
    // A failed scan removes that environment's entries, and recovery rebuilds them.
    let failed = ExecutorCapabilityDiscoveryV2Snapshot {
        environment_id: c.environment_id.clone(),
        result: Err("discovery failed".to_string()),
    };
    state.update_executor_catalog(&[failed.clone(), a.clone()]);
    assert_eq!(
        state.executor_catalog.as_ref().unwrap(),
        &catalog_from_snapshots(&[failed, a.clone()]),
    );
    state.update_executor_catalog(&[c.clone(), a.clone()]);
    assert_eq!(
        state.executor_catalog.as_ref().unwrap(),
        &catalog_from_snapshots(&[a, c]),
    );
    Ok(())
}
