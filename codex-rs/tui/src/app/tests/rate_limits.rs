use super::*;
use codex_app_server_protocol::AccountRateLimitsUpdatedNotification;
use codex_app_server_protocol::CodexErrorInfo;
use codex_app_server_protocol::CreditsSnapshot;
use codex_app_server_protocol::ErrorNotification;
use codex_app_server_protocol::GetAccountRateLimitsResponse;
use codex_app_server_protocol::RateLimitReachedType;
use codex_app_server_protocol::RateLimitResetCreditsSummary;
use codex_app_server_protocol::RateLimitSnapshot;
use codex_app_server_protocol::RateLimitWindow;
use crossterm::event::KeyCode;
use crossterm::event::KeyEvent;
use pretty_assertions::assert_eq;

fn rate_limit_snapshot(
    used_percent: i32,
    rate_limit_reached_type: Option<RateLimitReachedType>,
    spend_control_reached: Option<bool>,
) -> RateLimitSnapshot {
    RateLimitSnapshot {
        limit_id: Some("steve".to_string()),
        limit_name: None,
        normal_model_slug: None,
        primary: Some(RateLimitWindow {
            used_percent,
            window_duration_mins: Some(300),
            resets_at: None,
        }),
        secondary: None,
        credits: Some(CreditsSnapshot {
            has_credits: true,
            unlimited: false,
            balance: None,
        }),
        individual_limit: None,
        spend_control_reached,
        plan_type: None,
        rate_limit_reached_type,
    }
}

fn account_rate_limits_response(snapshot: RateLimitSnapshot) -> GetAccountRateLimitsResponse {
    GetAccountRateLimitsResponse {
        ordinary_usage_allowed: None,
        account_id: None,
        rate_limit_upsell: None,
        rate_limits: snapshot,
        rate_limits_by_limit_id: None,
        rate_limit_reset_credits: Some(RateLimitResetCreditsSummary {
            available_count: 0,
            credits: None,
        }),
    }
}

async fn deliver_rolling_rate_limit_snapshot(
    app: &mut App,
    app_server: &AppServerSession,
    snapshot: RateLimitSnapshot,
) {
    app.handle_app_server_event(
        app_server,
        codex_app_server_client::AppServerEvent::ServerNotification(Box::new(
            ServerNotification::AccountRateLimitsUpdated(AccountRateLimitsUpdatedNotification {
                rate_limits: snapshot,
            }),
        )),
    )
    .await;
}

fn render_status_output(
    app: &mut App,
    app_event_rx: &mut tokio::sync::mpsc::UnboundedReceiver<AppEvent>,
) -> String {
    while app_event_rx.try_recv().is_ok() {}
    app.chat_widget.add_status_output(
        /*refreshing_rate_limits*/ false, /*request_id*/ None,
    );
    match app_event_rx.try_recv() {
        Ok(AppEvent::InsertHistoryCell(cell)) => cell
            .display_lines(/*width*/ 120)
            .into_iter()
            .map(|line| line.to_string())
            .collect::<Vec<_>>()
            .join("\n"),
        other => panic!("expected status output, got {other:?}"),
    }
}

fn deliver_usage_limit_error(app: &mut App) {
    app.chat_widget.handle_server_notification(
        ServerNotification::Error(ErrorNotification {
            error: AppServerTurnError {
                misalignment: None,
                message: "Usage limit reached.".to_string(),
                codex_error_info: Some(CodexErrorInfo::UsageLimitExceeded),
                additional_details: None,
            },
            will_retry: false,
            thread_id: "thread-1".to_string(),
            turn_id: "turn-1".to_string(),
        }),
        /*replay_kind*/ None,
    );
}

#[tokio::test]
async fn backend_banner_state_survives_widget_replacement() -> Result<()> {
    for dismiss in [false, true] {
        let (mut app, _rx, _op_rx) = make_test_app_with_channels().await;
        set_chatgpt_auth(&mut app.chat_widget);
        let mut response = account_rate_limits_response(rate_limit_snapshot(
            /*used_percent*/ 100, /*rate_limit_reached_type*/ None,
            /*spend_control_reached*/ None,
        ));
        response.rate_limit_upsell = Some(serde_json::json!({
            "banner_type": "plus_rate_limit_reached", "title": "Usage limit reached",
            "presentation": "dismissible",
            "description": "Choose how to continue.",
            "ctas": [{"action": "view_usage", "label": "View usage"}]
        }));
        assert!(render_bottom_popup(&app.chat_widget, /*width*/ 90).contains("View usage"));
        if dismiss {
            app.chat_widget
                .handle_key_event(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        }
        let before = render_bottom_popup(&app.chat_widget, /*width*/ 90);
        let mut tui = crate::tui::test_support::make_test_tui()?;
        let init = app.chatwidget_init_for_forked_or_resumed_thread(
            &mut tui,
            app.config.clone(),
            /*initial_user_message*/ None,
        );
        app.replace_chat_widget(ChatWidget::new_with_app_event(init));
        set_active_cell(
            &mut app.chat_widget,
            Box::new(PlainHistoryCell::new(Vec::new())),
        );
        app.chat_widget.pre_draw_tick();
        assert_eq!(render_bottom_popup(&app.chat_widget, /*width*/ 90), before);
        assert_eq!(render_bottom_popup(&app.chat_widget, /*width*/ 90), before);
        response.rate_limit_upsell = None;
        assert!(!render_bottom_popup(&app.chat_widget, /*width*/ 90).contains("View usage"));
    }
    Ok(())
}

#[tokio::test]
async fn rolling_workspace_hard_stops_invalidate_older_rate_limit_reads() -> Result<()> {
    let (mut app, _app_event_rx, _op_rx) = make_test_app_with_channels().await;
    let app_server = crate::start_embedded_app_server_for_picker(app.chat_widget.config_ref())
        .await
        .expect("embedded app server");

    let cases = [
        (None, None, false),
        (Some(RateLimitReachedType::RateLimitReached), None, false),
        (None, Some(false), false),
        (None, Some(true), true),
        (
            Some(RateLimitReachedType::WorkspaceOwnerCreditsDepleted),
            None,
            true,
        ),
        (
            Some(RateLimitReachedType::WorkspaceMemberCreditsDepleted),
            None,
            true,
        ),
        (
            Some(RateLimitReachedType::WorkspaceOwnerUsageLimitReached),
            None,
            true,
        ),
        (
            Some(RateLimitReachedType::WorkspaceMemberUsageLimitReached),
            None,
            true,
        ),
    ];
    let mut expected_generation = 0;
    for (reached_type, spend_control_reached, invalidates) in cases {
        deliver_rolling_rate_limit_snapshot(
            &mut app,
            &app_server,
            rate_limit_snapshot(
                /*used_percent*/ 95,
                reached_type,
                spend_control_reached,
            ),
        )
        .await;
        if invalidates {
            expected_generation += 1;
        }
        assert_eq!(
            app.rate_limit_hard_stop_generation, expected_generation,
            "reached_type={reached_type:?}, spend_control_reached={spend_control_reached:?}"
        );
    }

    app_server.shutdown().await?;
    Ok(())
}





