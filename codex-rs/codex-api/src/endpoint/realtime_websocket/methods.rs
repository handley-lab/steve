use crate::endpoint::realtime_websocket::methods_common::conversation_function_call_output_message;
use crate::endpoint::realtime_websocket::methods_common::conversation_handoff_append_message;
use crate::endpoint::realtime_websocket::methods_common::conversation_item_create_message;
use crate::endpoint::realtime_websocket::methods_common::normalized_session_mode;
use crate::endpoint::realtime_websocket::methods_common::session_update_message;
use crate::endpoint::realtime_websocket::methods_common::standalone_handoff_message;
use crate::endpoint::realtime_websocket::methods_frameless_bidi::context_append_chunks;
use crate::endpoint::realtime_websocket::methods_frameless_bidi::delegation_context_append_message as frameless_delegation_context_append_message;
use crate::endpoint::realtime_websocket::methods_frameless_bidi::session_context_append_message as frameless_session_context_append_message;
use crate::endpoint::realtime_websocket::protocol::RealtimeAudioFrame;
use crate::endpoint::realtime_websocket::protocol::RealtimeContextAppendChannel;
use crate::endpoint::realtime_websocket::protocol::RealtimeEvent;
use crate::endpoint::realtime_websocket::protocol::RealtimeEventParser;
use crate::endpoint::realtime_websocket::protocol::RealtimeOutboundMessage;
use crate::endpoint::realtime_websocket::protocol::RealtimeOutputModality;
use crate::endpoint::realtime_websocket::protocol::RealtimeSessionConfig;
use crate::endpoint::realtime_websocket::protocol::RealtimeSessionMode;
use crate::endpoint::realtime_websocket::protocol::RealtimeTranscriptEntry;
use crate::endpoint::realtime_websocket::protocol::RealtimeVoice;
use crate::endpoint::realtime_websocket::protocol::parse_realtime_event;
use crate::error::ApiError;
use crate::provider::Provider;
use codex_http_client::HttpClientFactory;
use codex_protocol::protocol::ConversationTextParams;
use codex_protocol::protocol::ConversationTextRole;
use codex_protocol::protocol::RealtimeTranscriptDelta;
use http::HeaderMap;
use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;
use tokio::sync::Mutex;
use tokio_tungstenite::tungstenite::Error as WsError;
use tokio_tungstenite::tungstenite::Message;
use tracing::debug;
use tracing::error;
use tracing::info;
use tracing::trace;
use tungstenite::protocol::frame::coding::CloseCode;

const REALTIME_WIRE_LOG_TARGET: &str = "codex_api::realtime_websocket::wire";
const MAX_ACTIVE_TRANSCRIPT_BYTES: usize = 8 * 1024;
const TRUNCATED_TRANSCRIPT_PREFIX: &str = "…";

struct WsStream;

impl WsStream {
    fn closed_error(&self) -> WsError {
        let _ = self;
        WsError::ConnectionClosed
    }

    async fn send(&self, _message: Message) -> Result<(), WsError> {
        let _ = self;
        Err(WsError::ConnectionClosed)
    }

    async fn close(&self) -> Result<(), WsError> {
        let _ = self;
        Err(WsError::ConnectionClosed)
    }
}

pub struct RealtimeWebsocketConnection {
    writer: RealtimeWebsocketWriter,
    events: RealtimeWebsocketEvents,
}

#[derive(Clone)]
pub struct RealtimeWebsocketWriter {
    stream: Arc<WsStream>,
    is_closed: Arc<AtomicBool>,
    event_parser: RealtimeEventParser,
    context_append_channel: Option<RealtimeContextAppendChannel>,
}

#[derive(Clone)]
pub struct RealtimeWebsocketEvents {
    rx_message: async_channel::Receiver<Result<Message, WsError>>,
    pending_events: Arc<Mutex<VecDeque<RealtimeEvent>>>,
    transcript_state: RealtimeTranscriptState,
    event_parser: RealtimeEventParser,
    is_closed: Arc<AtomicBool>,
}

#[derive(Clone, Default)]
pub struct RealtimeTranscriptState {
    active: Arc<Mutex<ActiveTranscriptState>>,
}

#[derive(Default)]
struct ActiveTranscriptState {
    entries: Vec<RealtimeTranscriptEntry>,
    new_input_entry: bool,
    new_output_entry: bool,
}

impl RealtimeWebsocketConnection {
    pub async fn send_audio_frame(&self, frame: RealtimeAudioFrame) -> Result<(), ApiError> {
        self.writer.send_audio_frame(frame).await
    }

    pub async fn send_conversation_item_create(
        &self,
        text: String,
        role: ConversationTextRole,
    ) -> Result<(), ApiError> {
        self.writer.send_conversation_item_create(text, role).await
    }

    pub async fn send_conversation_function_call_output(
        &self,
        call_id: String,
        output_text: String,
    ) -> Result<(), ApiError> {
        self.writer
            .send_conversation_function_call_output(call_id, output_text)
            .await
    }

    pub async fn close(&self) -> Result<(), ApiError> {
        self.writer.close().await
    }

    pub async fn next_event(&self) -> Result<Option<RealtimeEvent>, ApiError> {
        self.events.next_event().await
    }

    pub fn writer(&self) -> RealtimeWebsocketWriter {
        self.writer.clone()
    }

    pub fn events(&self) -> RealtimeWebsocketEvents {
        self.events.clone()
    }

}

impl RealtimeWebsocketWriter {
    pub fn with_context_append_channel(mut self, channel: RealtimeContextAppendChannel) -> Self {
        self.context_append_channel = Some(channel);
        self
    }

    pub async fn send_audio_frame(&self, frame: RealtimeAudioFrame) -> Result<(), ApiError> {
        let message = match self.event_parser {
            RealtimeEventParser::V1 | RealtimeEventParser::RealtimeV2 => {
                RealtimeOutboundMessage::InputAudioBufferAppend { audio: frame.data }
            }
            RealtimeEventParser::FramelessBidi => {
                RealtimeOutboundMessage::InputAudioAppend { audio: frame.data }
            }
        };
        self.send_json(&message).await
    }

    pub async fn send_conversation_item_create(
        &self,
        text: String,
        role: ConversationTextRole,
    ) -> Result<(), ApiError> {
        self.send_json(&conversation_item_create_message(
            self.event_parser,
            text,
            role,
            self.context_append_channel,
        ))
        .await
    }

    pub async fn send_conversation_handoff_append(
        &self,
        handoff_id: String,
        output_text: String,
    ) -> Result<(), ApiError> {
        self.send_json(&conversation_handoff_append_message(
            self.event_parser,
            handoff_id,
            output_text,
            self.context_append_channel,
        ))
        .await
    }

    pub async fn send_standalone_handoff(
        &self,
        handoff_id: String,
        output_text: String,
    ) -> Result<(), ApiError> {
        self.send_json(&standalone_handoff_message(
            self.event_parser,
            handoff_id,
            output_text,
            self.context_append_channel,
        ))
        .await
    }

    pub async fn send_conversation_function_call_output(
        &self,
        call_id: String,
        output_text: String,
    ) -> Result<(), ApiError> {
        self.send_json(&conversation_function_call_output_message(
            self.event_parser,
            call_id,
            output_text,
            self.context_append_channel,
        ))
        .await
    }

    pub async fn send_response_create(&self) -> Result<(), ApiError> {
        self.send_json(&RealtimeOutboundMessage::ResponseCreate)
            .await
    }

    pub async fn send_session_update(
        &self,
        instructions: String,
        initial_items: Vec<ConversationTextParams>,
        session_mode: RealtimeSessionMode,
        output_modality: RealtimeOutputModality,
        voice: RealtimeVoice,
        delegation_ack_filler: Option<bool>,
    ) -> Result<(), ApiError> {
        let session_mode = normalized_session_mode(self.event_parser, session_mode);
        let message = session_update_message(
            self.event_parser,
            instructions,
            initial_items,
            session_mode,
            output_modality,
            voice,
            delegation_ack_filler,
        );
        self.send_json(&message).await
    }

    pub async fn close(&self) -> Result<(), ApiError> {
        if self.is_closed.swap(true, Ordering::SeqCst) {
            return Ok(());
        }
        if self.event_parser == RealtimeEventParser::FramelessBidi {
            let payload =
                serde_json::to_string(&RealtimeOutboundMessage::SessionClose).map_err(|err| {
                    ApiError::Stream(format!("failed to encode realtime request: {err}"))
                })?;
            trace!(target: REALTIME_WIRE_LOG_TARGET, "realtime websocket request: {payload}");
            if let Err(err) = self.stream.send(Message::Text(payload.into())).await
                && !matches!(err, WsError::ConnectionClosed | WsError::AlreadyClosed)
            {
                return Err(ApiError::Stream(format!(
                    "failed to close frameless realtime session: {err}"
                )));
            }
        }
        if let Err(err) = self.stream.close().await
            && !matches!(err, WsError::ConnectionClosed | WsError::AlreadyClosed)
        {
            return Err(ApiError::Stream(format!(
                "failed to close websocket: {err}"
            )));
        }
        Ok(())
    }

    async fn send_json(&self, message: &RealtimeOutboundMessage) -> Result<(), ApiError> {
        match message {
            RealtimeOutboundMessage::DelegationContextAppend {
                delegation_item_id,
                channel,
                content,
            } => {
                if let Some(content) = content.first() {
                    for chunk in context_append_chunks(&content.text) {
                        self.send_json_frame(&frameless_delegation_context_append_message(
                            delegation_item_id.clone(),
                            chunk,
                            *channel,
                        ))
                        .await?;
                    }
                    return Ok(());
                }
            }
            RealtimeOutboundMessage::SessionContextAppend { channel, content } => {
                if let Some(content) = content.first() {
                    for chunk in context_append_chunks(&content.text) {
                        self.send_json_frame(&frameless_session_context_append_message(
                            chunk, *channel,
                        ))
                        .await?;
                    }
                    return Ok(());
                }
            }
            _ => {}
        }
        self.send_json_frame(message).await
    }

    async fn send_json_frame(&self, message: &RealtimeOutboundMessage) -> Result<(), ApiError> {
        let payload = serde_json::to_string(message)
            .map_err(|err| ApiError::Stream(format!("failed to encode realtime request: {err}")))?;
        debug!(?message, "realtime websocket request");
        self.send_payload(payload).await
    }

    pub async fn send_payload(&self, payload: String) -> Result<(), ApiError> {
        if self.is_closed.load(Ordering::SeqCst) {
            return Err(map_realtime_websocket_connect_error(
                self.stream.closed_error(),
            ));
        }

        trace!(target: REALTIME_WIRE_LOG_TARGET, "realtime websocket request: {payload}");
        self.stream
            .send(Message::Text(payload.into()))
            .await
            .map_err(map_realtime_websocket_connect_error)?;
        Ok(())
    }
}

impl RealtimeWebsocketEvents {
    pub fn transcript_state(&self) -> RealtimeTranscriptState {
        self.transcript_state.clone()
    }

    pub async fn take_transcript_tail(&self) -> Vec<RealtimeTranscriptEntry> {
        self.transcript_state.take_transcript_tail().await
    }

    pub async fn next_event(&self) -> Result<Option<RealtimeEvent>, ApiError> {
        if self.is_closed.load(Ordering::SeqCst) {
            return Ok(None);
        }

        if let Some(event) = self.pending_events.lock().await.pop_front() {
            return Ok(Some(event));
        }

        loop {
            let msg = match self.rx_message.recv().await {
                Ok(Ok(msg)) => msg,
                Ok(Err(err)) => {
                    self.is_closed.store(true, Ordering::SeqCst);
                    error!("realtime websocket read failed: {err}");
                    return Err(map_realtime_websocket_connect_error(err));
                }
                Err(_) => {
                    self.is_closed.store(true, Ordering::SeqCst);
                    if self.event_parser == RealtimeEventParser::FramelessBidi {
                        error!("realtime websocket event stream ended unexpectedly");
                        return Err(ApiError::Stream(
                            "realtime websocket event stream ended unexpectedly".to_string(),
                        ));
                    }
                    info!("realtime websocket event stream ended");
                    return Ok(None);
                }
            };

            match msg {
                Message::Text(text) => {
                    trace!(target: REALTIME_WIRE_LOG_TARGET, "realtime websocket event: {text}");
                    if let Some(mut event) = parse_realtime_event(&text, self.event_parser) {
                        self.update_active_transcript(&mut event).await;
                        debug!(?event, "realtime websocket parsed event");
                        return Ok(Some(event));
                    }
                    debug!("realtime websocket ignored unsupported text frame");
                }
                Message::Close(frame) => {
                    self.is_closed.store(true, Ordering::SeqCst);
                    info!(
                        "realtime websocket closed: code={:?} reason={:?}",
                        frame.as_ref().map(|frame| frame.code),
                        frame.as_ref().map(|frame| frame.reason.as_str())
                    );
                    if self.event_parser != RealtimeEventParser::FramelessBidi {
                        return Ok(None);
                    }
                    return match frame {
                        Some(frame) if frame.code == CloseCode::Normal => Ok(None),
                        Some(frame) => Err(ApiError::Stream(format!(
                            "realtime websocket closed unexpectedly: code={:?} reason={:?}",
                            frame.code, frame.reason
                        ))),
                        None => Err(ApiError::Stream(
                            "realtime websocket closed unexpectedly without a status code"
                                .to_string(),
                        )),
                    };
                }
                Message::Binary(_) => {
                    return Ok(Some(RealtimeEvent::Error(
                        "unexpected binary realtime websocket event".to_string(),
                    )));
                }
                Message::Frame(_) | Message::Ping(_) | Message::Pong(_) => {}
            }
        }
    }


    async fn update_active_transcript(&self, event: &mut RealtimeEvent) {
        let mut active_transcript = self.transcript_state.active.lock().await;
        match event {
            RealtimeEvent::InputAudioSpeechStarted(_) => {
                active_transcript.new_input_entry = true;
            }
            RealtimeEvent::InputTranscriptDelta(RealtimeTranscriptDelta { delta, .. }) => {
                let force_new = active_transcript.new_input_entry;
                append_transcript_delta(
                    &mut active_transcript.entries,
                    "user",
                    delta,
                    force_new,
                    self.event_parser,
                );
                if !delta.is_empty() || self.event_parser != RealtimeEventParser::FramelessBidi {
                    active_transcript.new_input_entry = false;
                }
            }
            RealtimeEvent::OutputTranscriptDelta(RealtimeTranscriptDelta { delta, .. }) => {
                let force_new = active_transcript.new_output_entry;
                append_transcript_delta(
                    &mut active_transcript.entries,
                    "assistant",
                    delta,
                    force_new,
                    self.event_parser,
                );
                if !delta.is_empty() || self.event_parser != RealtimeEventParser::FramelessBidi {
                    active_transcript.new_output_entry = false;
                }
            }
            RealtimeEvent::InputTranscriptDone(done) => {
                let force_new = active_transcript.new_input_entry;
                apply_transcript_done(
                    &mut active_transcript.entries,
                    "user",
                    &done.text,
                    force_new,
                    self.event_parser,
                );
                active_transcript.new_input_entry =
                    self.event_parser == RealtimeEventParser::FramelessBidi;
            }
            RealtimeEvent::OutputTranscriptDone(done) => {
                let force_new = active_transcript.new_output_entry;
                apply_transcript_done(
                    &mut active_transcript.entries,
                    "assistant",
                    &done.text,
                    force_new,
                    self.event_parser,
                );
                active_transcript.new_output_entry =
                    self.event_parser == RealtimeEventParser::FramelessBidi;
            }
            RealtimeEvent::HandoffRequested(handoff) => {
                append_handoff_input(&mut active_transcript.entries, &handoff.input_transcript);
                handoff.active_transcript = std::mem::take(&mut active_transcript.entries);
                active_transcript.new_input_entry = true;
                active_transcript.new_output_entry = true;
            }
            RealtimeEvent::ResponseCreated(_) => {
                active_transcript.new_output_entry = true;
            }
            RealtimeEvent::SessionUpdated { .. }
            | RealtimeEvent::AudioOut(_)
            | RealtimeEvent::ResponseCancelled(_)
            | RealtimeEvent::ResponseDone(_)
            | RealtimeEvent::ConversationItemDone { .. }
            | RealtimeEvent::NoopRequested(_)
            | RealtimeEvent::ConversationItemAdded(_)
            | RealtimeEvent::Error(_)
            | RealtimeEvent::HistoryItemStarted(_)
            | RealtimeEvent::HistoryTranscriptDelta { .. }
            | RealtimeEvent::HistoryItemCompleted(_) => {}
        }
        truncate_active_transcript(&mut active_transcript.entries);
    }
}

impl RealtimeTranscriptState {
    pub async fn take_transcript_tail(&self) -> Vec<RealtimeTranscriptEntry> {
        std::mem::take(&mut self.active.lock().await.entries)
    }
}

fn truncate_active_transcript(entries: &mut Vec<RealtimeTranscriptEntry>) {
    let mut total_bytes = transcript_entries_bytes(entries);
    while total_bytes > MAX_ACTIVE_TRANSCRIPT_BYTES && entries.len() > 1 {
        total_bytes = total_bytes.saturating_sub(transcript_entry_bytes(&entries[0]));
        entries.remove(0);
    }
    let Some(entry) = entries.first_mut() else {
        return;
    };
    let entry_overhead = entry.role.len() + 3;
    let max_text_bytes = MAX_ACTIVE_TRANSCRIPT_BYTES.saturating_sub(entry_overhead);
    if entry.text.len() <= max_text_bytes {
        return;
    }
    let mut start = entry
        .text
        .len()
        .saturating_sub(max_text_bytes.saturating_sub(TRUNCATED_TRANSCRIPT_PREFIX.len()));
    while !entry.text.is_char_boundary(start) {
        start += 1;
    }
    entry.text = format!("{TRUNCATED_TRANSCRIPT_PREFIX}{}", &entry.text[start..]);
}

fn transcript_entries_bytes(entries: &[RealtimeTranscriptEntry]) -> usize {
    entries.iter().map(transcript_entry_bytes).sum()
}

fn transcript_entry_bytes(entry: &RealtimeTranscriptEntry) -> usize {
    entry.role.len() + entry.text.len() + 3
}

fn append_transcript_delta(
    entries: &mut Vec<RealtimeTranscriptEntry>,
    role: &str,
    delta: &str,
    force_new: bool,
    event_parser: RealtimeEventParser,
) {
    if delta.is_empty() {
        return;
    }

    // V3 interleaves the two speakers' transcripts. Legacy protocols retain
    // adjacent speaker runs and their existing response lifecycle semantics.
    let entry = match event_parser {
        RealtimeEventParser::FramelessBidi => {
            entries.iter_mut().rev().find(|entry| entry.role == role)
        }
        RealtimeEventParser::V1 | RealtimeEventParser::RealtimeV2 => {
            entries.last_mut().filter(|entry| entry.role == role)
        }
    };
    if !force_new && let Some(last_entry) = entry {
        last_entry.text.push_str(delta);
        return;
    }

    entries.push(RealtimeTranscriptEntry {
        role: role.to_string(),
        text: delta.to_string(),
    });
}

fn apply_transcript_done(
    entries: &mut Vec<RealtimeTranscriptEntry>,
    role: &str,
    text: &str,
    force_new: bool,
    event_parser: RealtimeEventParser,
) {
    if text.is_empty() {
        return;
    }

    // V3 interleaves the two speakers' transcripts. Legacy protocols retain
    // adjacent speaker runs and their existing response lifecycle semantics.
    let entry = match event_parser {
        RealtimeEventParser::FramelessBidi => {
            entries.iter_mut().rev().find(|entry| entry.role == role)
        }
        RealtimeEventParser::V1 | RealtimeEventParser::RealtimeV2 => {
            entries.last_mut().filter(|entry| entry.role == role)
        }
    };
    if !force_new && let Some(last_entry) = entry {
        // A delayed V3 final can arrive after deltas from the next utterance.
        // Preserve accumulated speech unless the final extends it.
        if event_parser != RealtimeEventParser::FramelessBidi || text.starts_with(&last_entry.text)
        {
            last_entry.text = text.to_string();
        }
        return;
    }

    entries.push(RealtimeTranscriptEntry {
        role: role.to_string(),
        text: text.to_string(),
    });
}

fn append_handoff_input(entries: &mut Vec<RealtimeTranscriptEntry>, input: &str) {
    let input = input.trim();
    if input.is_empty() || contains_transcript_entry(entries, "user", input) {
        return;
    }

    entries.push(RealtimeTranscriptEntry {
        role: "user".to_string(),
        text: input.to_string(),
    });
}

fn contains_transcript_entry(entries: &[RealtimeTranscriptEntry], role: &str, text: &str) -> bool {
    entries
        .iter()
        .any(|entry| entry.role == role && entry.text.trim() == text.trim())
}

pub struct RealtimeWebsocketClient;

impl RealtimeWebsocketClient {
    pub fn new(_provider: Provider, _http_client_factory: HttpClientFactory) -> Self {
        Self
    }

    pub fn with_webrtc_sideband_base_url(self, _base_url: String) -> Self {
        self
    }

    pub async fn connect(
        &self,
        _config: RealtimeSessionConfig,
        _extra_headers: HeaderMap,
        _default_headers: HeaderMap,
    ) -> Result<RealtimeWebsocketConnection, ApiError> {
        let _ = self;
        Err(ApiError::Stream(
            "Realtime calls are not available.".to_string(),
        ))
    }

    pub async fn connect_webrtc_sideband(
        &self,
        _config: RealtimeSessionConfig,
        _call_id: &str,
        _extra_headers: HeaderMap,
        _default_headers: HeaderMap,
        _transcript_state: RealtimeTranscriptState,
    ) -> Result<RealtimeWebsocketConnection, ApiError> {
        let _ = self;
        Err(ApiError::Stream(
            "Realtime calls are not available.".to_string(),
        ))
    }

    pub async fn connect_existing_call_sideband(
        &self,
        _config: RealtimeSessionConfig,
        _call_id: &str,
        _extra_headers: HeaderMap,
        _default_headers: HeaderMap,
        _transcript_state: RealtimeTranscriptState,
    ) -> Result<RealtimeWebsocketConnection, ApiError> {
        let _ = self;
        Err(ApiError::Stream(
            "Realtime calls are not available.".to_string(),
        ))
    }
}

fn map_realtime_websocket_connect_error(err: WsError) -> ApiError {
    if let Some(denied) = codex_websocket_client::network_policy_denial(&err) {
        return ApiError::Transport(codex_http_client::TransportError::Policy(denied));
    }
    match err {
        WsError::Http(response) => ApiError::Api {
            status: response.status(),
            message: "realtime websocket handshake failed".to_string(),
        },
        err => ApiError::Stream(format!("failed to connect realtime websocket: {err}")),
    }
}

#[cfg(test)]
#[path = "transcript_tests.rs"]
mod transcript_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::endpoint::realtime_websocket::protocol::RealtimeTranscriptEntry;
    use crate::provider::RetryConfig;
    use codex_http_client::OutboundProxyPolicy::ReqwestDefault;
    use codex_protocol::protocol::RealtimeHandoffRequested;
    use codex_protocol::protocol::RealtimeInputAudioSpeechStarted;
    use codex_protocol::protocol::RealtimeNoopRequested;
    use codex_protocol::protocol::RealtimeResponseCancelled;
    use codex_protocol::protocol::RealtimeResponseCreated;
    use codex_protocol::protocol::RealtimeResponseDone;
    use codex_protocol::protocol::RealtimeTranscriptDelta;
    use codex_protocol::protocol::RealtimeTranscriptDone;
    use codex_protocol::protocol::RealtimeVoice;
    use http::HeaderValue;
    use http::StatusCode;
    use pretty_assertions::assert_eq;
    use serde_json::Value;
    use serde_json::json;
    use std::collections::HashMap;
    use std::time::Duration;
    use tokio::net::TcpListener;
    use tokio_tungstenite::accept_async;
    use tokio_tungstenite::tungstenite::Message;
    use tungstenite::protocol::CloseFrame;

    fn test_client(provider: Provider) -> RealtimeWebsocketClient {
        RealtimeWebsocketClient::new(provider, HttpClientFactory::new(ReqwestDefault))
    }

    #[test]
    fn parse_session_updated_event() {
        let payload = json!({
            "type": "session.updated",
            "session": {"id": "sess_123", "instructions": "backend prompt"}
        })
        .to_string();

        assert_eq!(
            parse_realtime_event(payload.as_str(), RealtimeEventParser::V1),
            Some(RealtimeEvent::SessionUpdated {
                realtime_session_id: "sess_123".to_string(),
                instructions: Some("backend prompt".to_string()),
            })
        );
    }

    #[test]
    fn parse_audio_delta_event() {
        let payload = json!({
            "type": "conversation.output_audio.delta",
            "delta": "AAA=",
            "sample_rate": 48000,
            "channels": 1,
            "samples_per_channel": 960
        })
        .to_string();
        assert_eq!(
            parse_realtime_event(payload.as_str(), RealtimeEventParser::V1),
            Some(RealtimeEvent::AudioOut(RealtimeAudioFrame {
                data: "AAA=".to_string(),
                sample_rate: 48000,
                num_channels: 1,
                samples_per_channel: Some(960),
                item_id: None,
            }))
        );
    }

    #[test]
    fn parse_conversation_item_added_event() {
        let payload = json!({
            "type": "conversation.item.added",
            "item": {"type": "message", "seq": 7}
        })
        .to_string();
        assert_eq!(
            parse_realtime_event(payload.as_str(), RealtimeEventParser::V1),
            Some(RealtimeEvent::ConversationItemAdded(
                json!({"type": "message", "seq": 7})
            ))
        );
    }

    #[test]
    fn parse_conversation_item_done_event() {
        let payload = json!({
            "type": "conversation.item.done",
            "item": {"id": "item_123", "type": "message"}
        })
        .to_string();
        assert_eq!(
            parse_realtime_event(payload.as_str(), RealtimeEventParser::V1),
            Some(RealtimeEvent::ConversationItemDone {
                item_id: "item_123".to_string(),
            })
        );
    }

    #[test]
    fn parse_handoff_requested_event() {
        let payload = json!({
            "type": "conversation.handoff.requested",
            "handoff_id": "handoff_123",
            "item_id": "item_123",
            "input_transcript": "delegate this"
        })
        .to_string();

        assert_eq!(
            parse_realtime_event(payload.as_str(), RealtimeEventParser::V1),
            Some(RealtimeEvent::HandoffRequested(RealtimeHandoffRequested {
                handoff_id: "handoff_123".to_string(),
                item_id: "item_123".to_string(),
                input_transcript: "delegate this".to_string(),
                active_transcript: Vec::new(),
            }))
        );
    }

    #[tokio::test]
    async fn takes_only_transcript_after_last_handoff_once() {
        let (_tx_message, rx_message) = async_channel::unbounded();
        let events = RealtimeWebsocketEvents {
            rx_message,
            pending_events: Arc::new(Mutex::new(VecDeque::new())),
            transcript_state: RealtimeTranscriptState::default(),
            event_parser: RealtimeEventParser::V1,
            is_closed: Arc::new(AtomicBool::new(false)),
        };

        assert_eq!(events.take_transcript_tail().await, vec![]);

        let mut covered = RealtimeEvent::InputTranscriptDelta(RealtimeTranscriptDelta {
            delta: "already handed off".to_string(),
        });
        events.update_active_transcript(&mut covered).await;
        let mut handoff = RealtimeEvent::HandoffRequested(RealtimeHandoffRequested {
            handoff_id: "handoff_1".to_string(),
            item_id: "item_1".to_string(),
            input_transcript: "already handed off".to_string(),
            active_transcript: vec![],
        });
        events.update_active_transcript(&mut handoff).await;
        assert_eq!(
            handoff,
            RealtimeEvent::HandoffRequested(RealtimeHandoffRequested {
                handoff_id: "handoff_1".to_string(),
                item_id: "item_1".to_string(),
                input_transcript: "already handed off".to_string(),
                active_transcript: vec![RealtimeTranscriptEntry {
                    role: "user".to_string(),
                    text: "already handed off".to_string(),
                }],
            })
        );

        let mut tail = RealtimeEvent::OutputTranscriptDelta(RealtimeTranscriptDelta {
            delta: "tail".to_string(),
        });
        events.update_active_transcript(&mut tail).await;
        assert_eq!(
            events.take_transcript_tail().await,
            vec![RealtimeTranscriptEntry {
                role: "assistant".to_string(),
                text: "tail".to_string(),
            }]
        );
        assert_eq!(events.take_transcript_tail().await, vec![]);
    }

    #[tokio::test]
    async fn transcript_state_survives_reconnect_and_reconciles_done() {
        let transcript_state = RealtimeTranscriptState::default();
        let (_first_tx, first_rx) = async_channel::unbounded();
        let first_events = RealtimeWebsocketEvents {
            rx_message: first_rx,
            pending_events: Arc::new(Mutex::new(VecDeque::new())),
            transcript_state: transcript_state.clone(),
            event_parser: RealtimeEventParser::FramelessBidi,
            is_closed: Arc::new(AtomicBool::new(false)),
        };
        let mut partial = RealtimeEvent::InputTranscriptDelta(RealtimeTranscriptDelta {
            delta: "hello wor".to_string(),
        });
        first_events.update_active_transcript(&mut partial).await;

        let (_second_tx, second_rx) = async_channel::unbounded();
        let second_events = RealtimeWebsocketEvents {
            rx_message: second_rx,
            pending_events: Arc::new(Mutex::new(VecDeque::new())),
            transcript_state,
            event_parser: RealtimeEventParser::FramelessBidi,
            is_closed: Arc::new(AtomicBool::new(false)),
        };
        let mut done = RealtimeEvent::InputTranscriptDone(RealtimeTranscriptDone {
            text: "hello world".to_string(),
        });
        second_events.update_active_transcript(&mut done).await;

        assert_eq!(
            second_events.take_transcript_tail().await,
            vec![RealtimeTranscriptEntry {
                role: "user".to_string(),
                text: "hello world".to_string(),
            }]
        );
        assert_eq!(second_events.take_transcript_tail().await, vec![]);
    }

    #[tokio::test]
    async fn active_transcript_retains_a_bounded_suffix() {
        let (_tx_message, rx_message) = async_channel::unbounded();
        let events = RealtimeWebsocketEvents {
            rx_message,
            pending_events: Arc::new(Mutex::new(VecDeque::new())),
            transcript_state: RealtimeTranscriptState::default(),
            event_parser: RealtimeEventParser::FramelessBidi,
            is_closed: Arc::new(AtomicBool::new(false)),
        };
        let mut oversized = RealtimeEvent::InputTranscriptDelta(RealtimeTranscriptDelta {
            delta: format!("old{}new", "x".repeat(MAX_ACTIVE_TRANSCRIPT_BYTES)),
        });
        events.update_active_transcript(&mut oversized).await;

        let tail = events.take_transcript_tail().await;
        assert!(transcript_entries_bytes(&tail) <= MAX_ACTIVE_TRANSCRIPT_BYTES);
        assert!(tail[0].text.starts_with(TRUNCATED_TRANSCRIPT_PREFIX));
        assert!(tail[0].text.ends_with("new"));
    }

    #[test]
    fn parse_input_transcript_delta_event() {
        let payload = json!({
            "type": "conversation.input_transcript.delta",
            "delta": "hello "
        })
        .to_string();

        assert_eq!(
            parse_realtime_event(payload.as_str(), RealtimeEventParser::V1),
            Some(RealtimeEvent::InputTranscriptDelta(
                RealtimeTranscriptDelta {
                    delta: "hello ".to_string(),
                }
            ))
        );
    }

    #[test]
    fn parse_v1_input_audio_transcription_delta_event() {
        let payload = json!({
            "type": "conversation.item.input_audio_transcription.delta",
            "item_id": "item_input_1",
            "content_index": 0,
            "delta": "hello"
        })
        .to_string();

        assert_eq!(
            parse_realtime_event(payload.as_str(), RealtimeEventParser::V1),
            Some(RealtimeEvent::InputTranscriptDelta(
                RealtimeTranscriptDelta {
                    delta: "hello".to_string(),
                }
            ))
        );
    }

    #[test]
    fn parse_v1_input_audio_transcription_completed_event() {
        let payload = json!({
            "type": "conversation.item.input_audio_transcription.completed",
            "item_id": "item_input_1",
            "content_index": 0,
            "transcript": "hello world"
        })
        .to_string();

        assert_eq!(
            parse_realtime_event(payload.as_str(), RealtimeEventParser::V1),
            Some(RealtimeEvent::InputTranscriptDone(RealtimeTranscriptDone {
                text: "hello world".to_string(),
            }))
        );
    }

    #[test]
    fn parse_v1_input_transcript_turn_marked_event() {
        let payload = json!({
            "type": "conversation.input_transcript.turn_marked",
            "transcript": "hello realtime"
        })
        .to_string();

        assert_eq!(
            parse_realtime_event(payload.as_str(), RealtimeEventParser::V1),
            Some(RealtimeEvent::InputTranscriptDone(RealtimeTranscriptDone {
                text: "hello realtime".to_string(),
            }))
        );
    }

    #[test]
    fn parse_output_transcript_delta_event() {
        let payload = json!({
            "type": "conversation.output_transcript.delta",
            "delta": "hi"
        })
        .to_string();

        assert_eq!(
            parse_realtime_event(payload.as_str(), RealtimeEventParser::V1),
            Some(RealtimeEvent::OutputTranscriptDelta(
                RealtimeTranscriptDelta {
                    delta: "hi".to_string(),
                }
            ))
        );
    }

    #[test]
    fn parse_v1_output_audio_transcript_delta_event() {
        let payload = json!({
            "type": "response.output_audio_transcript.delta",
            "delta": "hi"
        })
        .to_string();

        assert_eq!(
            parse_realtime_event(payload.as_str(), RealtimeEventParser::V1),
            Some(RealtimeEvent::OutputTranscriptDelta(
                RealtimeTranscriptDelta {
                    delta: "hi".to_string(),
                }
            ))
        );
    }

    #[test]
    fn parse_v1_output_audio_transcript_done_event() {
        let payload = json!({
            "type": "response.output_audio_transcript.done",
            "transcript": "hi there"
        })
        .to_string();

        assert_eq!(
            parse_realtime_event(payload.as_str(), RealtimeEventParser::V1),
            Some(RealtimeEvent::OutputTranscriptDone(
                RealtimeTranscriptDone {
                    text: "hi there".to_string(),
                }
            ))
        );
    }

    #[test]
    fn parse_v1_item_done_output_text_event() {
        let payload = json!({
            "type": "conversation.item.done",
            "item": {
                "id": "item_output_1",
                "type": "message",
                "role": "assistant",
                "content": [
                    {"type": "output_text", "text": "hello"},
                    {"type": "output_text", "text": " world"}
                ]
            }
        })
        .to_string();

        assert_eq!(
            parse_realtime_event(payload.as_str(), RealtimeEventParser::V1),
            Some(RealtimeEvent::ConversationItemDone {
                item_id: "item_output_1".to_string(),
            })
        );
    }

    #[test]
    fn parse_realtime_v2_handoff_tool_call_event() {
        let payload = json!({
            "type": "conversation.item.done",
            "item": {
                "id": "item_123",
                "type": "function_call",
                "name": "background_agent",
                "call_id": "call_123",
                "arguments": "{\"prompt\":\"delegate this\"}"
            }
        })
        .to_string();

        assert_eq!(
            parse_realtime_event(payload.as_str(), RealtimeEventParser::RealtimeV2),
            Some(RealtimeEvent::HandoffRequested(RealtimeHandoffRequested {
                handoff_id: "call_123".to_string(),
                item_id: "item_123".to_string(),
                input_transcript: "delegate this".to_string(),
                active_transcript: Vec::new(),
            }))
        );
    }

    #[test]
    fn parse_realtime_v2_noop_tool_call_event() {
        let payload = json!({
            "type": "conversation.item.done",
            "item": {
                "id": "item_silent",
                "type": "function_call",
                "name": "remain_silent",
                "call_id": "call_silent",
                "arguments": "{}"
            }
        })
        .to_string();

        assert_eq!(
            parse_realtime_event(payload.as_str(), RealtimeEventParser::RealtimeV2),
            Some(RealtimeEvent::NoopRequested(RealtimeNoopRequested {
                call_id: "call_silent".to_string(),
                item_id: "item_silent".to_string(),
            }))
        );
    }

    #[test]
    fn parse_realtime_v2_input_audio_transcription_delta_event() {
        let payload = json!({
            "type": "conversation.item.input_audio_transcription.delta",
            "item_id": "item_input_1",
            "content_index": 0,
            "delta": "hello"
        })
        .to_string();

        assert_eq!(
            parse_realtime_event(payload.as_str(), RealtimeEventParser::RealtimeV2),
            Some(RealtimeEvent::InputTranscriptDelta(
                RealtimeTranscriptDelta {
                    delta: "hello".to_string(),
                }
            ))
        );
    }

    #[test]
    fn parse_realtime_v2_output_audio_transcript_done_event() {
        let payload = json!({
            "type": "response.output_audio_transcript.done",
            "transcript": "hello there"
        })
        .to_string();

        assert_eq!(
            parse_realtime_event(payload.as_str(), RealtimeEventParser::RealtimeV2),
            Some(RealtimeEvent::OutputTranscriptDone(
                RealtimeTranscriptDone {
                    text: "hello there".to_string(),
                }
            ))
        );
    }

    #[test]
    fn parse_realtime_v2_output_text_done_event() {
        let payload = json!({
            "type": "response.output_text.done",
            "text": "hello there"
        })
        .to_string();

        assert_eq!(
            parse_realtime_event(payload.as_str(), RealtimeEventParser::RealtimeV2),
            Some(RealtimeEvent::OutputTranscriptDone(
                RealtimeTranscriptDone {
                    text: "hello there".to_string(),
                }
            ))
        );
    }

    #[test]
    fn parse_realtime_v2_conversation_item_created_event() {
        let payload = json!({
            "type": "conversation.item.created",
            "item": {"type": "message", "role": "user"}
        })
        .to_string();

        assert_eq!(
            parse_realtime_event(payload.as_str(), RealtimeEventParser::RealtimeV2),
            Some(RealtimeEvent::ConversationItemAdded(
                json!({"type": "message", "role": "user"})
            ))
        );
    }

    #[test]
    fn parse_realtime_v2_item_done_output_text_event() {
        let payload = json!({
            "type": "conversation.item.done",
            "item": {
                "id": "item_output_1",
                "type": "message",
                "role": "assistant",
                "content": [
                    {"type": "output_text", "text": "hello"},
                    {"type": "output_text", "text": " world"}
                ]
            }
        })
        .to_string();

        assert_eq!(
            parse_realtime_event(payload.as_str(), RealtimeEventParser::RealtimeV2),
            Some(RealtimeEvent::ConversationItemDone {
                item_id: "item_output_1".to_string(),
            })
        );
    }

    #[test]
    fn parse_realtime_v2_output_audio_delta_defaults_audio_shape() {
        let payload = json!({
            "type": "response.output_audio.delta",
            "delta": "AQID"
        })
        .to_string();

        assert_eq!(
            parse_realtime_event(payload.as_str(), RealtimeEventParser::RealtimeV2),
            Some(RealtimeEvent::AudioOut(RealtimeAudioFrame {
                data: "AQID".to_string(),
                sample_rate: 24_000,
                num_channels: 1,
                samples_per_channel: None,
                item_id: None,
            }))
        );
    }

    #[test]
    fn parse_realtime_v2_response_audio_delta_with_item_id() {
        let payload = json!({
            "type": "response.audio.delta",
            "delta": "AQID",
            "item_id": "item_audio_1"
        })
        .to_string();

        assert_eq!(
            parse_realtime_event(payload.as_str(), RealtimeEventParser::RealtimeV2),
            Some(RealtimeEvent::AudioOut(RealtimeAudioFrame {
                data: "AQID".to_string(),
                sample_rate: 24_000,
                num_channels: 1,
                samples_per_channel: None,
                item_id: Some("item_audio_1".to_string()),
            }))
        );
    }

    #[test]
    fn parse_realtime_v2_speech_started_event() {
        let payload = json!({
            "type": "input_audio_buffer.speech_started",
            "item_id": "item_input_1"
        })
        .to_string();

        assert_eq!(
            parse_realtime_event(payload.as_str(), RealtimeEventParser::RealtimeV2),
            Some(RealtimeEvent::InputAudioSpeechStarted(
                RealtimeInputAudioSpeechStarted {
                    item_id: Some("item_input_1".to_string()),
                }
            ))
        );
    }

    #[test]
    fn parse_realtime_v2_response_cancelled_event() {
        let payload = json!({
            "type": "response.cancelled",
            "response": {"id": "resp_cancelled_1"}
        })
        .to_string();

        assert_eq!(
            parse_realtime_event(payload.as_str(), RealtimeEventParser::RealtimeV2),
            Some(RealtimeEvent::ResponseCancelled(
                RealtimeResponseCancelled {
                    response_id: Some("resp_cancelled_1".to_string()),
                }
            ))
        );
    }

    #[test]
    fn parse_realtime_v2_response_done_event() {
        let payload = json!({
            "type": "response.done",
            "response": {
                "output": [{
                    "id": "item_123",
                    "type": "function_call",
                    "name": "background_agent",
                    "call_id": "call_123",
                    "arguments": "{\"prompt\":\"delegate from done\"}"
                }]
            }
        })
        .to_string();

        assert_eq!(
            parse_realtime_event(payload.as_str(), RealtimeEventParser::RealtimeV2),
            Some(RealtimeEvent::ResponseDone(RealtimeResponseDone {
                response_id: None
            }))
        );
    }

    #[test]
    fn parse_realtime_v2_response_created_event() {
        let payload = json!({
            "type": "response.created",
            "response": {"id": "resp_created_1"}
        })
        .to_string();

        assert_eq!(
            parse_realtime_event(payload.as_str(), RealtimeEventParser::RealtimeV2),
            Some(RealtimeEvent::ResponseCreated(RealtimeResponseCreated {
                response_id: Some("resp_created_1".to_string())
            }))
        );
    }
}
