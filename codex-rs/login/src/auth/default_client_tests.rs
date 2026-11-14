use super::sanitize_user_agent;
use super::*;
use core_test_support::skip_if_no_network;
use pretty_assertions::assert_eq;

#[test]
fn test_get_codex_user_agent() {
    let user_agent = get_codex_user_agent();
    let originator = originator().value;
    let prefix = format!("{originator}/");
    assert!(user_agent.starts_with(&prefix));
}

#[test]
#[cfg(target_os = "linux")]
fn os_discovery_is_cached_without_freezing_user_agent_overrides() {
    const PROBE_DIR: &str = "STEVE_TEST_USER_AGENT_PROBE_DIR";
    if std::env::var_os(PROBE_DIR).is_some() {
        // Race the first lookup as well as exercising repeated requests.
        std::thread::scope(|scope| {
            for _ in 0..8 {
                scope.spawn(|| {
                    for _ in 0..4 {
                        assert!(get_codex_user_agent().contains("Ubuntu 99.7.3"));
                    }
                });
            }
        });
        set_default_originator("cached-os-test".to_string()).expect("set originator");
        *USER_AGENT_SUFFIX.lock().expect("suffix lock") = Some("updated-client".to_string());
        let user_agent = get_codex_user_agent();
        assert!(user_agent.starts_with("cached-os-test/"));
        assert!(user_agent.ends_with(" (updated-client)"));
        return;
    }

    // Use a fresh process so neither the cached OS nor the mutable overrides can
    // be initialized by another test. Only the child's environment is changed.
    let temp = tempfile::tempdir().expect("probe directory");
    for (program, output) in [
        ("lsb_release", "Distributor ID: Ubuntu\nRelease: 99.7.3\n"),
        ("getconf", "64\n"),
    ] {
        let script = temp.path().join(program);
        codex_utils_cargo_bin::write_executable(
            &script,
            &format!(
                "#!/bin/sh\nprintf '{program}\\n' >> \"${PROBE_DIR}/calls\"\nprintf '{output}'\n"
            ),
        )
        .expect("write probe");
    }
    let output = std::process::Command::new(std::env::current_exe().expect("test executable"))
        .args([
            "--exact",
            concat!(
                module_path!(),
                "::os_discovery_is_cached_without_freezing_user_agent_overrides"
            )
            .trim_start_matches("codex_login::"),
            "--nocapture",
        ])
        .env(PROBE_DIR, temp.path())
        .env("PATH", temp.path())
        .env_remove(CODEX_INTERNAL_ORIGINATOR_OVERRIDE_ENV_VAR)
        .output()
        .expect("run isolated user-agent test");
    assert!(output.status.success(), "{output:?}");
    let calls = std::fs::read_to_string(temp.path().join("calls")).expect("probe calls");
    let mut calls = calls.lines().collect::<Vec<_>>();
    calls.sort_unstable();
    assert_eq!(calls, vec!["getconf", "lsb_release"]);
}

#[test]
fn is_first_party_originator_matches_known_values() {
    assert_eq!(is_first_party_originator(DEFAULT_ORIGINATOR), true);
    assert_eq!(is_first_party_originator("steve-tui"), true);
    assert_eq!(is_first_party_originator("codex_vscode"), true);
    assert_eq!(is_first_party_originator("Codex Something Else"), true);
    assert_eq!(is_first_party_originator("codex_cli"), false);
    assert_eq!(is_first_party_originator("Other"), false);
}

#[test]
fn is_first_party_chat_originator_matches_known_values() {
    assert_eq!(is_first_party_chat_originator("codex_atlas"), true);
    assert_eq!(
        is_first_party_chat_originator("codex_chatgpt_desktop"),
        true
    );
    assert_eq!(is_first_party_chat_originator(DEFAULT_ORIGINATOR), false);
    assert_eq!(is_first_party_chat_originator("codex_vscode"), false);
}

#[test]
fn add_originator_header_inserts_non_default_originator() {
    let default_originator = originator();
    let thread_originator = if default_originator.value == "chatgpt_cca" {
        "codex_work_cca"
    } else {
        "chatgpt_cca"
    };
    let mut headers = HeaderMap::new();

    add_originator_header(&mut headers, thread_originator);

    assert_eq!(
        headers
            .get("originator")
            .and_then(|value| value.to_str().ok()),
        Some(thread_originator)
    );
}

#[test]
fn add_originator_header_preserves_provider_default() {
    let default_originator = originator();
    let mut headers = HeaderMap::new();
    headers.insert(
        "originator",
        HeaderValue::from_static("provider-originator"),
    );

    add_originator_header(&mut headers, &default_originator.value);

    assert_eq!(
        headers
            .get("originator")
            .and_then(|value| value.to_str().ok()),
        Some("provider-originator")
    );
}

#[test]
fn add_originator_header_omits_invalid_originator() {
    let mut headers = HeaderMap::new();

    add_originator_header(&mut headers, "invalid\noriginator");

    assert!(headers.is_empty());
}

#[tokio::test]
async fn test_create_client_sets_default_headers() {
    skip_if_no_network!();

    set_default_client_residency_requirement(Some(ResidencyRequirement::Us));

    use wiremock::Mock;
    use wiremock::MockServer;
    use wiremock::ResponseTemplate;
    use wiremock::matchers::method;
    use wiremock::matchers::path;

    let client = create_client();

    // Spin up a local mock server and capture a request.
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&server)
        .await;

    let resp = client
        .get(server.uri())
        .send()
        .await
        .expect("failed to send request");
    assert!(resp.status().is_success());

    let requests = server
        .received_requests()
        .await
        .expect("failed to fetch received requests");
    assert!(!requests.is_empty());
    let headers = &requests[0].headers;

    // originator header is set to the provided value
    let originator_header = headers
        .get("originator")
        .expect("originator header missing");
    assert_eq!(originator_header.to_str().unwrap(), originator().value);

    // User-Agent matches the computed Codex UA for that originator
    let expected_ua = get_codex_user_agent();
    let ua_header = headers
        .get("user-agent")
        .expect("user-agent header missing");
    assert_eq!(ua_header.to_str().unwrap(), expected_ua);

    let residency_header = headers
        .get(RESIDENCY_HEADER_NAME)
        .expect("residency header missing");
    assert_eq!(residency_header.to_str().unwrap(), "us");

    set_default_client_residency_requirement(/*enforce_residency*/ None);
}

#[test]
fn test_invalid_suffix_is_sanitized() {
    let prefix = "steve_cli_rs/0.0.0";
    let suffix = "bad\rsuffix";

    assert_eq!(
        sanitize_user_agent(format!("{prefix} ({suffix})"), prefix),
        "steve_cli_rs/0.0.0 (bad_suffix)"
    );
}

#[test]
fn test_invalid_suffix_is_sanitized2() {
    let prefix = "steve_cli_rs/0.0.0";
    let suffix = "bad\0suffix";

    assert_eq!(
        sanitize_user_agent(format!("{prefix} ({suffix})"), prefix),
        "steve_cli_rs/0.0.0 (bad_suffix)"
    );
}

#[test]
#[cfg(target_os = "macos")]
fn test_macos() {
    use regex_lite::Regex;
    let user_agent = get_codex_user_agent();
    let originator = regex_lite::escape(originator().value.as_str());
    let re = Regex::new(&format!(
        r"^{originator}/\d+\.\d+\.\d+ \(Mac OS \d+\.\d+\.\d+; (x86_64|arm64)\) (\S+)$"
    ))
    .unwrap();
    assert!(re.is_match(&user_agent));
}

#[tokio::test]
async fn private_backend_clients_do_not_delegate_requests() -> anyhow::Result<()> {
    use codex_http_client::DestinationPolicy;
    use codex_http_client::NetworkPolicyController;
    use wiremock::Mock;
    use wiremock::MockServer;
    use wiremock::ResponseTemplate;
    use wiremock::matchers::path;

    let mut observations = Vec::new();
    for (routed, managed) in [(false, false), (false, true), (true, false), (true, true)] {
        let origin = MockServer::start().await;
        let target = MockServer::start().await;
        let destination = format!("{}/receive", target.uri());
        Mock::given(path("/private"))
            .respond_with(
                ResponseTemplate::new(307).insert_header("location", destination.as_str()),
            )
            .mount(&origin)
            .await;
        Mock::given(path("/receive"))
            .respond_with(ResponseTemplate::new(200))
            .mount(&target)
            .await;
        let controller = NetworkPolicyController::default();
        let policy = controller.policy();
        assert!(controller.publish(policy.revision(), DestinationPolicy::Unrestricted));
        let mut factory = HttpClientFactory::new(OutboundProxyPolicy::ReqwestDefault);
        if managed {
            factory = factory.with_network_policy(policy);
        }
        let endpoint = format!("{}/private", origin.uri());
        let client = if routed {
            create_client_for_route_without_request_logging_async(
                factory,
                endpoint.clone(),
                ClientRouteClass::Api,
            )
            .await?
        } else {
            create_client_without_redirects_or_request_logging(&factory)
        };
        let body = "synthetic-private-body";
        let direct = client.post(&destination).body(body).send().await?;
        assert!(direct.status().is_success());
        let direct_requests = target.received_requests().await.unwrap();
        assert_eq!(direct_requests.len(), 1);
        assert_eq!(direct_requests[0].body, body.as_bytes());

        let get = client
            .get(&endpoint)
            .header("ChatGPT-Account-Id", "synthetic-account")
            .send()
            .await?;
        let post = client
            .post(&endpoint)
            .header("ChatGPT-Account-Id", "synthetic-account")
            .body(body)
            .send()
            .await?;
        assert_eq!(origin.received_requests().await.unwrap().len(), 2);
        observations.push((
            routed,
            managed,
            get.status(),
            post.status(),
            target.received_requests().await.unwrap(),
        ));
    }
    assert!(
        observations.iter().all(|(_, _, get, post, requests)| {
            requests.len() == 1
                && *get == http::StatusCode::TEMPORARY_REDIRECT
                && *post == http::StatusCode::TEMPORARY_REDIRECT
        }),
        "routed, managed, GET status, POST status, target requests: {observations:?}"
    );
    Ok(())
}

#[tokio::test]
async fn signed_reject_routes_preserve_invalid_ca_errors() {
    const CHILD: &str = "CODEX_TEST_SIGNED_ROUTE_INVALID_CA";
    if std::env::var_os(CHILD).is_none() {
        let dir = tempfile::tempdir().unwrap();
        let ca = dir.path().join("invalid.pem");
        std::fs::write(&ca, "invalid certificate").unwrap();
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .arg("signed_reject_routes_preserve_invalid_ca_errors")
            .arg("--nocapture")
            .env(CHILD, "1")
            .env("STEVE_CA_CERTIFICATE", ca)
            .env_remove("SSL_CERT_FILE")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(String::from_utf8_lossy(&output.stdout).contains("1 passed"));
        return;
    }

    let url = "https://127.0.0.1:9/";
    let factory = HttpClientFactory::new(OutboundProxyPolicy::ReqwestDefault)
        .with_sensitive_header_protection();
    let sync_client = create_client_for_route(
        &factory,
        url,
        ClientRouteClass::Api,
        ClientRedirectPolicy::Reject,
    )
    .unwrap();
    let async_client = create_client_for_route_async(
        factory,
        url.into(),
        ClientRouteClass::Api,
        ClientRedirectPolicy::Reject,
    )
    .await
    .unwrap();
    for client in [sync_client, async_client] {
        let error = client.get(url).send().await.unwrap_err();
        assert!(
            matches!(
                error,
                codex_http_client::HttpError::Route(
                    codex_http_client::RouteAwareClientPoolError::Build(
                        codex_http_client::BuildRouteAwareHttpClientError::CustomCa(_)
                    )
                )
            ),
            "{error}"
        );
    }
}
