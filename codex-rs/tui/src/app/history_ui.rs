//! Terminal history, desktop handoff, and clear-screen UI helpers for the TUI app.
//!
//! This module owns rendering the fresh session header, clearing inline or alternate-screen UI
//! state, and resetting transcript-related app state after `/clear` or Ctrl-L. Owned-screen sessions
//! keep committed cells as the render source and never enqueue terminal-scrollback rows here.

use super::*;

const DESKTOP_THREAD_OPENED_MESSAGE: &str = "Opened this session in the Desktop app.";

impl App {
    pub(super) fn link_viewed_image(&self, cell: &mut dyn HistoryCell) {
        if let Some(image) = cell
            .as_any_mut()
            .downcast_mut::<history_cell::ViewImageHistoryCell>()
            && !self.app_server_target.uses_remote_workspace()
            && crate::projectless::has_only_local_environments(&self.environment_manager)
            && !crate::clipboard_copy::is_ssh_session()
        {
            image.link_local_image();
        }
    }

    pub(super) fn insert_history_cell(
        &mut self,
        tui: &mut tui::Tui,
        mut cell: Box<dyn HistoryCell>,
    ) {
        self.link_viewed_image(cell.as_mut());
        // Global deprecations can be delivered again by hidden threads. Scope deduplication to
        // retained history so clearing or rebuilding a transcript can show the notice again.
        if let Some(notice) = cell
            .as_any()
            .downcast_ref::<history_cell::DeprecationNoticeCell>()
            && self.transcript_cells.iter().any(|existing| {
                existing
                    .as_any()
                    .downcast_ref::<history_cell::DeprecationNoticeCell>()
                    == Some(notice)
            })
        {
            return;
        }
        if !crate::empty_state_animation::is_startup_cell(cell.as_ref()) {
            self.chat_widget
                .empty_state_animation
                .borrow_mut()
                .dismiss();
        }
        if let Some(warnings) = cell
            .as_any()
            .downcast_ref::<history_cell::StartupWarningsCell>()
        {
            self.merge_startup_warnings(tui, warnings);
            return;
        }
        let is_session_header = cell.as_any().is::<history_cell::SessionInfoCell>();
        let cell: Arc<dyn HistoryCell> = cell.into();
        if let Some(Overlay::Transcript(t)) = &mut self.overlay {
            t.insert_cell(cell.clone());
            tui.frame_requester().schedule_frame();
        }
        self.transcript_cells.push(cell.clone());
        let deferred = tui.is_owned_screen() || self.native_history.insert(&cell);
        self.render_inserted_history_cell(tui, &cell, deferred);
        if is_session_header {
            self.merge_startup_warnings(tui, &history_cell::StartupWarningsCell::default());
        }
    }

    /// Track mutable status cards before choosing retained or terminal-owned rendering.
    pub(super) fn render_inserted_history_cell(
        &mut self,
        tui: &mut tui::Tui,
        cell: &Arc<dyn HistoryCell>,
        deferred: bool,
    ) {
        let width = self
            .chat_widget
            .history_wrap_width(tui.terminal.last_known_screen_size.width);
        // Owned replay must not eagerly format every historical entry.
        let lines = if !deferred {
            cell.display_hyperlink_lines_for_mode(width, self.chat_widget.history_render_mode())
        } else {
            Vec::new()
        };
        // Invisible diagnostics still update the badge without replacing the rendered tail.
        if deferred || lines.is_empty() {
            tui.frame_requester().schedule_frame();
            return;
        }
        if self.initial_history_replay_buffer.as_ref().is_some() {
            self.insert_history_cell_lines_with_initial_replay_buffer(tui, cell.as_ref(), width);
        } else {
            self.insert_history_cell_lines(tui, cell.as_ref(), width);
        }
    }

    pub(super) fn open_url_in_browser(&mut self, url: String) {
        if let Err(err) = webbrowser::open(&url) {
            self.chat_widget
                .add_error_message(format!("Failed to open browser for {url}: {err}"));
        }
    }

    pub(super) fn open_desktop_thread(&mut self, thread_id: ThreadId) {
        let url = format!("steve://threads/{thread_id}");
        if let Err(err) = open_desktop_thread_url(&url) {
            self.chat_widget
                .add_error_message(desktop_thread_open_error_message(&err));
            return;
        }

        self.chat_widget.add_info_message(
            DESKTOP_THREAD_OPENED_MESSAGE.to_string(),
            /*hint*/ None,
        );
    }

    pub(super) fn clear_ui_header_lines_with_version(
        &self,
        width: u16,
        version: &'static str,
    ) -> Vec<Line<'static>> {
        self.clear_ui_header_cell(version)
            .display_lines_for_mode(width, self.chat_widget.history_render_mode())
    }

    /// Share the source-backed header between retained drawing and legacy terminal insertion.
    fn clear_ui_header_cell(
        &self,
        version: &'static str,
    ) -> history_cell::SessionHeaderHistoryCell {
        history_cell::SessionHeaderHistoryCell::new(
            self.chat_widget.model_display_name().to_string(),
            self.chat_widget.current_reasoning_effort(),
            self.config.cwd.to_path_buf(),
            version,
        )
        .with_yolo_mode(history_cell::is_yolo_mode(&self.config))
    }

    pub(super) fn clear_ui_header_lines(&self, width: u16) -> Vec<Line<'static>> {
        self.clear_ui_header_lines_with_version(width, CODEX_CLI_VERSION)
    }

    pub(super) fn start_cleared_display(&mut self, tui: &mut tui::Tui) {
        self.reset_terminal_display_state();
        self.transcript_cells
            .retain(|cell| !cell.as_any().is::<history_cell::ClearScreenHeader>());
        let header = history_cell::ClearScreenHeader(self.clear_ui_header_cell(CODEX_CLI_VERSION));
        self.insert_history_cell(tui, Box::new(header));
    }

    pub(super) fn queue_clear_ui_header(&mut self, tui: &mut tui::Tui) {
        if tui.is_owned_screen() {
            if !self.transcript_cells.iter().any(|cell| {
                cell.as_any().is::<history_cell::SessionInfoCell>()
                    || cell.as_any().is::<history_cell::SessionHeaderHistoryCell>()
            }) {
                let header: Arc<dyn HistoryCell> =
                    Arc::new(self.clear_ui_header_cell(CODEX_CLI_VERSION));
                self.transcript_cells.insert(/*index*/ 0, header);
            }
            tui.frame_requester().schedule_frame();
            return;
        }
        let width = self
            .chat_widget
            .history_wrap_width(tui.terminal.last_known_screen_size.width);
        let header_lines = self.clear_ui_header_lines(width);
        if !header_lines.is_empty() {
            tui.insert_history_lines_with_wrap_policy(
                header_lines,
                self.history_line_wrap_policy(),
            );
            self.has_emitted_history_lines = true;
        }
    }

    pub(super) fn clear_terminal_ui(
        &mut self,
        tui: &mut tui::Tui,
        redraw_header: bool,
    ) -> Result<()> {
        let is_alt_screen_active = tui.is_alt_screen_active();

        // Drop queued history insertions so stale transcript lines cannot be flushed after /clear.
        tui.clear_pending_history_lines();

        if tui.is_owned_screen() || is_alt_screen_active {
            tui.terminal.clear_visible_screen()?;
        } else {
            // Some terminals (Terminal.app, Warp) do not reliably drop scrollback when purge and
            // clear are emitted as separate backend commands. Prefer a single ANSI sequence.
            tui.terminal.clear_scrollback_and_visible_screen_ansi()?;
        }

        let mut area = tui.terminal.viewport_area;
        if area.y > 0 {
            // After a full clear, anchor the inline viewport at the top and redraw a fresh header
            // box. `insert_history_lines()` will shift the viewport down by the rendered height.
            area.y = 0;
            tui.terminal.set_viewport_area(area);
        }
        self.has_emitted_history_lines = false;

        if redraw_header {
            self.queue_clear_ui_header(tui);
        }
        Ok(())
    }

    pub(super) fn reset_app_ui_state_after_clear(&mut self) {
        self.reset_transcript_state_after_clear();
    }

    pub(super) fn reset_transcript_state_after_clear(&mut self) {
        self.transcript_cells.clear();
        self.reset_terminal_display_state();
    }

    fn reset_terminal_display_state(&mut self) {
        self.overlay = None;
        self.turn_tips.dismiss();
        self.chat_widget.warning_display_state.dismissed.clear();
        self.chat_widget.warning_display_state.transcript = Arc::default();
        self.chat_widget.warning_display_state.synced_cells = None;
        self.native_history = Default::default();
        self.cancel_pending_key_chord();
        self.transcript_view = Default::default();
        *self.chat_widget.rendered_selection.borrow_mut() = Default::default();
        self.deferred_history_lines.clear();
        self.has_emitted_history_lines = false;
        self.transcript_reflow.clear();
        self.initial_history_replay_buffer = None;
        self.scrollback_has_older_history = false;
        self.backtrack = BacktrackState::default();
        self.backtrack_render_pending = false;
        self.skill_load_warnings.clear();
    }
}

fn desktop_thread_open_error_message(err: &str) -> String {
    format!(
        "Failed to open this session in the Desktop app: {err}. Install or launch the Desktop app and try again."
    )
}

#[cfg(target_os = "macos")]
fn open_desktop_thread_url(url: &str) -> Result<(), String> {
    let status = std::process::Command::new("open")
        .arg(url)
        .status()
        .map_err(|err| format!("failed to invoke `open`: {err}"))?;

    if status.success() {
        Ok(())
    } else {
        Err(format!("`open {url}` exited with {status}"))
    }
}

#[cfg(target_os = "windows")]
fn open_desktop_thread_url(url: &str) -> Result<(), String> {
    let script = windows_desktop_app_launch_script(url);
    let output = std::process::Command::new("powershell.exe")
        .arg("-NoProfile")
        .arg("-Command")
        .arg(&script)
        .output()
        .map_err(|err| format!("failed to launch the Desktop app through PowerShell: {err}"))?;

    if output.status.success() {
        return Ok(());
    }

    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    if stderr.is_empty() {
        Err(format!(
            "failed to launch the Desktop app through PowerShell with {}",
            output.status
        ))
    } else {
        Err(stderr)
    }
}

#[cfg(target_os = "windows")]
fn windows_desktop_app_launch_script(url: &str) -> String {
    let url = powershell_single_quoted_string(url);
    format!(
        r#"
$ErrorActionPreference = 'Stop'
$url = {url}

$package = Get-AppxPackage -Name OpenAI.Steve -ErrorAction SilentlyContinue
if ($null -eq $package) {{
    Write-Error 'Desktop app package is not installed'
    exit 1
}}

$manifest = Get-AppxPackageManifest -Package $package.PackageFullName
$application = $manifest.Package.Applications.Application |
    Where-Object {{
        @($_.Extensions.Extension) | Where-Object {{
            $_.Category -eq 'windows.protocol' -and $_.Protocol.Name -eq 'steve'
        }}
    }} |
    Select-Object -First 1
if ($null -eq $application -or [string]::IsNullOrWhiteSpace($application.Executable)) {{
    Write-Error 'Desktop app package does not declare a steve protocol executable'
    exit 1
}}

# Launch the package-declared protocol executable rather than an internal Electron shim.
# Windows can deny direct starts of internal executables under WindowsApps.
$exe = Join-Path $package.InstallLocation $application.Executable
$appDir = Split-Path -Parent $exe
$app = Join-Path $appDir 'resources\app.asar'
if (-not (Test-Path $exe)) {{
    Write-Error "Desktop app executable not found at $exe"
    exit 1
}}
if (-not (Test-Path $app)) {{
    Write-Error "Desktop app bundle not found at $app"
    exit 1
}}

Start-Process -FilePath $exe -WorkingDirectory $appDir -ArgumentList @('resources\app.asar', $url)
"#
    )
}

#[cfg(target_os = "windows")]
fn powershell_single_quoted_string(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn open_desktop_thread_url(_url: &str) -> Result<(), String> {
    Err("The Desktop app is only available on macOS and Windows".to_string())
}

#[cfg(test)]
#[path = "history_ui_tests.rs"]
mod tests;
