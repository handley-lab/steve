use super::*;
use pretty_assertions::assert_eq;

#[tokio::test]
async fn plugins_popup_uses_product_labels_for_remote_and_personal_tabs() {
    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(/*model_override*/ None).await;
    chat.set_feature_enabled(Feature::Plugins, /*enabled*/ true);

    render_loaded_plugins_popup(
        &mut chat,
        plugins_test_response(vec![
            plugins_test_remote_marketplace(
                "workspace-directory",
                "Raw Workspace Directory",
                vec![plugins_test_remote_summary(
                    "plugins~Plugin_buildkite",
                    "buildkite",
                    Some("Buildkite"),
                    Some("Workspace CI."),
                    /*installed*/ false,
                )],
            ),
            plugins_test_remote_marketplace(
                "workspace-shared-with-me-private",
                "Raw Shared Private",
                vec![plugins_test_remote_summary(
                    "plugins~Plugin_docs",
                    "docs",
                    Some("Docs"),
                    Some("Shared docs."),
                    /*installed*/ false,
                )],
            ),
            plugins_test_remote_marketplace(
                "workspace-shared-with-me-unlisted",
                "Raw Shared Link",
                vec![plugins_test_remote_summary(
                    "plugins~Plugin_link",
                    "link",
                    Some("Link Share"),
                    Some("Shared by link."),
                    /*installed*/ false,
                )],
            ),
            PluginMarketplaceEntry {
                name: "codex-curated".to_string(),
                path: Some(plugins_test_personal_marketplace_path()),
                interface: Some(MarketplaceInterface {
                    display_name: Some("Personal".to_string()),
                }),
                plugins: vec![plugins_test_summary(
                    "plugin-local-docs",
                    "local-docs",
                    Some("Local Docs"),
                    Some("Local editable docs."),
                    /*installed*/ false,
                    /*enabled*/ true,
                    PluginInstallPolicy::Available,
                )],
            },
        ]),
    );

    let rows = [
        (
            "[Workspace]",
            "Workspace.",
            "Buildkite",
            "Raw Workspace Directory.",
        ),
        (
            "[Shared with me]",
            "Shared with me.",
            "Docs",
            "Raw Shared Private.",
        ),
        (
            "[Shared with me (link)]",
            "Shared with me (link).",
            "Link Share",
            "Raw Shared Link.",
        ),
        ("[Local]", "Local.", "Local Docs", "Personal."),
    ]
    .into_iter()
    .map(|(selected_tab, product_label, plugin_name, raw_label)| {
        let popup = select_plugins_tab_containing(&mut chat, /*width*/ 120, product_label);
        assert!(
            popup.contains(product_label)
                && popup.contains(plugin_name)
                && !popup.contains(raw_label),
            "expected {selected_tab} to use its product label, got:\n{popup}"
        );
        popup
            .lines()
            .find(|line| line.contains(plugin_name))
            .expect("expected plugin row")
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
    })
    .collect::<Vec<_>>()
    .join("\n");

    insta::assert_snapshot!(
        rows,
        @r"
    › [-] Buildkite Available Press Enter to install or view plugin details
    › [-] Docs Available Press Enter to install or view plugin details
    › [-] Link Share Available Press Enter to install or view plugin details
    › [-] Local Docs Available Press Enter to install or view plugin details
    "
    );
}

#[tokio::test]
async fn plugins_popup_preserves_workspace_tab_across_load_and_detail_navigation() {
    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(/*model_override*/ None).await;
    chat.set_feature_enabled(Feature::Plugins, /*enabled*/ true);

    let workspace_marketplace = plugins_test_remote_marketplace(
        "workspace-directory",
        "Raw Workspace Directory",
        vec![plugins_test_remote_summary(
            "plugins~Plugin_buildkite",
            "buildkite",
            Some("Buildkite"),
            Some("Buildkite pipelines."),
            /*installed*/ false,
        )],
    );
    chat.add_plugins_output();
    let cwd = chat.config.cwd.clone();
    chat.on_plugins_loaded(
        cwd.to_path_buf(),
        Ok(plugins_test_response(vec![
            plugins_test_curated_marketplace(Vec::new()),
        ])),
    );
    let loading_popup =
        select_plugins_tab_containing(&mut chat, /*width*/ 100, "Loading Workspace plugins.");
    assert!(
        loading_popup.contains("Loading Workspace plugins."),
        "expected Workspace loading tab before remote sections resolve, got:\n{loading_popup}"
    );

    chat.on_plugin_remote_sections_loaded(
        cwd.to_path_buf(),
        vec![workspace_marketplace.clone()],
        Vec::new(),
    );
    let workspace_popup = render_bottom_popup(&chat, /*width*/ 100);
    assert!(
        workspace_popup.contains("Workspace.")
            && workspace_popup.contains("Buildkite")
            && !workspace_popup.contains("Loading Workspace plugins."),
        "expected remote section refresh to keep the Workspace tab active, got:\n{workspace_popup}"
    );

    chat.open_plugin_detail_loading_popup("Buildkite");
    chat.open_plugins_list(
        cwd.to_path_buf(),
        plugins_test_response(vec![
            plugins_test_curated_marketplace(Vec::new()),
            workspace_marketplace,
        ]),
    );
    let reopened_popup = render_bottom_popup(&chat, /*width*/ 100);
    assert!(
        reopened_popup.contains("Workspace.") && reopened_popup.contains("Buildkite"),
        "expected Back to plugins to preserve the Workspace tab, got:\n{reopened_popup}"
    );
}

#[tokio::test]
async fn plugins_popup_remote_local_dedupe_prefers_installed_remote_after_mapped_shares() {
    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(/*model_override*/ None).await;
    chat.set_feature_enabled(Feature::Plugins, /*enabled*/ true);

    let remote_plugin_id = "plugins~Plugin_docs";
    let local_summary = PluginSummary {
        remote_plugin_id: Some(remote_plugin_id.to_string()),
        ..plugins_test_summary(
            "plugin-docs",
            "docs",
            Some("Docs"),
            Some("Local curated docs plugin."),
            /*installed*/ false,
            /*enabled*/ true,
            PluginInstallPolicy::Available,
        )
    };
    let cwd = chat.config.cwd.clone();
    render_loaded_plugins_popup(
        &mut chat,
        plugins_test_response(vec![plugins_test_curated_marketplace(vec![local_summary])]),
    );
    chat.on_plugin_remote_sections_loaded(
        cwd.to_path_buf(),
        vec![plugins_test_remote_marketplace(
            "workspace-shared-with-me-private",
            "Shared with me",
            vec![plugins_test_remote_summary(
                remote_plugin_id,
                "docs",
                Some("Docs"),
                Some("Remote installed docs plugin."),
                /*installed*/ true,
            )],
        )],
        Vec::new(),
    );
    let popup = render_bottom_popup(&chat, /*width*/ 100);
    let PluginsCacheState::Ready(response) = &chat.plugins_cache else {
        panic!("expected cached plugins after remote section refresh");
    };
    assert_eq!(
        response
            .marketplaces
            .iter()
            .map(|marketplace| marketplace.name.as_str())
            .collect::<Vec<_>>(),
        vec![
            OPENAI_CURATED_MARKETPLACE_NAME,
            "workspace-shared-with-me-private"
        ]
    );
    let all_plugins_row = popup
        .lines()
        .find(|line| line.contains("Docs"))
        .expect("expected all-plugins row");
    assert!(
        popup.contains("Installed 1 of 1 available plugins."),
        "expected header count to reflect deduped plugin rows, got:\n{popup}"
    );
    assert!(
        all_plugins_row.contains("Installed")
            && !all_plugins_row.contains("Local curated docs plugin."),
        "expected installed remote duplicate to win when local row is not a mapped share, got:\n{all_plugins_row}"
    );
}

#[tokio::test]
async fn plugin_detail_not_installable_plugin_disables_install_action() {
    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(/*model_override*/ None).await;
    chat.set_feature_enabled(Feature::Plugins, /*enabled*/ true);

    let summary = plugins_test_summary(
        "plugin-internal",
        "internal",
        Some("Internal"),
        Some("Internal only."),
        /*installed*/ false,
        /*enabled*/ true,
        PluginInstallPolicy::NotAvailable,
    );
    let cwd = chat.config.cwd.clone();
    chat.on_plugins_loaded(
        cwd.to_path_buf(),
        Ok(plugins_test_response(vec![
            plugins_test_curated_marketplace(vec![summary.clone()]),
        ])),
    );
    chat.add_plugins_output();
    chat.on_plugin_detail_loaded(
        cwd.to_path_buf(),
        Ok(PluginReadResponse {
            plugin: plugins_test_detail(summary, Some("Internal only."), &[], &[], &[], &[]),
        }),
    );

    let popup = render_bottom_popup(&chat, /*width*/ 100);
    let install_row = popup
        .lines()
        .find(|line| line.contains("Install plugin"))
        .expect("expected install row");
    assert!(
        install_row.contains("This plugin is not installable from this marketplace"),
        "expected disabled not-installable row, got:\n{install_row}"
    );

    chat.handle_key_event(KeyEvent::from(KeyCode::Down));
    assert_eq!(
        render_bottom_popup(&chat, /*width*/ 100),
        popup,
        "expected navigation to skip the disabled install row"
    );
}

#[tokio::test]
async fn provider_neutral_plugin_policies_use_only_supplied_destinations() {
    let (chat, _rx, _ops) = make_chatwidget_manual(None).await;
    let terms = "https://policies.example/terms";
    let privacy = "https://openai.com/example/privacy";
    for (terms_url, privacy_url, installed, expected_urls) in [
        (None, None, false, vec![]),
        (Some(terms), None, false, vec![terms]),
        (None, Some(privacy), false, vec![privacy]),
        (Some(terms), Some(privacy), false, vec![terms, privacy]),
        (Some(terms), Some(privacy), true, vec![]),
        (
            Some("javascript:alert(1)"),
            Some("not-a-url"),
            false,
            vec![],
        ),
    ] {
        let mut summary = plugins_test_summary(
            "plugin-policy",
            "policy",
            Some("Policy app"),
            None,
            installed,
            true,
            PluginInstallPolicy::Available,
        );
        let interface = summary.interface.as_mut().expect("plugin interface");
        interface.terms_of_service_url = terms_url.map(str::to_string);
        interface.privacy_policy_url = privacy_url.map(str::to_string);
        let response = plugins_test_response(vec![plugins_test_curated_marketplace(vec![
            summary.clone(),
        ])]);
        let detail = plugins_test_detail(summary, None, &[], &[], &[], &[]);
        let params = chat.plugin_detail_popup_params(&response, &detail);
        let area = ratatui::layout::Rect::new(0, 0, 120, params.header.desired_height(120));
        let mut buf = ratatui::buffer::Buffer::empty(area);
        params.header.render(area, &mut buf);
        let encoded = buf
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        let observed = encoded
            .split("\x1b]8;;")
            .skip(1)
            .filter_map(|part| part.split_once('\x07').map(|(url, _)| url))
            .filter(|url| !url.is_empty())
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(
            observed,
            expected_urls
                .iter()
                .copied()
                .collect::<std::collections::BTreeSet<_>>()
        );
        for (url, label) in [(terms, "Termsofservice"), (privacy, "Privacypolicy")] {
            if expected_urls.contains(&url) {
                let prefix = format!("\x1b]8;;{url}\x07");
                let linked = buf
                    .content
                    .iter()
                    .filter(|cell| cell.symbol().contains(&prefix))
                    .map(|cell| crate::terminal_hyperlinks::strip_osc8(cell.symbol()))
                    .collect::<String>();
                assert_eq!(linked.split_whitespace().collect::<String>(), label);
            }
        }
    }
}
