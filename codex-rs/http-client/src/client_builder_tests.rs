use super::*;
use http::HeaderValue;
use std::io::Read;
use std::io::Write;
use std::path::PathBuf;

#[tokio::test]
async fn failed_legacy_construction_never_discards_redirect_rejection() {
    use tokio::io::AsyncReadExt;
    use tokio::io::AsyncWriteExt;

    const CHILD_URL: &str = "STEVE_HTTP_FAILED_CONSTRUCTION_TEST_URL";
    if let Ok(url) = std::env::var(CHILD_URL) {
        let mut provider = rustls::crypto::aws_lc_rs::default_provider();
        provider.cipher_suites.clear();
        provider
            .install_default()
            .expect("isolated crypto provider");
        assert!(
            rustls::crypto::CryptoProvider::get_default()
                .unwrap()
                .cipher_suites
                .is_empty()
        );
        eprintln!("installed empty-cipher crypto provider");
        #[allow(deprecated)]
        let client = HttpClientBuilder::new()
            .without_redirects()
            .http2_prior_knowledge()
            .build_direct_with_custom_ca_fallback();
        let response = client.get(url).send().await.expect("legacy request result");
        eprintln!("invalid-crypto client returned HTTP {}", response.status());
        return;
    }

    let source = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .unwrap();
    let target = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .unwrap();
    let source_url = format!("http://{}/source", source.local_addr().unwrap());
    let target_url = format!("http://{}/target", target.local_addr().unwrap());
    let serve = |listener: tokio::net::TcpListener, response: String| {
        let requests = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let captured = Arc::clone(&requests);
        let task = tokio::spawn(async move {
            loop {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut request = Vec::new();
                let mut buffer = [0; 1024];
                while !request.windows(4).any(|part| part == b"\r\n\r\n") {
                    let count = stream.read(&mut buffer).await.unwrap();
                    if count == 0 {
                        break;
                    }
                    request.extend_from_slice(&buffer[..count]);
                }
                if !request.is_empty() {
                    captured
                        .lock()
                        .unwrap()
                        .push(String::from_utf8(request).unwrap());
                    stream.write_all(response.as_bytes()).await.unwrap();
                }
            }
        });
        (requests, task)
    };
    let (source_requests, source_task) = serve(
        source,
        format!(
            "HTTP/1.1 307 Temporary Redirect\r\nLocation: {target_url}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
        ),
    );
    let (target_requests, target_task) = serve(
        target,
        "HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok".to_string(),
    );
    let control = HttpClientBuilder::new()
        .without_redirects()
        .build_direct()
        .unwrap();
    assert_eq!(
        control
            .get(&source_url)
            .send()
            .await
            .unwrap()
            .status()
            .as_u16(),
        307
    );
    assert_eq!(
        control
            .get(&target_url)
            .send()
            .await
            .unwrap()
            .status()
            .as_u16(),
        200
    );
    let output = tokio::task::spawn_blocking(move || {
        let mut command = std::process::Command::new(std::env::current_exe().unwrap());
        command.args(["--exact", "client_builder::tests::failed_legacy_construction_never_discards_redirect_rejection", "--nocapture"])
            .env(CHILD_URL, source_url)
            .env_remove("STEVE_CA_CERTIFICATE")
            .env_remove("SSL_CERT_FILE");
        for name in ["HTTP_PROXY", "HTTPS_PROXY", "ALL_PROXY", "http_proxy", "https_proxy", "all_proxy"] {
            command.env_remove(name);
        }
        command.output().unwrap()
    }).await.unwrap();
    source_task.abort();
    target_task.abort();
    eprintln!(
        "child status={}\nstdout={}\nstderr={}\nsource requests={:?}\ntarget requests={:?}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
        source_requests.lock().unwrap(),
        target_requests.lock().unwrap()
    );
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("installed empty-cipher crypto provider")
    );
    assert_eq!(
        target_requests.lock().unwrap().len(),
        1,
        "fallback followed an unconfigured redirect"
    );
    assert_eq!(
        source_requests.lock().unwrap().len(),
        1,
        "an unconstructed transport sent a request"
    );
    assert!(
        !output.status.success(),
        "construction failure must remain visible"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("failed to build configured HTTP client with system roots"));
    assert!(stderr.contains("invalid TLS versions"));
}

#[tokio::test]
async fn custom_ca_fallback_preserves_builder_configuration() {
    let listener =
        std::net::TcpListener::bind(("127.0.0.1", 0)).expect("HTTP listener should bind");
    let address = listener
        .local_addr()
        .expect("HTTP listener should have an address");
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("HTTP listener should accept");
        let mut request = Vec::new();
        let mut chunk = [0_u8; 1024];
        while !request.windows(4).any(|window| window == b"\r\n\r\n") {
            let bytes_read = stream.read(&mut chunk).expect("HTTP request should read");
            assert!(bytes_read > 0, "HTTP request should include headers");
            request.extend_from_slice(&chunk[..bytes_read]);
        }
        stream
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
            .expect("HTTP listener should write response");
        String::from_utf8(request).expect("HTTP request should be UTF-8")
    });
    let mut headers = HeaderMap::new();
    headers.insert("x-builder-test", HeaderValue::from_static("preserved"));
    let client = HttpClientBuilder::new()
        .default_headers(headers)
        .build_with_custom_ca_fallback_using(ProxyRouting::Direct, |_| {
            Err(BuildCustomCaTransportError::InvalidCaFile {
                source_env: "TEST_CA_ENV",
                path: PathBuf::from("invalid-test-ca.pem"),
                detail: "synthetic invalid CA".to_string(),
            })
        });

    let client = HttpClient {
        backend: HttpClientBackend::Direct(client),
    };

    let response = client
        .get(format!("http://{address}/fallback"))
        .send()
        .await
        .expect("fallback client should send request");
    assert!(response.status().is_success());
    let request = server.join().expect("HTTP listener should finish");
    assert!(
        request
            .lines()
            .any(|line| line.eq_ignore_ascii_case("x-builder-test: preserved"))
    );
}

/// A server cannot keep a connection reusable after the caller requests closure.
#[tokio::test]
async fn connection_close_prevents_reuse_when_server_ignores_it()
-> Result<(), Box<dyn std::error::Error>> {
    use tokio::io::AsyncReadExt;
    use tokio::io::AsyncWriteExt;

    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0)).await?;
    let url = format!("http://{}/", listener.local_addr()?);
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await?;
        let mut headers = Vec::new();
        while !headers.ends_with(b"\r\n\r\n") {
            headers.push(stream.read_u8().await?);
        }
        // Deliberately ignore the request's Connection: close header.
        let response = b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: keep-alive\r\n\r\n";
        stream.write_all(response).await?;
        let mut byte = [0];
        let reused = tokio::select! {
            read = stream.read(&mut byte) => {
                if read? == 0 {
                    stream = listener.accept().await?.0;
                    false
                } else {
                    true
                }
            }
            accepted = listener.accept() => {
                stream = accepted?.0;
                false
            }
        };
        headers.clear();
        if reused {
            headers.push(byte[0]);
        }
        while !headers.ends_with(b"\r\n\r\n") {
            headers.push(stream.read_u8().await?);
        }
        stream.write_all(response).await?;
        Ok::<bool, std::io::Error>(reused)
    });
    let client = HttpClientBuilder::new().build_direct()?;
    for _ in 0..2 {
        client
            .get(&url)
            .header(http::header::CONNECTION, HeaderValue::from_static("close"))
            .send()
            .await?
            .bytes()
            .await?;
    }
    assert!(
        !server.await??,
        "the server reused a connection marked closed"
    );
    Ok(())
}
