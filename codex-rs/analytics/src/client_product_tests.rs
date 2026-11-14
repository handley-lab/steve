//! Verify product attribution remains per thread across shared queues and HTTP batches.
//! Reconfiguration preserves buffered attribution; queue overflow falls back to unknown.

use super::AnalyticsEventsDestination;
use super::sample_regular_track_event;
use super::sample_skill_track_event;
use super::send_track_events;
use crate::product_attribution::ThreadProducts;
use codex_login::AuthManager;
use pretty_assertions::assert_eq;
use tokio::io::AsyncReadExt;
use tokio::io::AsyncWriteExt;

type CapturedRequest = (Option<String>, serde_json::Value);

#[test]
fn product_registry_clears_unset_threads_and_bounds_retention() {
    let mut products = ThreadProducts::default();
    products.register("unset".to_string(), Some("aeon".to_string()));
    products.register("unset".to_string(), /*product*/ None);
    for index in 0..=4096 {
        products.register(format!("thread-{index}"), Some("tpp".to_string()));
    }
    let batches = crate::product_attribution::product_event_batches(
        vec![
            sample_regular_track_event("unset"),
            sample_regular_track_event("thread-0"),
            sample_regular_track_event("thread-4096"),
        ],
        &products,
    );
    assert_eq!(
        batches
            .into_iter()
            .map(|(product, events)| (product, events.len()))
            .collect::<Vec<_>>(),
        vec![(None, 2), (Some("tpp"), 1)]
    );
    for index in 0..17 {
        products.register(format!("thread-{index}"), Some(format!("product-{index}")));
    }
    let events = (0..17)
        .map(|index| sample_regular_track_event(&format!("thread-{index}")))
        .collect::<Vec<_>>();
    let expected = serde_json::json!([[null, &events]]);
    assert_eq!(
        serde_json::to_value(crate::product_attribution::product_event_batches(
            events, &products
        ))
        .unwrap(),
        expected
    );
}

async fn capture_requests(
    count: usize,
) -> (
    AnalyticsEventsDestination,
    tokio::task::JoinHandle<Vec<CapturedRequest>>,
) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let destination = AnalyticsEventsDestination::Http {
        base_url: format!("http://{}", listener.local_addr().unwrap()).into(),
        api_key_headers: Default::default(),
    };
    let server = tokio::spawn(async move {
        let mut requests = Vec::new();
        let mut pending = Vec::new();
        for _ in 0..count {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            let (headers, body) = loop {
                let mut buffer = [0; 4096];
                let count = stream.read(&mut buffer).await.unwrap();
                assert!(count > 0);
                request.extend_from_slice(&buffer[..count]);
                if let Some(end) = request.windows(4).position(|part| part == b"\r\n\r\n") {
                    let headers = String::from_utf8_lossy(&request[..end]).to_lowercase();
                    let length: usize = headers
                        .lines()
                        .find_map(|line| line.strip_prefix("content-length: "))
                        .unwrap()
                        .parse()
                        .unwrap();
                    if request.len() >= end + 4 + length {
                        break (
                            headers,
                            serde_json::from_slice(&request[end + 4..end + 4 + length]).unwrap(),
                        );
                    }
                }
            };
            let product = headers
                .lines()
                .find_map(|line| line.strip_prefix("x-openai-product-sku: "))
                .map(str::to_string);
            requests.push((product, body));
            pending.push(stream);
            if requests.len() < count {
                continue;
            }
            for mut stream in pending.drain(..) {
                stream
                    .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
                    .await
                    .unwrap();
            }
        }
        requests
    });
    (destination, server)
}

#[tokio::test]
async fn product_streams_send_concurrently_and_preserve_event_order() {
    // Withhold every response until all 16 requests arrive: serial delivery would time out.
    let (destination, server) = capture_requests(/*count*/ 16).await;
    let manager = AuthManager::from_auth_for_testing(codex_login::CodexAuth::from_api_key("dummy"));
    let mut products = ThreadProducts::default();
    for index in 0..16 {
        products.register(format!("thread-{index}"), Some(format!("product-{index}")));
    }
    let event = |index| {
        let mut event = sample_skill_track_event(&format!("thread-{}", index % 16), Some("plugin"));
        if let crate::events::TrackEventRequest::SkillInvocation(skill) = &mut event {
            skill.event_params.turn_id = Some(format!("turn-{index}"));
        }
        event
    };
    let events = (0..32).map(event).collect();
    tokio::time::timeout(
        std::time::Duration::from_secs(15),
        send_track_events(
            &manager,
            &destination,
            events,
            &products,
            /*runtime_auth*/ None,
        ),
    )
    .await
    .unwrap();
    let mut requests = server.await.unwrap();
    requests.sort_by(|a, b| a.0.cmp(&b.0));
    let mut expected = (0..16)
        .map(|index| {
            (
                Some(format!("product-{index}")),
                serde_json::json!({"events": [event(index), event(index + 16)]}),
            )
        })
        .collect::<Vec<_>>();
    expected.sort_by(|a, b| a.0.cmp(&b.0));
    assert_eq!(requests, expected);
}
