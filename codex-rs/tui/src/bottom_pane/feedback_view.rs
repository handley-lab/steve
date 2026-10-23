use codex_feedback::CODEX_APP_DIRECTORY_CACHE_ATTACHMENT_FILENAME;
use codex_feedback::CODEX_APPS_TOOLS_CACHE_ATTACHMENT_FILENAME;
use codex_feedback::DOCTOR_REPORT_ATTACHMENT_FILENAME;
use codex_feedback::FEEDBACK_DIAGNOSTICS_ATTACHMENT_FILENAME;
use codex_feedback::FeedbackDiagnostics;
use codex_feedback::WINDOWS_SANDBOX_LOG_ATTACHMENT_FILENAME;
use ratatui::style::Stylize;
use ratatui::text::Line;

use crate::app_event::AppEvent;
use crate::app_event::FeedbackCategory;
use crate::app_event_sender::AppEventSender;
use crate::history_cell;

use super::popup_consts::standard_popup_hint_line;

pub(crate) fn should_show_feedback_connectivity_details(
    category: FeedbackCategory,
    diagnostics: &FeedbackDiagnostics,
) -> bool {
    category != FeedbackCategory::GoodResult && !diagnostics.is_empty()
}

pub(crate) fn feedback_classification(category: FeedbackCategory) -> &'static str {
    match category {
        FeedbackCategory::BadResult => "bad_result",
        FeedbackCategory::GoodResult => "good_result",
        FeedbackCategory::Bug => "bug",
        FeedbackCategory::SafetyCheck => "safety_check",
        FeedbackCategory::Other => "other",
    }
}

pub(crate) fn feedback_success_cell(
    _category: FeedbackCategory,
    include_logs: bool,
    thread_id: &str,
) -> history_cell::WebHyperlinkHistoryCell {
    let prefix = if include_logs {
        "• Feedback uploaded."
    } else {
        "• Feedback recorded (no logs)."
    };
    history_cell::WebHyperlinkHistoryCell::new(vec![
        Line::from(format!("{prefix} Thanks for the feedback!")),
        "".into(),
        Line::from(vec!["  Thread ID: ".into(), thread_id.to_string().bold()]),
    ])
}

// Build the selection popup params for feedback categories.
pub(crate) fn feedback_selection_params(
    app_event_tx: AppEventSender,
) -> super::SelectionViewParams {
    super::SelectionViewParams {
        title: Some("How was this?".to_string()),
        items: vec![
            make_feedback_item(
                app_event_tx.clone(),
                "bug",
                "Crash, error message, hang, or broken UI/behavior",
                FeedbackCategory::Bug,
            ),
            make_feedback_item(
                app_event_tx.clone(),
                "bad result",
                "Output was off-target, incorrect, incomplete, or unhelpful",
                FeedbackCategory::BadResult,
            ),
            make_feedback_item(
                app_event_tx.clone(),
                "good result",
                "Helpful, correct, high‑quality, or delightful result worth celebrating",
                FeedbackCategory::GoodResult,
            ),
            make_feedback_item(
                app_event_tx.clone(),
                "safety check",
                "Benign usage blocked due to safety checks or refusals",
                FeedbackCategory::SafetyCheck,
            ),
            make_feedback_item(
                app_event_tx,
                "other",
                "Slowness, feature suggestion, UX feedback, or anything else",
                FeedbackCategory::Other,
            ),
        ],
        ..super::SelectionViewParams::picker()
    }
}

/// Build the selection popup params shown when feedback is disabled.
pub(crate) fn feedback_disabled_params() -> super::SelectionViewParams {
    super::SelectionViewParams {
        title: Some("Sending feedback is disabled".to_string()),
        subtitle: Some("This action is disabled by configuration.".to_string()),
        footer_hint: Some(standard_popup_hint_line()),
        items: vec![super::SelectionItem {
            name: "Close".to_string(),
            dismiss_on_select: true,
            ..Default::default()
        }],
        ..super::SelectionViewParams::picker()
    }
}

fn make_feedback_item(
    app_event_tx: AppEventSender,
    name: &str,
    description: &str,
    category: FeedbackCategory,
) -> super::SelectionItem {
    let action: super::SelectionAction = Box::new(move |_sender: &AppEventSender| {
        app_event_tx.send(AppEvent::OpenFeedbackConsent { category });
    });
    super::SelectionItem {
        name: name.to_string(),
        description: Some(description.to_string()),
        actions: vec![action],
        dismiss_on_select: true,
        ..Default::default()
    }
}

/// Build the upload consent popup params for a given feedback category.
pub(crate) fn feedback_upload_consent_params(
    app_event_tx: AppEventSender,
    category: FeedbackCategory,
    rollout_path: Option<std::path::PathBuf>,
    auto_review_rollout_filename: Option<String>,
    include_windows_sandbox_log: bool,
    feedback_diagnostics: &FeedbackDiagnostics,
) -> super::SelectionViewParams {
    use super::popup_consts::standard_popup_hint_line;
    let yes_action: super::SelectionAction = Box::new({
        let tx = app_event_tx.clone();
        move |sender: &AppEventSender| {
            let _ = sender;
            tx.send(AppEvent::OpenFeedbackNote {
                category,
                include_logs: true,
            });
        }
    });

    let no_action: super::SelectionAction = Box::new({
        let tx = app_event_tx;
        move |sender: &AppEventSender| {
            let _ = sender;
            tx.send(AppEvent::OpenFeedbackNote {
                category,
                include_logs: false,
            });
        }
    });

    // Build header listing files that would be sent if user consents.
    let mut header_lines: Vec<Box<dyn crate::render::renderable::Renderable>> = vec![
        Line::from("Upload logs?".bold()).into(),
        Line::from("").into(),
        Line::from("The following files will be sent:".dim()).into(),
        Line::from(vec!["  • ".into(), "codex-logs.log".into()]).into(),
        Line::from(vec!["  • ".into(), "client-logs.txt (if available)".into()]).into(),
        Line::from(vec![
            "  • ".into(),
            DOCTOR_REPORT_ATTACHMENT_FILENAME.into(),
        ])
        .into(),
        Line::from(vec![
            "  • ".into(),
            format!("{CODEX_APPS_TOOLS_CACHE_ATTACHMENT_FILENAME} (if available)").into(),
        ])
        .into(),
        Line::from(vec![
            "  • ".into(),
            format!("{CODEX_APP_DIRECTORY_CACHE_ATTACHMENT_FILENAME} (if available)").into(),
        ])
        .into(),
    ];
    if include_windows_sandbox_log {
        header_lines.push(
            Line::from(vec![
                "  • ".into(),
                WINDOWS_SANDBOX_LOG_ATTACHMENT_FILENAME.into(),
            ])
            .into(),
        );
    }
    if let Some(path) = rollout_path.as_deref()
        && let Some(name) = path.file_name().map(|s| s.to_string_lossy().to_string())
    {
        header_lines.push(Line::from(vec!["  • ".into(), name.into()]).into());
    }
    if let Some(filename) = auto_review_rollout_filename {
        header_lines.push(Line::from(vec!["  • ".into(), filename.into()]).into());
    }
    if !feedback_diagnostics.is_empty() {
        header_lines.push(
            Line::from(vec![
                "  • ".into(),
                FEEDBACK_DIAGNOSTICS_ATTACHMENT_FILENAME.into(),
            ])
            .into(),
        );
    }
    if should_show_feedback_connectivity_details(category, feedback_diagnostics) {
        header_lines.push(Line::from("").into());
        header_lines.push(Line::from("Connectivity diagnostics".bold()).into());
        for diagnostic in feedback_diagnostics.diagnostics() {
            header_lines
                .push(Line::from(vec!["  - ".into(), diagnostic.headline.clone().into()]).into());
            for detail in &diagnostic.details {
                header_lines.push(Line::from(vec!["    - ".dim(), detail.clone().into()]).into());
            }
        }
    }

    super::SelectionViewParams {
        footer_hint: Some(standard_popup_hint_line()),
        items: vec![
            super::SelectionItem {
                name: "Yes".to_string(),
                description: Some(
                    "Include the listed logs and diagnostics with the note sent to the configured feedback destination"
                        .to_string(),
                ),
                actions: vec![yes_action],
                dismiss_on_select: true,
                ..Default::default()
            },
            super::SelectionItem {
                name: "No".to_string(),
                description: Some("Send the note without log attachments".to_string()),
                actions: vec![no_action],
                dismiss_on_select: true,
                ..Default::default()
            },
        ],
        header: Box::new(crate::render::renderable::ColumnRenderable::with(
            header_lines,
        )),
        ..super::SelectionViewParams::confirmation()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app_event::AppEvent;
    use crate::app_event_sender::AppEventSender;
    use crate::render::renderable::Renderable;
    use codex_feedback::FeedbackDiagnostic;
    use pretty_assertions::assert_eq;
    use ratatui::buffer::Buffer;
    use ratatui::layout::Rect;

    fn render_renderable(renderable: &dyn Renderable, width: u16) -> String {
        let height = renderable.desired_height(width);
        let area = Rect::new(0, 0, width, height);
        let mut buf = Buffer::empty(area);
        renderable.render(area, &mut buf);
        render_buffer(area, &buf)
    }

    fn render_buffer(area: Rect, buf: &Buffer) -> String {
        let mut lines: Vec<String> = (0..area.height)
            .map(|row| {
                let mut line = String::new();
                for col in 0..area.width {
                    let symbol = buf[(area.x + col, area.y + row)].symbol();
                    if symbol.is_empty() {
                        line.push(' ');
                    } else {
                        line.push_str(&crate::terminal_hyperlinks::strip_osc8(symbol));
                    }
                }
                line.trim_end().to_string()
            })
            .collect();

        while lines.first().is_some_and(|l| l.trim().is_empty()) {
            lines.remove(0);
        }
        while lines.last().is_some_and(|l| l.trim().is_empty()) {
            lines.pop();
        }
        lines.join("\n")
    }

    fn render_cell(cell: &impl history_cell::HistoryCell, width: u16) -> String {
        cell.display_lines(width)
            .into_iter()
            .map(|line| {
                line.spans
                    .into_iter()
                    .map(|span| span.content.into_owned())
                    .collect::<String>()
                    .trim_end()
                    .to_string()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn feedback_upload_consent_lists_doctor_report() {
        let (tx_raw, _rx) = tokio::sync::mpsc::unbounded_channel::<AppEvent>();
        let tx = AppEventSender::new(tx_raw);
        let params = feedback_upload_consent_params(
            tx,
            FeedbackCategory::Bug,
            Some(std::path::PathBuf::from("rollout.jsonl")),
            Some("auto-review-rollout.jsonl".to_string()),
            /*include_windows_sandbox_log*/ false,
            &FeedbackDiagnostics::default(),
        );

        let rendered = render_renderable(params.header.as_ref(), /*width*/ 60);

        insta::assert_snapshot!("feedback_upload_consent_lists_doctor_report", rendered);
    }

    #[test]
    fn feedback_upload_consent_lists_windows_sandbox_log_when_included() {
        let (tx_raw, _rx) = tokio::sync::mpsc::unbounded_channel::<AppEvent>();
        let tx = AppEventSender::new(tx_raw);
        let params = feedback_upload_consent_params(
            tx,
            FeedbackCategory::Bug,
            Some(std::path::PathBuf::from("rollout.jsonl")),
            Some("auto-review-rollout.jsonl".to_string()),
            /*include_windows_sandbox_log*/ true,
            &FeedbackDiagnostics::default(),
        );

        let rendered = render_renderable(params.header.as_ref(), /*width*/ 60);

        insta::assert_snapshot!(
            "feedback_upload_consent_lists_windows_sandbox_log_when_included",
            rendered
        );
    }

    #[test]
    fn should_show_feedback_connectivity_details_only_for_non_good_result_with_diagnostics() {
        let diagnostics = FeedbackDiagnostics::new(vec![FeedbackDiagnostic {
            headline: "Proxy environment variables are set and may affect connectivity."
                .to_string(),
            details: vec!["HTTP_PROXY = http://proxy.example.com:8080".to_string()],
        }]);

        assert_eq!(
            should_show_feedback_connectivity_details(FeedbackCategory::Bug, &diagnostics),
            true
        );
        assert_eq!(
            should_show_feedback_connectivity_details(FeedbackCategory::GoodResult, &diagnostics),
            false
        );
        assert_eq!(
            should_show_feedback_connectivity_details(
                FeedbackCategory::BadResult,
                &FeedbackDiagnostics::default()
            ),
            false
        );
    }

    #[test]
    fn feedback_success_cell_matches_external_bug_copy() {
        let rendered = render_cell(
            &feedback_success_cell(
                FeedbackCategory::Bug,
                /*include_logs*/ true,
                "thread-1",
            ),
            /*width*/ 120,
        );
        assert_eq!(
            rendered,
            "• Feedback uploaded. Thanks for the feedback!\n\n  Thread ID: thread-1"
        );
    }

    #[test]
    fn feedback_success_cell_matches_good_result_copy() {
        let rendered = render_cell(
            &feedback_success_cell(
                FeedbackCategory::GoodResult,
                /*include_logs*/ false,
                "thread-3",
            ),
            /*width*/ 120,
        );
        assert_eq!(
            rendered,
            "• Feedback recorded (no logs). Thanks for the feedback!\n\n  Thread ID: thread-3"
        );
    }

    #[test]
    fn feedback_success_cell_shows_thread_id_for_remaining_categories() {
        for category in [
            FeedbackCategory::BadResult,
            FeedbackCategory::SafetyCheck,
            FeedbackCategory::Other,
        ] {
            let rendered = render_cell(
                &feedback_success_cell(
                    category,
                    /*include_logs*/ false,
                    "thread-4",
                ),
                /*width*/ 120,
            );
            assert!(rendered.contains("Thanks for the feedback!"));
            assert!(rendered.contains("thread-4"));
        }
    }

    #[test]
    fn provider_neutral_feedback_success_has_no_unconfigured_followup() {
        use crate::history_cell::HistoryCell;
        for category in [
            FeedbackCategory::Bug,
            FeedbackCategory::BadResult,
            FeedbackCategory::GoodResult,
            FeedbackCategory::SafetyCheck,
            FeedbackCategory::Other,
        ] {
            for include_logs in [false, true] {
                let cell = feedback_success_cell(category, include_logs, "thread-test");
                assert!(
                    cell.display_hyperlink_lines(120)
                        .iter()
                        .all(|line| line.hyperlinks.is_empty())
                );
                let text = render_cell(&cell, 120);
                assert!(text.contains("thread-test"));
                assert!(text.contains("Thanks for the feedback!"), "{text}");
            }
        }
    }
}
