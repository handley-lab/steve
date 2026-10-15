use codex_app_server_protocol::ConsumeAccountRateLimitResetCreditOutcome;
use codex_app_server_protocol::ConsumeAccountRateLimitResetCreditResponse;

use super::*;

const USAGE_MENU_VIEW_ID: &str = "usage-menu";
const RATE_LIMIT_RESET_VIEW_ID: &str = "rate-limit-reset";
const RATE_LIMIT_RESET_CONFIRMATION_VIEW_ID: &str = "rate-limit-reset-confirmation";

impl ChatWidget {
    pub(super) fn open_usage_menu(&mut self) {
        self.clear_pending_rate_limit_reset_hint();
        self.bottom_pane
            .show_selection_view(self.usage_menu_params());
        self.request_redraw();
    }

    fn usage_menu_params(&self) -> SelectionViewParams {
        let reset_eligible = self.has_chatgpt_account;
        let (reset_action_enabled, reset_description) =
            match (reset_eligible, self.available_rate_limit_reset_credits) {
                (true, Some(available_count)) if available_count > 0 => {
                    (true, format!("{available_count} available"))
                }
                (true, None) => (true, "Check availability".to_string()),
                (true, Some(_)) | (false, _) => (false, "None available".to_string()),
            };

        SelectionViewParams {
            view_id: Some(USAGE_MENU_VIEW_ID),
            title: Some("Usage".to_string()),
            subtitle: Some("Account usage and resets.".to_string()),
            footer_hint: Some(usage_hint_line(&self.bottom_pane.list_keymap(), "open")),
            items: vec![
                SelectionItem {
                    name: "View analytics".to_string(),
                    description: Some("Usage history".to_string()),
                    actions: vec![Box::new(|tx| {
                        tx.send(AppEvent::OpenAnalytics { view: None });
                    })],
                    dismiss_on_select: true,
                    ..Default::default()
                },
                SelectionItem {
                    name: "Redeem reset".to_string(),
                    description: Some(reset_description),
                    is_disabled: !reset_action_enabled,
                    actions: vec![Box::new(|tx| {
                        tx.send(AppEvent::OpenRateLimitResetCredits);
                    })],
                    dismiss_on_select: true,
                    ..Default::default()
                },
            ],
            ..SelectionViewParams::picker()
        }
    }

    pub(crate) fn show_rate_limit_reset_loading_popup(&mut self) -> u64 {
        self.clear_pending_rate_limit_reset_hint();
        self.pending_rate_limit_reset_idempotency_key = None;
        self.rate_limit_reset_picker_request_id = None;
        let request_id = self.take_next_rate_limit_reset_request_id();
        self.pending_rate_limit_reset_request_id = Some(request_id);
        self.bottom_pane.show_selection_view(SelectionViewParams {
            view_id: Some(RATE_LIMIT_RESET_VIEW_ID),
            title: Some("Usage limit resets".to_string()),
            subtitle: Some("Checking your available resets...".to_string()),
            items: vec![SelectionItem {
                name: "Loading...".to_string(),
                is_disabled: true,
                ..Default::default()
            }],
            ..SelectionViewParams::picker()
        });
        self.request_redraw();
        request_id
    }




    pub(crate) fn start_rate_limit_reset_consumption(
        &mut self,
        idempotency_key: &str,
    ) -> Option<u64> {
        if self.pending_rate_limit_reset_idempotency_key.as_deref() != Some(idempotency_key) {
            return None;
        }
        Some(self.show_rate_limit_reset_consuming_popup())
    }

    fn rate_limit_reset_message_params(message: &str) -> SelectionViewParams {
        SelectionViewParams {
            view_id: Some(RATE_LIMIT_RESET_VIEW_ID),
            title: Some("Usage limit resets".to_string()),
            subtitle: Some(message.to_string()),
            items: vec![SelectionItem {
                name: "Close".to_string(),
                dismiss_on_select: true,
                ..Default::default()
            }],
            ..SelectionViewParams::picker()
        }
    }

    fn reset_refresh_params(message: &str) -> SelectionViewParams {
        SelectionViewParams {
            view_id: Some(RATE_LIMIT_RESET_VIEW_ID),
            title: Some("Usage limit resets".to_string()),
            subtitle: Some(message.to_string()),
            items: vec![
                SelectionItem {
                    name: "Try again".to_string(),
                    actions: vec![Box::new(|tx| {
                        tx.send(AppEvent::OpenRateLimitResetCredits);
                    })],
                    dismiss_on_select: true,
                    ..Default::default()
                },
                SelectionItem {
                    name: "Close".to_string(),
                    dismiss_on_select: true,
                    ..Default::default()
                },
            ],
            ..SelectionViewParams::picker()
        }
    }

    pub(crate) fn show_rate_limit_reset_consuming_popup(&mut self) -> u64 {
        self.clear_pending_rate_limit_reset_hint();
        self.pending_rate_limit_reset_idempotency_key = None;
        self.rate_limit_reset_picker_request_id = None;
        let request_id = self.take_next_rate_limit_reset_request_id();
        self.pending_rate_limit_reset_request_id = Some(request_id);
        self.bottom_pane
            .dismiss_view_by_id(RATE_LIMIT_RESET_CONFIRMATION_VIEW_ID);
        self.bottom_pane
            .dismiss_view_by_id(RATE_LIMIT_RESET_VIEW_ID);
        self.bottom_pane.show_selection_view(SelectionViewParams {
            view_id: Some(RATE_LIMIT_RESET_VIEW_ID),
            title: Some("Usage limit resets".to_string()),
            subtitle: Some("Resetting your usage...".to_string()),
            items: vec![SelectionItem {
                name: "Using a reset...".to_string(),
                is_disabled: true,
                ..Default::default()
            }],
            allow_cancel: false,
            ..SelectionViewParams::picker()
        });
        self.request_redraw();
        request_id
    }

    pub(crate) fn finish_rate_limit_reset_consume(
        &mut self,
        request_id: u64,
        idempotency_key: String,
        credit_id: Option<String>,
        result: Result<ConsumeAccountRateLimitResetCreditResponse, String>,
    ) -> bool {
        if self.pending_rate_limit_reset_request_id != Some(request_id) {
            return false;
        }

        match result {
            Ok(response)
                if matches!(
                    response.outcome,
                    ConsumeAccountRateLimitResetCreditOutcome::Reset
                        | ConsumeAccountRateLimitResetCreditOutcome::AlreadyRedeemed
                ) =>
            {
                self.available_rate_limit_reset_credits = None;
                self.pending_rate_limit_reset_request_id = None;
                self.replace_rate_limit_reset_popup(Self::rate_limit_reset_message_params(
                    "Usage reset.",
                ));
                true
            }
            Ok(response) => {
                self.pending_rate_limit_reset_request_id = None;
                let message = match response.outcome {
                    ConsumeAccountRateLimitResetCreditOutcome::NothingToReset => {
                        "Your usage does not need a reset right now."
                    }
                    ConsumeAccountRateLimitResetCreditOutcome::NoCredit if credit_id.is_some() => {
                        self.available_rate_limit_reset_credits = None;
                        self.replace_rate_limit_reset_popup(Self::reset_refresh_params(
                            "That reset is no longer available. Refresh to see your current resets.",
                        ));
                        return false;
                    }
                    ConsumeAccountRateLimitResetCreditOutcome::NoCredit => {
                        self.available_rate_limit_reset_credits = Some(0);
                        "No usage limit resets are available."
                    }
                    ConsumeAccountRateLimitResetCreditOutcome::Reset
                    | ConsumeAccountRateLimitResetCreditOutcome::AlreadyRedeemed => unreachable!(),
                };
                self.replace_rate_limit_reset_popup(Self::rate_limit_reset_message_params(message));
                false
            }
            Err(_) => {
                self.pending_rate_limit_reset_request_id = None;
                self.pending_rate_limit_reset_idempotency_key = Some(idempotency_key.clone());
                self.replace_rate_limit_reset_popup(SelectionViewParams {
                    view_id: Some(RATE_LIMIT_RESET_VIEW_ID),
                    title: Some("Usage limit resets".to_string()),
                    subtitle: Some("Couldn't reset usage. Please try again.".to_string()),
                    items: vec![
                        SelectionItem {
                            name: "Try again".to_string(),
                            actions: vec![Box::new(move |tx| {
                                tx.send(AppEvent::ConsumeRateLimitResetCredit {
                                    idempotency_key: idempotency_key.clone(),
                                    credit_id: credit_id.clone(),
                                });
                            })],
                            dismiss_on_select: true,
                            ..Default::default()
                        },
                        SelectionItem {
                            name: "Close".to_string(),
                            dismiss_on_select: true,
                            ..Default::default()
                        },
                    ],
                    ..SelectionViewParams::picker()
                });
                false
            }
        }
    }



    fn replace_rate_limit_reset_popup(&mut self, params: SelectionViewParams) {
        if self
            .bottom_pane
            .replace_selection_view_if_present(RATE_LIMIT_RESET_VIEW_ID, params)
        {
            self.request_redraw();
        }
    }

    pub(crate) fn start_rate_limit_reset_startup_check(&mut self) -> u64 {
        self.clear_pending_rate_limit_reset_hint();
        let request_id = self.take_next_rate_limit_reset_request_id();
        self.pending_rate_limit_reset_hint_request_id = Some(request_id);
        request_id
    }


    pub(crate) fn clear_pending_rate_limit_reset_requests(&mut self) {
        self.pending_rate_limit_reset_request_id = None;
        self.pending_rate_limit_reset_idempotency_key = None;
        self.rate_limit_reset_picker_request_id = None;
        self.available_rate_limit_reset_credits = None;
        self.rate_limit_snapshots_by_limit_id.clear();
        self.clear_pending_rate_limit_reset_hint();
        self.bottom_pane.dismiss_view_by_id(USAGE_MENU_VIEW_ID);
        self.bottom_pane
            .dismiss_view_by_id(RATE_LIMIT_RESET_VIEW_ID);
        self.bottom_pane
            .dismiss_view_by_id(RATE_LIMIT_RESET_CONFIRMATION_VIEW_ID);
    }

    pub(crate) fn clear_pending_rate_limit_reset_hint(&mut self) {
        self.pending_rate_limit_reset_hint_request_id = None;
        let cleared_hint = self.pending_rate_limit_reset_hint.take().is_some();
        if cleared_hint {
            self.bump_active_cell_revision();
            self.request_redraw();
        }
    }

    pub(super) fn pending_rate_limit_reset_hint(&self) -> Option<&PlainHistoryCell> {
        self.pending_rate_limit_reset_hint.as_ref()
    }

    pub(crate) fn take_pending_rate_limit_reset_hint(&mut self) -> Option<PlainHistoryCell> {
        let hint = self.pending_rate_limit_reset_hint.take()?;
        self.bump_active_cell_revision();
        Some(hint)
    }


    fn take_next_rate_limit_reset_request_id(&mut self) -> u64 {
        let request_id = self.next_rate_limit_reset_request_id;
        self.next_rate_limit_reset_request_id = self
            .next_rate_limit_reset_request_id
            .wrapping_add(/*rhs*/ 1);
        request_id
    }
}

/// Keep usage actions readable on narrow terminals and honor customized list bindings.
fn usage_hint_line(
    keymap: &crate::keymap::ListKeymap,
    accept_label: &'static str,
) -> Line<'static> {
    let mut spans = Vec::new();
    for (action, label) in [
        (crate::keymap::ListAction::Accept, accept_label),
        (crate::keymap::ListAction::Cancel, "back"),
    ] {
        if let Some(hint) = keymap.primary_hint(action) {
            if !spans.is_empty() {
                spans.push(" · ".into());
            }
            spans.extend(hint.spans());
            spans.push(format!(" {label}").into());
        }
    }
    Line::from(spans)
}

