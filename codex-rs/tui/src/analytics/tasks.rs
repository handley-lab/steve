//! In-memory consumer task rows. Nothing requests task usage.
use codex_backend_client::TaskUsage;

pub(super) struct Chat {
    pub title: String,
    pub task: Option<TaskUsage>,
}
impl Chat {
    /// Local history can span accounts; unavailable task data does not verify ownership.
    pub(super) fn display_title(&self) -> &str {
        if self.task.as_ref().is_some_and(|task| {
            task.data_status != codex_backend_client::TaskUsageStatus::Unavailable
        }) {
            &self.title
        } else {
            "Chat usage unavailable"
        }
    }
}

#[derive(Default)]
pub(super) struct Chats {
    pub rows: Vec<Chat>,
    pub truncated: bool,
    pub updated_at: Option<chrono::DateTime<chrono::Utc>>,
}
