use anyhow::Result;
use app_test_support::TestAppServer;
use codex_app_server_protocol::RequestId;
use pretty_assertions::assert_eq;
use serde_json::json;

#[tokio::test]
async fn feedback_upload_does_not_submit_a_sentry_envelope() -> Result<()> {
    let mut app_server = TestAppServer::builder().build_initialized().await?;
    let request_id = app_server
        .send_raw_request(
            "feedback/upload",
            Some(json!({ "classification": "bug", "includeLogs": false })),
        )
        .await?;
    let error = app_server
        .read_stream_until_error_message(RequestId::Integer(request_id))
        .await?;

    assert_eq!(error.error.code, -32600);
    assert_eq!(error.error.message, "sending feedback is not available");
    assert!(!error.error.message.contains("ingest.sentry.io"));
    Ok(())
}
