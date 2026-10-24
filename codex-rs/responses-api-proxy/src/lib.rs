use std::fs::File;
use std::fs::OpenOptions;
use std::fs::{self};
use std::io::Read;
use std::io::Write;
use std::net::SocketAddr;
use std::net::TcpListener;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use anyhow::Result;
use anyhow::anyhow;
use clap::Parser;
use reqwest::Url;
use reqwest::blocking::Client;
use reqwest::header::AUTHORIZATION;
use reqwest::header::HOST;
use reqwest::header::HeaderMap;
use reqwest::header::HeaderName;
use reqwest::header::HeaderValue;
use serde::Serialize;
use tiny_http::Header;
use tiny_http::Method;
use tiny_http::Request;
use tiny_http::Response;
use tiny_http::Server;
use tiny_http::StatusCode;

mod dump;
mod read_api_key;
use dump::ExchangeDumper;
use read_api_key::read_auth_header_from_stdin;

/// CLI arguments for the proxy.
#[derive(Debug, Clone, Parser)]
#[command(name = "responses-api-proxy", about = "Minimal OpenAI responses proxy")]
pub struct Args {
    /// Port to listen on. If not set, an ephemeral port is used.
    #[arg(long)]
    pub port: Option<u16>,

    /// Path to a JSON file to write startup info (single line). Includes {"port": <u16>}.
    #[arg(long, value_name = "FILE")]
    pub server_info: Option<PathBuf>,

    /// Enable HTTP shutdown endpoint at GET /shutdown
    #[arg(long)]
    pub http_shutdown: bool,

    /// Absolute URL the proxy should forward requests to.
    #[arg(long, required_unless_present = "listen")]
    pub upstream_url: Option<String>,

    /// Unix listener for the configured Steve providers.
    #[arg(long, requires = "client_config", conflicts_with_all = ["upstream_url", "port", "server_info", "http_shutdown", "dump_dir"])]
    pub listen: Option<String>,

    /// Publish the sanitized native client configuration after binding.
    #[arg(long, requires = "listen")]
    pub client_config: Option<PathBuf>,

    /// Directory where request/response dumps should be written as JSON.
    #[arg(long, value_name = "DIR")]
    pub dump_dir: Option<PathBuf>,
}

#[derive(Serialize)]
struct ServerInfo {
    port: u16,
    pid: u32,
}

struct ForwardConfig {
    upstream_url: Url,
    headers: HeaderMap,
}

/// Fixed upstream destinations and headers resolved by Steve's native provider loader.
pub struct ConfiguredRoute {
    pub provider: String,
    pub models_url: Url,
    pub responses_url: Url,
    pub headers: HeaderMap,
}

/// Entry point for the library main, for parity with other crates.
pub fn run_main(args: Args) -> Result<()> {
    let upstream = args
        .upstream_url
        .context("use steve responses-api-proxy for configured providers")?;
    let auth_header = read_auth_header_from_stdin()?;

    let upstream_url = Url::parse(&upstream).context("parsing --upstream-url")?;
    let host = match (upstream_url.host_str(), upstream_url.port()) {
        (Some(host), Some(port)) => format!("{host}:{port}"),
        (Some(host), None) => host.to_string(),
        _ => return Err(anyhow!("upstream URL must include a host")),
    };
    let host_header =
        HeaderValue::from_str(&host).context("constructing Host header from upstream URL")?;

    let mut headers = HeaderMap::new();
    let mut auth_header_value = HeaderValue::from_static(auth_header);
    auth_header_value.set_sensitive(true);
    headers.insert(AUTHORIZATION, auth_header_value);
    headers.insert(HOST, host_header);
    let forward_config = Arc::new(ForwardConfig {
        upstream_url,
        headers,
    });
    let dump_dir = args
        .dump_dir
        .map(ExchangeDumper::new)
        .transpose()
        .context("creating --dump-dir")?
        .map(Arc::new);

    let (listener, bound_addr) = bind_listener(args.port)?;
    if let Some(path) = args.server_info.as_ref() {
        write_server_info(path, bound_addr.port())?;
    }
    let server = Server::from_listener(listener, None)
        .map_err(|err| anyhow!("creating HTTP server: {err}"))?;
    let client = Arc::new(
        Client::builder()
            // Disable reqwest's 30s default so long-lived response streams keep flowing.
            .timeout(None::<Duration>)
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .context("building reqwest client")?,
    );

    eprintln!("responses-api-proxy listening on {bound_addr}");

    let http_shutdown = args.http_shutdown;
    for request in server.incoming_requests() {
        let client = client.clone();
        let forward_config = forward_config.clone();
        let dump_dir = dump_dir.clone();
        std::thread::spawn(move || {
            if http_shutdown && request.method() == &Method::Get && request.url() == "/shutdown" {
                let _ = request.respond(Response::new_empty(StatusCode(200)));
                std::process::exit(0);
            }

            if request.method() != &Method::Post || request.url() != "/v1/responses" {
                let _ = request.respond(Response::new_empty(StatusCode(403)));
                return;
            }

            if let Err(e) = forward_request(&client, &forward_config, dump_dir.as_deref(), request)
            {
                eprintln!("forwarding error: {e}");
            }
        });
    }

    Err(anyhow!("server stopped unexpectedly"))
}

/// Serve the already-resolved configured routes without loading configuration or credentials.
#[cfg(unix)]
pub fn run_configured(
    listen: &str,
    client_config_path: &Path,
    routes: Vec<ConfiguredRoute>,
    client_config: &[u8],
) -> Result<()> {
    let socket_path = listen
        .strip_prefix("unix://")
        .context("--listen requires unix://PATH")?;
    let server = Server::http_unix(Path::new(socket_path))
        .map_err(|error| anyhow!("binding configured proxy Unix socket failed: {error}"))?;
    let client = Arc::new(
        Client::builder()
            .timeout(None::<Duration>)
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .context("building reqwest client")?,
    );
    publish_client_config(client_config_path, client_config)?;
    let routes = Arc::new(routes);

    for request in server.incoming_requests() {
        let client = client.clone();
        let routes = routes.clone();
        std::thread::spawn(move || {
            let Some(config) = configured_destination(&routes, request.method(), request.url())
            else {
                let _ = request.respond(Response::new_empty(StatusCode(403)));
                return;
            };
            if let Err(error) = forward_request(&client, &config, None, request) {
                eprintln!("forwarding error: {error}");
            }
        });
    }
    Err(anyhow!("server stopped unexpectedly"))
}

fn configured_destination(
    routes: &[ConfiguredRoute],
    method: &Method,
    path: &str,
) -> Option<ForwardConfig> {
    let path = path.split_once('?').map_or(path, |(path, _query)| path);
    for route in routes {
        let models_path = format!("/{}/v1/models", route.provider);
        let upstream_url = if method == &Method::Get && path == models_path {
            &route.models_url
        } else if method == &Method::Post && path == format!("/{}/v1/responses", route.provider) {
            &route.responses_url
        } else {
            continue;
        };
        return Some(ForwardConfig {
            upstream_url: upstream_url.clone(),
            headers: route.headers.clone(),
        });
    }
    None
}

#[cfg(unix)]
fn publish_client_config(path: &Path, config: &[u8]) -> Result<()> {
    use std::os::unix::fs::OpenOptionsExt;

    let temporary = path.with_extension("toml.tmp");
    let mut file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&temporary)?;
    file.write_all(config)?;
    drop(file);
    fs::rename(temporary, path)?;
    Ok(())
}

fn bind_listener(port: Option<u16>) -> Result<(TcpListener, SocketAddr)> {
    let addr = SocketAddr::from(([127, 0, 0, 1], port.unwrap_or(0)));
    let listener = TcpListener::bind(addr).with_context(|| format!("failed to bind {addr}"))?;
    let bound = listener.local_addr().context("failed to read local_addr")?;
    Ok((listener, bound))
}

fn write_server_info(path: &Path, port: u16) -> Result<()> {
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        fs::create_dir_all(parent)?;
    }

    let info = ServerInfo {
        port,
        pid: std::process::id(),
    };
    let mut data = serde_json::to_string(&info)?;
    data.push('\n');
    let mut f = File::create(path)?;
    f.write_all(data.as_bytes())?;
    Ok(())
}

fn forward_request(
    client: &Client,
    config: &ForwardConfig,
    dump_dir: Option<&ExchangeDumper>,
    mut req: Request,
) -> Result<()> {
    let method = req.method().clone();
    let url_path = req.url().to_string();

    // Read request body
    let mut body = Vec::new();
    let reader = req.as_reader();
    reader.read_to_end(&mut body)?;

    let exchange_dump = dump_dir.and_then(|dump_dir| {
        dump_dir
            .dump_request(&method, &url_path, req.headers(), &body)
            .map_err(|err| {
                eprintln!("responses-api-proxy failed to dump request: {err}");
                err
            })
            .ok()
    });

    // Client authorization and host headers cannot override the fixed upstream.
    let mut headers = HeaderMap::new();
    for header in req.headers() {
        let name_ascii = header.field.as_str();
        let lower = name_ascii.to_ascii_lowercase();
        if lower.as_str() == "authorization" || lower.as_str() == "host" {
            continue;
        }

        let header_name = match HeaderName::from_bytes(lower.as_bytes()) {
            Ok(name) => name,
            Err(_) => continue,
        };
        if let Ok(value) = HeaderValue::from_bytes(header.value.as_bytes()) {
            headers.append(header_name, value);
        }
    }

    for (name, value) in &config.headers {
        headers.insert(name, value.clone());
    }

    let upstream_resp = client
        .request(
            reqwest::Method::from_bytes(method.as_str().as_bytes())?,
            config.upstream_url.clone(),
        )
        .headers(headers)
        .body(body)
        .send()
        .context("forwarding request to upstream")?;

    // We have to create an adapter between a `reqwest::blocking::Response`
    // and a `tiny_http::Response`. Fortunately, `reqwest::blocking::Response`
    // implements `Read`, so we can use it directly as the body of the
    // `tiny_http::Response`.
    let status = upstream_resp.status();
    let mut response_headers = Vec::new();
    for (name, value) in upstream_resp.headers().iter() {
        // Skip headers that tiny_http manages itself.
        if matches!(
            name.as_str(),
            "content-length" | "transfer-encoding" | "connection" | "trailer" | "upgrade"
        ) {
            continue;
        }

        if let Ok(header) = Header::from_bytes(name.as_str().as_bytes(), value.as_bytes()) {
            response_headers.push(header);
        }
    }

    let content_length = upstream_resp.content_length().and_then(|len| {
        if len <= usize::MAX as u64 {
            Some(len as usize)
        } else {
            None
        }
    });

    let response_body: Box<dyn Read + Send> = if let Some(exchange_dump) = exchange_dump {
        let headers = upstream_resp.headers().clone();
        Box::new(exchange_dump.tee_response_body(status.as_u16(), &headers, upstream_resp))
    } else {
        Box::new(upstream_resp)
    };

    let response = Response::new(
        StatusCode(status.as_u16()),
        response_headers,
        response_body,
        content_length,
        None,
    );

    let _ = req.respond(response);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    use std::os::unix::net::UnixStream;
    use std::process::Child;
    use std::process::Command;
    use std::process::Stdio;
    use std::sync::atomic::AtomicU64;
    use std::sync::atomic::Ordering;
    use std::sync::mpsc;
    use std::time::Instant;

    #[test]
    fn upstream_destination_is_required() {
        assert!(Args::try_parse_from(["responses-api-proxy"]).is_err());
    }

    #[test]
    fn explicit_upstream_destinations_are_preserved() {
        for url in [
            "http://127.0.0.1:43123/responses",
            "https://api.openai.com/v1/responses",
        ] {
            let args =
                Args::try_parse_from(["responses-api-proxy", "--upstream-url", url]).unwrap();
            assert_eq!(args.upstream_url.as_deref(), Some(url));
        }
    }

    #[test]
    fn configured_arguments_require_the_pair_and_exclude_legacy_options() {
        let options = [
            "responses-api-proxy",
            "--listen",
            "unix:///tmp/provider.sock",
            "--client-config",
            "/tmp/client.toml",
        ];
        assert!(Args::try_parse_from(options).is_ok());
        assert!(Args::try_parse_from(&options[..3]).is_err());
        assert!(
            Args::try_parse_from(["responses-api-proxy", "--client-config", "/tmp/client.toml"])
                .is_err()
        );
        for extra in [
            vec!["--upstream-url", "http://localhost/responses"],
            vec!["--port", "3128"],
            vec!["--http-shutdown"],
            vec!["--server-info", "/tmp/info"],
            vec!["--dump-dir", "/tmp/dumps"],
        ] {
            assert!(Args::try_parse_from(options.into_iter().chain(extra)).is_err());
        }
    }

    #[cfg(unix)]
    struct ProxyFixture {
        root: PathBuf,
        child: Child,
    }

    #[cfg(unix)]
    impl ProxyFixture {
        fn start(upstream: &str) -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let root = std::env::temp_dir().join(format!(
                "configured-proxy-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&root).unwrap();
            let log = File::create(root.join("child.log")).unwrap();
            let child = Command::new(std::env::current_exe().unwrap())
                .args([
                    "--ignored",
                    "--exact",
                    "tests::configured_server_child",
                    "--nocapture",
                ])
                .env("CONFIGURED_PROXY_TEST_ROOT", &root)
                .env("CONFIGURED_PROXY_TEST_UPSTREAM", upstream)
                .stdout(Stdio::from(log.try_clone().unwrap()))
                .stderr(Stdio::from(log))
                .spawn()
                .unwrap();
            let mut fixture = Self { root, child };
            let deadline = Instant::now() + Duration::from_secs(5);
            while !fixture.root.join("client.toml").exists() {
                assert!(
                    fixture.child.try_wait().unwrap().is_none(),
                    "{}",
                    fs::read_to_string(fixture.root.join("child.log")).unwrap()
                );
                assert!(Instant::now() < deadline, "proxy readiness timed out");
                std::thread::sleep(Duration::from_millis(10));
            }
            assert!(fixture.root.join("provider.sock").exists());
            assert_eq!(
                fs::read(fixture.root.join("client.toml")).unwrap(),
                b"model='alpha/native/id'\n"
            );
            fixture
        }

        fn request(&self, method: &str, path: &str, body: &str) -> String {
            let mut stream = UnixStream::connect(self.root.join("provider.sock")).unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            write!(stream, "{method} {path} HTTP/1.1\r\nHost: hostile.invalid\r\nAuthorization: Bearer client-secret\r\nX-Provider-Header: client-value\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{body}", body.len()).unwrap();
            let mut response = String::new();
            stream.read_to_string(&mut response).unwrap();
            response
        }
    }

    #[cfg(unix)]
    impl Drop for ProxyFixture {
        fn drop(&mut self) {
            let _ = self.child.kill();
            self.child.wait().unwrap();
            fs::remove_dir_all(&self.root).unwrap();
        }
    }

    #[test]
    #[ignore = "subprocess fixture"]
    #[cfg(unix)]
    fn configured_server_child() {
        let root = PathBuf::from(std::env::var_os("CONFIGURED_PROXY_TEST_ROOT").unwrap());
        let upstream = std::env::var("CONFIGURED_PROXY_TEST_UPSTREAM").unwrap();
        let mut headers = HeaderMap::new();
        headers.insert(
            AUTHORIZATION,
            HeaderValue::from_static("Bearer upstream-secret"),
        );
        headers.insert(
            "x-provider-header",
            HeaderValue::from_static("configured-value"),
        );
        let routes = vec![
            ConfiguredRoute {
                provider: "alpha".into(),
                models_url: Url::parse(&format!("{upstream}/discovery?api-version=fixed")).unwrap(),
                responses_url: Url::parse(&format!("{upstream}/responses")).unwrap(),
                headers,
            },
            ConfiguredRoute {
                provider: "beta".into(),
                models_url: Url::parse(&format!("{upstream}/v1/models")).unwrap(),
                responses_url: Url::parse(&format!("{upstream}/v1/responses")).unwrap(),
                headers: HeaderMap::new(),
            },
        ];
        run_configured(
            &format!("unix://{}", root.join("provider.sock").display()),
            &root.join("client.toml"),
            routes,
            b"model='alpha/native/id'\n",
        )
        .unwrap();
    }

    #[test]
    #[cfg(unix)]
    fn configured_proxy_keeps_fixed_routes_native_body_and_provider_headers() {
        let upstream = Server::http("127.0.0.1:0").unwrap();
        let origin = format!("http://{}", upstream.server_addr());
        let (sender, receiver) = mpsc::channel();
        let upstream_thread = std::thread::spawn(move || {
            for _ in 0..5 {
                let mut request = upstream.recv().unwrap();
                let mut body = String::new();
                request.as_reader().read_to_string(&mut body).unwrap();
                sender
                    .send((
                        request.method().to_string(),
                        request.url().to_string(),
                        request.headers().to_vec(),
                        body,
                    ))
                    .unwrap();
                request
                    .respond(Response::from_string("data: result\n\n"))
                    .unwrap();
            }
        });
        let proxy = ProxyFixture::start(&origin);
        for (method, path, body, expected_path, authenticated) in [
            (
                "GET",
                "/alpha/v1/models?client_version=0.0.0",
                "",
                "/discovery?api-version=fixed",
                true,
            ),
            (
                "GET",
                "/alpha/v1/models?arbitrary=ignored&client_version=also-ignored",
                "",
                "/discovery?api-version=fixed",
                true,
            ),
            (
                "POST",
                "/alpha/v1/responses",
                r#"{"model":"native/id","input":"hello"}"#,
                "/responses",
                true,
            ),
            (
                "POST",
                "/alpha/v1/responses?arbitrary=ignored",
                r#"{"model":"native/id","input":"query ignored"}"#,
                "/responses",
                true,
            ),
            (
                "POST",
                "/beta/v1/responses",
                r#"{"model":"native/id","input":"again"}"#,
                "/v1/responses",
                false,
            ),
        ] {
            let response = proxy.request(method, path, body);
            assert!(response.starts_with("HTTP/1.1 200"), "{response}");
            assert!(response.ends_with("data: result\n\n"));
            let (actual_method, actual_path, headers, actual_body) =
                receiver.recv_timeout(Duration::from_secs(5)).unwrap();
            assert_eq!(actual_method, method);
            assert_eq!(actual_path, expected_path);
            assert_eq!(actual_body, body);
            let authorization = headers
                .iter()
                .find(|header| header.field.equiv("Authorization"))
                .map(|header| header.value.as_str());
            assert_eq!(
                authorization,
                authenticated.then_some("Bearer upstream-secret")
            );
            if authenticated {
                assert_eq!(
                    headers
                        .iter()
                        .find(|header| header.field.equiv("X-Provider-Header"))
                        .unwrap()
                        .value
                        .as_str(),
                    "configured-value"
                );
            }
            assert!(
                !headers
                    .iter()
                    .any(|header| header.value.as_str().contains("hostile.invalid"))
            );
        }
        for (method, path) in [
            ("GET", "/unknown/v1/models"),
            ("POST", "/alpha/v1/models"),
            ("GET", "/alpha/v1/responses"),
            ("CONNECT", "example.com:443"),
            ("GET", "/shutdown"),
        ] {
            assert!(proxy.request(method, path, "").starts_with("HTTP/1.1 403"));
        }
        upstream_thread.join().unwrap();
    }

    #[test]
    #[cfg(unix)]
    fn configured_proxy_does_not_follow_upstream_redirects() {
        let upstream = Server::http("127.0.0.1:0").unwrap();
        let origin = format!("http://{}", upstream.server_addr());
        let target = format!("{origin}/unexpected-destination");
        let thread = std::thread::spawn(move || {
            let request = upstream.recv().unwrap();
            request
                .respond(
                    Response::new_empty(StatusCode(302))
                        .with_header(Header::from_bytes("Location", target).unwrap()),
                )
                .unwrap();
        });
        let proxy = ProxyFixture::start(&origin);
        assert!(
            proxy
                .request("GET", "/alpha/v1/models", "")
                .starts_with("HTTP/1.1 302")
        );
        thread.join().unwrap();
    }

    #[test]
    #[cfg(unix)]
    fn startup_failures_do_not_publish_client_config() {
        let root = std::env::temp_dir().join(format!(
            "configured-proxy-bind-failure-{}",
            std::process::id()
        ));
        fs::create_dir(&root).unwrap();
        fs::write(root.join("provider.sock"), "occupied").unwrap();
        let result = run_configured(
            &format!("unix://{}", root.join("provider.sock").display()),
            &root.join("client.toml"),
            vec![],
            b"config",
        );
        assert!(result.is_err());
        assert!(!root.join("client.toml").exists());
        fs::remove_file(root.join("provider.sock")).unwrap();
        let result = run_configured(
            &format!("unix://{}", root.join("provider.sock").display()),
            &root.join("missing-parent/client.toml"),
            vec![],
            b"config",
        );
        assert!(result.is_err());
        assert!(!root.join("missing-parent/client.toml").exists());
        fs::remove_dir_all(root).unwrap();
    }
}
