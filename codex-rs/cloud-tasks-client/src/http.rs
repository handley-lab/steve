use crate::ApplyOutcome;
use crate::ApplyStatus;
use crate::AttemptStatus;
use crate::CloudBackend;
use crate::CloudBackendFuture;
use crate::CloudTaskError;
use crate::Result;
use crate::TaskId;
use crate::TurnAttempt;
use chrono::DateTime;
use chrono::Utc;

use codex_api::SharedAuthProvider;
use codex_backend_client as backend;
use codex_git_utils::ApplyGitRequest;
use codex_git_utils::apply_git_patch;

#[derive(Clone)]
pub struct HttpClient {
    pub base_url: String,
    backend: backend::Client,
}

impl HttpClient {
    pub fn new(
        base_url: impl Into<String>,
        http_client_factory: codex_http_client::HttpClientFactory,
    ) -> Self {
        let base_url = base_url.into();
        let backend = backend::Client::new_without_redirects(base_url.clone(), http_client_factory);
        Self { base_url, backend }
    }

    pub fn with_user_agent(mut self, ua: impl Into<String>) -> Self {
        self.backend = self.backend.clone().with_user_agent(ua);
        self
    }

    pub fn with_auth_provider(mut self, auth: SharedAuthProvider) -> Self {
        self.backend = self.backend.clone().with_auth_provider(auth);
        self
    }

    pub fn with_chatgpt_account_id(mut self, account_id: impl Into<String>) -> Self {
        self.backend = self.backend.clone().with_chatgpt_account_id(account_id);
        self
    }

    fn tasks_api(&self) -> api::Tasks<'_> {
        api::Tasks::new(self)
    }

    fn attempts_api(&self) -> api::Attempts<'_> {
        api::Attempts::new(self)
    }
}

impl CloudBackend for HttpClient {
    fn list_sibling_attempts(
        &self,
        task: TaskId,
        turn_id: String,
    ) -> CloudBackendFuture<'_, Vec<TurnAttempt>> {
        Box::pin(async move { self.attempts_api().list(task, turn_id).await })
    }

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

    fn create_task<'a>(
        &'a self,
        env_id: &'a str,
        prompt: &'a str,
        git_ref: &'a str,
        qa_mode: bool,
        best_of_n: usize,
    ) -> CloudBackendFuture<'a, crate::CreatedTask> {
        Box::pin(async move {
            self.tasks_api()
                .create(env_id, prompt, git_ref, qa_mode, best_of_n)
                .await
        })
    }
}

mod api {
    use super::*;
    use serde_json::Value;
    use std::cmp::Ordering;
    use std::collections::HashMap;

    pub(crate) struct Tasks<'a> {
        backend: &'a backend::Client,
    }

    impl<'a> Tasks<'a> {
        pub(crate) fn new(client: &'a HttpClient) -> Self {
            Self {
                backend: &client.backend,
            }
        }

        pub(crate) async fn create(
            &self,
            env_id: &str,
            prompt: &str,
            git_ref: &str,
            qa_mode: bool,
            best_of_n: usize,
        ) -> Result<crate::CreatedTask> {
            let mut input_items: Vec<serde_json::Value> = Vec::new();
            input_items.push(serde_json::json!({
                "type": "message",
                "role": "user",
                "content": [{ "content_type": "text", "text": prompt }]
            }));

            if let Ok(diff) = std::env::var("STEVE_STARTING_DIFF")
                && !diff.is_empty()
            {
                input_items.push(serde_json::json!({
                    "type": "pre_apply_patch",
                    "output_diff": { "diff": diff }
                }));
            }

            let mut request_body = serde_json::json!({
                "new_task": {
                    "environment_id": env_id,
                    "branch": git_ref,
                    "run_environment_in_qa_mode": qa_mode,
                },
                "input_items": input_items,
            });

            if best_of_n > 1
                && let Some(obj) = request_body.as_object_mut()
            {
                obj.insert(
                    "metadata".to_string(),
                    serde_json::json!({ "best_of_n": best_of_n }),
                );
            }

            match self.backend.create_task(request_body).await {
                Ok(id) => {
                    append_error_log(&format!(
                        "new_task: created id={id} env={} prompt_chars={}",
                        env_id,
                        prompt.chars().count()
                    ));
                    Ok(crate::CreatedTask { id: TaskId(id) })
                }
                Err(e) => {
                    append_error_log(&format!(
                        "new_task: create failed env={} prompt_chars={}: {}",
                        env_id,
                        prompt.chars().count(),
                        e
                    ));
                    Err(CloudTaskError::Http(format!("create_task failed: {e}")))
                }
            }
        }
    }

    pub(crate) struct Attempts<'a> {
        backend: &'a backend::Client,
    }

    impl<'a> Attempts<'a> {
        pub(crate) fn new(client: &'a HttpClient) -> Self {
            Self {
                backend: &client.backend,
            }
        }

        pub(crate) async fn list(&self, task: TaskId, turn_id: String) -> Result<Vec<TurnAttempt>> {
            let resp = self
                .backend
                .list_sibling_turns(&task.0, &turn_id)
                .await
                .map_err(|e| CloudTaskError::Http(format!("list_sibling_turns failed: {e}")))?;

            let mut attempts: Vec<TurnAttempt> = resp
                .sibling_turns
                .iter()
                .filter_map(turn_attempt_from_map)
                .collect();
            attempts.sort_by(compare_attempts);
            Ok(attempts)
        }
    }

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

    fn turn_attempt_from_map(turn: &HashMap<String, Value>) -> Option<TurnAttempt> {
        let turn_id = turn.get("id").and_then(Value::as_str)?.to_string();
        let attempt_placement = turn.get("attempt_placement").and_then(Value::as_i64);
        let created_at = parse_timestamp_value(turn.get("created_at"));
        let status = attempt_status_from_str(turn.get("turn_status").and_then(Value::as_str));
        let diff = extract_diff_from_turn(turn);
        let messages = extract_assistant_messages_from_turn(turn);
        Some(TurnAttempt {
            turn_id,
            attempt_placement,
            created_at,
            status,
            diff,
            messages,
        })
    }

    fn compare_attempts(a: &TurnAttempt, b: &TurnAttempt) -> Ordering {
        match (a.attempt_placement, b.attempt_placement) {
            (Some(lhs), Some(rhs)) => lhs.cmp(&rhs),
            (Some(_), None) => Ordering::Less,
            (None, Some(_)) => Ordering::Greater,
            (None, None) => match (a.created_at, b.created_at) {
                (Some(lhs), Some(rhs)) => lhs.cmp(&rhs),
                (Some(_), None) => Ordering::Less,
                (None, Some(_)) => Ordering::Greater,
                (None, None) => a.turn_id.cmp(&b.turn_id),
            },
        }
    }

    fn extract_diff_from_turn(turn: &HashMap<String, Value>) -> Option<String> {
        let items = turn.get("output_items").and_then(Value::as_array)?;
        for item in items {
            match item.get("type").and_then(Value::as_str) {
                Some("output_diff") => {
                    if let Some(diff) = item.get("diff").and_then(Value::as_str)
                        && !diff.is_empty()
                    {
                        return Some(diff.to_string());
                    }
                }
                Some("pr") => {
                    if let Some(diff) = item
                        .get("output_diff")
                        .and_then(Value::as_object)
                        .and_then(|od| od.get("diff"))
                        .and_then(Value::as_str)
                        && !diff.is_empty()
                    {
                        return Some(diff.to_string());
                    }
                }
                _ => {}
            }
        }
        None
    }

    fn extract_assistant_messages_from_turn(turn: &HashMap<String, Value>) -> Vec<String> {
        let mut msgs = Vec::new();
        if let Some(items) = turn.get("output_items").and_then(Value::as_array) {
            for item in items {
                if item.get("type").and_then(Value::as_str) != Some("message") {
                    continue;
                }
                if let Some(content) = item.get("content").and_then(Value::as_array) {
                    for part in content {
                        if part.get("content_type").and_then(Value::as_str) == Some("text")
                            && let Some(txt) = part.get("text").and_then(Value::as_str)
                            && !txt.is_empty()
                        {
                            msgs.push(txt.to_string());
                        }
                    }
                }
            }
        }
        msgs
    }

    fn attempt_status_from_str(raw: Option<&str>) -> AttemptStatus {
        match raw.unwrap_or_default() {
            "failed" => AttemptStatus::Failed,
            "completed" => AttemptStatus::Completed,
            "in_progress" => AttemptStatus::InProgress,
            "pending" => AttemptStatus::Pending,
            _ => AttemptStatus::Pending,
        }
    }

    fn parse_timestamp_value(v: Option<&Value>) -> Option<DateTime<Utc>> {
        let ts = v?.as_f64()?;
        let secs = ts as i64;
        let nanos = ((ts - secs as f64) * 1_000_000_000.0) as u32;
        Some(DateTime::<Utc>::from(
            std::time::UNIX_EPOCH + std::time::Duration::new(secs.max(0) as u64, nanos),
        ))
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
