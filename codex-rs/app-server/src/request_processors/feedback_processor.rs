use super::*;

#[derive(Clone)]
pub(crate) struct FeedbackRequestProcessor;

impl FeedbackRequestProcessor {
    pub(crate) fn new() -> Self {
        Self
    }

    pub(crate) async fn feedback_upload(
        &self,
        _params: FeedbackUploadParams,
    ) -> Result<Option<ClientResponsePayload>, JSONRPCErrorError> {
        Err(invalid_request("sending feedback is not available"))
    }
}
