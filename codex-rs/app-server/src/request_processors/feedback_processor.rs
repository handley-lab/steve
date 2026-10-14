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

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn feedback_upload_does_not_submit_a_sentry_envelope() {
        let processor = FeedbackRequestProcessor::new();
        let error = processor
            .feedback_upload(FeedbackUploadParams {
                classification: "bug".to_string(),
                reason: Some("local note".to_string()),
                thread_id: None,
                include_logs: true,
                extra_log_files: None,
                tags: None,
            })
            .await
            .expect_err("feedback upload has no sentry transport");
        assert_eq!(error.message, "sending feedback is not available");
        assert!(!error.message.contains("ingest.sentry.io"));
    }
}
