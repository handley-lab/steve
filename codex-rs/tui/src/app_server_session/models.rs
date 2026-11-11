//! Fetch picker models without blocking the event loop and keep new-thread defaults in sync.

use super::AppServerSession;
use super::ThreadParamsMode;
use super::model_preset_from_api_model;
use crate::app_event::AppEvent;
use crate::app_event_sender::AppEventSender;
use codex_app_server_client::AppServerRequestHandle;
use codex_app_server_client::TypedRequestError;
use codex_app_server_protocol::ClientRequest;
use codex_app_server_protocol::ModelListParams;
use codex_app_server_protocol::ModelListResponse;
use codex_app_server_protocol::RequestId;
use codex_protocol::openai_models::ModelPreset;
use uuid::Uuid;

impl AppServerSession {
    pub(crate) async fn list_models_for_thread(
        &self,
        thread_id: codex_protocol::ThreadId,
    ) -> color_eyre::Result<ModelListResponse> {
        Ok(self
            .request_handle()
            .request_typed(ClientRequest::ModelList {
                request_id: RequestId::String(format!("model-list-{}", Uuid::new_v4())),
                params: ModelListParams {
                    thread_id: Some(thread_id.to_string()),
                    model_provider: None,
                    cursor: None,
                    limit: None,
                    include_hidden: Some(true),
                },
            })
            .await?)
    }

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
        let mode = self.thread_params_mode;
        tokio::spawn(async move {
            let response = list(
                mode,
                request_handle,
                RequestId::String(format!("model-list-{request_id}")),
                thread_id,
            )
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
            app_event_tx.send(AppEvent::ModelsLoaded {
                request_id,
                result,
                provider_errors,
            });
        });
    }
}

pub(super) async fn list(
    mode: ThreadParamsMode,
    request_handle: AppServerRequestHandle,
    request_id: RequestId,
    thread_id: Option<codex_protocol::ThreadId>,
) -> Result<ModelListResponse, TypedRequestError> {
    let response: ModelListResponse = match request_handle
        .request_typed(ClientRequest::ModelList {
            request_id,
            params: ModelListParams {
                thread_id: thread_id.map(|id| id.to_string()),
                cursor: None,
                limit: None,
                include_hidden: Some(true),
                model_provider: None,
            },
        })
        .await
    {
        Ok(response) => response,
        // Older Cloud servers do not expose model discovery. Preserve their startup behavior.
        Err(TypedRequestError::Server { source, .. })
            if mode == ThreadParamsMode::Cloud && source.code == -32601 =>
        {
            return Ok(ModelListResponse {
                data: Vec::new(),
                next_cursor: None,
                provider_errors: Vec::new(),
                cyber_access_programs_enabled: None,
            });
        }
        Err(error) => return Err(error),
    };
    Ok(response)
}

#[cfg(test)]
#[path = "models_tests.rs"]
mod tests;
