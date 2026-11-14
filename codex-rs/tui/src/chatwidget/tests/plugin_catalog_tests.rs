use super::*;
use pretty_assertions::assert_eq;

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
