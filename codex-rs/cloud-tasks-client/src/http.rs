use crate::ApplyOutcome;
use crate::ApplyStatus;
use crate::CloudBackend;
use crate::CloudBackendFuture;
use crate::CloudTaskError;
use crate::Result;
use crate::TaskId;
use chrono::Utc;

use codex_git_utils::ApplyGitRequest;
use codex_git_utils::apply_git_patch;

#[derive(Clone)]
pub struct HttpClient {
    pub base_url: String,
}

impl HttpClient {
    pub fn new(base_url: impl Into<String>) -> Self {
        Self {
            base_url: base_url.into(),
        }
    }
}

impl CloudBackend for HttpClient {
    fn apply_task(
        &self,
        id: TaskId,
        diff_override: Option<String>,
    ) -> CloudBackendFuture<'_, ApplyOutcome> {
        Box::pin(async move { api::Apply::run(id, diff_override, /*preflight*/ false).await })
    }

    fn apply_task_preflight(
        &self,
        id: TaskId,
        diff_override: Option<String>,
    ) -> CloudBackendFuture<'_, ApplyOutcome> {
        Box::pin(async move { api::Apply::run(id, diff_override, /*preflight*/ true).await })
    }
}

mod api {
    use super::*;

    pub(crate) struct Apply;

    impl Apply {
        pub(crate) async fn run(
            task_id: TaskId,
            diff_override: Option<String>,
            preflight: bool,
        ) -> Result<ApplyOutcome> {
            let id = task_id.0.clone();
            let diff = match diff_override {
                Some(diff) => diff,
                None => {
                    return Err(CloudTaskError::Msg(format!(
                        "No diff available for task {id}"
                    )));
                }
            };

            if !is_unified_diff(&diff) {
                let summary = summarize_patch_for_logging(&diff);
                let mode = if preflight { "preflight" } else { "apply" };
                append_error_log(&format!(
                    "apply_error: id={id} mode={mode} format=non-unified; {summary}"
                ));
                return Ok(ApplyOutcome {
                    applied: false,
                    status: ApplyStatus::Error,
                    message: "Expected unified git diff; backend returned an incompatible format."
                        .to_string(),
                    skipped_paths: Vec::new(),
                    conflict_paths: Vec::new(),
                });
            }

            let req = ApplyGitRequest {
                cwd: std::env::current_dir().unwrap_or_else(|_| std::env::temp_dir()),
                diff: diff.clone(),
                revert: false,
                preflight,
            };
            let r = apply_git_patch(&req)
                .map_err(|e| CloudTaskError::Io(format!("git apply failed to run: {e}")))?;

            let status = if r.exit_code == 0 {
                ApplyStatus::Success
            } else if !r.applied_paths.is_empty() || !r.conflicted_paths.is_empty() {
                ApplyStatus::Partial
            } else {
                ApplyStatus::Error
            };
            let applied = matches!(status, ApplyStatus::Success) && !preflight;

            let message = if preflight {
                match status {
                    ApplyStatus::Success => {
                        format!("Preflight passed for task {id} (applies cleanly)")
                    }
                    ApplyStatus::Partial => format!(
                        "Preflight: patch does not fully apply for task {id} (applied={}, skipped={}, conflicts={})",
                        r.applied_paths.len(),
                        r.skipped_paths.len(),
                        r.conflicted_paths.len()
                    ),
                    ApplyStatus::Error => format!(
                        "Preflight failed for task {id} (applied={}, skipped={}, conflicts={})",
                        r.applied_paths.len(),
                        r.skipped_paths.len(),
                        r.conflicted_paths.len()
                    ),
                }
            } else {
                match status {
                    ApplyStatus::Success => format!(
                        "Applied task {id} locally ({} files)",
                        r.applied_paths.len()
                    ),
                    ApplyStatus::Partial => format!(
                        "Apply partially succeeded for task {id} (applied={}, skipped={}, conflicts={})",
                        r.applied_paths.len(),
                        r.skipped_paths.len(),
                        r.conflicted_paths.len()
                    ),
                    ApplyStatus::Error => format!(
                        "Apply failed for task {id} (applied={}, skipped={}, conflicts={})",
                        r.applied_paths.len(),
                        r.skipped_paths.len(),
                        r.conflicted_paths.len()
                    ),
                }
            };

            if matches!(status, ApplyStatus::Partial | ApplyStatus::Error)
                || (preflight && !matches!(status, ApplyStatus::Success))
            {
                let mut log = String::new();
                let summary = summarize_patch_for_logging(&diff);
                let mode = if preflight { "preflight" } else { "apply" };
                use std::fmt::Write as _;
                let _ = writeln!(
                    &mut log,
                    "apply_result: mode={} id={} status={:?} applied={} skipped={} conflicts={} cmd={}",
                    mode,
                    id,
                    status,
                    r.applied_paths.len(),
                    r.skipped_paths.len(),
                    r.conflicted_paths.len(),
                    r.cmd_for_log
                );
                let _ = writeln!(
                    &mut log,
                    "stdout_tail=\n{}\nstderr_tail=\n{}",
                    tail(&r.stdout, /*max*/ 2000),
                    tail(&r.stderr, /*max*/ 2000)
                );
                let _ = writeln!(&mut log, "{summary}");
                let _ = writeln!(
                    &mut log,
                    "----- PATCH BEGIN -----\n{diff}\n----- PATCH END -----"
                );
                append_error_log(&log);
            }

            Ok(ApplyOutcome {
                applied,
                status,
                message,
                skipped_paths: r.skipped_paths,
                conflict_paths: r.conflicted_paths,
            })
        }
    }

    fn is_unified_diff(diff: &str) -> bool {
        let t = diff.trim_start();
        if t.starts_with("diff --git ") {
            return true;
        }
        let has_dash_headers = diff.contains("\n--- ") && diff.contains("\n+++ ");
        let has_hunk = diff.contains("\n@@ ") || diff.starts_with("@@ ");
        has_dash_headers && has_hunk
    }

    fn tail(s: &str, max: usize) -> String {
        if s.len() <= max {
            s.to_string()
        } else {
            s[s.len() - max..].to_string()
        }
    }

    fn summarize_patch_for_logging(patch: &str) -> String {
        let trimmed = patch.trim_start();
        let kind = if trimmed.starts_with("*** Begin Patch") {
            "codex-patch"
        } else if trimmed.starts_with("diff --git ") || trimmed.contains("\n*** End Patch\n") {
            "git-diff"
        } else if trimmed.starts_with("@@ ") || trimmed.contains("\n@@ ") {
            "unified-diff"
        } else {
            "unknown"
        };
        let lines = patch.lines().count();
        let chars = patch.len();
        let cwd = std::env::current_dir()
            .ok()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "<unknown>".to_string());
        let head: String = patch.lines().take(20).collect::<Vec<&str>>().join("\n");
        let head_trunc = if head.len() > 800 {
            format!("{}…", &head[..800])
        } else {
            head
        };
        format!(
            "patch_summary: kind={kind} lines={lines} chars={chars} cwd={cwd} ; head=\n{head_trunc}"
        )
    }
}

fn append_error_log(message: &str) {
    let ts = Utc::now().to_rfc3339();
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open("error.log")
    {
        use std::io::Write as _;
        let _ = writeln!(f, "[{ts}] {message}");
    }
}
