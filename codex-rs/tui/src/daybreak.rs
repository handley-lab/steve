//! Account-scoped Daybreak eligibility and per-turn program selection.
//! Pending or failed discovery never implies access.

use codex_protocol::openai_models::ModelPreset;
use codex_protocol::turn_input::CyberAccessProgram;

pub(crate) fn program_for_turn(
    models: &[ModelPreset],
    model: &str,
    eligible_account: bool,
    enabled: bool,
) -> Result<Option<CyberAccessProgram>, String> {
    if !eligible_account {
        return if enabled {
            Err("Daybreak requires the OpenAI provider with a supplied token and Daybreak support enabled. Turn it off to continue.".into())
        } else {
            Ok(None)
        };
    }
    let programs = models
        .iter()
        .find(|entry| entry.model == model)
        .and_then(|entry| entry.available_access_programs.as_ref())
        .filter(|programs| !programs.cyber.is_empty());
    let Some(programs) = programs else {
        // Missing discovery metadata preserves the server's automatic treatment.
        return Ok(None);
    };
    if enabled {
        programs
            .daybreak()
            .map(Some)
            .ok_or_else(|| format!("Daybreak support for model {model} could not be confirmed by the connected server. Use /daybreak to turn it off, or choose a compatible model and server."))
    } else {
        Ok(programs.standard())
    }
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
