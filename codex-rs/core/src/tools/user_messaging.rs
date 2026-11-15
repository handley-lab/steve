//! Extracts confirmed User Messaging text after input-rewriting hooks.
//! Tracks nested sends through cancellation and captures evidence before post-tool hooks.

use crate::function_tool::FunctionCallError;
use crate::guardian::GUARDIAN_MAX_ROOT_MESSAGE_TOKENS;
use crate::guardian::guardian_truncate_text;
use crate::session::session::Session;
use crate::tools::context::ToolCallSource;
use crate::tools::context::ToolCallState;
use crate::tools::registry::AnyToolResult;
use codex_tools::ToolName;
use tokio_util::task::task_tracker::TaskTrackerToken;

pub(super) fn admit_code_mode_send(
    session: &Session,
    source: &ToolCallSource,
    tool_name: &ToolName,
) -> Result<Option<TaskTrackerToken>, FunctionCallError> {
    // All User Messaging aliases end in `send_message`; confirmation checks the exact identity.
    if !matches!(source, ToolCallSource::CodeMode { .. })
        || !tool_name.name.ends_with("send_message")
    {
        return Ok(None);
    }
    session.track_code_mode_message().map(Some).ok_or_else(|| {
        FunctionCallError::RespondToModel("code mode nested tool call cancelled".to_owned())
    })
}

/// Captures confirmed delivery before any further await, including post-tool hooks.
pub(super) fn capture_delivery(result: &AnyToolResult, call_state: Option<&ToolCallState>) {
    if let Some(call_state) = call_state
        && let Some(text) = result.delivered_assistant_message()
    {
        let _ = call_state.delivered_assistant_message.set(text);
    }
}

impl AnyToolResult {
    fn delivered_assistant_message(&self) -> Option<String> {
        if !self.result.success_for_logging() {
            return None;
        }
        let payload = self.post_tool_use_payload.as_ref()?;
        // MCP hook names normalize prefixed/unprefixed and flat/namespaced calls.
        // Both connector spellings also have a form for catalogs without connector metadata.
        if !matches!(
            payload.tool_name.name(),
            "mcp__codex_apps__user_messaging__send_message"
                | "mcp__codex_apps__user_messaging_send_message"
                | "mcp__codex_apps__user_message__send_message"
                | "mcp__codex_apps__user_message_send_message"
        ) {
            return None;
        }
        let text = payload.tool_input.get("text")?.as_str()?;
        if text.trim().is_empty() {
            return None;
        }
        Some(guardian_truncate_text(text, GUARDIAN_MAX_ROOT_MESSAGE_TOKENS).0)
    }
}

#[cfg(test)]
#[path = "user_messaging_tests.rs"]
mod tests;
