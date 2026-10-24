//! Fetch picker models without blocking the event loop and keep new-thread defaults in sync.

use super::AppServerSession;
use super::model_preset_from_api_model;
use crate::app_event::AppEvent;
use crate::app_event_sender::AppEventSender;
use codex_app_server_protocol::ClientRequest;
use codex_app_server_protocol::ModelListParams;
use codex_app_server_protocol::ModelListResponse;
use codex_app_server_protocol::RequestId;
use codex_protocol::openai_models::ModelPreset;
use uuid::Uuid;

impl AppServerSession {
    pub(crate) fn set_available_models(&mut self, models: Vec<ModelPreset>) {
        self.available_models = models;
    }

    pub(crate) fn fetch_models(
        &self,
        request_id: Uuid,
        thread_id: Option<codex_protocol::ThreadId>,
        app_event_tx: AppEventSender,
    ) {
        let request_handle = self.request_handle();
        tokio::spawn(async move {
            let response = request_handle
                .request_typed::<ModelListResponse>(ClientRequest::ModelList {
                    request_id: RequestId::String(format!("model-list-{request_id}")),
                    params: ModelListParams {
                        thread_id: thread_id.map(|id| id.to_string()),
                        cursor: None,
                        limit: None,
                        include_hidden: Some(true),
                    },
                })
                .await;
            let (result, provider_errors) = match response {
                Ok(response) => (
                    Ok(response
                        .data
                        .into_iter()
                        .map(model_preset_from_api_model)
                        .collect()),
                    response.provider_errors,
                ),
                Err(err) => (Err(err.to_string()), Vec::new()),
            };
            app_event_tx.send(AppEvent::ModelsLoaded { request_id, result, provider_errors });
        });
    }
}
