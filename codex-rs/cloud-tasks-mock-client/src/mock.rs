use chrono::Utc;
use codex_cloud_tasks_client::ApplyOutcome;
use codex_cloud_tasks_client::ApplyStatus;
use codex_cloud_tasks_client::AttemptStatus;
use codex_cloud_tasks_client::CloudBackend;
use codex_cloud_tasks_client::CloudBackendFuture;
use codex_cloud_tasks_client::Result;
use codex_cloud_tasks_client::TaskId;
use codex_cloud_tasks_client::TurnAttempt;

#[derive(Clone, Default)]
pub struct MockClient;

impl MockClient {
    async fn apply_task(&self, id: TaskId, _diff_override: Option<String>) -> Result<ApplyOutcome> {
        Ok(ApplyOutcome {
            applied: true,
            status: ApplyStatus::Success,
            message: format!("Applied task {} locally (mock)", id.0),
            skipped_paths: Vec::new(),
            conflict_paths: Vec::new(),
        })
    }

    async fn apply_task_preflight(
        &self,
        id: TaskId,
        _diff_override: Option<String>,
    ) -> Result<ApplyOutcome> {
        Ok(ApplyOutcome {
            applied: false,
            status: ApplyStatus::Success,
            message: format!("Preflight passed for task {} (mock)", id.0),
            skipped_paths: Vec::new(),
            conflict_paths: Vec::new(),
        })
    }

    async fn list_sibling_attempts(
        &self,
        task: TaskId,
        _turn_id: String,
    ) -> Result<Vec<TurnAttempt>> {
        if task.0 == "T-1000" {
            return Ok(vec![TurnAttempt {
                turn_id: "T-1000-attempt-2".to_string(),
                attempt_placement: Some(1),
                created_at: Some(Utc::now()),
                status: AttemptStatus::Completed,
                diff: Some(mock_diff_for(&task)),
                messages: vec!["Mock alternate attempt".to_string()],
            }]);
        }
        Ok(Vec::new())
    }
}

impl CloudBackend for MockClient {
    fn apply_task(
        &self,
        id: TaskId,
        diff_override: Option<String>,
    ) -> CloudBackendFuture<'_, ApplyOutcome> {
        Box::pin(MockClient::apply_task(self, id, diff_override))
    }

    fn apply_task_preflight(
        &self,
        id: TaskId,
        diff_override: Option<String>,
    ) -> CloudBackendFuture<'_, ApplyOutcome> {
        Box::pin(MockClient::apply_task_preflight(self, id, diff_override))
    }

    fn list_sibling_attempts(
        &self,
        task: TaskId,
        turn_id: String,
    ) -> CloudBackendFuture<'_, Vec<TurnAttempt>> {
        Box::pin(MockClient::list_sibling_attempts(self, task, turn_id))
    }
}

fn mock_diff_for(id: &TaskId) -> String {
    match id.0.as_str() {
        "T-1000" => {
            "diff --git a/README.md b/README.md\nindex 000000..111111 100644\n--- a/README.md\n+++ b/README.md\n@@ -1,2 +1,3 @@\n Intro\n-Hello\n+Hello, world!\n+Task: T-1000\n".to_string()
        }
        "T-1001" => {
            "diff --git a/core/src/lib.rs b/core/src/lib.rs\nindex 000000..111111 100644\n--- a/core/src/lib.rs\n+++ b/core/src/lib.rs\n@@ -1,2 +1,1 @@\n-use foo;\n use bar;\n".to_string()
        }
        _ => {
            "diff --git a/CONTRIBUTING.md b/CONTRIBUTING.md\nindex 000000..111111 100644\n--- /dev/null\n+++ b/CONTRIBUTING.md\n@@ -0,0 +1,3 @@\n+## Contributing\n+Please open PRs.\n+Thanks!\n".to_string()
        }
    }
}
