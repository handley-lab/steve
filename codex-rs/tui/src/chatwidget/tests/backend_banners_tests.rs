use super::*;

#[tokio::test]
async fn owner_notification_completion_cannot_cross_account_change() {
    let (mut chat, mut rx, _ops) = make_chatwidget_manual(/*model_override*/ None).await;
    let previous = chat
        .start_add_credits_nudge_email_request(AddCreditsNudgeCreditType::Credits)
        .expect("start previous account request");
    chat.update_account_state(
        /*status_account_display*/ None, /*plan_type*/ None,
        /*has_chatgpt_account*/ true, /*has_codex_backend_auth*/ true,
    );
    let current = chat
        .start_add_credits_nudge_email_request(AddCreditsNudgeCreditType::UsageLimit)
        .expect("new account must be actionable");
    drain_insert_history(&mut rx);
    chat.finish_add_credits_nudge_email_request(previous, Ok(AddCreditsNudgeEmailStatus::Sent));
    assert!(drain_insert_history(&mut rx).is_empty());
    assert!(
        chat.start_add_credits_nudge_email_request(AddCreditsNudgeCreditType::UsageLimit)
            .is_none()
    );
    chat.finish_add_credits_nudge_email_request(current, Ok(AddCreditsNudgeEmailStatus::Sent));
    let rendered = drain_insert_history(&mut rx)
        .into_iter()
        .map(|lines| lines_to_single_string(&lines))
        .collect::<String>();
    insta::assert_snapshot!("owner_notification_after_account_change", rendered);
}
