//! Incoming events cannot use a colliding native thread ID to mutate another backend's state.

use super::*;
use pretty_assertions::assert_eq;

#[tokio::test]
async fn transport_events_retain_the_session_backend() -> Result<()> {
    let app = make_test_app().await;
    let (mut server, _, proxy) =
        session_lifecycle_requests::start_recording_remote_app_server(&app.config).await?;
    let started = server.start_thread(&app.config).await?;
    server
        .thread_settings_update(codex_app_server_protocol::ThreadSettingsUpdateParams {
            thread_id: started.session.thread_id.to_string(),
            permissions: Some(":read-only".into()),
            ..Default::default()
        })
        .await?;
    let event = tokio::time::timeout(
        std::time::Duration::from_secs(/*secs*/ 5),
        server.next_event(),
    )
    .await?
    .expect("settings update emits an event");
    assert_eq!(event.backend_id, server.backend_id());
    server.shutdown().await?;
    proxy.await??;
    Ok(())
}
