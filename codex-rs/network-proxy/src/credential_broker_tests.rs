use super::*;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use pretty_assertions::assert_eq;
use rama_http::HeaderValue;
use rama_http::header::AUTHORIZATION;

fn env_map<const N: usize>(entries: [(&str, &str); N]) -> HashMap<String, String> {
    entries
        .into_iter()
        .map(|(key, value)| (key.to_string(), value.to_string()))
        .collect()
}

fn headers_with_bearer(value: &str) -> HeaderMap {
    headers_with_authorization(&format!("Bearer {value}"))
}

fn headers_with_authorization(value: &str) -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert(
        AUTHORIZATION,
        HeaderValue::from_str(value).expect("valid authorization header"),
    );
    headers
}

fn authorization(headers: &HeaderMap) -> Option<&str> {
    headers
        .get(AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
}

fn assert_credential_shape(real_value: &str, dummy_value: &str, prefix: &str) {
    assert_ne!(dummy_value, real_value);
    assert_eq!(dummy_value.len(), real_value.len());
    assert_eq!(&dummy_value[..prefix.len()], prefix);
    let same_shape = real_value
        .bytes()
        .zip(dummy_value.bytes())
        .skip(prefix.len())
        .all(|(real, dummy)| {
            real.is_ascii_alphanumeric() && dummy.is_ascii_alphanumeric() || real == dummy
        });
    assert!(same_shape);
}
#[test]
fn virtualize_child_env_replaces_supported_credentials() {
    let broker = CredentialBroker::new(/*enabled*/ true);
    let github_token = "github_pat_11AA0bbCC_abcdefghijklmnopqrstuvwxyz0123456789ABCDEFGH";
    let authorization = format!("Bearer {github_token}");
    let mut env = env_map([
        ("GH_TOKEN", github_token),
        ("HOMEBREW_GITHUB_API_TOKEN", github_token),
        ("AUTH_HEADER", authorization.as_str()),
        ("GH_ENTERPRISE_TOKEN", github_token),
    ]);

    broker.virtualize_child_env(&mut env);

    let github_dummy = env.get("GH_TOKEN").expect("dummy GitHub token");
    assert_credential_shape(github_token, github_dummy, "github_pat_");
    assert_eq!(env.get("HOMEBREW_GITHUB_API_TOKEN"), Some(github_dummy));
    assert_eq!(env.get("GH_ENTERPRISE_TOKEN"), Some(github_dummy));
    assert_eq!(
        env.get("AUTH_HEADER"),
        Some(&format!("Bearer {github_dummy}"))
    );
    let mut persisted_credentials = github_token.to_string();
    assert!(broker.virtualize_text(&mut persisted_credentials, &env));
    assert_eq!(&persisted_credentials, github_dummy);

    let unknown_github_token = "ghp_0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefgh";
    let mut unregistered = format!("{unknown_github_token}\nghp_x");
    assert!(!broker.virtualize_text(&mut unregistered, &env));
    assert_eq!(unregistered, "\nghp_x");
    for key in [
        "sk-ant-api03-0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmno",
        "sk-ant-oat01-0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmno",
        "sk-or-v1-0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmno",
    ] {
        let mut unrelated_provider = key.to_string();
        assert!(broker.virtualize_text(&mut unrelated_provider, &env));
        assert_eq!(unrelated_provider, key);
        assert!(!credential_broker_provider_sources_allowed(
            key,
            key,
            &HashMap::new(),
            |_| true,
        ));
    }
    let mut embedded_tokens = format!("v1{unknown_github_token}");
    assert!(!broker.virtualize_text(&mut embedded_tokens, &env));
    assert_eq!(embedded_tokens, "v1");
    let equivalent_sources = env_map([
        ("GH_TOKEN", unknown_github_token),
        ("GITHUB_TOKEN", unknown_github_token),
    ]);
    assert!(credential_broker_provider_sources_allowed(
        unknown_github_token,
        "",
        &equivalent_sources,
        |source| source == "GH_TOKEN",
    ));
    let distinct_github_token = "ghp_abcdefghijklmnopqrstuvwxyz0123456789ABCDEFGH";
    let distinct_sources = env_map([
        ("GH_TOKEN", unknown_github_token),
        ("GITHUB_TOKEN", distinct_github_token),
    ]);
    assert!(!credential_broker_provider_sources_allowed(
        &format!("{unknown_github_token}\n{distinct_github_token}"),
        "",
        &distinct_sources,
        |source| source == "GH_TOKEN",
    ));
    let placeholder_broker = CredentialBroker::new(/*enabled*/ true);
    let mut placeholder_env = env_map([("GH_TOKEN", "ghp_")]);
    placeholder_broker.virtualize_child_env(&mut placeholder_env);
    let mut alias = format!("Bearer {unknown_github_token}");
    assert!(!placeholder_broker.virtualize_text(&mut alias, &placeholder_env));
    assert!(!alias.contains(unknown_github_token));

    let mut command = vec![format!("Authorization: Bearer {github_dummy}")];
    let github_dummy = github_dummy.clone();
    env.insert(
        "GIT_CONFIG_VALUE_0".to_string(),
        format!("Authorization: Bearer {github_dummy}"),
    );
    assert_eq!(
        brokered_credential_dummy_env_keys(&env),
        vec!["GH_TOKEN".to_string()]
    );

    broker.restore_child_env(&mut env, &mut command);
    assert_eq!(env.get("GH_TOKEN").map(String::as_str), Some(github_token));
    assert_eq!(
        env.get("HOMEBREW_GITHUB_API_TOKEN").map(String::as_str),
        Some(github_token)
    );
    assert_eq!(
        env.get("GH_ENTERPRISE_TOKEN").map(String::as_str),
        Some(github_token)
    );
    assert_eq!(env.get("AUTH_HEADER"), Some(&authorization));
    assert_eq!(
        env.get("GIT_CONFIG_VALUE_0"),
        Some(&format!("Authorization: Bearer {github_dummy}"))
    );
    assert_eq!(
        command,
        vec![format!("Authorization: Bearer {github_dummy}")]
    );

    env.insert("GH_TOKEN".to_string(), "user-override".to_string());
    broker.restore_child_env(&mut env, &mut []);
    assert_eq!(
        env.get("GH_TOKEN").map(String::as_str),
        Some("user-override")
    );
}


#[test]
fn unsupported_children_restore_credentials_and_disable_brokerage() {
    let broker = CredentialBroker::new(/*enabled*/ true);
    let github_token = "ghp_abcdefghijklmnopqrstuvwxyz1234567890";
    let mut env = env_map([("GH_TOKEN", github_token)]);
    broker.virtualize_child_env(&mut env);

    assert_ne!(env.get("GH_TOKEN").map(String::as_str), Some(github_token));
    assert_eq!(
        env.get(CREDENTIAL_BROKER_ACTIVE_ENV_KEY)
            .map(String::as_str),
        Some("1")
    );
    assert!(env.contains_key(BROKERED_CREDENTIALS_ENV_KEY));

    broker.restore_and_disable_child_env(&mut env, &mut []);

    assert_eq!(env.get("GH_TOKEN").map(String::as_str), Some(github_token));
    assert!(!env.contains_key(CREDENTIAL_BROKER_ACTIVE_ENV_KEY));
    assert!(!env.contains_key(BROKERED_CREDENTIALS_ENV_KEY));
}

#[cfg(windows)]
#[test]
fn brokered_credentials_match_environment_keys_case_insensitively_on_windows() {
    let broker = CredentialBroker::new(/*enabled*/ true);
    let mut env = env_map([
        ("gh_host", "github.example.com"),
        ("gh_enterprise_token", "ghp-enterprise-real"),
    ]);

    broker.virtualize_child_env(&mut env);
    let dummy = env
        .get("GH_ENTERPRISE_TOKEN")
        .expect("dummy GitHub enterprise token");
    let mut headers = headers_with_bearer(dummy);
    broker.inject_request_headers("github.example.com", &mut headers);

    assert_eq!(
        brokered_credential_dummy_env_keys(&env),
        vec!["GH_ENTERPRISE_TOKEN".to_string()]
    );
    assert_eq!(authorization(&headers), Some("Bearer ghp-enterprise-real"));
}

#[test]
fn virtualize_child_env_preserves_live_dummy_mappings() {
    let broker = CredentialBroker::new(/*enabled*/ true);
    let mut first_env = env_map([("GH_TOKEN", "ghp-real-one")]);
    let mut second_env = env_map([("GH_TOKEN", "ghp-real-two")]);

    broker.virtualize_child_env(&mut first_env);
    broker.virtualize_child_env(&mut second_env);
    let first_dummy = first_env.get("GH_TOKEN").expect("first dummy token");
    let second_dummy = second_env.get("GH_TOKEN").expect("second dummy token");
    let mut first_headers = headers_with_bearer(first_dummy);
    let mut second_headers = headers_with_bearer(second_dummy);

    broker.inject_request_headers("api.github.com", &mut first_headers);
    broker.inject_request_headers("api.github.com", &mut second_headers);

    assert_eq!(authorization(&first_headers), Some("Bearer ghp-real-one"));
    assert_eq!(authorization(&second_headers), Some("Bearer ghp-real-two"));

    let mut alias_only = env_map([("HOMEBREW_GITHUB_API_TOKEN", "ghp-real-one")]);
    broker.virtualize_child_env(&mut alias_only);
    assert_eq!(
        alias_only.get("HOMEBREW_GITHUB_API_TOKEN"),
        Some(first_dummy)
    );
    broker.restore_child_env(&mut alias_only, &mut []);
    assert_eq!(alias_only["HOMEBREW_GITHUB_API_TOKEN"], "ghp-real-one");

    let mut overridden = env_map([
        ("GH_TOKEN", "ghp-real-two"),
        ("HOMEBREW_GITHUB_API_TOKEN", "ghp-real-one"),
    ]);
    broker.virtualize_child_env(&mut overridden);
    assert_eq!(overridden.get("GH_TOKEN"), Some(second_dummy));
    assert_eq!(
        overridden.get("HOMEBREW_GITHUB_API_TOKEN"),
        Some(first_dummy)
    );

    let mut cloud_alias = env_map([
        ("GH_TOKEN", "ghp-real-one"),
        ("GITHUB_TOKEN", "ghp-real-one"),
    ]);
    broker.virtualize_child_env(&mut cloud_alias);
    cloud_alias.insert("GITHUB_TOKEN".to_string(), first_dummy.clone());
    broker.restore_child_env(&mut cloud_alias, &mut []);
    assert_eq!(cloud_alias["GITHUB_TOKEN"], "ghp-real-one");

    let mut distinct_credentials = env_map([
        ("GH_TOKEN", "ghp-primary-secret"),
        ("GITHUB_TOKEN", "ghp-secondary-secret"),
    ]);
    broker.virtualize_child_env(&mut distinct_credentials);
    let secondary_dummy = distinct_credentials["GITHUB_TOKEN"].clone();
    distinct_credentials.remove("GITHUB_TOKEN");
    distinct_credentials.insert("GH_TOKEN".to_string(), secondary_dummy.clone());
    broker.virtualize_child_env(&mut distinct_credentials);
    broker.restore_child_env(&mut distinct_credentials, &mut []);
    assert_eq!(distinct_credentials["GH_TOKEN"], secondary_dummy);
}

#[test]
fn unbound_enterprise_aliases_retain_source_ownership() {
    let token = "ghp_abcdefghijklmnopqrstuvwxyz0123456789";
    let real_header = format!("Bearer {token}");
    for source in ["GH_ENTERPRISE_TOKEN", "GITHUB_ENTERPRISE_TOKEN"] {
        for host in [None, Some("")] {
            for parent_discovery in [false, true] {
                let broker = CredentialBroker::new(/*enabled*/ true);
                let mut parent = env_map([(source, token)]);
                if let Some(host) = host {
                    parent.insert("GH_HOST".to_string(), host.to_string());
                }
                let mut child = env_map([("AUTH_HEADER", real_header.as_str())]);
                if parent_discovery {
                    broker.discover_parent_credentials(&parent, &child);
                } else {
                    broker.virtualize_child_env(&mut parent);
                }
                broker.virtualize_child_env(&mut child);
                assert_eq!(child["AUTH_HEADER"], real_header, "{source}, {host:?}");
                assert!(broker.read_state().credentials.is_empty());

                // A later explicit source and destination can still register normally.
                child.insert(source.to_string(), token.to_string());
                child.insert("GH_HOST".to_string(), "enterprise.example".to_string());
                broker.virtualize_child_env(&mut child);
                let dummy = &child[source];
                assert_ne!(dummy, token);
                let mut headers = headers_with_bearer(dummy);
                let original = headers.clone();
                broker.inject_request_headers("api.github.com", &mut headers);
                assert_eq!(headers, original);
                broker.inject_request_headers("enterprise.example", &mut headers);
                assert_eq!(authorization(&headers), Some(real_header.as_str()));
            }
        }
    }
}

#[test]
fn virtualize_child_env_replaces_aliases_of_filtered_parent_credentials() {
    let broker = CredentialBroker::new(/*enabled*/ true);
    let github_token = "ghp_abcdefghijklmnopqrstuvwxyz1234567890";
    let authorization_header = format!("Bearer {github_token}");
    let parent_env = env_map([
        ("GH_TOKEN", github_token),
        ("HOMEBREW_GITHUB_API_TOKEN", github_token),
    ]);
    let mut child_env = env_map([
        ("HOMEBREW_GITHUB_API_TOKEN", github_token),
        ("AUTH_HEADER", authorization_header.as_str()),
    ]);

    broker.discover_parent_credentials(&parent_env, &child_env);
    broker.virtualize_child_env(&mut child_env);

    let dummy = child_env["HOMEBREW_GITHUB_API_TOKEN"].clone();
    assert_ne!(dummy, github_token);
    assert_eq!(child_env["AUTH_HEADER"], format!("Bearer {dummy}"));
    assert!(!child_env.contains_key("GH_TOKEN"));

    let mut virtualized_alias = child_env["AUTH_HEADER"].clone();
    assert!(broker.virtualize_text(&mut virtualized_alias, &child_env));
    assert_eq!(virtualized_alias, child_env["AUTH_HEADER"]);

    let mut headers = headers_with_bearer(&dummy);
    broker.inject_request_headers("api.github.com", &mut headers);
    assert_eq!(authorization(&headers), Some(authorization_header.as_str()));

    broker.restore_child_env(&mut child_env, &mut []);
    assert_eq!(child_env["HOMEBREW_GITHUB_API_TOKEN"], github_token);
    assert_eq!(child_env["AUTH_HEADER"], authorization_header);
    assert!(!child_env.contains_key("GH_TOKEN"));
}

#[test]
fn virtualize_child_env_discovers_credentials_without_canonical_variables() {
    for (token, canonical_key, host) in [
        (
            "ghp_abcdefghijklmnopqrstuvwxyz1234567890",
            "GH_TOKEN",
            "api.github.com",
        ),
    ] {
        let broker = CredentialBroker::new(/*enabled*/ true);
        let authorization_header = format!("Bearer {token}");
        let mut env = env_map([("AUTH_HEADER", authorization_header.as_str())]);

        broker.virtualize_child_env(&mut env);

        let dummy_header = &env["AUTH_HEADER"];
        assert_ne!(dummy_header, &authorization_header);
        assert!(!env.contains_key(canonical_key));
        let mut headers = headers_with_authorization(dummy_header);
        broker.inject_request_headers(host, &mut headers);
        assert_eq!(authorization(&headers), Some(authorization_header.as_str()));

        let mut snapshot = format!("export AUTH_HEADER='{authorization_header}'");
        assert!(broker.virtualize_text(&mut snapshot, &env));
        assert_eq!(snapshot, format!("export AUTH_HEADER='{dummy_header}'"));

        let broker = CredentialBroker::new(/*enabled*/ true);
        let mut env = env_map([
            (canonical_key, "another-canonical-credential"),
            ("AUTH_HEADER", authorization_header.as_str()),
        ]);
        broker.virtualize_child_env(&mut env);
        assert_ne!(env["AUTH_HEADER"], authorization_header);
        let mut headers = headers_with_authorization(&env["AUTH_HEADER"]);
        broker.inject_request_headers(host, &mut headers);
        assert_eq!(authorization(&headers), Some(authorization_header.as_str()));
    }
}

#[test]
fn virtualize_child_env_binds_filtered_enterprise_credentials_to_child_host() {
    let github_token = "ghp_abcdefghijklmnopqrstuvwxyz1234567890";
    let authorization_header = format!("Bearer {github_token}");

    for (parent_host, include_cloud_token) in [
        (None, false),
        (Some("github.previous.example"), false),
        (None, true),
    ] {
        let broker = CredentialBroker::new(/*enabled*/ true);
        let mut parent_env = env_map([("GH_ENTERPRISE_TOKEN", github_token)]);
        if let Some(parent_host) = parent_host {
            parent_env.insert("GH_HOST".to_string(), parent_host.to_string());
        }
        if include_cloud_token {
            parent_env.insert("GH_TOKEN".to_string(), github_token.to_string());
        }
        let mut child_env = env_map([
            ("GH_HOST", "github.current.example"),
            ("AUTH_HEADER", authorization_header.as_str()),
        ]);

        broker.discover_parent_credentials(&parent_env, &child_env);
        broker.virtualize_child_env(&mut child_env);

        assert_ne!(child_env["AUTH_HEADER"], authorization_header);
        let mut headers = headers_with_authorization(&child_env["AUTH_HEADER"]);
        broker.inject_request_headers("github.current.example", &mut headers);
        assert_eq!(authorization(&headers), Some(authorization_header.as_str()));

        let mut previous_headers = headers_with_authorization(&child_env["AUTH_HEADER"]);
        broker.inject_request_headers("github.previous.example", &mut previous_headers);
        assert_eq!(
            authorization(&previous_headers),
            Some(child_env["AUTH_HEADER"].as_str())
        );
    }
}
#[test]
fn brokered_credential_env_keys_only_include_registered_credentials() {
    let broker = CredentialBroker::new(/*enabled*/ true);
    let mut env = env_map([
        ("GH_TOKEN", ""),
        ("GH_HOST", "github.example.com"),
    ]);

    broker.virtualize_child_env(&mut env);
    env.insert(
        "GH_TOKEN".to_string(),
        "ghp_added_after_brokerage".to_string(),
    );

    assert_eq!(
        brokered_credential_env_keys(&env).collect::<Vec<_>>(),
        Vec::<&str>::new()
    );
}


#[test]
fn virtualize_child_env_keeps_unbound_enterprise_token_out_of_persisted_text() {
    let broker = CredentialBroker::new(/*enabled*/ true);
    let token = "ghp_abcdefghijklmnopqrstuvwxyz1234567890";
    let authorization_header = format!("Bearer {token}");
    let mut env = env_map([
        ("GH_ENTERPRISE_TOKEN", token),
        ("AUTH_HEADER", authorization_header.as_str()),
    ]);

    broker.virtualize_child_env(&mut env);
    assert_eq!(env["GH_ENTERPRISE_TOKEN"], token);
    assert_eq!(env["AUTH_HEADER"], authorization_header);
    for alias in [
        format!("export GH_ENTERPRISE_TOKEN={token}"),
        format!("export AUTH_HEADER='Bearer {token}_suffix'"),
        format!("export AUTH_HEADER='Bearer {token}-suffix'"),
    ] {
        let mut persisted = alias;
        assert!(!broker.virtualize_text(&mut persisted, &env));
        assert!(!persisted.contains(token));
    }
    let distinct_token = "ghp_ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789abcdefghijkl";
    let mut adjacent = format!("{token}_{distinct_token}");
    assert!(!broker.virtualize_text(&mut adjacent, &env));
    assert_eq!(adjacent, "_");

    let mut truncated_env = env_map([("GH_ENTERPRISE_TOKEN", "ghp_abcdefghijkl")]);
    broker.virtualize_child_env(&mut truncated_env);
    let mut hidden = token.to_string();
    assert!(!broker.virtualize_text(&mut hidden, &truncated_env));
    assert!(hidden.is_empty());
    assert!(!credential_broker_provider_sources_allowed(
        token,
        "",
        &truncated_env,
        |source| source != "GH_TOKEN",
    ));
    assert!(!credential_broker_provider_sources_allowed(
        token,
        "",
        &HashMap::new(),
        |source| source != "GH_TOKEN",
    ));

    let fine_grained = "github_pat_11AA0bbCC_abcdefghijklmnopqrstuvwxyz0123456789ABCDEFGH";
    let mut truncated_env = env_map([(
        "GH_ENTERPRISE_TOKEN",
        "github_pat_11AA0bbCC_abcdefghijklmnopqrs",
    )]);
    broker.virtualize_child_env(&mut truncated_env);
    let mut hidden = fine_grained.to_string();
    assert!(!broker.virtualize_text(&mut hidden, &truncated_env));
    assert!(hidden.is_empty());
    assert!(!credential_broker_provider_sources_allowed(
        fine_grained,
        "",
        &env_map([("GH_TOKEN", "github_pat_11AA0bbCC")]),
        |source| source == "GH_TOKEN",
    ));
    let mut headers = headers_with_bearer(token);
    broker.inject_request_headers("attacker.example", &mut headers);

    assert_eq!(env["GH_ENTERPRISE_TOKEN"], token);
    assert_eq!(headers, headers_with_bearer(token));
    assert!(!broker.host_requires_mitm("attacker.example", /*port*/ 443));

    env.insert("GH_HOST".to_string(), "github.example.com".to_string());
    broker.virtualize_child_env(&mut env);
    let mut headers = headers_with_bearer(&env["GH_ENTERPRISE_TOKEN"]);
    broker.inject_request_headers("github.example.com", &mut headers);
    assert_eq!(
        authorization(&headers),
        Some(format!("Bearer {token}").as_str())
    );
}

#[test]
fn inject_request_headers_requires_dummy_to_select_ambiguous_github_credential() {
    let broker = CredentialBroker::new(/*enabled*/ true);
    let mut env = env_map([
        ("GH_TOKEN", "ghp-real-one"),
        ("GITHUB_TOKEN", "ghp-real-two"),
    ]);
    broker.virtualize_child_env(&mut env);
    let github_token = env.get("GITHUB_TOKEN").expect("dummy github token");
    let mut headers = HeaderMap::new();

    broker.inject_request_headers("api.github.com", &mut headers);
    assert_eq!(authorization(&headers), None);

    headers = headers_with_bearer(github_token);

    broker.inject_request_headers("api.github.com", &mut headers);

    assert_eq!(authorization(&headers), Some("Bearer ghp-real-two"));
}

#[test]
fn request_translation_preserves_provider_scheme_and_host_binding() {
    let broker = CredentialBroker::new(/*enabled*/ true);
    let mut env = env_map([("GH_TOKEN", "ghp-real")]);
    broker.virtualize_child_env(&mut env);
    let gh = &env["GH_TOKEN"];
    let basic_dummy = STANDARD.encode(format!("x-access-token:{gh}"));
    let basic_real = STANDARD.encode("x-access-token:ghp-real");
    let basic_username_dummy = STANDARD.encode(format!("{gh}:x-oauth-basic"));
    let basic_username_real = STANDARD.encode("ghp-real:x-oauth-basic");
    let basic_dummy = basic_dummy.as_str();
    let basic_real = basic_real.as_str();
    let basic_username_dummy = basic_username_dummy.as_str();
    let basic_username_real = basic_username_real.as_str();

    for (host, scheme, input, expected) in [
        ("github.com", "Basic", basic_dummy, basic_real),
        ("example.com", "Basic", basic_dummy, basic_dummy),
        (
            "github.com",
            "Basic",
            basic_username_dummy,
            basic_username_real,
        ),
        (
            "example.com",
            "Basic",
            basic_username_dummy,
            basic_username_dummy,
        ),
        ("api.github.com", "Bearer", gh.as_str(), "ghp-real"),
        ("uploads.github.com", "Bearer", gh.as_str(), "ghp-real"),
        ("api.github.com", "token", gh.as_str(), "ghp-real"),
    ] {
        let mut headers = headers_with_authorization(&format!("{scheme} {input}"));
        broker.inject_request_headers(host, &mut headers);
        let expected = format!("{scheme} {expected}");
        assert_eq!(authorization(&headers), Some(expected.as_str()), "{host}");
    }
}

#[test]
fn concurrent_commands_preserve_discovered_credential_destinations() {
    for (key, host_key, token) in [
        (
            "GH_ENTERPRISE_TOKEN",
            "GH_HOST",
            "ghp_abcdefghijklmnopqrstuvwxyz1234567890",
        ),
        (
            "PROVIDER_TOKEN",
            "PROVIDER_ENDPOINT",
            "provider_abcdefghijklmnopqrstuvwx",
        ),
    ] {
        let broker = CredentialBroker::new(/*enabled*/ true);
        broker.configure(&NetworkProxyConfig {
            credential_broker: true,
            credential_providers: BTreeMap::from([(
                "custom".to_string(),
                CredentialProviderConfig {
                    env: vec!["PROVIDER_TOKEN".to_string()],
                    patterns: vec!["^provider_[a-z]{24}$".to_string()],
                    url_prefix_from_env: Some("PROVIDER_ENDPOINT".to_string()),
                    ..CredentialProviderConfig::default()
                },
            )]),
            ..NetworkProxyConfig::default()
        });
        let mut commands = ["first.example", "second.example"].map(|host| {
            let destination = if host_key == "GH_HOST" {
                host.to_string()
            } else {
                format!("https://{host}/v1")
            };
            env_map([
                (key, token),
                (host_key, &destination),
                ("AUTH_HEADER", &format!("Bearer {token}")),
            ])
        });
        for env in &mut commands {
            broker.virtualize_child_env_for_environment(env, Some("shared-environment"));
        }
        let dummy = commands[0][key].clone();
        assert_ne!(dummy, token);
        assert_eq!(commands[1][key], dummy);
        let assert_destinations = || {
            for (environment_id, host, injected) in [
                ("shared-environment", "first.example", true),
                ("shared-environment", "second.example", true),
                ("shared-environment", "unrelated.example", false),
                ("other-environment", "first.example", false),
                ("other-environment", "second.example", false),
            ] {
                let mut headers = headers_with_bearer(&dummy);
                broker.inject_request_headers_for_environment(
                    &format!("https://{host}/v1/models"),
                    &mut headers,
                    Some(environment_id),
                );
                assert_eq!(
                    headers,
                    headers_with_bearer(if injected { token } else { &dummy }),
                    "{key}, {environment_id}, {host}"
                );
                assert_eq!(
                    broker
                        .host_protocols_for_environment(
                            host,
                            /*port*/ 443,
                            Some(environment_id),
                        )
                        .tls,
                    injected,
                    "{key}, {environment_id}, {host}"
                );
            }
        };
        assert_destinations();
        for env in &mut commands {
            env.remove(key);
            broker.virtualize_child_env_for_environment(env, Some("shared-environment"));
            assert_eq!(env["AUTH_HEADER"], format!("Bearer {dummy}"));
            assert_destinations();
        }

        commands[0].insert(key.to_string(), token.to_string());
        broker.virtualize_child_env_for_environment(&mut commands[0], Some("shared-environment"));
        assert_eq!(commands[0][key], dummy);
        assert_destinations();

        let mut inherited = env_map([(key, token), (host_key, &commands[0][host_key])]);
        broker.virtualize_child_env_for_environment(&mut inherited, Some("parent-environment"));
        let parent_dummy = inherited[key].clone();
        assert_ne!(parent_dummy, dummy);
        broker.virtualize_child_env_for_environment(&mut inherited, Some("shared-environment"));
        assert_eq!(inherited[key], dummy);
        assert_destinations();
        let mut headers = headers_with_bearer(&parent_dummy);
        broker.inject_request_headers_for_environment(
            "https://second.example/v1/models",
            &mut headers,
            Some("parent-environment"),
        );
        assert_eq!(headers, headers_with_bearer(&parent_dummy));
    }
}

#[test]
fn builtin_credentials_use_private_destination_context() {
    let broker = CredentialBroker::new(/*enabled*/ true);
    let mut config = NetworkProxyConfig::default();
    config.set_credential_broker_enabled(/*enabled*/ true);
    config.configure_credential_broker_environment(&env_map([(
        "GH_HOST",
        "github.enterprise.example",
    )]));
    broker.configure(&config);
    for (key, context_key, token, host) in [
        (
            "GH_ENTERPRISE_TOKEN",
            "GH_HOST",
            "ghp-real",
            "github.enterprise.example",
        ),
    ] {
        let mut env = env_map([(key, token)]);
        broker.virtualize_child_env(&mut env);
        assert_ne!(env[key], token);
        assert!(!env.contains_key("GH_HOST"));
        let mut headers = headers_with_bearer(&env[key]);
        broker.inject_request_headers(host, &mut headers);
        assert_eq!(
            authorization(&headers),
            Some(format!("Bearer {token}").as_str())
        );

        let snapshot_destination = if context_key == "GH_HOST" {
            "snapshot.example"
        } else {
            "https://snapshot.example/v1"
        };
        env.insert(context_key.to_string(), snapshot_destination.to_string());
        broker.virtualize_child_env(&mut env);
        env.remove(context_key);
        env.insert(
            "NEW_AUTH_HEADER".to_string(),
            format!("Bearer {}", env[key]),
        );
        env.insert("REAL_COPY".to_string(), token.to_string());
        let dummy = env[key].clone();
        broker.virtualize_child_env(&mut env);
        assert_eq!((&env[key], &env["REAL_COPY"]), (&dummy, &dummy));
        assert!(!env.contains_key(context_key));
        for destination in ["snapshot.example", host] {
            let mut headers = headers_with_bearer(&dummy);
            broker.inject_request_headers(destination, &mut headers);
            assert_eq!(
                authorization(&headers),
                Some(format!("Bearer {token}").as_str())
            );
        }
        let mut inherited_env = env.clone();
        broker.virtualize_child_env_for_environment(&mut inherited_env, Some("child"));
        assert_eq!(inherited_env[key], dummy);
        for (destination, injected) in [("snapshot.example", true), (host, false)] {
            let mut headers = headers_with_bearer(&dummy);
            broker.inject_request_headers_for_environment(destination, &mut headers, Some("child"));
            assert_eq!(
                headers,
                headers_with_bearer(if injected { token } else { &dummy }),
            );
        }
        broker.restore_child_env(&mut env, &mut []);
        assert_eq!(env["NEW_AUTH_HEADER"], format!("Bearer {token}"));
    }
}

#[test]
fn private_destination_updates_reconcile_registered_fallbacks() {
    for ((key, context_key, token), previously_used_fallback) in [
        ("GH_ENTERPRISE_TOKEN", "GH_HOST", "ghp-real"),
        (
            "VENDOR_TOKEN",
            "VENDOR_HOST",
            "vendor_abcdefghijklmnopqrstuvwx",
        ),
    ]
    .into_iter()
    .flat_map(|source| [(source, false), (source, true)])
    {
        let destination = |host: &str| {
            if context_key == "GH_HOST" {
                host.to_string()
            } else {
                format!("https://{host}")
            }
        };
        let broker = CredentialBroker::new(/*enabled*/ true);
        let mut config = NetworkProxyConfig {
            credential_broker: true,
            credential_providers: BTreeMap::from([(
                "vendor".to_string(),
                CredentialProviderConfig {
                    env: vec!["VENDOR_TOKEN".to_string()],
                    patterns: vec!["^vendor_[a-z]{24}$".to_string()],
                    url_prefix_from_env: Some("VENDOR_HOST".to_string()),
                    ..CredentialProviderConfig::default()
                },
            )]),
            ..NetworkProxyConfig::default()
        };
        config.configure_credential_broker_environment(&env_map([(
            context_key,
            &destination("first.example"),
        )]));
        broker.configure(&config);
        let mut env = env_map([(key, token)]);
        broker.virtualize_child_env(&mut env);
        let dummy = env[key].clone();
        assert_ne!(dummy, token);

        let mut explicit_env = env_map([(key, token)]);
        if previously_used_fallback {
            broker.virtualize_child_env_for_environment(&mut explicit_env, Some("explicit"));
        }
        explicit_env.insert(
            context_key.to_string(),
            destination("other-explicit.example"),
        );
        broker.virtualize_child_env_for_environment(&mut explicit_env, Some("explicit"));
        explicit_env.insert(context_key.to_string(), destination("explicit.example"));
        broker.virtualize_child_env_for_environment(&mut explicit_env, Some("explicit"));
        explicit_env.remove(context_key);
        let explicit_dummy = explicit_env[key].clone();
        let assert_inherited_destination = |child: &str| {
            let mut inherited_env = explicit_env.clone();
            inherited_env.insert("CREDENTIAL_COPY".to_string(), token.to_string());
            broker.virtualize_child_env_for_environment(&mut inherited_env, Some(child));
            assert_eq!(
                inherited_env["CREDENTIAL_COPY"], explicit_dummy,
                "{key}: {child}"
            );
            for host in ["explicit.example", "first.example", "second.example"] {
                let mut headers = headers_with_bearer(&explicit_dummy);
                broker.inject_request_headers_for_environment(
                    &format!("https://{host}/v1"),
                    &mut headers,
                    Some(child),
                );
                assert_eq!(
                    headers,
                    headers_with_bearer(if host == "explicit.example" {
                        token
                    } else {
                        &explicit_dummy
                    }),
                    "{key}: {child}, {host}"
                );
            }
        };
        let unrelated_key = if context_key == "GH_HOST" {
            "UNRELATED_HOST"
        } else {
            "GH_HOST"
        };
        config.configure_credential_broker_environment(&env_map([
            (context_key, &destination("first.example")),
            (unrelated_key, "https://unrelated.example"),
        ]));
        broker.configure(&config);
        assert_inherited_destination("unchanged-fallback-child");

        config.configure_credential_broker_environment(&env_map([(
            context_key,
            &destination("second.example"),
        )]));
        let revision = broker.config_revision();
        broker.configure(&config);
        assert_eq!(broker.config_revision(), revision + 1);
        assert_inherited_destination("updated-fallback-child");
        broker.virtualize_child_env(&mut env);
        broker.virtualize_child_env_for_environment(&mut explicit_env, Some("explicit"));
        assert_eq!(env[key], dummy);
        for (destination, environment_id, value, injected) in [
            ("first.example", None, &dummy, true),
            ("second.example", None, &dummy, true),
            ("explicit.example", Some("explicit"), &explicit_dummy, true),
            (
                "second.example",
                Some("explicit"),
                &explicit_dummy,
                previously_used_fallback,
            ),
        ] {
            let mut headers = headers_with_bearer(value);
            broker.inject_request_headers_for_environment(
                &format!("https://{destination}/v1"),
                &mut headers,
                environment_id,
            );
            assert_eq!(
                headers,
                headers_with_bearer(if injected { token } else { value }),
                "{key}: {destination}, {environment_id:?}"
            );
        }

        config.configure_credential_broker_environment(&env_map([(context_key, "")]));
        broker.configure(&config);
        for destination in ["first.example", "second.example"] {
            for (environment, value) in [(None, &dummy), (Some("explicit"), &explicit_dummy)] {
                let mut headers = headers_with_bearer(value);
                broker.inject_request_headers_for_environment(
                    &format!("https://{destination}/v1"),
                    &mut headers,
                    environment,
                );
                assert_eq!(headers, headers_with_bearer(value));
            }
        }
        let mut headers = headers_with_bearer(&explicit_dummy);
        broker.inject_request_headers_for_environment(
            "https://explicit.example/v1",
            &mut headers,
            Some("explicit"),
        );
        assert_eq!(headers, headers_with_bearer(token));
        let mut headers = headers_with_bearer(&explicit_dummy);
        broker.inject_request_headers_for_environment(
            "https://other-explicit.example/v1",
            &mut headers,
            Some("explicit"),
        );
        assert_eq!(headers, headers_with_bearer(token));

        // A fallback can become primary again without erasing captured destinations.
        config.configure_credential_broker_environment(&env_map([(
            context_key,
            &destination("third.example"),
        )]));
        broker.configure(&config);
        explicit_env.insert(context_key.to_string(), destination("third.example"));
        broker.virtualize_child_env_for_environment(&mut explicit_env, Some("explicit"));
        config.configure_credential_broker_environment(&env_map([(context_key, "")]));
        broker.configure(&config);
        for host in [
            "explicit.example",
            "other-explicit.example",
            "third.example",
        ] {
            let mut headers = headers_with_bearer(&explicit_dummy);
            broker.inject_request_headers_for_environment(
                &format!("https://{host}/v1"),
                &mut headers,
                Some("explicit"),
            );
            assert_eq!(
                headers,
                headers_with_bearer(if host == "third.example" {
                    &explicit_dummy
                } else {
                    token
                })
            );
        }
    }
}

#[test]
fn selected_alias_checks_equivalent_and_merged_credential_bindings() {
    let broker = CredentialBroker::new(/*enabled*/ true);
    let register = |env_var: &str, token: &str, host: &str| {
        let mut env = env_map([("GH_HOST", host), (env_var, token)]);
        broker.virtualize_child_env_for_environment(&mut env, Some("selected"));
        env.insert("GH_HOST".to_string(), "alias.example".to_string());
        broker.virtualize_child_env_for_environment(&mut env, Some("selected"));
        env[env_var].clone()
    };
    let first = register("GH_ENTERPRISE_TOKEN", "ghp-first-real", "alias.example");
    let equivalent = register(
        "GITHUB_ENTERPRISE_TOKEN",
        "ghp-first-real",
        "hooked.example",
    );
    let second = register(
        "GITHUB_ENTERPRISE_TOKEN",
        "ghp-second-real",
        "second-hooked.example",
    );
    assert_ne!(first, equivalent);

    assert!(broker.request_matches_hooked_host_alias(
        "alias.example",
        &headers_with_bearer(&equivalent),
        &["hooked.example".to_string()],
        Some("selected"),
    ));
    let basic = STANDARD.encode(format!("{first}:{second}"));
    let headers = headers_with_authorization(&format!("Basic {basic}"));
    assert!(broker.request_matches_hooked_host_alias(
        "alias.example",
        &headers,
        &["second-hooked.example".to_string()],
        Some("selected"),
    ));
    assert!(!broker.request_matches_hooked_host_alias(
        "alias.example",
        &headers,
        &["second-hooked.example".to_string()],
        Some("other"),
    ));
}

#[test]
fn github_cloud_credentials_match_ghe_com_host_hint() {
    let broker = CredentialBroker::new(/*enabled*/ true);
    let mut env = env_map([("GH_HOST", "astemu.ghe.com"), ("GH_TOKEN", "ghp-real")]);
    broker.virtualize_child_env(&mut env);
    assert!(!brokered_credential_binding_env_keys(&env).any(|key| key == "GH_HOST"));
    let github_token = env.get("GH_TOKEN").expect("dummy GitHub token");
    let mut headers = headers_with_bearer(github_token);

    broker.inject_request_headers("api.astemu.ghe.com", &mut headers);

    assert_eq!(authorization(&headers), Some("Bearer ghp-real"));
}

#[test]
fn github_cloud_credentials_do_not_bind_to_ghes_host_hint() {
    let broker = CredentialBroker::new(/*enabled*/ true);
    let mut env = env_map([("GH_HOST", "github.example.com"), ("GH_TOKEN", "ghp-real")]);
    broker.virtualize_child_env(&mut env);
    let github_token = env.get("GH_TOKEN").expect("dummy github token");
    let expected_authorization = format!("Bearer {github_token}");
    let mut headers = headers_with_bearer(github_token);

    broker.inject_request_headers("github.example.com", &mut headers);

    assert_eq!(
        authorization(&headers),
        Some(expected_authorization.as_str())
    );
    assert!(!broker.host_requires_mitm("github.example.com", /*port*/ 443));
    assert!(broker.host_requires_mitm("api.github.com", /*port*/ 443));
}

#[test]
fn github_enterprise_credentials_bind_to_gh_host() {
    for (hint, host) in [
        (" GitHub.Example.Com.:8443 ", "github.example.com"),
        ("127.0.0.1:8443", "127.0.0.1"),
        ("[::1]:8443", "::1"),
        ("::1", "::1"),
        ("[fe80::1%en0]:8443", "fe80::1%en0"),
        ("fe80::1%en0", "fe80::1%en0"),
    ] {
        let broker = CredentialBroker::new(/*enabled*/ true);
        let mut env = env_map([
            ("GH_HOST", hint),
            ("GH_ENTERPRISE_TOKEN", "ghp-enterprise-real"),
        ]);
        broker.virtualize_child_env(&mut env);
        let dummy = env["GH_ENTERPRISE_TOKEN"].clone();
        assert_ne!(dummy, "ghp-enterprise-real", "{hint}");
        env.remove("GH_HOST");
        env.insert(
            "GH_ENTERPRISE_TOKEN".to_string(),
            "ghp-enterprise-real".to_string(),
        );
        broker.virtualize_child_env(&mut env);
        let mut headers = headers_with_bearer(&dummy);
        broker.inject_request_headers(host, &mut headers);
        assert_eq!(
            authorization(&headers),
            Some("Bearer ghp-enterprise-real"),
            "{hint}"
        );
    }
    let broker = CredentialBroker::new(/*enabled*/ true);
    let mut env = env_map([
        ("GH_HOST", "github.example.com"),
        ("GH_TOKEN", "ghp-enterprise-real"),
        ("GH_ENTERPRISE_TOKEN", "ghp-enterprise-real"),
        ("AUTH_HEADER", "Bearer ghp-enterprise-real"),
    ]);
    broker.virtualize_child_env(&mut env);
    let github_dummy = env["GH_TOKEN"].clone();
    assert!(brokered_credential_env_keys(&env).any(|key| key == "GH_HOST"));
    assert!(brokered_credential_binding_env_keys(&env).any(|key| key == "GH_HOST"));
    let github_token = env
        .get("GH_ENTERPRISE_TOKEN")
        .expect("dummy GitHub enterprise token");
    assert_ne!(github_token, &github_dummy);
    let mut headers = headers_with_bearer(github_token);

    broker.inject_request_headers("github.example.com", &mut headers);

    assert_eq!(authorization(&headers), Some("Bearer ghp-enterprise-real"));
    assert_eq!(env["AUTH_HEADER"], format!("Bearer {github_token}"));
    let mut alias_headers = headers_with_authorization(&env["AUTH_HEADER"]);
    broker.inject_request_headers("github.example.com", &mut alias_headers);
    assert_eq!(
        authorization(&alias_headers),
        Some("Bearer ghp-enterprise-real")
    );
    let mut persisted_alias = "Bearer ghp-enterprise-real".to_string();
    assert!(broker.virtualize_text(&mut persisted_alias, &env));
    assert_eq!(persisted_alias, format!("Bearer {github_token}"));
    assert_eq!(
        brokered_credential_dummy_env_keys(&env).first(),
        Some(&"GH_ENTERPRISE_TOKEN".to_string())
    );
    let mut cloud_headers = headers_with_bearer(&github_dummy);
    broker.inject_request_headers("github.example.com", &mut cloud_headers);
    assert_eq!(cloud_headers, headers_with_bearer(&github_dummy));
    let mut enterprise_headers = headers_with_bearer(github_token);
    broker.inject_request_headers("api.github.com", &mut enterprise_headers);
    assert_eq!(enterprise_headers, headers_with_bearer(github_token));
    let mut cloud_only = env_map([
        ("GH_HOST", "github.example.com"),
        ("GH_TOKEN", "ghp-enterprise-real"),
        ("AUTH_HEADER", "Bearer ghp-enterprise-real"),
    ]);
    broker.virtualize_child_env(&mut cloud_only);
    assert_eq!(cloud_only["AUTH_HEADER"], format!("Bearer {github_dummy}"));
    assert!(broker.host_requires_mitm("github.example.com", /*port*/ 443));
    assert!(broker.host_requires_mitm("api.github.com", /*port*/ 443));

    env.insert("GH_HOST".to_string(), "attacker.example".to_string());
    env.insert("GH_ENTERPRISE_TOKEN".to_string(), github_dummy.clone());
    broker.virtualize_child_env(&mut env);
    let mut attacker_headers = headers_with_bearer(&github_dummy);
    broker.inject_request_headers("attacker.example", &mut attacker_headers);
    assert_eq!(attacker_headers, headers_with_bearer(&github_dummy));
    assert!(!broker.host_requires_mitm("attacker.example", /*port*/ 443));

    let mut alternate_enterprise_key = env_map([
        ("GH_HOST", "github.alternate.example"),
        ("GH_TOKEN", "ghp-alternate-real"),
        ("GITHUB_ENTERPRISE_TOKEN", "ghp-alternate-real"),
        ("AUTH_HEADER", "Bearer ghp-alternate-real"),
    ]);
    broker.virtualize_child_env(&mut alternate_enterprise_key);
    assert_eq!(
        alternate_enterprise_key["AUTH_HEADER"],
        format!(
            "Bearer {}",
            alternate_enterprise_key["GITHUB_ENTERPRISE_TOKEN"]
        )
    );
    assert_eq!(
        brokered_credential_dummy_env_keys(&alternate_enterprise_key).first(),
        Some(&"GITHUB_ENTERPRISE_TOKEN".to_string())
    );
}
