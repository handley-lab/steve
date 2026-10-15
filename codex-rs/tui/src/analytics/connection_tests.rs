//! Exercise local Analytics plan discovery and report recovery while connected to an app-server.

use super::tests::live;
use codex_app_server_client::AppServerRequestHandle;
use codex_app_server_client::RemoteAppServerClient;
use codex_app_server_client::RemoteAppServerConnectArgs;
use codex_app_server_client::RemoteAppServerEndpoint;
use futures::SinkExt;
use futures::StreamExt;
use pretty_assertions::assert_eq;
use serde_json::Value;
use serde_json::json;
use tokio::net::TcpListener;
use tokio::net::TcpStream;
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::tungstenite::Message;
async fn read(socket: &mut WebSocketStream<TcpStream>) -> Value {
    let frame = socket.next().await.unwrap().unwrap();
    serde_json::from_str(frame.to_text().unwrap()).unwrap()
}

async fn remote() -> (RemoteAppServerClient, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
        let initialize = read(&mut socket).await;
        assert_eq!(initialize["method"], "initialize");
        socket
            .send(Message::Text(
                json!({"id": initialize["id"], "result": {
                    "userAgent": "analytics-test", "codexHome": "/server/.steve",
                }})
                .to_string()
                .into(),
            ))
            .await
            .unwrap();
        assert_eq!(read(&mut socket).await["method"], "initialized");
        // Analytics uses the local account without requesting connected-server account data.
        assert!(socket.next().await.unwrap().unwrap().is_close());
    });
    let client = RemoteAppServerClient::connect(RemoteAppServerConnectArgs {
        endpoint: RemoteAppServerEndpoint::WebSocket {
            websocket_url: format!("ws://{address}"),
            auth_token: None,
        },
        client_name: "analytics-test".into(),
        client_version: "0.0.0-test".into(),
        experimental_api: true,
        mcp_server_openai_form_elicitation: false,
        opt_out_notification_methods: Vec::new(),
        channel_capacity: 8,
    })
    .await
    .unwrap();
    (client, server)
}

#[tokio::test]
async fn local_reports_share_an_account_and_recover_profile_errors() {
    use crate::analytics::AnalyticsView;
    use crate::analytics::sections::Section;

    let http = crate::analytics::test_support::server().await;
    let (home, local) = live(&http, "enterprise").await;
    let (client, server) = remote().await;
    let mut view = AnalyticsView::new(crate::keymap::RuntimeKeymap::defaults().list);
    view.open(
        AppServerRequestHandle::Remote(client.request_handle()),
        crate::tui::FrameRequester::test_dummy(),
        Vec::new(),
        std::sync::Arc::clone(&local.config),
    );
    crate::analytics::test_support::settle(&mut view).await;
    assert_eq!(
        view.visible_sections(),
        &[
            Section::Summary,
            Section::Credits,
            Section::Usage,
            Section::Plugins,
            Section::Skills,
        ]
    );
    for section in view.visible_sections() {
        if section.report().is_some() {
            assert!(view.sections[*section].history.ready().is_some());
        }
    }
    view.refresh();
    crate::analytics::test_support::settle(&mut view).await;
    for request in http.received_requests().await.unwrap() {
        assert_eq!(
            request.headers.get("chatgpt-account-id").unwrap(),
            "account-a"
        );
    }
    http.verify().await;
    view.cancel_loads();
    client.shutdown().await.unwrap();
    server.await.unwrap();
}
