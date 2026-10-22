//! Thread-scoped Daybreak discovery and per-turn program selection.
//! Pending or failed discovery never implies access.

use codex_app_server_client::AppServerRequestHandle;
use codex_app_server_protocol::ClientRequest;
use codex_app_server_protocol::ModelListParams;
use codex_app_server_protocol::ModelListResponse;
use codex_app_server_protocol::RequestId;
use codex_features::Feature;
use codex_features::Features;
use codex_protocol::openai_models::ModelPreset;
use codex_protocol::turn_input::CyberAccessProgram;

pub(crate) async fn program_for_thread(
    features: &Features,
    handle: &AppServerRequestHandle,
    thread_id: &str,
    model: &str,
    enabled: bool,
) -> Result<Option<CyberAccessProgram>, String> {
    if !features.enabled(Feature::CliDaybreak) {
        return Ok(None);
    }
    let response = handle
        .request_typed::<ModelListResponse>(ClientRequest::ModelList {
            request_id: RequestId::String(format!("tui-daybreak-{}", uuid::Uuid::new_v4())),
            params: ModelListParams {
                thread_id: Some(thread_id.to_string()),
                cursor: None,
                limit: None,
                include_hidden: Some(true),
            },
        })
        .await
        .map_err(|error| error.to_string())?;
    let models = response
        .data
        .into_iter()
        .map(crate::app_server_session::model_preset_from_api_model)
        .collect::<Vec<_>>();
    program_for_turn(
        &models,
        model,
        response.cyber_access_programs_enabled,
        enabled,
    )
}

pub(crate) fn program_for_turn(
    models: &[ModelPreset],
    model: &str,
    forwarding_enabled: Option<bool>,
    enabled: bool,
) -> Result<Option<CyberAccessProgram>, String> {
    let programs = models
        .iter()
        .find(|entry| entry.model == model)
        .and_then(|entry| entry.available_access_programs.as_ref())
        .filter(|programs| !programs.cyber.is_empty());
    let Some(programs) = programs else {
        // Missing discovery metadata preserves the server's automatic treatment.
        return Ok(None);
    };
    let program = if enabled {
        programs.daybreak().ok_or_else(|| format!("Daybreak support for model {model} could not be confirmed by the connected server. Use /daybreak to turn it off, or choose a compatible model and server."))?
    } else {
        let Some(standard) = programs.standard() else {
            return Ok(None);
        };
        standard
    };
    let forwarding_enabled = forwarding_enabled.ok_or_else(|| {
        "Cyber access program forwarding could not be determined for this thread.".to_string()
    })?;
    if !forwarding_enabled {
        return if enabled {
            Err("Cyber access programs are disabled for this session.".into())
        } else {
            Ok(None)
        };
    }
    Ok(Some(program))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Notice {
    Undetermined,
    Unavailable,
    Advertised,
    Disabled,
    Enabled,
}

pub(crate) fn notice_for_setting(
    models: &[ModelPreset],
    model: &str,
    enabled: bool,
    can_enable_daybreak: bool,
) -> Notice {
    if enabled {
        return Notice::Enabled;
    }
    let programs = models
        .iter()
        .find(|entry| entry.model == model)
        .and_then(|entry| entry.available_access_programs.as_ref());
    match programs {
        Some(programs) if programs.daybreak().is_some() => {
            if can_enable_daybreak {
                Notice::Disabled
            } else {
                Notice::Advertised
            }
        }
        Some(programs) if programs.standard().is_some() => Notice::Unavailable,
        _ => Notice::Undetermined,
    }
}

pub(crate) fn available(models: &[ModelPreset]) -> bool {
    models.iter().any(|model| {
        model
            .available_access_programs
            .as_ref()
            .and_then(codex_protocol::openai_models::ModelAccessPrograms::daybreak)
            .is_some()
    })
}

/// Missing or empty program lists do not establish that the account lacks access.
pub(crate) fn availability(models: &[ModelPreset]) -> Option<bool> {
    if available(models) {
        return Some(true);
    }
    if models.is_empty() {
        return None;
    }
    models
        .iter()
        .all(|model| {
            model
                .available_access_programs
                .as_ref()
                .is_some_and(|programs| !programs.cyber.is_empty())
        })
        .then_some(false)
}

#[cfg(test)]
#[path = "daybreak_tests.rs"]
mod tests;
