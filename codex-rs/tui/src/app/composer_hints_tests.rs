//! Usage warnings preserve composer geometry and yield to active interactions.

use super::*;
use pretty_assertions::assert_eq;

#[tokio::test]
async fn escape_closes_shortcut_help_before_transcript_backtracking() -> Result<()> {
    let mut app = crate::app::test_support::make_test_app().await;
    let mut server = Box::pin(crate::start_embedded_app_server_for_picker(&app.config)).await?;
    let mut tui = crate::tui::test_support::make_test_tui()?;
    tui.set_owned_screen(/*owned*/ true)?;
    app.transcript_cells = vec![Arc::new(crate::history_cell::PlainHistoryCell::new(
        (0..60)
            .map(|row| format!("response row {row}").into())
            .collect(),
    ))];
    app.render_owned_transcript(&mut tui, Size::new(/*width*/ 80, /*height*/ 24))?;
    app.transcript_view
        .scroll(&app.transcript_cells, /*rows*/ -20);
    let area = Rect::new(
        /*x*/ 0, /*y*/ 0, /*width*/ 80, /*height*/ 8,
    );
    let mut before = ratatui::buffer::Buffer::empty(area);
    app.transcript_view
        .render(area, &mut before, &app.transcript_cells);
    let escape = KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE);
    let toggle = KeyEvent::new(KeyCode::Char('?'), KeyModifiers::NONE);
    assert!(app.should_handle_backtrack_esc(escape));
    app.handle_tui_event(&mut tui, &mut server, TuiEvent::Key(toggle))
        .await?;
    assert!(app.chat_widget.shortcut_overlay_visible());
    assert!(!app.should_handle_backtrack_esc(escape));
    assert!(!app.should_reject_side_backtrack_esc(escape));

    app.handle_tui_event(&mut tui, &mut server, TuiEvent::Key(escape))
        .await?;
    assert!(!app.chat_widget.shortcut_overlay_visible());
    assert!(!app.backtrack.primed);
    assert!(!app.transcript_view.is_following());
    assert!(app.should_handle_backtrack_esc(escape));
    let mut after = ratatui::buffer::Buffer::empty(area);
    app.transcript_view
        .render(area, &mut after, &app.transcript_cells);
    assert_eq!(after, before);

    // Search and selection replace the help footer and keep their own first Escape.
    for search in [true, false] {
        app.handle_tui_event(&mut tui, &mut server, TuiEvent::Key(toggle))
            .await?;
        if search {
            app.transcript_view.begin_search();
        } else {
            tui.screen_size_for_event(&TuiEvent::Resize(Size::new(
                /*width*/ 80, /*height*/ 40,
            )))?;
            app.handle_owned_transcript_event(
                &mut tui,
                &mut server,
                &TuiEvent::Key(KeyEvent::new(KeyCode::Char(' '), KeyModifiers::CONTROL)),
            )?;
        }
        assert!(app.transcript_view.has_active_interaction());
        app.handle_tui_event(&mut tui, &mut server, TuiEvent::Key(escape))
            .await?;
        assert!(!app.transcript_view.has_active_interaction());
        assert!(app.chat_widget.shortcut_overlay_visible());
        app.handle_tui_event(&mut tui, &mut server, TuiEvent::Key(escape))
            .await?;
        assert!(!app.chat_widget.shortcut_overlay_visible());
    }
    server.shutdown().await?;
    tui.set_owned_screen(/*owned*/ false)?;
    Ok(())
}
