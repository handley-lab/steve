use super::*;
use pretty_assertions::assert_eq;

#[test]
fn catalog_cache_invalidation_clears_global_collections_and_preserves_other_scopes() {
    let codex_home = tempfile::tempdir().expect("create codex home");
    let config = RemotePluginServiceConfig::new(
        "https://chatgpt.com/backend-api".to_string(),
        crate::test_support::test_http_client_factory(),
        /*product_sku*/ None,
    );
    let auth = CodexAuth::create_dummy_chatgpt_auth_for_testing();
    for scope in [RemotePluginScope::Global, RemotePluginScope::Workspace] {
        catalog_cache::write_cached_directory_plugins(
            codex_home.path(),
            &config,
            &auth,
            scope,
            /*collection*/ None,
            &[],
        );
    }

    assert_eq!(
        cached_remote_plugin_catalog_scopes(codex_home.path(), &config, Some(&auth)),
        BTreeSet::from([RemotePluginScope::Global, RemotePluginScope::Workspace])
    );
    catalog_cache::write_cached_directory_plugins(
        codex_home.path(),
        &config,
        &auth,
        RemotePluginScope::Global,
        Some(OPENAI_CURATED_REMOTE_COLLECTION_KEY),
        &[],
    );

    invalidate_cached_remote_plugin_catalog_scopes(
        codex_home.path(),
        &config,
        Some(&auth),
        &[RemotePluginScope::Global],
    );

    assert_eq!(
        cached_remote_plugin_catalog_scopes(codex_home.path(), &config, Some(&auth)),
        BTreeSet::from([RemotePluginScope::Workspace]),
    );
    assert!(
        catalog_cache::load_cached_directory_plugins(
            codex_home.path(),
            &config,
            &auth,
            RemotePluginScope::Global,
            Some(OPENAI_CURATED_REMOTE_COLLECTION_KEY),
        )
        .is_none()
    );
}

#[test]
fn build_remote_marketplace_preserves_directory_order_and_appends_installed_only_plugins() {
    let directory_plugins = vec![
        directory_plugin("plugin-z", "zulu"),
        directory_plugin("plugin-m", "mike"),
    ];
    let installed_plugins = vec![RemotePluginInstalledItem {
        plugin: directory_plugin("plugin-a", "alpha"),
        installed_at: None,
        enabled: true,
        disabled_skill_names: Vec::new(),
    }];

    let marketplace = build_remote_marketplace(
        "marketplace",
        "Marketplace",
        directory_plugins,
        installed_plugins,
        /*include_installed_only*/ true,
    )
    .expect("marketplace should be valid")
    .expect("marketplace should not be empty");

    assert_eq!(
        marketplace
            .plugins
            .into_iter()
            .map(|plugin| plugin.remote_plugin_id)
            .collect::<Vec<_>>(),
        vec!["plugin-z", "plugin-m", "plugin-a"]
    );
}

#[test]
fn installation_policy_source_is_preserved_across_remote_summary_paths() {
    let mut directory_plugin = directory_plugin("plugin-linear", "linear");
    directory_plugin.installation_policy_source =
        Some(RemotePluginInstallPolicySource::ImplicitCanonicalApp);
    let installed_plugin = RemotePluginInstalledItem {
        plugin: directory_plugin.clone(),
        installed_at: None,
        enabled: true,
        disabled_skill_names: Vec::new(),
    };

    let marketplace = build_remote_marketplace(
        REMOTE_GLOBAL_MARKETPLACE_NAME,
        REMOTE_GLOBAL_MARKETPLACE_DISPLAY_NAME,
        vec![directory_plugin],
        vec![installed_plugin.clone()],
        /*include_installed_only*/ false,
    )
    .expect("marketplace should be valid")
    .expect("marketplace should not be empty");
    assert_eq!(
        marketplace
            .plugins
            .into_iter()
            .map(|plugin| plugin.install_policy_source)
            .collect::<Vec<_>>(),
        vec![Some(PluginInstallPolicySource::ImplicitCanonicalApp)]
    );

    let mut installed_plugin = installed_plugin;
    installed_plugin.plugin.installation_policy_source =
        Some(RemotePluginInstallPolicySource::WorkspaceSetting);
    let installed_plugin = remote_installed_plugin_to_cache_entry(&installed_plugin)
        .expect("installed plugin should be valid");
    let marketplaces = group_remote_installed_plugins_by_marketplaces(
        &[installed_plugin],
        &[REMOTE_GLOBAL_MARKETPLACE_NAME],
    );
    assert_eq!(
        marketplaces
            .into_iter()
            .flat_map(|marketplace| marketplace.plugins)
            .map(|plugin| plugin.install_policy_source)
            .collect::<Vec<_>>(),
        vec![Some(PluginInstallPolicySource::WorkspaceSetting)]
    );
}

#[test]
fn plan_eligibility_is_preserved_across_remote_summary_paths() {
    let mut directory_plugin = directory_plugin("plugin-gmail", "gmail");
    directory_plugin.installation_policy = PluginInstallPolicy::NotAvailable;
    directory_plugin.availability = PluginAvailability::DisabledByAdmin;
    directory_plugin.disabled_reason = Some(PluginDisabledReason::PlanNotEligible);
    directory_plugin.eligible_plan_types = Some(vec![
        "plus".to_string(),
        "pro".to_string(),
        "enterprise_cbp_automation".to_string(),
    ]);
    let installed_plugin = RemotePluginInstalledItem {
        plugin: directory_plugin.clone(),
        installed_at: None,
        enabled: false,
        disabled_skill_names: Vec::new(),
    };

    let marketplace = build_remote_marketplace(
        REMOTE_GLOBAL_MARKETPLACE_NAME,
        REMOTE_GLOBAL_MARKETPLACE_DISPLAY_NAME,
        vec![directory_plugin],
        Vec::new(),
        /*include_installed_only*/ false,
    )
    .expect("marketplace should be valid")
    .expect("marketplace should not be empty");
    let expected = vec![(
        PluginAvailability::DisabledByAdmin,
        Some(PluginDisabledReason::PlanNotEligible),
        Some(vec![
            "plus".to_string(),
            "pro".to_string(),
            "enterprise_cbp_automation".to_string(),
        ]),
    )];
    assert_eq!(
        marketplace
            .plugins
            .into_iter()
            .map(|plugin| (
                plugin.availability,
                plugin.disabled_reason,
                plugin.eligible_plan_types,
            ))
            .collect::<Vec<_>>(),
        expected
    );

    let installed_plugin = remote_installed_plugin_to_cache_entry(&installed_plugin)
        .expect("installed plugin should be valid");
    let marketplaces = group_remote_installed_plugins_by_marketplaces(
        &[installed_plugin],
        &[REMOTE_GLOBAL_MARKETPLACE_NAME],
    );
    assert_eq!(
        marketplaces
            .into_iter()
            .flat_map(|marketplace| marketplace.plugins)
            .map(|plugin| (
                plugin.availability,
                plugin.disabled_reason,
                plugin.eligible_plan_types,
            ))
            .collect::<Vec<_>>(),
        expected
    );
}

#[test]
fn unknown_plugin_disabled_reason_preserves_remote_catalog_compatibility() {
    let plugin = directory_plugin("plugin-gmail", "gmail");
    let mut plugin_json = serde_json::to_value(plugin).expect("plugin should serialize");
    plugin_json["disabled_reason"] = serde_json::json!("future_disabled_reason");

    let plugin: RemotePluginDirectoryItem =
        serde_json::from_value(plugin_json).expect("unknown reason should deserialize");

    assert_eq!(plugin.disabled_reason, Some(PluginDisabledReason::Unknown));
}

#[test]
fn installation_interstitial_requirement_is_preserved_across_remote_summary_paths() {
    let mut directory_plugin = directory_plugin("plugin-linear", "linear");
    directory_plugin.must_show_installation_interstitial = Some(true);
    let marketplace = build_remote_marketplace(
        REMOTE_GLOBAL_MARKETPLACE_NAME,
        REMOTE_GLOBAL_MARKETPLACE_DISPLAY_NAME,
        vec![directory_plugin.clone()],
        Vec::new(),
        /*include_installed_only*/ false,
    )
    .expect("marketplace should be valid")
    .expect("marketplace should not be empty");
    assert_eq!(
        marketplace
            .plugins
            .into_iter()
            .map(|plugin| plugin.must_show_installation_interstitial)
            .collect::<Vec<_>>(),
        vec![Some(true)]
    );

    directory_plugin.must_show_installation_interstitial = Some(false);
    let installed_plugin = remote_installed_plugin_to_cache_entry(&RemotePluginInstalledItem {
        plugin: directory_plugin,
        installed_at: None,
        enabled: true,
        disabled_skill_names: Vec::new(),
    })
    .expect("installed plugin should be valid");
    let marketplaces = group_remote_installed_plugins_by_marketplaces(
        &[installed_plugin],
        &[REMOTE_GLOBAL_MARKETPLACE_NAME],
    );
    assert_eq!(
        marketplaces
            .into_iter()
            .flat_map(|marketplace| marketplace.plugins)
            .map(|plugin| plugin.must_show_installation_interstitial)
            .collect::<Vec<_>>(),
        vec![Some(false)]
    );
}

#[test]
fn missing_installation_interstitial_requirement_deserializes_to_none() {
    let plugin = directory_plugin("plugin-linear", "linear");
    let mut plugin_json = serde_json::to_value(plugin).expect("plugin should serialize");
    plugin_json
        .as_object_mut()
        .expect("plugin should serialize to an object")
        .remove("must_show_installation_interstitial");

    let plugin: RemotePluginDirectoryItem =
        serde_json::from_value(plugin_json).expect("missing requirement should deserialize");

    assert_eq!(plugin.must_show_installation_interstitial, None);
}

#[test]
fn unknown_installation_policy_source_maps_to_none() {
    let plugin = directory_plugin("plugin-linear", "linear");
    let mut plugin_json = serde_json::to_value(plugin).expect("plugin should serialize");
    plugin_json["installation_policy_source"] =
        serde_json::Value::String("FUTURE_POLICY_SOURCE".to_string());
    let plugin: RemotePluginDirectoryItem =
        serde_json::from_value(plugin_json).expect("unknown source should deserialize");

    let summary = build_remote_plugin_summary(&plugin, /*installed_plugin*/ None)
        .expect("summary should be valid");

    assert_eq!(summary.install_policy_source, None);
}

#[test]
fn scheduled_task_metadata_distinguishes_unavailable_from_empty() {
    let release = serde_json::json!({
        "display_name": "Example",
        "description": "Example plugin",
        "interface": {},
    });
    let without_metadata: RemotePluginReleaseResponse =
        serde_json::from_value(release.clone()).expect("release should deserialize");
    assert_eq!(without_metadata.scheduled_tasks, None);

    let mut with_empty_metadata = release;
    with_empty_metadata["scheduled_tasks"] = serde_json::json!([]);
    let with_empty_metadata: RemotePluginReleaseResponse =
        serde_json::from_value(with_empty_metadata).expect("release should deserialize");
    assert_eq!(with_empty_metadata.scheduled_tasks, Some(Vec::new()));
}

#[test]
fn workspace_share_context_preserves_publish_capability() {
    let mut plugin = directory_plugin("plugin-workspace", "workspace plugin");
    plugin.scope = RemotePluginScope::Workspace;
    plugin.discoverability = Some(RemotePluginShareDiscoverability::Private);
    plugin.can_publish_to_workspace = Some(true);

    let context = remote_plugin_share_context(&plugin)
        .expect("workspace plugin should be valid")
        .expect("workspace plugin should have share context");

    assert_eq!(context.can_publish_to_workspace, Some(true));
}

fn directory_plugin(id: &str, name: &str) -> RemotePluginDirectoryItem {
    RemotePluginDirectoryItem {
        canonical_app_id: None,
        id: id.to_string(),
        name: name.to_string(),
        scope: RemotePluginScope::Global,
        discoverability: None,
        creator_account_user_id: None,
        creator_name: None,
        share_url: None,
        share_principals: None,
        can_publish_to_workspace: None,
        installation_policy: PluginInstallPolicy::Available,
        installation_policy_source: None,
        must_show_installation_interstitial: None,
        authentication_policy: PluginAuthPolicy::OnUse,
        availability: PluginAvailability::Available,
        disabled_reason: None,
        eligible_plan_types: None,
        release: RemotePluginReleaseResponse {
            version: None,
            display_name: name.to_string(),
            description: String::new(),
            bundle_download_url: None,
            app_ids: Vec::new(),
            app_manifest: None,
            app_templates: Vec::new(),
            keywords: Vec::new(),
            interface: RemotePluginReleaseInterfaceResponse {
                short_description: None,
                long_description: None,
                developer_name: None,
                category: None,
                capabilities: Vec::new(),
                website_url: None,
                privacy_policy_url: None,
                terms_of_service_url: None,
                brand_color: None,
                default_prompt: None,
                default_prompts: None,
                composer_icon_url: None,
                logo_url: None,
                logo_url_dark: None,
                screenshot_urls: Vec::new(),
            },
            skills: Vec::new(),
            onboarding_skill_name: None,
            mcp_servers: Vec::new(),
            scheduled_tasks: None,
        },
    }
}

#[test]
fn remote_plugin_interface_maps_dark_logo_url() {
    let mut plugin = directory_plugin("plugin-linear", "linear");
    plugin.release.interface.logo_url_dark =
        Some("https://example.com/linear/logo-dark.png".to_string());

    assert_eq!(
        remote_plugin_interface_to_info(&plugin)
            .expect("plugin interface")
            .logo_url_dark,
        Some("https://example.com/linear/logo-dark.png".to_string())
    );
}
