//! Header overrides for host-created threads on the Codex Responses endpoint.
//! Headers are scoped to the host-created thread and selected model.

use http::HeaderMap;

/// Seed in `StartThreadOptions::thread_extension_init` to supply Responses headers.
///
/// Applied only to the selected model in this host-created thread. The headers
/// are sent on HTTP requests and WebSocket handshakes; changes require a new socket.
/// Unrelated threads and other models do not inherit these headers.
#[derive(Clone)]
pub struct CodexResponsesHeaders {
    pub model: String,
    pub headers: HeaderMap,
}

impl std::fmt::Debug for CodexResponsesHeaders {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CodexResponsesHeaders")
            .field("model", &self.model)
            .field("header_names", &self.headers.keys())
            .finish()
    }
}
