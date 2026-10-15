//! Plugin reconciliation. The installed catalog is not requested.

use super::*;
use crate::request_serialization::RequestSerializationQueues;
use codex_app_server_protocol::PluginReconcileParams;
use codex_app_server_protocol::PluginReconcileResponse;

impl PluginRequestProcessor {
    #[tracing::instrument(level = "debug", skip_all, fields(reason = ?params.reason))]
    pub(crate) async fn plugin_reconcile(
        &self,
        params: PluginReconcileParams,
        config_processor: ConfigRequestProcessor,
        request_serialization_queues: &RequestSerializationQueues,
    ) -> Result<Option<ClientResponsePayload>, JSONRPCErrorError> {
        let _ = (config_processor, request_serialization_queues);
        Ok(Some(PluginReconcileResponse::default().into()))
    }
}
