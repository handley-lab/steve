use super::*;

const USAGE_MENU_VIEW_ID: &str = "usage-menu";

impl ChatWidget {
    pub(super) fn open_usage_menu(&mut self) {
        self.clear_pending_rate_limit_reset_hint();
        self.bottom_pane
            .show_selection_view(self.usage_menu_params());
        self.request_redraw();
    }

    fn usage_menu_params(&self) -> SelectionViewParams {
        SelectionViewParams {
            view_id: Some(USAGE_MENU_VIEW_ID),
            title: Some("Usage".to_string()),
            subtitle: Some("Account usage and resets.".to_string()),
            footer_hint: Some(usage_hint_line(&self.bottom_pane.list_keymap(), "open")),
            items: vec![SelectionItem {
                name: "View analytics".to_string(),
                description: Some("Usage history".to_string()),
                actions: vec![Box::new(|tx| {
                    tx.send(AppEvent::OpenAnalytics { view: None });
                })],
                dismiss_on_select: true,
                ..Default::default()
            }],
            ..SelectionViewParams::picker()
        }
    }

    pub(crate) fn clear_pending_rate_limit_reset_requests(&mut self) {
        self.rate_limit_snapshots_by_limit_id.clear();
        self.clear_pending_rate_limit_reset_hint();
        self.bottom_pane.dismiss_view_by_id(USAGE_MENU_VIEW_ID);
    }

    pub(crate) fn clear_pending_rate_limit_reset_hint(&mut self) {
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
