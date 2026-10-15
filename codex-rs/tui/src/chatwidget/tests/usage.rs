use super::*;
use chrono::TimeZone;
use codex_app_server_protocol::ConsumeAccountRateLimitResetCreditOutcome;
use codex_app_server_protocol::ConsumeAccountRateLimitResetCreditResponse;
use codex_app_server_protocol::RateLimitResetCredit;
use codex_app_server_protocol::RateLimitResetCreditStatus;
use codex_app_server_protocol::RateLimitResetCreditsSummary;
use codex_app_server_protocol::RateLimitResetType;
use pretty_assertions::assert_eq;
use uuid::Uuid;

const TEST_OVERLAY_VIEW_ID: &str = "usage-test-overlay";

#[tokio::test]
async fn usage_menu_opens_analytics() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(/*model_override*/ None).await;
    set_chatgpt_auth(&mut chat);
    chat.dispatch_command(SlashCommand::Usage);
    chat.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    assert_matches!(rx.try_recv(), Ok(AppEvent::OpenAnalytics { view: None }));
}

fn reset_credits(available_count: i64) -> RateLimitResetCreditsSummary {
    RateLimitResetCreditsSummary {
        available_count,
        credits: None,
    }
}

fn detailed_reset_credits(
    available_count: i64,
    credits: Vec<RateLimitResetCredit>,
) -> RateLimitResetCreditsSummary {
    RateLimitResetCreditsSummary {
        available_count,
        credits: Some(credits),
    }
}

fn reset_credit(id: &str, expires_at: Option<i64>) -> RateLimitResetCredit {
    RateLimitResetCredit {
        id: id.to_string(),
        reset_type: RateLimitResetType::CodexRateLimits,
        status: RateLimitResetCreditStatus::Available,
        granted_at: 0,
        expires_at,
        title: None,
        description: None,
    }
}

fn reset_credit_with_title(id: &str, expires_at: Option<i64>, title: &str) -> RateLimitResetCredit {
    RateLimitResetCredit {
        title: Some(title.to_string()),
        ..reset_credit(id, expires_at)
    }
}

fn expiry_timestamp(day: u32, hour: u32, minute: u32) -> i64 {
    chrono::Local
        .with_ymd_and_hms(2026, 6, day, hour, minute, 0)
        .single()
        .expect("valid test timestamp")
        .timestamp()
}





#[tokio::test]
async fn usage_command_can_check_reset_availability_before_startup_refresh_finishes_snapshot() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(/*model_override*/ None).await;
    set_chatgpt_auth(&mut chat);
    chat.start_rate_limit_reset_startup_check();

    chat.dispatch_command(SlashCommand::Usage);

    assert_chatwidget_snapshot!(
        "usage_command_menu_before_reset_refresh",
        render_bottom_popup(&chat, /*width*/ 80)
    );
    chat.handle_key_event(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
    chat.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    assert_matches!(rx.try_recv(), Ok(AppEvent::OpenRateLimitResetCredits));
}

#[tokio::test]
async fn usage_command_can_check_reset_availability_for_workspace_accounts() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(/*model_override*/ None).await;
    set_chatgpt_auth(&mut chat);
    chat.plan_type = Some(PlanType::Business);

    chat.dispatch_command(SlashCommand::Usage);

    chat.handle_key_event(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
    chat.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    assert_matches!(rx.try_recv(), Ok(AppEvent::OpenRateLimitResetCredits));
}









#[tokio::test]
async fn rate_limit_reset_retry_reuses_idempotency_key() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(/*model_override*/ None).await;
    let request_id = chat.show_rate_limit_reset_consuming_popup();
    assert!(!chat.finish_rate_limit_reset_consume(
        request_id,
        "stable-redeem-id".to_string(),
        Some("credit-1".to_string()),
        Err("response lost".to_string()),
    ));

    chat.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

    let Ok(AppEvent::ConsumeRateLimitResetCredit {
        idempotency_key,
        credit_id,
    }) = rx.try_recv()
    else {
        panic!("expected reset retry event");
    };
    assert_eq!(
        (idempotency_key.as_str(), credit_id.as_deref()),
        ("stable-redeem-id", Some("credit-1"))
    );
    assert_eq!(chat.start_rate_limit_reset_consumption("wrong-key"), None);
    assert!(
        chat.start_rate_limit_reset_consumption(&idempotency_key)
            .is_some()
    );
}



#[tokio::test]
async fn rate_limit_reset_redemption_allows_ctrl_c_to_quit_while_in_flight() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(/*model_override*/ None).await;

    chat.show_rate_limit_reset_consuming_popup();
    chat.handle_key_event(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL));

    assert_matches!(rx.try_recv(), Ok(AppEvent::Exit(ExitMode::ShutdownFirst)));
    assert!(render_bottom_popup(&chat, /*width*/ 80).contains("Using a reset..."));
}














fn consume_response(
    outcome: ConsumeAccountRateLimitResetCreditOutcome,
) -> ConsumeAccountRateLimitResetCreditResponse {
    ConsumeAccountRateLimitResetCreditResponse { outcome }
}

fn finish_reset_consume_outcome(
    chat: &mut ChatWidget,
    request_id: u64,
    idempotency_key: &str,
    outcome: ConsumeAccountRateLimitResetCreditOutcome,
) -> bool {
    chat.finish_rate_limit_reset_consume(
        request_id,
        idempotency_key.to_string(),
        /*credit_id*/ None,
        Ok(consume_response(outcome)),
    )
}

fn record_popup(chat: &ChatWidget, states: &mut Vec<String>) {
    states.push(render_bottom_popup(chat, /*width*/ 80));
}

fn dismiss_popup(chat: &mut ChatWidget) {
    chat.handle_key_event(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
}

fn show_usage_test_overlay(chat: &mut ChatWidget) {
    chat.bottom_pane.show_selection_view(SelectionViewParams {
        view_id: Some(TEST_OVERLAY_VIEW_ID),
        title: Some("Covering overlay".to_string()),
        items: vec![SelectionItem {
            name: "Close".to_string(),
            dismiss_on_select: true,
            ..Default::default()
        }],
        ..Default::default()
    });
}
