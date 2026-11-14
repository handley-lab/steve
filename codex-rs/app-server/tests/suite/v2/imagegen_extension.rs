use std::path::Path;
use std::time::Duration;

use anyhow::Context;
use anyhow::Result;
use app_test_support::MockResponsesConfig;
use app_test_support::TestAppServer;
use base64::Engine;
use codex_app_server_protocol::GeneratedImageRef;
use codex_app_server_protocol::ImageGenerationInputImage;
use codex_app_server_protocol::ImageGenerationItem;
use codex_app_server_protocol::ImageReference as V2ImageReference;
use codex_app_server_protocol::ItemCompletedNotification;
use codex_app_server_protocol::ThreadItemPayload;
use codex_app_server_protocol::ThreadStartParams;
use codex_app_server_protocol::ThreadStartResponse;
use codex_app_server_protocol::TurnStartParams;
use codex_app_server_protocol::TurnStartResponse;
use codex_app_server_protocol::UserInput as V2UserInput;
use codex_utils_absolute_path::AbsolutePathBuf;
use core_test_support::responses;
use core_test_support::skip_if_remote;
use pretty_assertions::assert_eq;
use serde_json::json;
use sha2::Digest;
use sha2::Sha256;
use tempfile::TempDir;
use test_case::test_case;
use tokio::time::timeout;
use wiremock::Mock;
use wiremock::MockServer;
use wiremock::ResponseTemplate;
use wiremock::matchers::method;
use wiremock::matchers::path;

use super::analytics::mount_analytics_capture;
use super::analytics::wait_for_analytics_event;

const RESULT: &str = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR4nGP4z8DwHwAFAAH/iZk9HQAAAABJRU5ErkJggg==";
// Two one-pixel frames, red then blue, with a 200 ms frame duration.
const ANIMATED_RESULT: &str = "R0lGODlhAQABAIEAAP8AAAAAAAAAAAAAACH/C05FVFNDQVBFMi4wAwEAAAAh+QQAFAAAACwAAAAAAQABAAAIBAABBAQAIfkEARQAAQAsAAAAAAEAAQCBAAD/AAAAAAAAAAAACAQAAQQEADs=";
const TINY_PNG_BYTES: &[u8] = &[
    137, 80, 78, 71, 13, 10, 26, 10, 0, 0, 0, 13, 73, 72, 68, 82, 0, 0, 0, 1, 0, 0, 0, 1, 8, 6, 0,
    0, 0, 31, 21, 196, 137, 0, 0, 0, 13, 73, 68, 65, 84, 120, 156, 99, 248, 207, 192, 240, 31, 0,
    5, 0, 1, 255, 137, 153, 61, 29, 0, 0, 0, 0, 73, 69, 78, 68, 174, 66, 96, 130,
];
const TINY_PNG_DATA_URL: &str = "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR4nGP4z8DwHwAFAAH/iZk9HQAAAABJRU5ErkJggg==";

// macOS and Windows Bazel CI can spend tens of seconds starting app-server
// subprocesses or processing test RPCs under load.
#[cfg(any(target_os = "macos", windows))]
const DEFAULT_READ_TIMEOUT: Duration = Duration::from_secs(60);
#[cfg(not(any(target_os = "macos", windows)))]
const DEFAULT_READ_TIMEOUT: Duration = Duration::from_secs(10);

#[test_case(Some(true), "transparent", false; "transparent")]
#[test_case(Some(false), "opaque", false; "opaque")]
#[test_case(None, "opaque", false; "omitted_defaults_to_opaque")]
#[test_case(None, "opaque", true; "gif")]
#[tokio::test]
async fn standalone_image_generation_returns_saved_path_hint_to_model(
    requested_transparent_background: Option<bool>,
    expected_background: &str,
    gif_enabled: bool,
) -> Result<()> {
    let call_id = "image-run-1";
    let (result, extension) = if gif_enabled {
        (ANIMATED_RESULT, "gif")
    } else {
        (RESULT, "png")
    };
    let server = responses::start_mock_server().await;
    Mock::given(method("POST"))
        .and(path("/api/codex/images/generations"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("x-codex-imagegen-request-id", "req-imagegen-123")
                .set_body_json(json!({
                    "created": 1,
                    "background": "opaque",
                    "data": [
                        {"b64_json": result, "generation_id": "gen-image-123"},
                        {"b64_json": "ignored", "generation_id": "gen-other"},
                    ],
                })),
        )
        .expect(1)
        .mount(&server)
        .await;
    let mut arguments = imagegen_arguments("paint a blue whale", requested_transparent_background);

    if gif_enabled {
        arguments["gif"] = json!({"frame_count": 2, "frame_duration_ms": 200});
    }

    let response_mock = responses::mount_sse_sequence(
        &server,
        vec![
            responses::sse(vec![
                responses::ev_response_created("resp-1"),
                responses::ev_function_call_with_namespace(
                    call_id,
                    "image_gen",
                    "imagegen",
                    &arguments.to_string(),
                ),
                responses::ev_completed("resp-1"),
            ]),
            responses::sse(vec![
                responses::ev_assistant_message("msg-1", "Done"),
                responses::ev_completed("resp-2"),
            ]),
        ],
    )
    .await;

    let codex_home = TempDir::new()?;
    create_config_toml(codex_home.path(), &server.uri())?;
    mount_analytics_capture(&server, codex_home.path()).await?;

    let mut mcp = TestAppServer::builder()
        .with_codex_home(codex_home.path())
        .with_env_overrides(&[("OPENAI_API_KEY", None)])
        .build_initialized_with_timeout(DEFAULT_READ_TIMEOUT)
        .await?;
    let mut config = std::collections::HashMap::from([(
        "responses_api_metadata".to_string(),
        json!({
            "display_name": "Image test 🚀",
        }),
    )]);
    if gif_enabled {
        config.insert(
            "features.image_generation_animation".to_string(),
            json!(true),
        );
    }
    let turn_id = start_turn(
        &mut mcp,
        ThreadStartParams {
            service_name: Some("chatgpt_cca".to_string()),
            config: Some(config),
            ..Default::default()
        },
        TurnStartParams {
            input: vec![V2UserInput::Text {
                text: "Generate an image".to_string(),
                text_elements: Vec::new(),
            }],
            turn_trigger: Some("page_prompt".to_string()),
            ..Default::default()
        },
    )
    .await?;

    let completed = timeout(
        DEFAULT_READ_TIMEOUT,
        wait_for_image_generation_completed(&mut mcp),
    )
    .await??;
    timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_notification_message("turn/completed"),
    )
    .await??;

    let ThreadItemPayload::ImageGeneration(ImageGenerationItem {
        status,
        revised_prompt,
        image,
        transparent_background,
        saved_path: Some(saved_path),
        generation_id,
        ..
    }) = completed.item.payload
    else {
        panic!("expected completed image generation item with saved path");
    };
    assert_eq!(status, "completed");
    assert_eq!(revised_prompt.as_deref(), Some("paint a blue whale"));
    assert_eq!(generation_id.as_deref(), Some("gen-image-123"));
    assert_eq!(
        image,
        GeneratedImageRef::Inline {
            result: result.to_string(),
        }
    );
    assert_eq!(transparent_background, Some(false));
    assert_eq!(
        std::fs::read(&saved_path)?,
        base64::engine::general_purpose::STANDARD.decode(result)?
    );
    assert_eq!(
        saved_path.extension().and_then(|ext| ext.to_str()),
        Some(extension)
    );

    let image_request = server
        .received_requests()
        .await
        .context("failed to fetch received requests")?
        .into_iter()
        .find(|request| request.url.path() == "/api/codex/images/generations")
        .context("image generation request should be sent")?;
    let mut expected_request = json!({
        "prompt": "paint a blue whale",
        "background": expected_background,
        "model": "gpt-image-2",
        "quality": "auto",
        "size": "auto",
    });
    if gif_enabled {
        expected_request["output_format"] = json!("gif");
        expected_request["frame_count"] = json!(2);
        expected_request["frame_duration_ms"] = json!(200);
    }
    assert_eq!(
        image_request.body_json::<serde_json::Value>()?,
        expected_request
    );
    assert_eq!(
        image_request
            .headers
            .get("originator")
            .context("standalone image generation should include the thread originator")?
            .to_str()
            .context("standalone image generation originator should be valid ASCII")?,
        "chatgpt_cca"
    );
    assert_image_turn_id_header(&image_request, &turn_id)?;
    let metadata = serde_json::from_str::<serde_json::Value>(
        image_request
            .headers
            .get("x-codex-turn-metadata")
            .context("image request should include the current turn metadata")?
            .to_str()?,
    )?;
    assert_eq!(metadata["display_name"], "Image test 🚀");
    assert_eq!(metadata["turn_trigger"], "page_prompt");

    let requests = response_mock.requests();
    assert_eq!(requests.len(), 2);
    let output = requests[1].function_call_output(call_id);
    if !gif_enabled {
        assert_eq!(
            output["output"][0],
            json!({
                "type": "input_image",
                "image_url": format!("data:image/png;base64,{RESULT}"),
                "detail": "high",
            })
        );
    }
    let output_hint = output["output"][1]["text"]
        .as_str()
        .context("image output should include model-visible path hint")?;
    assert!(
        output_hint.contains(&saved_path.display().to_string()),
        "output hint should identify the path the extension saved"
    );
    assert!(
        output_hint.contains("already displayed to the user"),
        "output hint should tell the model not to repeat the generated image: {output_hint}"
    );
    assert!(
        !requests[1]
            .message_input_texts("developer")
            .iter()
            .any(|text| text.contains("Generated images are saved to")),
        "standalone image generation should not emit the legacy developer-message hint"
    );

    let event = wait_for_analytics_event(
        &server,
        DEFAULT_READ_TIMEOUT,
        "codex_image_generation_event",
    )
    .await?;
    assert_eq!(
        event["event_params"]["imagegen_request_id"],
        json!("req-imagegen-123")
    );
    assert_eq!(
        event["event_params"]["generation_id"],
        json!("gen-image-123")
    );

    Ok(())
}

#[test_case(Some(true), "transparent"; "transparent")]
#[test_case(Some(false), "opaque"; "opaque")]
#[test_case(None, "opaque"; "omitted_defaults_to_opaque")]
#[tokio::test]
async fn standalone_image_edit_uses_attached_model_visible_image(
    requested_transparent_background: Option<bool>,
    expected_background: &str,
) -> Result<()> {
    skip_if_remote!(
        Ok(()),
        "remote executors use different imagegen storage approaches, so host-local image paths are unavailable"
    );

    let mut expected_input_path = None;
    let (edit_request, completed_image) = run_image_edit_test(|codex_home| {
        let image_path = codex_home.join("attached.png");
        std::fs::write(&image_path, TINY_PNG_BYTES)?;
        expected_input_path = Some(AbsolutePathBuf::from_absolute_path(&image_path)?);
        let mut arguments = imagegen_arguments("add a red hat", requested_transparent_background);
        arguments["referenced_image_paths"] = json!([image_path.display().to_string()]);
        Ok((
            arguments,
            vec![
                V2UserInput::Text {
                    text: "Edit the attached image".to_string(),
                    text_elements: Vec::new(),
                },
                V2UserInput::LocalImage {
                    path: image_path,
                    detail: None,
                },
            ],
        ))
    })
    .await?;
    assert_eq!(
        edit_request,
        json!({
            "prompt": "add a red hat",
            "images": [{"image_url": TINY_PNG_DATA_URL}],
            "background": expected_background,
            "model": "gpt-image-2",
            "quality": "auto",
            "size": "auto",
        })
    );

    assert_eq!(
        completed_image.input_images,
        vec![ImageGenerationInputImage {
            path: expected_input_path.expect("input path"),
            sha256: format!("{:x}", Sha256::digest(TINY_PNG_BYTES)),
        }]
    );

    Ok(())
}

#[test_case(Some(true), "transparent"; "transparent")]
#[test_case(Some(false), "opaque"; "opaque")]
#[test_case(None, "opaque"; "omitted_defaults_to_opaque")]
#[tokio::test]
async fn transparent_image_edit_preserves_metadata_and_recent_pathless_image(
    requested_transparent_background: Option<bool>,
    expected_background: &str,
) -> Result<()> {
    let image_url = TINY_PNG_DATA_URL;
    let (edit_request, completed_image) = run_image_edit_test(|_| {
        let mut arguments = imagegen_arguments("add a red hat", requested_transparent_background);
        arguments["num_last_images_to_include"] = json!(1);
        Ok((
            arguments,
            vec![
                V2UserInput::Text {
                    text: "Edit the attached image".to_string(),
                    text_elements: Vec::new(),
                },
                V2UserInput::Image {
                    image: V2ImageReference::Inline {
                        url: image_url.to_string(),
                    },
                    detail: None,
                },
            ],
        ))
    })
    .await?;
    assert_eq!(
        edit_request,
        json!({
            "prompt": "add a red hat",
            "images": [{"image_url": image_url}],
            "background": expected_background,
            "model": "gpt-image-2",
            "quality": "auto",
            "size": "auto",
        })
    );
    assert_eq!(completed_image.transparent_background, Some(true));

    Ok(())
}

fn imagegen_arguments(prompt: &str, transparent_background: Option<bool>) -> serde_json::Value {
    let mut arguments = json!({"prompt": prompt});
    if let Some(transparent_background) = transparent_background {
        arguments["transparent_background"] = json!(transparent_background);
    }
    arguments
}

async fn run_image_edit_test(
    input: impl FnOnce(&Path) -> Result<(serde_json::Value, Vec<V2UserInput>)>,
) -> Result<(serde_json::Value, ImageGenerationItem)> {
    let call_id = "image-edit-1";
    let server = responses::start_mock_server().await;
    mount_image_edit_response(&server).await;

    let codex_home = TempDir::new()?;
    let (arguments, input) = input(codex_home.path())?;
    let response_mock = responses::mount_sse_sequence(
        &server,
        vec![
            responses::sse(vec![
                responses::ev_response_created("resp-1"),
                responses::ev_function_call_with_namespace(
                    call_id,
                    "image_gen",
                    "imagegen",
                    &arguments.to_string(),
                ),
                responses::ev_completed("resp-1"),
            ]),
            responses::sse(vec![
                responses::ev_assistant_message("msg-1", "Done"),
                responses::ev_completed("resp-2"),
            ]),
        ],
    )
    .await;

    create_config_toml(codex_home.path(), &server.uri())?;

    let mut mcp = TestAppServer::builder()
        .with_codex_home(codex_home.path())
        .with_env_overrides(&[("OPENAI_API_KEY", None)])
        .build_initialized_with_timeout(DEFAULT_READ_TIMEOUT)
        .await?;
    let turn_id = start_turn(
        &mut mcp,
        ThreadStartParams {
            service_name: Some("chatgpt_cca".to_string()),
            config: Some(
                [(
                    "responses_api_metadata".to_string(),
                    json!({
                        "display_name": "Image edit test",
                    }),
                )]
                .into(),
            ),
            ..Default::default()
        },
        TurnStartParams {
            input,
            turn_trigger: Some("page_prompt".to_string()),
            ..Default::default()
        },
    )
    .await?;
    let completed = timeout(
        DEFAULT_READ_TIMEOUT,
        wait_for_image_generation_completed(&mut mcp),
    )
    .await??;
    let ThreadItemPayload::ImageGeneration(completed_image) = completed.item.payload else {
        panic!("expected completed image-generation item");
    };
    timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_notification_message("turn/completed"),
    )
    .await??;

    assert_eq!(response_mock.requests().len(), 2);
    let requests = server
        .received_requests()
        .await
        .context("failed to fetch received requests")?;
    let image_request = requests
        .iter()
        .find(|request| request.url.path() == "/api/codex/images/edits")
        .context("image edit request should be sent")?;
    assert_eq!(
        image_request
            .headers
            .get("originator")
            .context("standalone image edit should include the thread originator")?
            .to_str()
            .context("standalone image edit originator should be valid ASCII")?,
        "chatgpt_cca"
    );
    assert_image_turn_id_header(image_request, &turn_id)?;
    let metadata = serde_json::from_str::<serde_json::Value>(
        image_request
            .headers
            .get("x-codex-turn-metadata")
            .context("image edit request should include the current turn metadata")?
            .to_str()?,
    )?;
    assert_eq!(metadata["display_name"], "Image edit test");
    assert_eq!(metadata["turn_trigger"], "page_prompt");
    Ok((
        image_request.body_json::<serde_json::Value>()?,
        completed_image,
    ))
}

fn assert_image_turn_id_header(request: &wiremock::Request, expected_turn_id: &str) -> Result<()> {
    let turn_id = request
        .headers
        .get("x-codex-image-turn-id")
        .context("image request should include the current turn id")?
        .to_str()
        .context("image turn id should be valid ASCII")?;
    uuid::Uuid::parse_str(turn_id).context("image turn id should be a UUID")?;
    assert_eq!(turn_id, expected_turn_id);
    Ok(())
}

async fn start_turn(
    mcp: &mut TestAppServer,
    thread_start_params: ThreadStartParams,
    turn_start_params: TurnStartParams,
) -> Result<String> {
    let thread_req = mcp
        .send_thread_start_request_with_auto_env(thread_start_params)
        .await?;
    let ThreadStartResponse { thread, .. } =
        timeout(DEFAULT_READ_TIMEOUT, mcp.read_response(thread_req)).await??;

    let turn_req = mcp
        .send_turn_start_request(TurnStartParams {
            thread_id: thread.id,
            ..turn_start_params
        })
        .await?;
    let TurnStartResponse { turn } =
        timeout(DEFAULT_READ_TIMEOUT, mcp.read_response(turn_req)).await??;

    Ok(turn.id)
}

async fn wait_for_image_generation_completed(
    mcp: &mut TestAppServer,
) -> Result<ItemCompletedNotification> {
    loop {
        let completed: ItemCompletedNotification = mcp.read_notification("item/completed").await?;
        if matches!(
            &completed.item.payload,
            ThreadItemPayload::ImageGeneration(_)
        ) {
            return Ok(completed);
        }
    }
}

async fn mount_image_edit_response(server: &MockServer) {
    Mock::given(method("POST"))
        .and(path("/api/codex/images/edits"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "created": 1,
            "background": "transparent",
            "data": [{"b64_json": RESULT, "generation_id": "gen-transparent"}],
        })))
        .expect(1)
        .mount(server)
        .await;
}

fn create_config_toml(codex_home: &Path, server_uri: &str) -> std::io::Result<()> {
    MockResponsesConfig::new(server_uri)
        .with_model_provider("openai-custom")
        .with_provider_name("OpenAI")
        .with_provider_base_url(&format!("{server_uri}/api/codex"))
        .with_root_config(&format!("chatgpt_base_url = \"{server_uri}\""))
        .with_provider_config("supports_websockets = false\nrequires_openai_auth = true")
        .write(codex_home)
}
