use codex_cloud_tasks_client::ApplyOutcome;
use codex_cloud_tasks_client::ApplyStatus;
use codex_cloud_tasks_client::CloudBackend;
use codex_cloud_tasks_client::CloudBackendFuture;
use codex_cloud_tasks_client::Result;
use codex_cloud_tasks_client::TaskId;

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
}
