//! In-memory chat rows. Nothing lists them for an OpenAI usage request.
use codex_backend_client::ThreadUsage;

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
