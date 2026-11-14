use super::*;
use crate::config::CONFIG_TOML_FILE;
use crate::config::ConfigBuilder;
use codex_config::test_support::CloudConfigBundleFixture;
use codex_config::types::ApprovalsReviewer;
use codex_mcp::CODEX_APPS_MCP_SERVER_NAME;
use pretty_assertions::assert_eq;
use tempfile::tempdir;

#[tokio::test]
async fn app_approvals_reviewer_uses_link_then_app_then_default_then_global() {
    for (global, app_default, app, expected_global, expected_default, expected_app) in [
        (
            "user",
            "auto_review",
            "user",
            ApprovalsReviewer::User,
            ApprovalsReviewer::AutoReview,
            ApprovalsReviewer::User,
        ),
        (
            "auto_review",
            "user",
            "auto_review",
            ApprovalsReviewer::AutoReview,
            ApprovalsReviewer::User,
            ApprovalsReviewer::AutoReview,
        ),
    ] {
        let codex_home = tempdir().expect("tempdir should succeed");
        std::fs::write(
            codex_home.path().join(CONFIG_TOML_FILE),
            format!(
                r#"
approvals_reviewer = "{global}"

[apps._default]
approvals_reviewer = "{app_default}"

[apps.calendar]
approvals_reviewer = "{app}"

[apps.calendar.links.link_calendar]
approvals_reviewer = "{app_default}"

[apps.calendar.links.link_without_privacy]

[apps.drive.links.link_drive]
approvals_reviewer = "{app_default}"

[apps.without_links]
approvals_reviewer = "{app}"

[apps.empty_links]
approvals_reviewer = "{app}"

[apps.empty_links.links]
"#
            ),
        )
        .expect("write config");
        let config = ConfigBuilder::default()
            .codex_home(codex_home.path().to_path_buf())
            .build()
            .await
            .expect("config should build");

        for (link_id, expected) in [
            (Some("link_calendar"), expected_default),
            (Some("link_without_privacy"), expected_app),
            (Some("link_drive"), expected_app),
            (None, expected_app),
        ] {
            assert_eq!(
                mcp_approvals_reviewer_from_layers(
                    &config.config_layer_stack,
                    config.approvals_reviewer,
                    config.model.as_deref(),
                    CODEX_APPS_MCP_SERVER_NAME,
                    Some("calendar"),
                    link_id,
                ),
                expected
            );
        }
        for connector_id in ["without_links", "empty_links"] {
            assert_eq!(
                mcp_approvals_reviewer_from_layers(
                    &config.config_layer_stack,
                    config.approvals_reviewer,
                    config.model.as_deref(),
                    CODEX_APPS_MCP_SERVER_NAME,
                    Some(connector_id),
                    Some("link_calendar"),
                ),
                expected_app
            );
        }
        assert_eq!(
            mcp_approvals_reviewer_from_layers(
                &config.config_layer_stack,
                config.approvals_reviewer,
                config.model.as_deref(),
                CODEX_APPS_MCP_SERVER_NAME,
                Some("drive"),
                /*link_id*/ None,
            ),
            expected_default
        );
        assert_eq!(
            mcp_approvals_reviewer_from_layers(
                &config.config_layer_stack,
                config.approvals_reviewer,
                config.model.as_deref(),
                CODEX_APPS_MCP_SERVER_NAME,
                /*connector_id*/ None,
                /*link_id*/ None,
            ),
            expected_default
        );
        assert_eq!(
            mcp_approvals_reviewer_from_layers(
                &config.config_layer_stack,
                config.approvals_reviewer,
                config.model.as_deref(),
                "custom_server",
                Some("calendar"),
                Some("link_calendar"),
            ),
            expected_global
        );
    }
}

#[tokio::test]
async fn default_app_approvals_reviewer_respects_global_reviewer_requirements() {
    let codex_home = tempdir().expect("tempdir should succeed");
    std::fs::write(
        codex_home.path().join(CONFIG_TOML_FILE),
        r#"
approvals_reviewer = "auto_review"

[apps._default]
approvals_reviewer = "user"
"#,
    )
    .expect("write config");
    let config = ConfigBuilder::default()
        .codex_home(codex_home.path().to_path_buf())
        .cloud_config_bundle(
            CloudConfigBundleFixture::loader_with_enterprise_requirement(
                r#"allowed_approvals_reviewers = ["auto_review"]"#,
            ),
        )
        .build()
        .await
        .expect("config should build");

    assert_eq!(
        mcp_approvals_reviewer_from_layers(
            &config.config_layer_stack,
            config.approvals_reviewer,
            config.model.as_deref(),
            CODEX_APPS_MCP_SERVER_NAME,
            Some("calendar"),
            /*link_id*/ None,
        ),
        ApprovalsReviewer::AutoReview
    );
}

#[tokio::test]
async fn app_approvals_reviewer_respects_global_reviewer_requirements() {
    let codex_home = tempdir().expect("tempdir should succeed");
    std::fs::write(
        codex_home.path().join(CONFIG_TOML_FILE),
        r#"
approvals_reviewer = "auto_review"

[apps.calendar]
approvals_reviewer = "user"

[apps.calendar.links.link_calendar]
approvals_reviewer = "user"
"#,
    )
    .expect("write config");
    let config = ConfigBuilder::default()
        .codex_home(codex_home.path().to_path_buf())
        .cloud_config_bundle(
            CloudConfigBundleFixture::loader_with_enterprise_requirement(
                r#"allowed_approvals_reviewers = ["auto_review"]"#,
            ),
        )
        .build()
        .await
        .expect("config should build");

    for link_id in [None, Some("link_calendar")] {
        assert_eq!(
            mcp_approvals_reviewer_from_layers(
                &config.config_layer_stack,
                config.approvals_reviewer,
                config.model.as_deref(),
                CODEX_APPS_MCP_SERVER_NAME,
                Some("calendar"),
                link_id,
            ),
            ApprovalsReviewer::AutoReview
        );
    }
}
