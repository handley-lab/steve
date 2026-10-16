use std::time::Instant;

// Environment filter data models for the TUI
#[derive(Clone, Debug, Default)]
pub struct EnvironmentRow {
    pub id: String,
    pub label: Option<String>,
    pub is_pinned: bool,
    pub repo_hints: Option<String>, // e.g., "openai/codex"
}

#[derive(Clone, Debug, Default)]
pub struct EnvModalState {
    pub query: String,
    pub selected: usize,
}

#[derive(Clone, Debug, Copy, PartialEq, Eq)]
pub enum ApplyResultLevel {
    Success,
    Partial,
    Error,
}

#[derive(Clone, Debug)]
pub struct ApplyModalState {
    pub task_id: TaskId,
    pub title: String,
    pub result_message: Option<String>,
    pub result_level: Option<ApplyResultLevel>,
    pub skipped_paths: Vec<String>,
    pub conflict_paths: Vec<String>,
    pub diff_override: Option<String>,
}

use crate::scrollable_diff::ScrollableDiff;
use codex_cloud_tasks_client::TaskId;
use codex_cloud_tasks_client::TaskSummary;
#[derive(Default)]
pub struct App {
    pub tasks: Vec<TaskSummary>,
    pub selected: usize,
    pub status: String,
    pub diff_overlay: Option<DiffOverlay>,
    pub spinner_start: Option<Instant>,
    pub refresh_inflight: bool,
    pub details_inflight: bool,
    // Environment filter state
    pub env_filter: Option<String>,
    pub env_modal: Option<EnvModalState>,
    pub apply_modal: Option<ApplyModalState>,
    pub environments: Vec<EnvironmentRow>,
    pub env_loading: bool,
    // Apply preflight spinner state
    pub apply_preflight_inflight: bool,
    // Apply action spinner state
    pub apply_inflight: bool,
    // Background enrichment coordination
    pub in_flight: std::collections::HashSet<String>,
    // Background enrichment caches were planned; currently unused.
}

impl App {
    pub fn new() -> Self {
        Self {
            tasks: Vec::new(),
            selected: 0,
            status: "Press r to refresh".to_string(),
            diff_overlay: None,
            spinner_start: None,
            refresh_inflight: false,
            details_inflight: false,
            env_filter: None,
            env_modal: None,
            apply_modal: None,
            environments: Vec::new(),
            env_loading: false,
            apply_preflight_inflight: false,
            apply_inflight: false,
            in_flight: std::collections::HashSet::new(),
        }
    }

    pub fn next(&mut self) {
        if self.tasks.is_empty() {
            return;
        }
        self.selected = (self.selected + 1).min(self.tasks.len().saturating_sub(1));
    }

    pub fn prev(&mut self) {
        if self.tasks.is_empty() {
            return;
        }
        if self.selected > 0 {
            self.selected -= 1;
        }
    }
}

pub struct DiffOverlay {
    pub title: String,
    pub task_id: TaskId,
    pub sd: ScrollableDiff,
    pub base_can_apply: bool,
    pub diff_lines: Vec<String>,
    pub text_lines: Vec<String>,
    pub prompt: Option<String>,
    pub attempts: Vec<AttemptView>,
    pub selected_attempt: usize,
    pub current_view: DetailView,
    pub attempt_total_hint: Option<usize>,
}

#[derive(Clone, Debug, Default)]
pub struct AttemptView {
    pub status: codex_cloud_tasks_client::AttemptStatus,
    pub diff_lines: Vec<String>,
    pub text_lines: Vec<String>,
    pub prompt: Option<String>,
    pub diff_raw: Option<String>,
}

impl AttemptView {
    pub fn has_diff(&self) -> bool {
        !self.diff_lines.is_empty()
    }

    pub fn has_text(&self) -> bool {
        !self.text_lines.is_empty() || self.prompt.is_some()
    }
}

impl DiffOverlay {
    pub fn current_attempt(&self) -> Option<&AttemptView> {
        self.attempts.get(self.selected_attempt)
    }

    pub fn set_view(&mut self, view: DetailView) {
        self.current_view = view;
        self.apply_selection_to_fields();
    }

    pub fn expected_attempts(&self) -> Option<usize> {
        self.attempt_total_hint.or({
            if self.attempts.is_empty() {
                None
            } else {
                Some(self.attempts.len())
            }
        })
    }

    pub fn attempt_count(&self) -> usize {
        self.attempts.len()
    }

    pub fn attempt_display_total(&self) -> usize {
        self.expected_attempts()
            .unwrap_or_else(|| self.attempts.len().max(1))
    }

    pub fn step_attempt(&mut self, delta: isize) -> bool {
        let total = self.attempts.len();
        if total <= 1 {
            return false;
        }
        let total_isize = total as isize;
        let current = self.selected_attempt as isize;
        let mut next = current + delta;
        next = ((next % total_isize) + total_isize) % total_isize;
        let next = next as usize;
        self.selected_attempt = next;
        self.apply_selection_to_fields();
        true
    }

    pub fn current_can_apply(&self) -> bool {
        matches!(self.current_view, DetailView::Diff)
            && self
                .current_attempt()
                .and_then(|attempt| attempt.diff_raw.as_ref())
                .map(|diff| !diff.is_empty())
                .unwrap_or(false)
    }

    pub fn apply_selection_to_fields(&mut self) {
        let (diff_lines, text_lines, prompt) = if let Some(attempt) = self.current_attempt() {
            (
                attempt.diff_lines.clone(),
                attempt.text_lines.clone(),
                attempt.prompt.clone(),
            )
        } else {
            self.diff_lines.clear();
            self.text_lines.clear();
            self.prompt = None;
            self.sd.set_content(vec!["<loading attempt>".to_string()]);
            return;
        };

        self.diff_lines = diff_lines.clone();
        self.text_lines = text_lines.clone();
        self.prompt = prompt;

        match self.current_view {
            DetailView::Diff => {
                if diff_lines.is_empty() {
                    self.sd.set_content(vec!["<no diff available>".to_string()]);
                } else {
                    self.sd.set_content(diff_lines);
                }
            }
            DetailView::Prompt => {
                if text_lines.is_empty() {
                    self.sd.set_content(vec!["<no output>".to_string()]);
                } else {
                    self.sd.set_content(text_lines);
                }
            }
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DetailView {
    Diff,
    Prompt,
}

/// Internal app events delivered from background tasks.
/// These let the UI event loop remain responsive and keep the spinner animating.
#[derive(Debug)]
pub enum AppEvent {
    /// Background completion of apply preflight when opening modal or on demand
    ApplyPreflightFinished {
        id: TaskId,
        title: String,
        message: String,
        level: ApplyResultLevel,
        skipped: Vec<String>,
        conflicts: Vec<String>,
    },
    /// Background completion of apply action (actual patch application)
    ApplyFinished {
        id: TaskId,
        result: std::result::Result<codex_cloud_tasks_client::ApplyOutcome, String>,
    },
}

