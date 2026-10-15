use super::*;

#[tokio::test]
async fn usage_menu_opens_analytics() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(/*model_override*/ None).await;
    set_chatgpt_auth(&mut chat);
    chat.dispatch_command(SlashCommand::Usage);
    chat.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    assert_matches!(rx.try_recv(), Ok(AppEvent::OpenAnalytics { view: None }));
}
