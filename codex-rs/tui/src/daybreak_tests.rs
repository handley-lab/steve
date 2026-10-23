use super::*;
use crate::history_cell::HistoryCell;
use codex_protocol::openai_models::ModelAccessPrograms;
use pretty_assertions::assert_eq;

#[test]
fn turn_program_uses_catalog_and_preserves_legacy_models() {
    let mut model = crate::test_support::TEST_MODEL_PRESETS[0].clone();
    model.available_access_programs = Some(ModelAccessPrograms {
        cyber: vec![
            CyberAccessProgram::Standard,
            CyberAccessProgram::DaybreakBlue,
        ],
    });
    let models = vec![model.clone()];
    for (name, enabled, expected) in [
        (
            model.model.as_str(),
            true,
            Some(CyberAccessProgram::DaybreakBlue),
        ),
        (
            model.model.as_str(),
            false,
            Some(CyberAccessProgram::Standard),
        ),
        ("unlisted-model", false, None),
        ("unlisted-model", true, None),
    ] {
        assert_eq!(
            program_for_turn(&models, name, Some(true), enabled),
            Ok(expected)
        );
    }
    assert!(program_for_turn(&models, &model.model, Some(false), /*enabled*/ true).is_err());
    assert_eq!(
        program_for_turn(&models, &model.model, Some(false), false),
        Ok(None)
    );
    for enabled in [false, true] {
        let error = program_for_turn(&models, &model.model, None, enabled).unwrap_err();
        assert!(error.contains("could not be determined"));
        assert!(!error.contains("disabled for this session"));
    }
    for (programs, rejects) in [
        (None, false),
        (Some(vec![]), false),
        (Some(vec![CyberAccessProgram::Standard]), true),
    ] {
        model.available_access_programs = programs.map(|cyber| ModelAccessPrograms { cyber });
        let result = program_for_turn(
            std::slice::from_ref(&model),
            &model.model,
            Some(true),
            /*enabled*/ true,
        );
        if rejects {
            assert!(result.is_err());
        } else {
            assert_eq!(result, Ok(None));
        }
    }
}

#[test]
fn refusal_guidance_uses_the_model_catalog() {
    let mut model = crate::test_support::TEST_MODEL_PRESETS[0].clone();
    model.available_access_programs = Some(ModelAccessPrograms {
        cyber: vec![CyberAccessProgram::Standard],
    });
    for (name, expected) in [
        ("gpt-5.6-sol", Notice::Unavailable),
        ("gpt-6-astra", Notice::Unavailable),
        ("gpt-6-astra-wm", Notice::Unavailable),
    ] {
        model.model = name.into();
        assert_eq!(
            notice_for_setting(
                std::slice::from_ref(&model),
                name,
                /*enabled*/ false,
                /*can_enable_daybreak*/ true,
            ),
            expected
        );
    }
    model
        .available_access_programs
        .as_mut()
        .unwrap()
        .cyber
        .push(CyberAccessProgram::DaybreakBlue);
    assert_eq!(
        notice_for_setting(
            std::slice::from_ref(&model),
            &model.model,
            /*enabled*/ false,
            /*can_enable_daybreak*/ true,
        ),
        Notice::Disabled
    );
    assert_eq!(
        notice_for_setting(
            std::slice::from_ref(&model),
            &model.model,
            /*enabled*/ true,
            /*can_enable_daybreak*/ true,
        ),
        Notice::Enabled
    );
    model.available_access_programs.as_mut().unwrap().cyber =
        vec![CyberAccessProgram::DaybreakBlue];
    assert_eq!(
        notice_for_setting(
            std::slice::from_ref(&model),
            &model.model,
            /*enabled*/ false,
            /*can_enable_daybreak*/ true
        ),
        Notice::Disabled
    );
    let other = model.clone();
    model.model = "gpt-5.6-sol".into();
    model.available_access_programs.as_mut().unwrap().cyber = vec![CyberAccessProgram::Standard];
    assert_eq!(
        notice_for_setting(
            &[model.clone(), other],
            &model.model,
            /*enabled*/ false,
            /*can_enable_daybreak*/ true
        ),
        Notice::Unavailable
    );
    assert_eq!(
        notice_for_setting(
            &[],
            "gpt-6-astra",
            /*enabled*/ false,
            /*can_enable_daybreak*/ true
        ),
        Notice::Undetermined
    );
}

#[test]
fn provider_neutral_refusal_describes_standard_without_toggle_eligibility() {
    let mut model = crate::test_support::TEST_MODEL_PRESETS[0].clone();
    model.model = "provider-model".into();
    model.available_access_programs = Some(ModelAccessPrograms {
        cyber: vec![CyberAccessProgram::Standard],
    });
    for can_enable in [false, true] {
        let notice = notice_for_setting(
            std::slice::from_ref(&model),
            &model.model,
            false,
            can_enable,
        );
        let cell = crate::history_cell::new_cyber_policy_error_event(notice);
        let text = cell
            .display_lines(120)
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join(" ");
        assert!(
            text.contains("does not advertise a Daybreak program"),
            "{text}"
        );
    }
}

#[test]
fn provider_neutral_refusal_describes_advertised_program_without_toggle() {
    let mut model = crate::test_support::TEST_MODEL_PRESETS[0].clone();
    model.available_access_programs = Some(ModelAccessPrograms {
        cyber: vec![CyberAccessProgram::DaybreakBlue],
    });
    let notice = notice_for_setting(std::slice::from_ref(&model), &model.model, false, false);
    let cell = crate::history_cell::new_cyber_policy_error_event(notice);
    let text = cell
        .display_lines(120)
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(" ");
    assert!(text.contains("advertises a Daybreak program"), "{text}");
    assert!(!text.contains("turn it on"), "{text}");
}

#[test]
fn provider_neutral_refusal_uses_metadata_for_every_model_name() {
    let mut model = crate::test_support::TEST_MODEL_PRESETS[0].clone();
    model.model = "provider-model".into();
    model.available_access_programs = Some(ModelAccessPrograms {
        cyber: vec![CyberAccessProgram::Standard],
    });
    for can_enable in [true, false] {
        let expected = notice_for_setting(
            std::slice::from_ref(&model),
            &model.model,
            false,
            can_enable,
        );
        for name in ["gpt-6-astra", "gpt-6-astra-wm", "another-model"] {
            model.model = name.into();
            assert_eq!(
                notice_for_setting(std::slice::from_ref(&model), name, false, can_enable),
                expected
            );
        }
    }
}

#[tokio::test]
async fn daybreak_queries_the_target_and_preserves_off_unknown_and_errors() {
    use codex_app_server_client::RemoteAppServerClient;
    use codex_app_server_client::RemoteAppServerConnectArgs;
    use codex_app_server_client::RemoteAppServerEndpoint;
    use codex_app_server_protocol::JSONRPCMessage;
    use futures::SinkExt;
    use futures::StreamExt;
    use serde_json::json;
    use tokio_tungstenite::tungstenite::Message;

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}", listener.local_addr().unwrap());
    let thread = codex_protocol::ThreadId::new().to_string();
    let expected_thread = thread.clone();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
        let mut captures = Vec::new();
        let mut responses = [
            Some(true),
            Some(true),
            Some(false),
            Some(false),
            None,
            None,
            None,
        ]
        .into_iter();
        while let Some(Ok(Message::Text(text))) = socket.next().await {
            let JSONRPCMessage::Request(request) = serde_json::from_str(&text).unwrap() else {
                continue;
            };
            let response = if request.method == "initialize" {
                json!({"id": request.id, "result": {"userAgent": "daybreak-test"}})
            } else {
                assert_eq!(request.method, "model/list");
                assert_eq!(
                    request.params,
                    Some(json!({"threadId": expected_thread,
                    "cursor": null, "limit": null, "includeHidden": true}))
                );
                if let Some(enabled) = responses.next() {
                    let mut result = json!({"data": [{"id": "custom", "model": "custom",
                        "displayName": "Custom", "description": "Custom provider",
                        "hidden": false, "supportedReasoningEfforts": [],
                        "defaultReasoningEffort": "low", "isDefault": true,
                        "availableAccessPrograms": {"cyber": ["standard", "daybreakBlue"]}}],
                        "nextCursor": null});
                    if captures.len() == 6 {
                        result["data"][0]["availableAccessPrograms"] = json!(null);
                    }
                    if let Some(enabled) = enabled {
                        result["cyberAccessProgramsEnabled"] = json!(enabled);
                    }
                    json!({"id": request.id, "result": result})
                } else {
                    json!({"id": request.id, "error": {"code": -32603, "message": "catalog failed"}})
                }
            };
            if request.method != "initialize" {
                captures.push(json!({"request": serde_json::from_str::<serde_json::Value>(&text).unwrap(), "response": response}));
            }
            socket
                .send(Message::Text(response.to_string().into()))
                .await
                .unwrap();
        }
        captures
    });
    let client = RemoteAppServerClient::connect(RemoteAppServerConnectArgs {
        endpoint: RemoteAppServerEndpoint::WebSocket {
            websocket_url: url,
            auth_token: None,
        },
        client_name: "daybreak-test".into(),
        client_version: "0.0.0".into(),
        experimental_api: true,
        mcp_server_openai_form_elicitation: false,
        opt_out_notification_methods: Vec::new(),
        channel_capacity: 8,
    })
    .await
    .unwrap();
    let handle = AppServerRequestHandle::Remote(client.request_handle());
    let mut features = Features::with_defaults();
    features.set_enabled(Feature::CliDaybreak, false);
    assert_eq!(
        program_for_thread(&features, &handle, "invalid", "custom", true).await,
        Ok(None)
    );
    features.set_enabled(Feature::CliDaybreak, true);
    for (preference, expected) in [
        (false, Some(CyberAccessProgram::Standard)),
        (true, Some(CyberAccessProgram::DaybreakBlue)),
        (false, None),
    ] {
        assert_eq!(
            program_for_thread(&features, &handle, &thread, "custom", preference).await,
            Ok(expected)
        );
    }
    assert!(
        program_for_thread(&features, &handle, &thread, "custom", true)
            .await
            .unwrap_err()
            .contains("disabled for this session")
    );
    for preference in [false, true] {
        assert!(
            program_for_thread(&features, &handle, &thread, "custom", preference)
                .await
                .unwrap_err()
                .contains("could not be determined")
        );
    }
    assert_eq!(
        program_for_thread(&features, &handle, &thread, "custom", true).await,
        Ok(None)
    );
    assert!(
        program_for_thread(&features, &handle, &thread, "custom", true)
            .await
            .unwrap_err()
            .contains("catalog failed")
    );
    drop(handle);
    client.shutdown().await.unwrap();
    let captures = server.await.unwrap();
    if let Some(path) = std::env::var_os("STEVE_TEST_DAYBREAK_RPC_CAPTURE") {
        std::fs::write(path, serde_json::to_vec_pretty(&captures).unwrap()).unwrap();
    }
    assert_eq!(captures.len(), 8);
}
