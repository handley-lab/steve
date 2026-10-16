mod app;
mod cli;
pub(crate) mod scrollable_diff;
mod ui;
pub(crate) mod util;
pub use cli::Cli;

use codex_login::default_client::get_codex_user_agent;
use std::io::IsTerminal;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use std::time::Instant;
use tokio::sync::mpsc::UnboundedSender;
use tracing::info;
use tracing_subscriber::EnvFilter;
use util::append_error_log;
use util::set_user_agent_suffix;

struct ApplyJob {
    task_id: codex_cloud_tasks_client::TaskId,
    diff_override: Option<String>,
}

struct BackendContext {
    backend: Arc<dyn codex_cloud_tasks_client::CloudBackend>,
}

async fn init_backend(user_agent_suffix: &str) -> anyhow::Result<BackendContext> {
    #[cfg(debug_assertions)]
    let use_mock = matches!(
        std::env::var("STEVE_CLOUD_TASKS_MODE").ok().as_deref(),
        Some("mock") | Some("MOCK")
    );
    let base_url = util::configured_cloud_tasks_base_url()?;

    set_user_agent_suffix(user_agent_suffix);

    #[cfg(debug_assertions)]
    if use_mock {
        return Ok(BackendContext {
            backend: Arc::new(codex_cloud_tasks_mock_client::MockClient),
        });
    }

    let (auth_manager, _http_client_factory) =
        util::load_auth_manager(Some(base_url.clone())).await;
    let http = codex_cloud_tasks_client::HttpClient::new(base_url.clone());
    let style = if base_url.contains("/backend-api") {
        "wham"
    } else {
        "codex-api"
    };
    append_error_log(format!("startup: base_url={base_url} path_style={style}"));

    let auth = match auth_manager.as_ref() {
        Some(manager) => manager.auth().await,
        None => None,
    };
    let auth = match auth {
        Some(auth) => auth,
        None => {
            eprintln!(
                "Not signed in. Please run 'codex login' to sign in with ChatGPT, then re-run 'codex cloud'."
            );
            std::process::exit(1);
        }
    };

    if let Some(acc) = auth.get_account_id() {
        append_error_log(format!("auth: mode=ChatGPT account_id={acc}"));
    }

    if !auth.uses_codex_backend() {
        eprintln!(
            "Not signed in. Please run 'codex login' to sign in with ChatGPT, then re-run 'codex cloud'."
        );
        std::process::exit(1);
    }

    Ok(BackendContext {
        backend: Arc::new(http),
    })
}

fn level_from_status(status: codex_cloud_tasks_client::ApplyStatus) -> app::ApplyResultLevel {
    match status {
        codex_cloud_tasks_client::ApplyStatus::Success => app::ApplyResultLevel::Success,
        codex_cloud_tasks_client::ApplyStatus::Partial => app::ApplyResultLevel::Partial,
        codex_cloud_tasks_client::ApplyStatus::Error => app::ApplyResultLevel::Error,
    }
}

fn spawn_preflight(
    app: &mut app::App,
    backend: &Arc<dyn codex_cloud_tasks_client::CloudBackend>,
    tx: &UnboundedSender<app::AppEvent>,
    frame_tx: &UnboundedSender<Instant>,
    title: String,
    job: ApplyJob,
) -> bool {
    if app.apply_inflight {
        app.status = "An apply is already running; wait for it to finish first.".to_string();
        return false;
    }
    if app.apply_preflight_inflight {
        app.status = "A preflight is already running; wait for it to finish first.".to_string();
        return false;
    }

    app.apply_preflight_inflight = true;
    let _ = frame_tx.send(Instant::now() + Duration::from_millis(100));

    let backend = backend.clone();
    let tx = tx.clone();
    tokio::spawn(async move {
        let ApplyJob {
            task_id,
            diff_override,
        } = job;
        let result = codex_cloud_tasks_client::CloudBackend::apply_task_preflight(
            &*backend,
            task_id.clone(),
            diff_override,
        )
        .await;

        let event = match result {
            Ok(outcome) => {
                let level = level_from_status(outcome.status);
                app::AppEvent::ApplyPreflightFinished {
                    id: task_id,
                    title,
                    message: outcome.message,
                    level,
                    skipped: outcome.skipped_paths,
                    conflicts: outcome.conflict_paths,
                }
            }
            Err(e) => app::AppEvent::ApplyPreflightFinished {
                id: task_id,
                title,
                message: format!("Preflight failed: {e}"),
                level: app::ApplyResultLevel::Error,
                skipped: Vec::new(),
                conflicts: Vec::new(),
            },
        };

        let _ = tx.send(event);
    });

    true
}

fn spawn_apply(
    app: &mut app::App,
    backend: &Arc<dyn codex_cloud_tasks_client::CloudBackend>,
    tx: &UnboundedSender<app::AppEvent>,
    frame_tx: &UnboundedSender<Instant>,
    job: ApplyJob,
) -> bool {
    if app.apply_inflight {
        app.status = "An apply is already running; wait for it to finish first.".to_string();
        return false;
    }
    if app.apply_preflight_inflight {
        app.status = "Finish the current preflight before starting another apply.".to_string();
        return false;
    }

    app.apply_inflight = true;
    let _ = frame_tx.send(Instant::now() + Duration::from_millis(100));

    let backend = backend.clone();
    let tx = tx.clone();
    tokio::spawn(async move {
        let ApplyJob {
            task_id,
            diff_override,
        } = job;
        let result = codex_cloud_tasks_client::CloudBackend::apply_task(
            &*backend,
            task_id.clone(),
            diff_override,
        )
        .await;

        let event = match result {
            Ok(outcome) => app::AppEvent::ApplyFinished {
                id: task_id,
                result: Ok(outcome),
            },
            Err(e) => app::AppEvent::ApplyFinished {
                id: task_id,
                result: Err(format!("{e}")),
            },
        };

        let _ = tx.send(event);
    });

    true
}

// logging helper lives in util module

// (no standalone patch summarizer needed – UI displays raw diffs)

/// Entry point for the `codex cloud` subcommand.
pub async fn run_main(_cli: Cli, _codex_linux_sandbox_exe: Option<PathBuf>) -> anyhow::Result<()> {
    // Very minimal logging setup; mirrors other crates' pattern.
    let default_level = "error";
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env()
                .or_else(|_| EnvFilter::try_new(default_level))
                .unwrap_or_else(|_| EnvFilter::new(default_level)),
        )
        .with_ansi(std::io::stderr().is_terminal())
        .with_writer(std::io::stderr)
        .try_init();

    info!("Launching Cloud Tasks list UI");
    let BackendContext { backend } = init_backend("codex_cloud_tasks_tui").await?;

    // Terminal setup
    use crossterm::ExecutableCommand;
    use crossterm::event::DisableBracketedPaste;
    use crossterm::event::EnableBracketedPaste;
    use crossterm::event::KeyboardEnhancementFlags;
    use crossterm::event::PopKeyboardEnhancementFlags;
    use crossterm::event::PushKeyboardEnhancementFlags;
    use crossterm::terminal::EnterAlternateScreen;
    use crossterm::terminal::LeaveAlternateScreen;
    use crossterm::terminal::disable_raw_mode;
    use crossterm::terminal::enable_raw_mode;
    use ratatui::Terminal;
    use ratatui::backend::CrosstermBackend;
    let mut stdout = std::io::stdout();
    enable_raw_mode()?;
    stdout.execute(EnterAlternateScreen)?;
    stdout.execute(EnableBracketedPaste)?;
    // Enable enhanced key reporting so Shift+Enter is distinguishable from Enter.
    // Some terminals may not support these flags; ignore errors if enabling fails.
    let _ = crossterm::execute!(
        std::io::stdout(),
        PushKeyboardEnhancementFlags(
            KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES
                | KeyboardEnhancementFlags::REPORT_EVENT_TYPES
                | KeyboardEnhancementFlags::REPORT_ALTERNATE_KEYS
        )
    );
    let backend_ui = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend_ui)?;
    terminal.clear()?;

    // App state
    let mut app = app::App::new();
    // Initial load
    let force_internal = matches!(
        std::env::var("STEVE_CLOUD_TASKS_FORCE_INTERNAL")
            .ok()
            .as_deref(),
        Some("1") | Some("true") | Some("TRUE")
    );
    append_error_log(format!(
        "startup: wham_force_internal={} ua={}",
        force_internal,
        get_codex_user_agent()
    ));
    app.in_flight.clear();

    // Event stream
    use crossterm::event::Event;
    use crossterm::event::EventStream;
    use crossterm::event::KeyCode;
    use crossterm::event::KeyEventKind;
    use crossterm::event::KeyModifiers;
    use tokio_stream::StreamExt;
    let mut events = EventStream::new();

    // Channel for non-blocking background loads
    use tokio::sync::mpsc::unbounded_channel;
    let (tx, mut rx) = unbounded_channel::<app::AppEvent>();

    // Event-driven redraws with a tiny coalescing scheduler (snappy UI, no fixed 250ms tick).
    let mut needs_redraw = true;
    use std::time::Instant;
    use tokio::time::Instant as TokioInstant;
    use tokio::time::sleep_until;
    let (frame_tx, mut frame_rx) = tokio::sync::mpsc::unbounded_channel::<Instant>();
    let (redraw_tx, mut redraw_rx) = tokio::sync::mpsc::unbounded_channel::<()>();

    // Coalesce frame requests to the earliest deadline; emit a single redraw signal.
    tokio::spawn(async move {
        let mut next_deadline: Option<Instant> = None;
        loop {
            let target =
                next_deadline.unwrap_or_else(|| Instant::now() + Duration::from_secs(24 * 60 * 60));
            let sleeper = sleep_until(TokioInstant::from_std(target));
            tokio::pin!(sleeper);
            tokio::select! {
                recv = frame_rx.recv() => {
                    match recv {
                        Some(at) => {
                            if next_deadline.is_none_or(|cur| at < cur) {
                                next_deadline = Some(at);
                            }
                            continue; // recompute sleep target
                        }
                        None => break,
                    }
                }
                _ = &mut sleeper => {
                    if next_deadline.take().is_some() {
                        let _ = redraw_tx.send(());
                    }
                }
            }
        }
    });
    // Kick an initial draw so the UI appears immediately.
    let _ = frame_tx.send(Instant::now());

    // Render helper to centralize immediate redraws after handling events.
    let render_if_needed = |terminal: &mut Terminal<CrosstermBackend<std::io::Stdout>>,
                            app: &mut app::App,
                            needs_redraw: &mut bool|
     -> anyhow::Result<()> {
        if *needs_redraw {
            terminal.draw(|f| ui::draw(f, app))?;
            *needs_redraw = false;
        }
        Ok(())
    };

    let exit_code = loop {
        tokio::select! {
            // Coalesced redraw requests: spinner animation and paste-burst micro‑flush.
            Some(()) = redraw_rx.recv() => {
                // Keep spinner pulsing only while loading.
                if app.refresh_inflight
                    || app.details_inflight
                    || app.env_loading
                    || app.apply_preflight_inflight
                    || app.apply_inflight
                {
                    if app.spinner_start.is_none() {
                        app.spinner_start = Some(Instant::now());
                    }
                    needs_redraw = true;
                    let _ = frame_tx.send(Instant::now() + Duration::from_millis(600));
                } else {
                    app.spinner_start = None;
                }
                render_if_needed(&mut terminal, &mut app, &mut needs_redraw)?;
            }
            maybe_app_event = rx.recv() => {
                if let Some(ev) = maybe_app_event {
                    match ev {
                        // (removed TaskSummaryUpdated; unused in this prototype)
                        app::AppEvent::ApplyPreflightFinished { id, title, message, level, skipped, conflicts } => {
                            // Only update if modal is still open and ids match
                            if let Some(m) = app.apply_modal.as_mut()
                                && m.task_id == id
                            {
                                    m.title = title;
                                    m.result_message = Some(message);
                                    m.result_level = Some(level);
                                    m.skipped_paths = skipped;
                                    m.conflict_paths = conflicts;
                                    app.apply_preflight_inflight = false;
                                    needs_redraw = true;
                                    let _ = frame_tx.send(Instant::now());
                            }
                        }
                        app::AppEvent::ApplyFinished { id, result } => {
                            // Only update if the modal still corresponds to this id.
                            if let Some(m) = &app.apply_modal {
                                if m.task_id != id { continue; }
                            } else {
                                continue;
                            }
                            app.apply_inflight = false;
                            match result {
                                Ok(outcome) => {
                                    app.status = outcome.message.clone();
                                    if matches!(outcome.status, codex_cloud_tasks_client::ApplyStatus::Success) {
                                        app.apply_modal = None;
                                        app.diff_overlay = None;
                                    }
                                }
                                Err(e) => {
                                    append_error_log(format!("apply_task failed for {}: {e}", id.0));
                                    app.status = format!("Apply failed: {e}");
                                }
                            }
                            needs_redraw = true;
                        }
                    }
                }
                // Render immediately after processing app events.
                render_if_needed(&mut terminal, &mut app, &mut needs_redraw)?;
            }
            maybe_event = events.next() => {
                match maybe_event {
                    Some(Ok(Event::Paste(pasted))) => {
                        if app.env_modal.is_some() {
                            if let Some(m) = app.env_modal.as_mut() {
                                for ch in pasted.chars() {
                                    match ch {
                                        '\r' | '\n' => continue,
                                        '\t' => m.query.push(' '),
                                        _ => m.query.push(ch),
                                    }
                                }
                            }
                            needs_redraw = true;
                        }
                    }
                    Some(Ok(Event::Key(key))) if matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) => {
                        // Treat Ctrl-C like pressing 'q' in the current context.
                        if key.modifiers.contains(KeyModifiers::CONTROL)
                            && matches!(key.code, KeyCode::Char('c') | KeyCode::Char('C'))
                        {
                            if app.env_modal.is_some() {
                                // Close environment selector if open (don’t quit composer).
                                app.env_modal = None;
                                needs_redraw = true;
                            } else if app.apply_modal.is_some() {
                                app.apply_modal = None;
                                app.status = "Apply canceled".to_string();
                                needs_redraw = true;
                            } else if app.diff_overlay.is_some() {
                                app.diff_overlay = None;
                                needs_redraw = true;
                            } else {
                                break 0;
                            }
                            // Render updated state immediately before continuing to next loop iteration.
                            render_if_needed(&mut terminal, &mut app, &mut needs_redraw)?;
                            continue;
                        }
                        // If a diff overlay is open, handle its keys first.
                        if app.apply_modal.is_some() {
                            // Simple apply confirmation modal: y apply, p preflight, n/Esc cancel
                            match key.code {
                                KeyCode::Char('y') => {
                                    if let Some(m) = app.apply_modal.as_ref() {
                                        let title = m.title.clone();
                                        let job = ApplyJob {
                                            task_id: m.task_id.clone(),
                                            diff_override: m.diff_override.clone(),
                                        };
                                        if spawn_apply(&mut app, &backend, &tx, &frame_tx, job) {
                                            app.status = format!("Applying '{title}'...");
                                        }
                                        needs_redraw = true;
                                    }
                                }
                                KeyCode::Char('p') => {
                                    if let Some(m) = app.apply_modal.take() {
                                        let title = m.title.clone();
                                        let job = ApplyJob {
                                            task_id: m.task_id.clone(),
                                            diff_override: m.diff_override.clone(),
                                        };
                                        if spawn_preflight(&mut app, &backend, &tx, &frame_tx, title.clone(), job) {
                                            app.apply_modal = Some(app::ApplyModalState {
                                                task_id: m.task_id,
                                                title: title.clone(),
                                                result_message: None,
                                                result_level: None,
                                                skipped_paths: Vec::new(),
                                                conflict_paths: Vec::new(),
                                                diff_override: m.diff_override,
                                            });
                                            app.status = format!("Preflighting '{title}'...");
                                        } else {
                                            app.apply_modal = Some(m);
                                        }
                                        needs_redraw = true;
                                    }
                                }
                                KeyCode::Esc
                                | KeyCode::Char('n')
                                | KeyCode::Char('q')
                                | KeyCode::Char('Q') => { app.apply_modal = None; app.status = "Apply canceled".to_string(); needs_redraw = true; }
                                _ => {}
                            }
                        } else if app.diff_overlay.is_some() {
                            let mut cycle_attempt = |delta: isize| {
                                if let Some(ov) = app.diff_overlay.as_mut()
                                    && ov.attempt_count() > 1 {
                                        ov.step_attempt(delta);
                                        let total = ov.attempt_display_total();
                                        let current = ov.selected_attempt + 1;
                                        app.status = format!("Viewing attempt {current} of {total}");
                                        ov.sd.scroll_to_top();
                                        needs_redraw = true;
                                    }
                            };

                            match key.code {
                                KeyCode::Char('a') => {
                                    if app.apply_inflight || app.apply_preflight_inflight {
                                        app.status = "Finish the current apply/preflight before starting another.".to_string();
                                        needs_redraw = true;
                                        continue;
                                    }
                                    let snapshot = app.diff_overlay.as_ref().map(|ov| {
                                        (
                                            ov.task_id.clone(),
                                            ov.title.clone(),
                                            ov.current_can_apply(),
                                            ov.current_attempt().and_then(|attempt| attempt.diff_raw.clone()),
                                        )
                                    });
                                    if let Some((task_id, title, can_apply, diff_override)) = snapshot {
                                        if can_apply {
                                            let job = ApplyJob {
                                                task_id: task_id.clone(),
                                                diff_override: diff_override.clone(),
                                            };
                                            if spawn_preflight(&mut app, &backend, &tx, &frame_tx, title.clone(), job) {
                                                app.apply_modal = Some(app::ApplyModalState {
                                                    task_id,
                                                    title: title.clone(),
                                                    result_message: None,
                                                    result_level: None,
                                                    skipped_paths: Vec::new(),
                                                    conflict_paths: Vec::new(),
                                                    diff_override,
                                                });
                                                app.status = format!("Preflighting '{title}'...");
                                            }
                                        } else {
                                            app.status = "No diff available to apply.".to_string();
                                        }
                                        needs_redraw = true;
                                    }
                                }
                                KeyCode::Tab => {
                                    cycle_attempt(1);
                                }
                                KeyCode::BackTab => {
                                    cycle_attempt(-1);
                                }
                                // From task modal, 'o' should close it and open the env selector
                                KeyCode::Char('o') | KeyCode::Char('O') => {
                                    app.diff_overlay = None;
                                    app.env_modal = Some(app::EnvModalState { query: String::new(), selected: 0 });
                                    needs_redraw = true;
                                }
                                KeyCode::Left => {
                                    if let Some(ov) = &mut app.diff_overlay {
                                        let has_text = ov.current_attempt().is_some_and(app::AttemptView::has_text);
                                        let has_diff = ov.current_attempt().is_some_and(app::AttemptView::has_diff) || ov.base_can_apply;
                                        if has_text && has_diff {
                                            ov.set_view(app::DetailView::Prompt);
                                            ov.sd.scroll_to_top();
                                            needs_redraw = true;
                                        }
                                    }
                                }
                                KeyCode::Right => {
                                    if let Some(ov) = &mut app.diff_overlay {
                                        let has_text = ov.current_attempt().is_some_and(app::AttemptView::has_text);
                                        let has_diff = ov.current_attempt().is_some_and(app::AttemptView::has_diff) || ov.base_can_apply;
                                        if has_text && has_diff {
                                            ov.set_view(app::DetailView::Diff);
                                            ov.sd.scroll_to_top();
                                            needs_redraw = true;
                                        }
                                    }
                                }
                                KeyCode::Char(']') | KeyCode::Char('}') => {
                                    cycle_attempt(1);
                                }
                                KeyCode::Char('[') | KeyCode::Char('{') => {
                                    cycle_attempt(-1);
                                }
                                KeyCode::Esc | KeyCode::Char('q') => {
                                    app.diff_overlay = None;
                                    needs_redraw = true;
                                }
                                KeyCode::Down | KeyCode::Char('j') => {
                                    if let Some(ov) = &mut app.diff_overlay { ov.sd.scroll_by(/*delta*/ 1); }
                                    needs_redraw = true;
                                }
                                KeyCode::Up | KeyCode::Char('k') => {
                                    if let Some(ov) = &mut app.diff_overlay { ov.sd.scroll_by(/*delta*/ -1); }
                                    needs_redraw = true;
                                }
                                KeyCode::PageDown | KeyCode::Char(' ') => {
                                    if let Some(ov) = &mut app.diff_overlay { let step = ov.sd.state.viewport_h.saturating_sub(1) as i16; ov.sd.page_by(step); }
                                    needs_redraw = true;
                                }
                                KeyCode::PageUp => {
                                    if let Some(ov) = &mut app.diff_overlay { let step = ov.sd.state.viewport_h.saturating_sub(1) as i16; ov.sd.page_by(-step); }
                                    needs_redraw = true;
                                }
                                KeyCode::Home => { if let Some(ov) = &mut app.diff_overlay { ov.sd.scroll_to_top(); } needs_redraw = true; }
                                KeyCode::End  => { if let Some(ov) = &mut app.diff_overlay { ov.sd.scroll_to_bottom(); } needs_redraw = true; }
                                _ => {}
                            }
                        } else if app.env_modal.is_some() {
                            // Environment modal key handling
                            match key.code {
                                KeyCode::Esc => { app.env_modal = None; needs_redraw = true; }
                                KeyCode::Char(ch) if !key.modifiers.contains(KeyModifiers::CONTROL) && !key.modifiers.contains(KeyModifiers::ALT) => {
                                    if let Some(m) = app.env_modal.as_mut() { m.query.push(ch); }
                                    needs_redraw = true;
                                }
                                KeyCode::Backspace => { if let Some(m) = app.env_modal.as_mut() { m.query.pop(); } needs_redraw = true; }
                                KeyCode::Down | KeyCode::Char('j') => { if let Some(m) = app.env_modal.as_mut() { m.selected = m.selected.saturating_add(1); } needs_redraw = true; }
                                KeyCode::Up | KeyCode::Char('k') => { if let Some(m) = app.env_modal.as_mut() { m.selected = m.selected.saturating_sub(1); } needs_redraw = true; }
                                KeyCode::Home => { if let Some(m) = app.env_modal.as_mut() { m.selected = 0; } needs_redraw = true; }
                                KeyCode::End => { if let Some(m) = app.env_modal.as_mut() { m.selected = app.environments.len(); } needs_redraw = true; }
                                KeyCode::PageDown | KeyCode::Char(' ') => { if let Some(m) = app.env_modal.as_mut() { let step = 10usize; m.selected = m.selected.saturating_add(step); } needs_redraw = true; }
                                KeyCode::PageUp => { if let Some(m) = app.env_modal.as_mut() { let step = 10usize; m.selected = m.selected.saturating_sub(step); } needs_redraw = true; }
                                KeyCode::Enter => {
                                    // Resolve selection over filtered set
                                    if let Some(state) = app.env_modal.take() {
                                        let q = state.query.to_lowercase();
                                        let filtered: Vec<&app::EnvironmentRow> = app.environments.iter().filter(|r| {
                                            if q.is_empty() { return true; }
                                            let mut hay = String::new();
                                            if let Some(l) = &r.label { hay.push_str(&l.to_lowercase()); hay.push(' '); }
                                            hay.push_str(&r.id.to_lowercase());
                                            if let Some(h) = &r.repo_hints { hay.push(' '); hay.push_str(&h.to_lowercase()); }
                                            hay.contains(&q)
                                        }).collect();
                                        // Keep original order (already sorted) — no need to re-sort
                                        let idx = state.selected;
                                        if idx == 0 { app.env_filter = None; append_error_log("env.select: All"); }
                                        else {
                                            let env_idx = idx.saturating_sub(1);
                                            if let Some(row) = filtered.get(env_idx) {
                                                append_error_log(format!(
                                                    "env.select: id={} label={}",
                                                    row.id,
                                                    row.label.clone().unwrap_or_else(|| "<none>".to_string())
                                                ));
                                                app.env_filter = Some(row.id.clone());
                                            }
                                        }
                                        needs_redraw = true;
                                    }
                                }
                                _ => {}
                            }
                        } else {
                            // Base list view keys
                            match key.code {
                                KeyCode::Char('q') | KeyCode::Esc => {
                                    break 0;
                                }
                                KeyCode::Down | KeyCode::Char('j') => {
                                    app.next();
                                    needs_redraw = true;
                                }
                                KeyCode::Up | KeyCode::Char('k') => {
                                    app.prev();
                                    needs_redraw = true;
                                }
                                // Ensure 'r' does not refresh tasks when the env modal is open.
                                KeyCode::Char('r') | KeyCode::Char('R') => {}
                                KeyCode::Char('o') | KeyCode::Char('O') => {
                                    app.env_modal = Some(app::EnvModalState { query: String::new(), selected: 0 });
                                    needs_redraw = true;
                                }
                                KeyCode::Enter => {}
                                KeyCode::Char('a') => {
                                    if app.apply_inflight || app.apply_preflight_inflight {
                                        app.status = "Finish the current apply/preflight before starting another.".to_string();
                                        needs_redraw = true;
                                        continue;
                                    }

                                    if app.tasks.get(app.selected).is_some() {
                                        app.status = "No diff available to apply".to_string();
                                        needs_redraw = true;
                                    }
                                }
                                _ => {}
                            }
                        }
                        // Render after handling a key event (when not quitting).
                        render_if_needed(&mut terminal, &mut app, &mut needs_redraw)?;
                    }
                    Some(Ok(Event::Resize(_, _))) => {
                        needs_redraw = true;
                        // Redraw immediately on resize for snappier UX.
                        render_if_needed(&mut terminal, &mut app, &mut needs_redraw)?;
                    }
                    Some(Err(_)) | None => {}
                    _ => {}
                }
                // Fallback: if any other event path requested a redraw, render now.
                render_if_needed(&mut terminal, &mut app, &mut needs_redraw)?;
            }
        }
    };

    // Restore terminal
    disable_raw_mode().ok();
    terminal.show_cursor().ok();
    let _ = crossterm::execute!(std::io::stdout(), DisableBracketedPaste);
    // Best-effort restore of keyboard enhancement flags before leaving alt screen.
    let _ = crossterm::execute!(std::io::stdout(), PopKeyboardEnhancementFlags);
    let _ = crossterm::execute!(std::io::stdout(), LeaveAlternateScreen);

    if exit_code != 0 {
        std::process::exit(exit_code);
    }
    Ok(())
}
