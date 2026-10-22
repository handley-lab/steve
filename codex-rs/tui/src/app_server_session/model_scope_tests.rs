//! Catalog RPCs must use the live thread's provider and credential scope.

use super::*;
use crate::legacy_core::config::ConfigBuilder;
use codex_config::LoaderOverrides;
use codex_protocol::openai_models::ModelAccessPrograms;
use codex_protocol::openai_models::ModelVisibility;
use codex_protocol::openai_models::ModelsResponse;
use codex_protocol::turn_input::CyberAccessProgram;
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;
use serde_json::json;
use wiremock::Mock;
use wiremock::MockServer;
use wiremock::ResponseTemplate;
use wiremock::matchers::header;
use wiremock::matchers::method;
use wiremock::matchers::path;

async fn recorded_request<T: DeserializeOwned + Serialize>(
    client: &AppServerClient,
    next_id: &mut i64,
    method: &str,
    params: Value,
    trace: &mut Vec<Value>,
) -> Result<T> {
    let raw = json!({"id": *next_id, "method": method, "params": params});
    *next_id += 1;
    let response = client
        .request_typed::<T>(serde_json::from_value(raw.clone())?)
        .await;
    trace.push(json!({
        "request": raw,
        "response": match &response {
            Ok(response) => serde_json::to_value(response)?,
            Err(error) => json!({"error": format!("{error:?}")}),
        },
    }));
    if let Some(destination) = std::env::var_os("STEVE_TEST_MODEL_SCOPE_CAPTURE") {
        std::fs::write(
            destination,
            serde_json::to_vec_pretty(&json!({"rpc": trace}))?,
        )?;
    }
    Ok(response?)
}

#[tokio::test]
async fn model_scope_rpc_uses_the_thread_catalog_and_gateway() -> Result<()> {
    let a = MockServer::start().await;
    let b = MockServer::start().await;
    let oauth = MockServer::start().await;
    let catalog = |name: &str, program| {
        let mut visible = codex_models_manager::bundled_models_response()
            .expect("bundled catalog")
            .models
            .remove(0);
        visible.slug = "scope-model".into();
        visible.display_name = name.into();
        visible.visibility = ModelVisibility::List;
        visible.supported_in_api = false;
        visible.priority = 0;
        visible.available_access_programs = Some(ModelAccessPrograms {
            cyber: vec![program],
        });
        let mut hidden = visible.clone();
        hidden.slug = "scope-hidden".into();
        hidden.display_name = format!("Hidden {name}");
        hidden.visibility = ModelVisibility::Hide;
        hidden.priority = 1;
        ModelsResponse {
            models: vec![visible, hidden],
        }
    };
    for (server, name, credential, program) in [
        (
            &a,
            "Catalog A",
            "credential-a",
            CyberAccessProgram::DaybreakBlue,
        ),
        (
            &b,
            "Catalog B",
            "credential-b",
            CyberAccessProgram::DaybreakRed,
        ),
    ] {
        Mock::given(method("GET"))
            .and(path("/models"))
            .and(header("authorization", format!("Bearer {credential}")))
            .respond_with(ResponseTemplate::new(200).set_body_json(catalog(name, program)))
            .mount(server)
            .await;
    }
    let home = tempfile::tempdir()?;
    let requirements = home.path().join("requirements.toml");
    std::fs::write(&requirements, "")?;
    std::fs::write(
        home.path().join("config.toml"),
        format!(
            r#"model_provider = "scope"
model = "scope-model"
[features]
api_key_model_discovery = true
guardian_approval = false
shell_snapshot = false
[model_providers.scope]
name = "Scope provider"
base_url = "{a}/v1"
model_catalog_url = "{a}/models"
experimental_bearer_token = "credential-a"
[model_providers.alias]
name = "Scope provider"
base_url = "{a}/v1"
model_catalog_url = "{a}/models"
experimental_bearer_token = "credential-a"
"#,
            a = a.uri(),
        ),
    )?;
    let mut overrides = LoaderOverrides::without_managed_config_for_tests();
    overrides.system_requirements_path = Some(requirements.clone());
    overrides.system_config_path = Some(home.path().join("system.toml"));
    overrides.managed_config_path = Some(home.path().join("managed.toml"));
    let config = ConfigBuilder::default()
        .codex_home(home.path().to_path_buf())
        .loader_overrides(overrides.clone())
        .build()
        .await?;
    let state_db =
        crate::init_state_db_for_app_server_target(&config, &crate::AppServerTarget::Embedded)
            .await?;
    let client = crate::start_embedded_app_server(
        crate::Arg0DispatchPaths::default(),
        config,
        Vec::new(),
        overrides,
        false,
        codex_config::CloudConfigBundleLoader::default(),
        codex_feedback::CodexFeedback::new(),
        None,
        state_db,
        Arc::new(crate::EnvironmentManager::default_for_tests()),
        Default::default(),
    )
    .await?;
    let session = AppServerSession::new(
        AppServerClient::InProcess(client),
        ThreadParamsMode::Embedded,
    );
    let mut next_id = 100;
    let mut trace = Vec::new();
    let mut failures = Vec::new();
    let list = json!({"includeHidden": true});
    let process: ModelListResponse = recorded_request(
        &session.client,
        &mut next_id,
        "model/list",
        list.clone(),
        &mut trace,
    )
    .await?;
    assert_eq!(process.data[0].display_name, "Catalog A");
    let provider_b = json!({
        "name": "Scope provider", "base_url": format!("{}/v1", b.uri()),
        "model_catalog_url": format!("{}/models", b.uri()),
        "experimental_bearer_token": "credential-b",
    });
    let local_path = home.path().join("local-models.json");
    std::fs::write(
        &local_path,
        serde_json::to_vec(&catalog("Local", CyberAccessProgram::Standard))?,
    )?;
    let mut selected_b = None;
    for (label, provider, settings, expected) in [
        (
            "same id, different definition",
            "scope",
            json!({"model_providers": {"scope": provider_b.clone()}}),
            "Catalog B",
        ),
        ("equal definition, alias", "alias", json!({}), "Catalog A"),
        (
            "local catalog",
            "scope",
            json!({"model_catalog_json": local_path}),
            "Local",
        ),
    ] {
        let started: ThreadStartResponse = recorded_request(
            &session.client, &mut next_id, "thread/start",
            json!({"model": "scope-model", "modelProvider": provider, "cwd": home.path(), "config": settings}),
            &mut trace,
        ).await?;
        let a_before = a.received_requests().await.unwrap().len();
        let response: ModelListResponse = recorded_request(
            &session.client,
            &mut next_id,
            "model/list",
            json!({"threadId": started.thread.id, "includeHidden": true}),
            &mut trace,
        )
        .await?;
        if response
            .data
            .first()
            .map(|model| model.display_name.as_str())
            != Some(expected)
        {
            failures.push(format!(
                "{label}: expected {expected}, received {:?}",
                response.data
            ));
        }
        let expected_program = match expected {
            "Catalog B" => CyberAccessProgram::DaybreakRed,
            "Local" => CyberAccessProgram::Standard,
            _ => CyberAccessProgram::DaybreakBlue,
        };
        let programs = response
            .data
            .first()
            .and_then(|model| model.available_access_programs.clone())
            .map(ModelAccessPrograms::from);
        if !programs.is_some_and(|programs| programs.cyber == vec![expected_program]) {
            failures.push(format!("{label}: wrong advertised program"));
        }
        let a_after = a.received_requests().await.unwrap().len();
        if expected != "Catalog A" && a_after != a_before {
            failures.push(format!(
                "{label}: process requests changed from {a_before} to {a_after}"
            ));
        }
        if expected == "Catalog B" {
            selected_b = Some(started.thread.id);
        }
    }
    let selected_b = selected_b.expect("B thread");
    for (cursor, hidden, expected, next_cursor) in [
        (None, true, "Catalog B", Some("1")),
        (Some("1"), true, "Hidden Catalog B", None),
        (None, false, "Catalog B", None),
    ] {
        let response: ModelListResponse = recorded_request(
            &session.client,
            &mut next_id,
            "model/list",
            json!({"threadId": selected_b, "cursor": cursor, "limit": 1, "includeHidden": hidden}),
            &mut trace,
        )
        .await?;
        if response
            .data
            .first()
            .map(|model| model.display_name.as_str())
            != Some(expected)
            || response.next_cursor.as_deref() != next_cursor
        {
            failures.push(format!(
                "scoped page: expected {expected}/{next_cursor:?}, received {response:?}"
            ));
        }
    }
    for id in ["not-a-thread".to_string(), ThreadId::new().to_string()] {
        if recorded_request::<ModelListResponse>(
            &session.client,
            &mut next_id,
            "model/list",
            json!({"threadId": id}),
            &mut trace,
        )
        .await
        .is_ok()
        {
            failures.push(format!(
                "invalid or unknown thread {id} listed the process catalog"
            ));
        }
    }
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let refresh_id = uuid::Uuid::new_v4();
    session.fetch_models(
        refresh_id,
        Some(ThreadId::from_string(&selected_b)?),
        crate::app_event_sender::AppEventSender::new(tx),
    );
    let event = tokio::time::timeout(std::time::Duration::from_secs(10), rx.recv())
        .await?
        .expect("model refresh response");
    let crate::app_event::AppEvent::ModelsLoaded { request_id, result } = event else {
        panic!("expected model refresh response");
    };
    assert_eq!(request_id, refresh_id);
    trace.push(json!({"refresh_thread": selected_b, "result": format!("{result:?}")}));
    if !result.as_ref().is_ok_and(|models| {
        models
            .first()
            .is_some_and(|model| model.display_name == "Catalog B")
    }) {
        failures.push(format!("picker refresh used another catalog: {result:?}"));
    }
    // A controlled unreadable credential file fails before keyring access or browser login.
    let blocked_store = home.path().join("secrets/gateway_oauth.age");
    std::fs::create_dir_all(&blocked_store)?;
    assert!(std::fs::read(&blocked_store).is_err());
    let mut gateway_b = provider_b;
    gateway_b["gateway_oauth"] = json!({
        "authorization_url": format!("{}/authorize", oauth.uri()),
        "token_url": format!("{}/token", oauth.uri()), "client_id": "scope-test",
        "delivery": {"kind": "header", "name": "X-Gateway-Authorization"},
    });
    let started: ThreadStartResponse = recorded_request(
        &session.client, &mut next_id, "thread/start",
        json!({"model": "scope-model", "modelProvider": "scope", "cwd": home.path(), "config": {"model_providers": {"scope": gateway_b}}}),
        &mut trace,
    ).await?;
    let gateway = recorded_request::<ModelListResponse>(
        &session.client,
        &mut next_id,
        "model/list",
        json!({"threadId": started.thread.id}),
        &mut trace,
    )
    .await;
    if !gateway.as_ref().is_err_and(|error| {
        error
            .to_string()
            .contains("Gateway sign-in required or unavailable")
    }) {
        failures.push(format!("selected gateway readiness: {gateway:?}"));
    }
    if !oauth.received_requests().await.unwrap().is_empty() {
        failures.push("gateway failure contacted an OAuth endpoint".into());
    }
    let before_a = a.received_requests().await.unwrap().len();
    let before_b = b.received_requests().await.unwrap().len();
    std::fs::write(&requirements, "model_provider = \"alias\"\n")?;
    if recorded_request::<ModelListResponse>(
        &session.client,
        &mut next_id,
        "model/list",
        json!({"threadId": selected_b}),
        &mut trace,
    )
    .await
    .is_ok()
    {
        failures.push("managed rejection returned a warm scoped catalog".into());
    }
    let rejected_gateway = recorded_request::<ModelListResponse>(
        &session.client,
        &mut next_id,
        "model/list",
        json!({"threadId": started.thread.id}),
        &mut trace,
    )
    .await;
    if !rejected_gateway.as_ref().is_err_and(|error| {
        error
            .to_string()
            .contains("required model provider settings changed")
    }) {
        failures.push(format!(
            "gateway preceded managed rejection: {rejected_gateway:?}"
        ));
    }
    if a.received_requests().await.unwrap().len() != before_a
        || b.received_requests().await.unwrap().len() != before_b
    {
        failures.push("managed rejection issued a catalog request".into());
    }
    if let Some(destination) = std::env::var_os("STEVE_TEST_MODEL_SCOPE_CAPTURE") {
        let mut captures = Vec::new();
        for (name, server) in [("A", &a), ("B", &b), ("OAuth", &oauth)] {
            for request in server.received_requests().await.unwrap() {
                captures.push(json!({"server": name, "url": request.url.to_string(),
                    "method": request.method.to_string(),
                    "headers": request.headers.iter().map(|(name, value)| (name.to_string(), String::from_utf8_lossy(value.as_bytes()).into_owned())).collect::<std::collections::BTreeMap<_, _>>(),
                    "body": String::from_utf8_lossy(&request.body),
                }));
            }
        }
        std::fs::write(
            destination,
            serde_json::to_vec_pretty(
                &json!({"rpc": trace, "http": captures, "failures": failures}),
            )?,
        )?;
    }
    session.shutdown().await?;
    assert!(failures.is_empty(), "{failures:#?}");
    Ok(())
}
