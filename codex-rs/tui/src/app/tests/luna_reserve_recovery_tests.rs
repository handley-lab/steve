//! Verify Reserve recovery through accepted usage reads and the task settings RPC.

use super::*;
use crate::chatwidget::UserMessage;
use codex_app_server_protocol::GetAccountRateLimitsResponse;
use codex_app_server_protocol::ThreadSettingsUpdateParams;
use pretty_assertions::assert_eq;
use serde_json::json;

pub(super) fn reserve_response() -> GetAccountRateLimitsResponse {
    serde_json::from_value(json!({
        "accountId": "workspace-a", "ordinaryUsageAllowed": false,
        "rateLimits": {"limitId": "steve", "primary": {"usedPercent": 100}},
        "rateLimitsByLimitId": {"base_model_inference": {
            "limitId": "base_model_inference", "limitName": "gpt-reserve",
            "primary": {"usedPercent": 48, "windowDurationMins": 10080}
        }},
        "rateLimitUpsell": {
            "banner_type": "luna_reserve", "presentation": "dismissible",
            "title": "You’re now using Luna, a faster model for simpler tasks.",
            "description": "Add credits or upgrade to continue using the most advanced models.",
            "ctas": [{"action": "add_credits", "label": "Add credits"}]
        }
    }))
    .unwrap()
}

pub(super) fn configure_reserve_catalog(app: &mut App) {
    let mut models = app.model_catalog.try_list_models().unwrap();
    let mut reserve = models
        .iter()
        .find(|model| model.model == "gpt-5.5")
        .unwrap()
        .clone();
    reserve.model = "gpt-reserve".into();
    reserve.show_in_picker = false;
    models.push(reserve);
    let original = models
        .iter_mut()
        .find(|model| model.model == "gpt-5.5")
        .unwrap();
    let mut high = original.supported_reasoning_efforts[0].clone();
    high.effort = ReasoningEffortConfig::High;
    original.supported_reasoning_efforts.push(high);
    Arc::make_mut(&mut app.model_catalog).models = models;
    let mut tui = crate::tui::test_support::make_test_tui().unwrap();
    let mut init = app.chatwidget_init_for_forked_or_resumed_thread(
        &mut tui,
        app.config.clone(),
        /*initial_user_message*/ None,
    );
    init.has_chatgpt_account = true;
    init.has_codex_backend_auth = true;
    app.replace_chat_widget(ChatWidget::new_with_app_event(init));
    app.chat_widget
        .handle_thread_session(app.primary_session_configured.clone().unwrap());
    app.chat_widget.set_model("gpt-5.5");
}

#[tokio::test]
async fn luna_reserve_recovery_survives_task_reconstruction() -> Result<()> {
    let (mut app, _events, _ops) = make_test_app_with_channels().await;
    let (mut server, requests, proxy) =
        backend_banner_fallback_tests::start_fallback_thread(&mut app).await?;
    configure_reserve_catalog(&mut app);
    app.chat_widget
        .set_reasoning_effort(Some(ReasoningEffortConfig::High));
    app.apply_backend_banner_fallback(&mut server).await;
    let mut resumed = app.primary_session_configured.clone().unwrap();
    resumed.model = "gpt-reserve".into();
    resumed.collaboration_mode = Some(Box::new(app.chat_widget.effective_collaboration_mode()));
    let mut tui = crate::tui::test_support::make_test_tui()?;
    let mut init = app.chatwidget_init_for_forked_or_resumed_thread(
        &mut tui,
        app.config.clone(),
        /*initial_user_message*/ None,
    );
    init.has_chatgpt_account = true;
    init.has_codex_backend_auth = true;
    // No in-memory transition state is inherited: resume must recover the persisted target.
    app.replace_chat_widget(ChatWidget::new_with_app_event(init));
    app.chat_widget.handle_thread_session(resumed);
    assert_eq!(app.chat_widget.current_model(), "gpt-reserve");
    let mut recovered = reserve_response();
    recovered.ordinary_usage_allowed = Some(true);
    recovered.rate_limit_upsell = None;
    app.apply_backend_banner_fallback(&mut server).await;
    assert_eq!(app.chat_widget.current_model(), "gpt-5.5");
    assert_eq!(
        app.chat_widget.current_reasoning_effort(),
        Some(ReasoningEffortConfig::High)
    );
    assert_eq!(
        requests
            .lock()
            .unwrap()
            .iter()
            .filter(|request| request.method == "thread/settings/update")
            .count(),
        2
    );
    assert!(
        !app.config
            .codex_home
            .join("tui-luna-reserve")
            .join(format!("{}.json", app.active_thread_id.unwrap()))
            .exists()
    );
    server.shutdown().await?;
    proxy.await??;
    Ok(())
}



#[tokio::test]
async fn luna_reserve_recovery_requires_permission_and_no_remaining_blocker() -> Result<()> {
    let (mut app, _events, _ops) = make_test_app_with_channels().await;
    let (mut server, requests, proxy) =
        backend_banner_fallback_tests::start_fallback_thread(&mut app).await?;
    configure_reserve_catalog(&mut app);
    app.apply_backend_banner_fallback(&mut server).await;
    requests.lock().unwrap().clear();
    let mut recovered = reserve_response();
    recovered.rate_limit_upsell = None;
    recovered.rate_limits.primary.as_mut().unwrap().used_percent = 0;
    recovered.rate_limits.primary.as_mut().unwrap().resets_at = Some(0);
    for allowed in [None, Some(false)] {
        recovered.ordinary_usage_allowed = allowed;
        app.apply_backend_banner_fallback(&mut server).await;
        assert_eq!(app.chat_widget.current_model(), "gpt-reserve");
    }
    recovered.ordinary_usage_allowed = Some(true);
    for blocker in [
        json!({"accountId": null, "ordinaryUsageAllowed": null}),
        json!({"rateLimitUpsell": {"unsupported": true}}),
        json!({"rateLimits": {"spendControlReached": true}}),
        json!({"rateLimits": {"rateLimitReachedType": "workspace_owner_usage_limit_reached"}}),
    ] {
        let mut raw = serde_json::to_value(&recovered)?;
        for (key, value) in blocker.as_object().unwrap() {
            raw[key] = value.clone();
        }
        app.apply_backend_banner_fallback(&mut server).await;
        assert_eq!(app.chat_widget.current_model(), "gpt-reserve");
    }
    // Reset redemption clears presentation before the authoritative follow-up read.
    app.chat_widget.clear_backend_banner();
    app.apply_backend_banner_fallback(&mut server).await;
    assert_eq!(app.chat_widget.current_model(), "gpt-5.5");
    assert_eq!(requests.lock().unwrap().len(), 1);
    server.shutdown().await?;
    proxy.await??;
    Ok(())
}

#[tokio::test]
async fn luna_reserve_recovery_does_not_override_manual_choice_or_account_change() -> Result<()> {
    for change_account in [false, true] {
        let (mut app, _events, _ops) = make_test_app_with_channels().await;
        let (mut server, requests, proxy) =
            backend_banner_fallback_tests::start_fallback_thread(&mut app).await?;
        configure_reserve_catalog(&mut app);
        app.apply_backend_banner_fallback(&mut server).await;
        requests.lock().unwrap().clear();
        let mut recovered = reserve_response();
        recovered.ordinary_usage_allowed = Some(true);
        recovered.rate_limit_upsell = None;
        let expected = if change_account {
            recovered.account_id = Some("workspace-b".into());
            "gpt-reserve"
        } else {
            app.chat_widget.set_model("gpt-5.6-terra");
            "gpt-5.6-terra"
        };
        app.apply_backend_banner_fallback(&mut server).await;
        recovered.account_id = Some("workspace-a".into());
        app.apply_backend_banner_fallback(&mut server).await;
        assert_eq!(app.chat_widget.current_model(), expected);
        assert!(requests.lock().unwrap().is_empty());
        server.shutdown().await?;
        proxy.await??;
    }
    Ok(())
}
