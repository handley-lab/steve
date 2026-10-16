//! Recent local thread roots shared by analytics task reporting.

use super::data;
use codex_app_server_client::AppServerRequestHandle;
use codex_app_server_protocol::ClientRequest;
use codex_app_server_protocol::RequestId;
use codex_app_server_protocol::SortDirection;
use codex_app_server_protocol::ThreadListParams;
use codex_app_server_protocol::ThreadListResponse;
use codex_app_server_protocol::ThreadSortKey;
use codex_backend_client::ThreadUsage;
use std::collections::HashSet;

pub(super) struct Chat {
    pub title: String,
    pub usage: Option<ThreadUsage>,
}

impl Chat {
    /// Local history can span accounts; expose titles only with a returned account estimate.
    pub(super) fn display_title(&self) -> &str {
        if self.usage.is_some() {
            &self.title
        } else {
            "Chat usage unavailable"
        }
    }
}

#[derive(Default)]
pub(super) struct Chats {
    pub rows: Vec<Chat>,
}

/// Recent local roots are shared by consumer task reporting.
pub(super) async fn roots(
    handle: &AppServerRequestHandle,
) -> Result<Vec<codex_app_server_protocol::Thread>, String> {
    let cutoff = chrono::Utc::now().timestamp() - 30 * 24 * 60 * 60;
    let mut threads = Vec::new();
    let mut cursor = None;
    let mut seen_cursors = HashSet::new();
    loop {
        let page: ThreadListResponse = handle
            .request_typed(ClientRequest::ThreadList {
                request_id: RequestId::String(uuid::Uuid::new_v4().to_string()),
                params: ThreadListParams {
                    cursor,
                    ..list_params()
                },
            })
            .await
            .map_err(data::error)?;
        let reached_cutoff = page.data.iter().any(|thread| thread.updated_at < cutoff);
        threads.extend(page.data.into_iter().filter(|thread| {
            thread.updated_at >= cutoff && !thread.ephemeral && thread.parent_thread_id.is_none()
        }));
        match page.next_cursor {
            Some(next) if !seen_cursors.insert(next.clone()) => {
                return Err("Chat listing repeated a cursor. Press R to retry.".into());
            }
            Some(next) if !reached_cutoff => cursor = Some(next),
            _ => break,
        }
    }
    // Keep repeated rows from overlapping pages out of the backend's distinct-ID batches.
    let mut seen_ids = HashSet::new();
    threads.retain(|thread| seen_ids.insert(thread.id.clone()));
    Ok(threads)
}

pub(super) fn list_params() -> ThreadListParams {
    ThreadListParams {
        excluded_thread_ids: None,
        limit: Some(100),
        sort_key: Some(ThreadSortKey::UpdatedAt),
        sort_direction: Some(SortDirection::Desc),
        model_providers: Some(Vec::new()),
        source_kinds: None,
        originators: None,
        archived: Some(false),
        cursor: None,
        section_id: None,
        project_id: None,
        cwd: None,
        use_state_db_only: false,
        search_term: None,
        parent_thread_id: None,
        ancestor_thread_id: None,
    }
}

